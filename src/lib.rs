//! PipeWire video capture for Python.
//!
//! This crate provides Python bindings for capturing video frames from
//! PipeWire streams using the xdg-desktop-portal ScreenCast interface.

// False positive in Rust 1.85 clippy for PyO3 return type annotations
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use std::sync::Once;
use tracing_subscriber::EnvFilter;

mod error;
mod portal;
mod stream;

pub use error::CaptureError;
pub use portal::{PortalCapture, PortalSession};
pub use stream::CaptureStream;

static LOGGING_INIT: Once = Once::new();

/// Initialize logging for the pipewire-capture library.
///
/// Args:
///     level: Log level - "error", "warn", "info", "debug", or "trace".
///            Defaults to "info". Can be overridden by RUST_LOG env var.
///
/// Example:
///     init_logging()        # info level
///     init_logging("debug") # debug level for troubleshooting
#[pyfunction]
#[pyo3(signature = (level="info"))]
fn init_logging(level: &str) {
    LOGGING_INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("pipewire_capture={}", level)));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    });
}

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
    m.add_function(wrap_pyfunction!(init_logging, m)?)?;
    m.add_class::<PortalCapture>()?;
    m.add_class::<PortalSession>()?;
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
