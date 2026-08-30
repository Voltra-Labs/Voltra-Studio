//! The shared error type.
//!
//! Voltra components report failures instead of panicking. A capture device
//! that disappears, a decoder that chokes on a corrupt frame or a stream that
//! drops mid-broadcast are all *expected* conditions during a live show: the
//! engine has to degrade one component, not lose the session.

/// Result alias used across the workspace.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every failure a Voltra component can report.
///
/// The enum is `#[non_exhaustive]` because subsystems still to be written —
/// capture, encoding, transport — will add variants, and downstream matches
/// should keep compiling when they do.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A setting was missing, malformed or out of range.
    #[error("configuration error: {0}")]
    Config(String),

    /// A required capability is not available on this build or platform.
    ///
    /// Used when a feature was compiled out or the host lacks the hardware, so
    /// callers can fall back rather than fail.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// An underlying I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Build a [`Error::Config`] from anything printable.
    pub fn config(message: impl std::fmt::Display) -> Self {
        Error::Config(message.to_string())
    }

    /// Build an [`Error::Unsupported`] from anything printable.
    pub fn unsupported(message: impl std::fmt::Display) -> Self {
        Error::Unsupported(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn messages_read_well() {
        assert_eq!(
            Error::config("canvas width must be even").to_string(),
            "configuration error: canvas width must be even"
        );
        assert_eq!(
            Error::unsupported("VAAPI encoder").to_string(),
            "unsupported: VAAPI encoder"
        );
    }

    #[test]
    fn io_errors_convert_transparently() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "scene.toml");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "scene.toml");
    }
}
