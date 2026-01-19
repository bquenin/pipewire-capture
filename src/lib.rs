//! PipeWire video capture for Python.
//!
//! This crate provides Python bindings for capturing video frames from
//! PipeWire streams using the xdg-desktop-portal ScreenCast interface.

// False positive in Rust 1.85 clippy for PyO3 return type annotations
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use std::sync::{Once, OnceLock};
use tracing_subscriber::EnvFilter;

mod error;
mod portal;
mod stream;

pub use error::CaptureError;
pub use portal::{PortalCapture, PortalSession};
pub use stream::CaptureStream;

static LOGGING_INIT: Once = Once::new();
static AVAILABILITY_CACHE: OnceLock<bool> = OnceLock::new();

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
/// Returns True if the ScreenCast portal is available via D-Bus.
/// This works on Wayland compositors including Gamescope (Steam Deck).
///
/// The result is cached since portal availability doesn't change during process lifetime.
#[pyfunction]
fn is_available() -> bool {
    *AVAILABILITY_CACHE.get_or_init(check_screencast_portal)
}

/// Check if ScreenCast portal is available via D-Bus introspection.
fn check_screencast_portal() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };

    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.DBus.Introspectable"),
        "Introspect",
        &(),
    )
    .ok()
    .and_then(|reply| reply.body().deserialize::<String>().ok())
    .map(|xml| xml.contains("org.freedesktop.portal.ScreenCast"))
    .unwrap_or(false)
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

    #[test]
    fn test_is_available_does_not_panic() {
        // is_available() checks D-Bus for ScreenCast portal availability.
        // We can't easily mock D-Bus, so just verify it doesn't panic.
        // On systems with xdg-desktop-portal, this returns true.
        // On systems without D-Bus session bus, this returns false.
        let _result = is_available();
    }
}
