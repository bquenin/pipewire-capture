//! Error types for PipeWire capture.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use thiserror::Error;

/// Errors that can occur during capture operations.
#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("Portal not available: {0}")]
    PortalNotAvailable(String),

    #[error("Session creation failed: {0}")]
    SessionFailed(String),

    #[error("User cancelled window selection")]
    UserCancelled,

    #[error("No stream available")]
    NoStream,

    #[error("PipeWire error: {0}")]
    PipeWire(String),

    #[error("D-Bus error: {0}")]
    DBus(String),
}

impl From<CaptureError> for PyErr {
    fn from(err: CaptureError) -> PyErr {
        PyRuntimeError::new_err(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = CaptureError::UserCancelled;
        assert_eq!(err.to_string(), "User cancelled window selection");
    }

    #[test]
    fn test_portal_not_available_error() {
        let err = CaptureError::PortalNotAvailable("test reason".to_string());
        assert_eq!(err.to_string(), "Portal not available: test reason");
    }

    #[test]
    fn test_pipewire_error() {
        let err = CaptureError::PipeWire("connection failed".to_string());
        assert_eq!(err.to_string(), "PipeWire error: connection failed");
    }
}
