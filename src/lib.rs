//! PipeWire video capture for Python.
//!
//! This crate provides Python bindings for capturing video frames from
//! PipeWire streams using the xdg-desktop-portal ScreenCast interface.

use pyo3::prelude::*;

mod error;
mod portal;
mod stream;

pub use error::CaptureError;
pub use portal::PortalCapture;
pub use stream::CaptureStream;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available_without_wayland() {
        // When WAYLAND_DISPLAY is not set, should return false
        std::env::remove_var("WAYLAND_DISPLAY");
        assert!(!is_available());
    }

    #[test]
    fn test_is_available_with_wayland() {
        // When WAYLAND_DISPLAY is set, should return true
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert!(is_available());
        // Clean up
        std::env::remove_var("WAYLAND_DISPLAY");
    }
}
