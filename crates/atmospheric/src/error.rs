//! One error enum for the whole protocol layer.

use thiserror::Error;

/// Every failure the protocol layer reports to the game side, as a message.
#[derive(Debug, Error)]
pub enum AtmosphericError {
    #[error("serialisation failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("file could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("not logged in")]
    NotLoggedIn,
    #[error("protocol call failed: {0}")]
    Protocol(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("{0}")]
    Message(String),
}

impl AtmosphericError {
    /// Wrap any protocol error. The XRPC error types are generic per endpoint
    /// (`atrium_xrpc::Error<E>`), so a single `#[from]` cannot cover them.
    pub fn protocol<E: std::error::Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Protocol(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, AtmosphericError>;
