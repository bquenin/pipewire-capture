//! PipeWire stream capture.
//!
//! This module handles capturing video frames from a PipeWire stream.

use numpy::{PyArray3, PyArrayMethods};
use pyo3::prelude::*;

/// PipeWire-based video capture stream.
///
/// Captures frames from a PipeWire stream obtained via the portal.
/// Frames are returned as numpy arrays in BGRA format.
#[pyclass]
pub struct CaptureStream {
    fd: i32,
    node_id: u32,
    capture_interval: f64,
    running: bool,
    window_closed: bool,
    // TODO: Add fields for:
    // - PipeWire main loop
    // - Stream object
    // - Latest frame buffer
}

#[pymethods]
impl CaptureStream {
    /// Create a new capture stream.
    ///
    /// Args:
    ///     fd: PipeWire file descriptor from portal.
    ///     node_id: PipeWire node ID for the stream.
    ///     capture_interval: Target interval between frames in seconds.
    #[new]
    #[pyo3(signature = (fd, node_id, capture_interval=0.25))]
    pub fn new(fd: i32, node_id: u32, capture_interval: f64) -> PyResult<Self> {
        Ok(Self {
            fd,
            node_id,
            capture_interval,
            running: false,
            window_closed: false,
        })
    }

    /// Start capturing frames from the stream.
    pub fn start(&mut self) -> PyResult<()> {
        // TODO: Initialize PipeWire stream
        // - Connect to PipeWire with fd
        // - Create stream for node_id
        // - Start receiving frames
        self.running = true;
        Ok(())
    }

    /// Get the latest captured frame.
    ///
    /// Returns a numpy array of shape (height, width, 4) in BGRA format,
    /// or None if no frame is available yet.
    pub fn get_frame<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyArray3<u8>>>> {
        // TODO: Return the latest frame from the buffer
        // For now, return None
        Ok(None)
    }

    /// Check if the captured window has been closed.
    pub fn is_window_closed(&self) -> bool {
        self.window_closed
    }

    /// Stop capturing and release resources.
    pub fn stop(&mut self) -> PyResult<()> {
        // TODO: Stop PipeWire stream
        // TODO: Clean up resources
        self.running = false;
        Ok(())
    }
}
