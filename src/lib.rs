//! PipeWire video capture for Python.
//!
//! This crate provides Python bindings for capturing video frames from
//! PipeWire streams using the xdg-desktop-portal ScreenCast interface.

use pyo3::prelude::*;

mod error;
mod gpu;
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

    // Note: These tests modify environment variables, so they must run serially.
    // We combine them into one test to avoid race conditions.
    #[test]
    fn test_is_available() {
        // Save original value
        let original = std::env::var("WAYLAND_DISPLAY").ok();

        // Test without WAYLAND_DISPLAY
        std::env::remove_var("WAYLAND_DISPLAY");
        assert!(
            !is_available(),
            "Should return false when WAYLAND_DISPLAY is not set"
        );

        // Test with WAYLAND_DISPLAY
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert!(
            is_available(),
            "Should return true when WAYLAND_DISPLAY is set"
        );

        // Restore original value
        match original {
            Some(val) => std::env::set_var("WAYLAND_DISPLAY", val),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }
}
