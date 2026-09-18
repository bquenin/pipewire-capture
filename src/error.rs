//! Error types for PipeWire capture.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use thiserror::Error;

// PyO3 0.22's exception macro checks its legacy gil-refs feature in this crate.
#[allow(unexpected_cfgs)]
mod python_errors {
    pyo3::create_exception!(
        pipewire_capture,
        UnsupportedCompositorError,
        pyo3::exceptions::PyRuntimeError,
        "KWin's current compositing backend cannot provide a screen capture stream."
    );
}
pub use python_errors::UnsupportedCompositorError;

/// Errors that can occur during capture operations.
#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("Portal not available: {0}")]
    PortalNotAvailable(String),

    #[error("Session creation failed: {0}")]
    SessionFailed(String),

    #[error("Portal {stage} failed: {source}")]
    PortalFailed {
        stage: &'static str,
        source: ashpd::Error,
    },

    #[error(
        "KDE screen capture requires OpenGL compositing; KWin reports '{backend}'. \
         Update KDE and your graphics drivers, then log out and back in. \
         If the problem persists, check KWin's logs or use an X11 desktop session.\n\
         Portal Start failed: {source}"
    )]
    UnsupportedCompositor {
        backend: String,
        source: ashpd::Error,
    },

    #[error("User cancelled window selection")]
    UserCancelled,

    #[error("No stream available")]
    NoStream,

    #[error("PipeWire error: {0}")]
    PipeWire(String),

    #[error("D-Bus error: {0}")]
    DBus(String),

    #[error("Stream already started")]
    AlreadyStarted,

    #[error("Failed to spawn thread: {0}")]
    ThreadSpawnFailed(String),

    #[error("Frame size mismatch: expected {expected}, got {actual}")]
    FrameSizeMismatch { expected: usize, actual: usize },
}

impl From<CaptureError> for PyErr {
    fn from(err: CaptureError) -> PyErr {
        match err {
            CaptureError::UnsupportedCompositor { .. } => {
                UnsupportedCompositorError::new_err(err.to_string())
            }
            _ => PyRuntimeError::new_err(err.to_string()),
        }
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
