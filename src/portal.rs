//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use crate::error::CaptureError;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::{PersistMode, ResponseError, Session};
use pyo3::prelude::*;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// Global runtime for D-Bus operations.
/// Using a persistent runtime ensures D-Bus connections are properly maintained
/// across multiple select_window() calls.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("Failed to create tokio runtime"))
}

/// Result from the portal flow, including channels to close the session.
struct PortalResult {
    fd: OwnedFd,
    node_id: u32,
    width: i32,
    height: i32,
    close_tx: oneshot::Sender<()>,
    done_rx: oneshot::Receiver<()>,
}

/// A portal session that keeps the screen capture stream alive.
///
/// The session must remain open for the PipeWire stream to be valid.
/// Call `close()` when done capturing, or let it be garbage collected.
#[pyclass]
pub struct PortalSession {
    /// Own the portal descriptor even if no CaptureStream is ever constructed.
    fd: Option<OwnedFd>,
    /// PipeWire node ID for the stream.
    #[pyo3(get)]
    pub node_id: u32,
    /// Stream width in pixels.
    #[pyo3(get)]
    pub width: i32,
    /// Stream height in pixels.
    #[pyo3(get)]
    pub height: i32,
    /// Channel to signal session close. None if already closed.
    close_tx: Option<oneshot::Sender<()>>,
    /// Channel to receive close completion notification.
    done_rx: Option<oneshot::Receiver<()>>,
}

#[pymethods]
impl PortalSession {
    /// Close the portal session and release resources.
    ///
    /// This blocks until the session is fully closed.
    pub fn close(&mut self, py: Python<'_>) {
        self.fd.take();
        if let Some(tx) = self.close_tx.take() {
            debug!("Closing portal session");
            let _ = tx.send(());

            // Block and wait for close to complete
            if let Some(done_rx) = self.done_rx.take() {
                py.allow_threads(|| {
                    let _ = get_runtime()
                        .block_on(async { tokio::time::timeout(CLOSE_TIMEOUT, done_rx).await });
                });
            }
        }
    }

    /// Borrowed descriptor, or -1 after close. CaptureStream duplicates it.
    #[getter]
    pub fn fd(&self) -> i32 {
        self.fd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    /// Check if the session is still open.
    #[getter]
    pub fn is_open(&self) -> bool {
        self.close_tx.is_some()
    }

    fn __repr__(&self) -> String {
        format!(
            "PortalSession(fd={}, node_id={}, size={}x{}, open={})",
            self.fd(),
            self.node_id,
            self.width,
            self.height,
            self.is_open()
        )
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        // Never block Python's garbage collector or an in-flight window picker.
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Portal-based window selection for screen capture.
///
/// Uses xdg-desktop-portal ScreenCast interface to show a system
/// window picker dialog and obtain a PipeWire stream for the
/// selected window.
#[pyclass]
#[derive(Default)]
pub struct PortalCapture;

fn portal_error(stage: &'static str, source: ashpd::Error) -> CaptureError {
    if matches!(source, ashpd::Error::Response(ResponseError::Cancelled)) {
        CaptureError::UserCancelled
    } else {
        CaptureError::PortalFailed { stage, source }
    }
}

async fn close_session(session: &Session<'_, Screencast<'_>>) {
    match tokio::time::timeout(CLOSE_TIMEOUT, session.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("Failed to close portal session: {}", e),
        Err(_) => warn!("Timed out closing portal session"),
    }
}

async fn kwin_compositing_type() -> Option<String> {
    // Diagnostic only: unavailable services or an unresponsive bus must not
    // mask the original portal error or start a different compositor.
    tokio::time::timeout(Duration::from_secs(1), async {
        let connection = zbus::Connection::session().await.ok()?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.kde.KWin",
            "/Compositor",
            "org.freedesktop.DBus.Properties",
        )
        .await
        .ok()?;
        let value: zbus::zvariant::OwnedValue = proxy
            .call_with_flags(
                "Get",
                zbus::proxy::MethodFlags::NoAutoStart.into(),
                &("org.kde.kwin.Compositing", "compositingType"),
            )
            .await
            .ok()??;
        String::try_from(value).ok()
    })
    .await
    .ok()
    .flatten()
}

async fn diagnose_portal_error(error: CaptureError) -> CaptureError {
    // KDE returns only Other and an empty result when KWin refuses a stream.
    // Do not infer a compositor problem from unrelated errors or cancellation.
    if matches!(
        &error,
        CaptureError::PortalFailed {
            stage: "Start",
            source: ashpd::Error::Response(ResponseError::Other),
        }
    ) {
        if let Some(backend) = kwin_compositing_type().await {
            debug!(backend, "KWin compositing backend after portal failure");
            if matches!(backend.as_str(), "qpainter" | "none") {
                if let CaptureError::PortalFailed { source, .. } = error {
                    return CaptureError::UnsupportedCompositor { backend, source };
                }
            }
        }
    }
    error
}

/// Run the async portal flow to select a window.
async fn run_portal_flow() -> Result<PortalResult, CaptureError> {
    debug!("Starting portal flow");

    // 1. Create screencast proxy
    debug!("Creating screencast proxy");
    let screencast = Screencast::new()
        .await
        .map_err(|e| CaptureError::PortalNotAvailable(e.to_string()))?;

    // 2. Create session
    debug!("Creating session");
    let session = screencast
        .create_session()
        .await
        .map_err(|e| portal_error("CreateSession", e))?;

    // Keep ownership of the session until every setup step succeeds, so all
    // errors (including user cancellation) close it explicitly.
    let result = async {
        debug!("Selecting sources (window only)");
        let request = screencast
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Window.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(|e| portal_error("SelectSources", e))?;
        request
            .response()
            .map_err(|e| portal_error("SelectSources", e))?;

        debug!("Starting window picker");
        let response = screencast
            .start(&session, None)
            .await
            .map_err(|e| portal_error("Start", e))?;
        let streams = response.response().map_err(|e| portal_error("Start", e))?;
        let stream = streams.streams().first().ok_or(CaptureError::NoStream)?;
        let node_id = stream.pipe_wire_node_id();
        let (width, height) = stream.size().unwrap_or((0, 0));
        debug!(node_id, width, height, "Window selected");

        let fd = screencast
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| portal_error("OpenPipeWireRemote", e))?;
        Ok::<_, CaptureError>((fd, node_id, width, height))
    }
    .await;

    let (fd, node_id, width, height) = match result {
        Ok(result) => result,
        Err(error) => {
            close_session(&session).await;
            return Err(diagnose_portal_error(error).await);
        }
    };

    // Create channels for close signaling and completion notification
    let (close_tx, close_rx) = oneshot::channel::<()>();
    let (done_tx, done_rx) = oneshot::channel::<()>();

    // Spawn a task that keeps the session alive until close is signaled.
    tokio::spawn(async move {
        // Wait for close signal (or channel drop)
        match close_rx.await {
            Ok(()) => debug!("Session close requested"),
            Err(_) => warn!("Session close channel dropped without explicit close"),
        }

        // Explicitly close the session via D-Bus
        close_session(&session).await;

        // Drop screencast proxy
        drop(screencast);

        // Signal that close is complete
        let _ = done_tx.send(());
        debug!("Portal session task ending");
    });

    info!(
        node_id,
        width,
        height,
        fd = fd.as_raw_fd(),
        "Portal flow completed successfully"
    );

    Ok(PortalResult {
        fd,
        node_id,
        width,
        height,
        close_tx,
        done_rx,
    })
}

#[pymethods]
impl PortalCapture {
    /// Create a new PortalCapture instance.
    #[new]
    pub fn new() -> Self {
        Self
    }

    /// Show the system window picker and return a PortalSession.
    ///
    /// This is a blocking operation that shows the system window picker dialog.
    /// Returns a PortalSession on success, or None if the user cancelled.
    /// Raises an exception on error.
    ///
    /// The PortalSession keeps the stream alive. Call `session.close()` when
    /// done capturing, or let it be garbage collected.
    ///
    /// Example:
    ///     session = portal.select_window()
    ///     if session:
    ///         stream = CaptureStream(session.fd, session.node_id,
    ///                                session.width, session.height)
    ///         stream.start()
    ///         # ... capture frames ...
    ///         stream.stop()
    ///         session.close()
    pub fn select_window(&self) -> PyResult<Option<PortalSession>> {
        // Release GIL before blocking D-Bus operations
        let result =
            Python::with_gil(|py| py.allow_threads(|| get_runtime().block_on(run_portal_flow())));

        match result {
            Ok(info) => Ok(Some(PortalSession {
                fd: Some(info.fd),
                node_id: info.node_id,
                width: info.width,
                height: info.height,
                close_tx: Some(info.close_tx),
                done_rx: Some(info.done_rx),
            })),
            Err(CaptureError::UserCancelled) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
