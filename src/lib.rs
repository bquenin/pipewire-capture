//! PipeWire video capture for Python.
//!
//! This crate provides Python bindings for capturing video frames from
//! PipeWire streams using the xdg-desktop-portal ScreenCast interface.

use pyo3::prelude::*;

mod portal;
mod stream;
mod error;

pub use portal::PortalCapture;
pub use stream::CaptureStream;
pub use error::CaptureError;

/// Check if PipeWire capture is available on this system.
///
/// Returns True if running on Wayland with xdg-desktop-portal support.
#[pyfunction]
fn is_available() -> bool {
    // Check for WAYLAND_DISPLAY environment variable
    std::env::var("WAYLAND_DISPLAY").is_ok()
    // TODO: Also check for portal availability via D-Bus
}

/// Python module definition.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(is_available, m)?)?;
    m.add_class::<PortalCapture>()?;
    m.add_class::<CaptureStream>()?;
    Ok(())
}
