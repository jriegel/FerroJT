//! Crate wide error / result types.

/// Result alias used throughout FerroJT.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while reading, modelling or writing a file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON (glTF) document could not be read or written.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The requested container format has no reader/writer yet.
    #[error("format '{0}' is not supported")]
    UnsupportedFormat(String),

    /// The source model uses something the target format cannot express.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// A code path is part of the planned design but not implemented yet.
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),

    /// The file is malformed, truncated, or uses a feature we cannot parse.
    #[error("invalid data at offset {offset:#010x}: {message}")]
    Invalid {
        /// Byte offset in the stream where the problem was detected.
        offset: u64,
        /// Human readable description of the problem.
        message: String,
    },

    /// A checksum or signature did not match.
    #[error("checksum mismatch in {0}")]
    Corrupt(String),
}

impl Error {
    /// Convenience constructor for [`Error::Invalid`].
    pub fn invalid(offset: u64, message: impl Into<String>) -> Self {
        Self::Invalid {
            offset,
            message: message.into(),
        }
    }

    /// Convenience constructor for [`Error::NotImplemented`].
    pub fn not_implemented(what: &'static str) -> Self {
        Self::NotImplemented(what)
    }
}
