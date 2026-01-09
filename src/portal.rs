//! xdg-desktop-portal ScreenCast integration.
//!
//! This module handles the D-Bus communication with xdg-desktop-portal
//! to show a window picker and obtain a PipeWire stream.

use pyo3::prelude::*;

use crate::error::CaptureError;

/// Portal-based window selection for screen capture.
///
/// Uses xdg-desktop-portal ScreenCast interface to show a system
/// window picker dialog and obtain a PipeWire stream for the
/// selected window.
#[pyclass]
pub struct PortalCapture {
    // TODO: Add fields for:
    // - D-Bus connection
    // - Session path
    // - Stream info (fd, node_id)
    fd: Option<i32>,
    node_id: Option<u32>,
}

#[pymethods]
impl PortalCapture {
    /// Create a new PortalCapture instance.
    #[new]
    pub fn new() -> PyResult<Self> {
        // TODO: Initialize D-Bus connection
        Ok(Self {
            fd: None,
            node_id: None,
        })
    }

    /// Start the window selection flow.
    ///
    /// This will show the system window picker dialog. The provided
    /// callback will be called with True on success or False on
    /// cancellation/error.
    ///
    /// Note: This is a blocking operation that runs the D-Bus main loop.
    pub fn select_window(&mut self, callback: PyObject) -> PyResult<()> {
        // TODO: Implement portal flow:
        // 1. CreateSession
        // 2. SelectSources (type=WINDOW)
        // 3. Start (shows picker)
        // 4. OpenPipeWireRemote (get fd)

        // For now, just call the callback with false
        Python::with_gil(|py| {
            callback.call1(py, (false,))?;
            Ok(())
        })
    }

    /// Get the PipeWire stream info after successful window selection.
    ///
    /// Returns a tuple of (fd, node_id) or None if no stream is available.
    pub fn get_stream_info(&self) -> Option<(i32, u32)> {
        match (self.fd, self.node_id) {
            (Some(fd), Some(node_id)) => Some((fd, node_id)),
            _ => None,
        }
    }

    /// Close the portal session and release resources.
    pub fn close(&mut self) -> PyResult<()> {
        // TODO: Close D-Bus session
        // TODO: Close PipeWire fd
        self.fd = None;
        self.node_id = None;
        Ok(())
    }
}

impl Default for PortalCapture {
    fn default() -> Self {
        Self {
            fd: None,
            node_id: None,
        }
    }
}
