//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use crate::error::CaptureError;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::desktop::PersistMode;
use pyo3::prelude::*;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::runtime::Runtime;
use tracing::{debug, error, info};

/// Result of a successful portal flow.
struct PortalResult {
    fd: OwnedFd,
    node_id: u32,
    width: i32,
    height: i32,
}

/// Portal-based window selection for screen capture.
///
/// Uses xdg-desktop-portal ScreenCast interface to show a system
/// window picker dialog and obtain a PipeWire stream for the
/// selected window.
#[pyclass]
#[derive(Default)]
pub struct PortalCapture {
    owned_fd: Option<OwnedFd>,
    node_id: Option<u32>,
    width: Option<i32>,
    height: Option<i32>,
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

    info!(node_id, width, height, "Portal flow completed successfully");

    Ok(PortalResult {
        fd,
        node_id,
        width,
        height,
    })
}

#[pymethods]
impl PortalCapture {
    /// Create a new PortalCapture instance.
    #[new]
    pub fn new() -> PyResult<Self> {
        Ok(Self::default())
    }

    /// Start the window selection flow.
    ///
    /// This will show the system window picker dialog. The provided
    /// callback will be called with True on success or False on
    /// cancellation/error.
    ///
    /// Note: This is a blocking operation that runs the D-Bus main loop.
    pub fn select_window(&mut self, callback: PyObject) -> PyResult<()> {
        // Release GIL before blocking D-Bus operations
        let result: Result<PortalResult, CaptureError> = Python::with_gil(|py| {
            py.allow_threads(|| {
                let rt = Runtime::new().map_err(|e| CaptureError::DBus(e.to_string()))?;
                rt.block_on(run_portal_flow())
            })
        });

        // Store results if successful, log errors
        let success = match result {
            Ok(portal_result) => {
                self.owned_fd = Some(portal_result.fd);
                self.node_id = Some(portal_result.node_id);
                self.width = Some(portal_result.width);
                self.height = Some(portal_result.height);
                true
            }
            Err(e) => {
                error!(error = %e, "Portal flow failed");
                false
            }
        };

        // Call callback with result
        Python::with_gil(|py| {
            callback.call1(py, (success,))?;
            Ok(())
        })
    }

    /// Get the PipeWire stream info after successful window selection.
    ///
    /// Returns a tuple of (fd, node_id, width, height) or None if no stream is available.
    pub fn get_stream_info(&self) -> Option<(i32, u32, i32, i32)> {
        match (&self.owned_fd, self.node_id, self.width, self.height) {
            (Some(fd), Some(node_id), Some(w), Some(h)) => Some((fd.as_raw_fd(), node_id, w, h)),
            _ => None,
        }
    }

    /// Close the portal session and release resources.
    pub fn close(&mut self) -> PyResult<()> {
        debug!("Closing portal capture");
        self.owned_fd = None;
        self.node_id = None;
        self.width = None;
        self.height = None;
        Ok(())
    }
}
