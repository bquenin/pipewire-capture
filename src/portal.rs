//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use crate::error::CaptureError;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use pyo3::prelude::*;
use std::os::fd::AsRawFd;
use tokio::runtime::Runtime;
use tracing::{debug, info};

/// Portal-based window selection for screen capture.
///
/// Uses xdg-desktop-portal ScreenCast interface to show a system
/// window picker dialog and obtain a PipeWire stream for the
/// selected window.
#[pyclass]
#[derive(Default)]
pub struct PortalCapture;

/// Run the async portal flow to select a window.
async fn run_portal_flow() -> Result<(i32, u32, i32, i32), CaptureError> {
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
    // The original OwnedFd will be dropped when this function returns
    let dup_fd = unsafe { libc::dup(fd.as_raw_fd()) };
    if dup_fd < 0 {
        return Err(CaptureError::PipeWire("Failed to duplicate fd".to_string()));
    }

    info!(
        node_id,
        width,
        height,
        fd = dup_fd,
        "Portal flow completed successfully"
    );

    Ok((dup_fd, node_id, width, height))
}

#[pymethods]
impl PortalCapture {
    /// Create a new PortalCapture instance.
    #[new]
    pub fn new() -> Self {
        Self
    }

    /// Show the system window picker and return stream info.
    ///
    /// This is a blocking operation that shows the system window picker dialog.
    /// Returns a tuple of (fd, node_id, width, height) on success, or None if
    /// the user cancelled the selection. Raises an exception on error.
    ///
    /// The returned fd is a duplicated file descriptor that the caller owns.
    /// Pass it to CaptureStream to start capturing frames.
    pub fn select_window(&self) -> PyResult<Option<(i32, u32, i32, i32)>> {
        // Release GIL before blocking D-Bus operations
        let result = Python::with_gil(|py| {
            py.allow_threads(|| {
                let rt = Runtime::new().map_err(|e| CaptureError::DBus(e.to_string()))?;
                rt.block_on(run_portal_flow())
            })
        });

        match result {
            Ok(info) => Ok(Some(info)),
            Err(CaptureError::UserCancelled) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
