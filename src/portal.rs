//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use crate::error::CaptureError;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use parking_lot::Mutex;
use pyo3::prelude::*;
use std::os::fd::AsRawFd;
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

/// Result from the portal flow, including a channel to close the session.
struct PortalResult {
    fd: i32,
    node_id: u32,
    width: i32,
    height: i32,
    close_tx: oneshot::Sender<()>,
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
}

#[pymethods]
impl PortalSession {
    /// Close the portal session and release resources.
    ///
    /// This should be called when done capturing. After closing,
    /// the PipeWire stream will become invalid.
    pub fn close(&mut self) {
        if let Some(tx) = self.close_tx.take() {
            debug!("Closing portal session");
            let _ = tx.send(());
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

    // 3. Select sources (window only)
    debug!("Selecting sources (window only)");
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Window.into(),
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

    // Duplicate the fd so caller gets independent ownership
    let dup_fd = unsafe { libc::dup(fd.as_raw_fd()) };
    if dup_fd < 0 {
        return Err(CaptureError::PipeWire("Failed to duplicate fd".to_string()));
    }

    // Create channel to signal session close
    let (close_tx, close_rx) = oneshot::channel::<()>();

    // Spawn a task that keeps the session alive until close is signaled.
    // The session must stay open for the PipeWire stream to remain valid.
    tokio::spawn(async move {
        // Move session and screencast into this task to keep them alive
        let _session = session;
        let _screencast = screencast;

        // Wait for close signal (or channel drop)
        match close_rx.await {
            Ok(()) => debug!("Session close requested"),
            Err(_) => warn!("Session close channel dropped without explicit close"),
        }

        // Session will be dropped here, triggering cleanup
        debug!("Portal session task ending, session will be closed");
    });

    info!(
        node_id,
        width,
        height,
        fd = dup_fd,
        "Portal flow completed successfully"
    );

    Ok(PortalResult {
        fd: dup_fd,
        node_id,
        width,
        height,
        close_tx,
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
            })),
            Err(CaptureError::UserCancelled) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
