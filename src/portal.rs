//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use crate::error::CaptureError;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use parking_lot::Mutex;
use pyo3::prelude::*;
use std::os::fd::IntoRawFd;
use std::sync::OnceLock;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

/// Global runtime for D-Bus operations.
/// Using a persistent runtime ensures D-Bus connections are properly maintained
/// across multiple select_window() calls.
static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();

fn get_runtime() -> &'static Mutex<Runtime> {
    RUNTIME.get_or_init(|| Mutex::new(Runtime::new().expect("Failed to create tokio runtime")))
}

/// Result from the portal flow, including channels to close the session.
struct PortalResult {
    fd: i32,
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
    /// PipeWire file descriptor.
    #[pyo3(get)]
    pub fd: i32,
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
    pub fn close(&mut self) {
        if let Some(tx) = self.close_tx.take() {
            debug!("Closing portal session");
            let _ = tx.send(());

            // Block and wait for close to complete
            if let Some(done_rx) = self.done_rx.take() {
                let rt = get_runtime().lock();
                let _ = rt.block_on(done_rx);
            }
        }
    }

    /// Check if the session is still open.
    #[getter]
    pub fn is_open(&self) -> bool {
        self.close_tx.is_some()
    }

    fn __repr__(&self) -> String {
        format!(
            "PortalSession(fd={}, node_id={}, size={}x{}, open={})",
            self.fd,
            self.node_id,
            self.width,
            self.height,
            self.is_open()
        )
    }
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        if self.close_tx.is_some() {
            debug!("PortalSession dropped, closing session");
            self.close();
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
        .map_err(|e| CaptureError::SessionFailed(e.to_string()))?;

    // 3. Select sources
    debug!("Selecting sources");
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            (SourceType::Monitor | SourceType::Window),
            false, // single selection
            None,  // no restore token
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| CaptureError::SessionFailed(e.to_string()))?;

    // 4. Start - shows window picker
    debug!("Starting window picker");
    let response = screencast
        .start(&session, None)
        .await
        .map_err(|e| CaptureError::SessionFailed(e.to_string()))?;

    let streams = response.response().map_err(|e| {
        if matches!(
            e,
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)
        ) {
            debug!("User cancelled window selection");
            CaptureError::UserCancelled
        } else {
            CaptureError::SessionFailed(e.to_string())
        }
    })?;

    // 5. Get first stream info
    let stream = streams.streams().first().ok_or(CaptureError::NoStream)?;
    let node_id = stream.pipe_wire_node_id();
    let (width, height) = stream.size().unwrap_or((0, 0));
    let position = stream.position();
    let source_type = stream.source_type();
    debug!(
        node_id,
        width,
        height,
        ?position,
        ?source_type,
        "Window selected"
    );

    // 6. Get PipeWire file descriptor
    debug!("Opening PipeWire remote");
    let fd = screencast
        .open_pipe_wire_remote(&session)
        .await
        .map_err(|e| CaptureError::PipeWire(e.to_string()))?;

    // Use into_raw_fd() to transfer ownership without duplicating.
    // CaptureStream will take sole ownership via from_raw_fd().
    let fd_raw = fd.into_raw_fd();

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
        if let Err(e) = session.close().await {
            warn!("Failed to close session: {}", e);
        }

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
        fd = fd_raw,
        "Portal flow completed successfully"
    );

    Ok(PortalResult {
        fd: fd_raw,
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
        let result = Python::with_gil(|py| {
            py.allow_threads(|| {
                let rt = get_runtime().lock();
                rt.block_on(run_portal_flow())
            })
        });

        match result {
            Ok(info) => Ok(Some(PortalSession {
                fd: info.fd,
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
