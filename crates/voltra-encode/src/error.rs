//! Errors of the encoding crate.

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What can go wrong while encoding.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The settings cannot be encoded: odd size, zero bitrate, an input in the
    /// wrong format.
    #[error("invalid encoder settings: {0}")]
    Settings(String),

    /// A bitstream is not what it claims to be.
    #[error("malformed bitstream: {0}")]
    Bitstream(&'static str),

    /// The encoder was not compiled into this build.
    #[error("unavailable: {0}")]
    Unavailable(String),

    /// The native encoder reported an error.
    #[error("{call} failed: {message}")]
    Native {
        /// The call that failed.
        call: &'static str,
        /// What the library said.
        message: String,
    },

    /// Whoever received a packet refused it, for instance a muxer that could
    /// not write it. Encoding stops and the error is passed on.
    #[error("packet sink: {0}")]
    Sink(String),
}
