//! One error type for everything that can go wrong talking to GitHub.

use std::fmt;

/// Why an operation failed, in terms the person using the IDE can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The request never got an answer (offline, DNS, blocked by the browser).
    Network(String),
    /// GitHub rejected the token.
    Unauthorized,
    /// The token is valid but may not do this.
    Forbidden(String),
    /// The repository, branch or file does not exist (or the token cannot see it).
    NotFound(String),
    /// Too many requests. `reset_epoch` is when GitHub will accept more.
    RateLimited { reset_epoch: Option<u64> },
    /// GitHub understood the request and refused it, for example a branch
    /// update that is not a fast-forward.
    Rejected(String),
    /// Any other HTTP failure.
    Api { status: u16, message: String },
    /// GitHub's answer was not what the API documents.
    Decode(String),
    /// Files changed on GitHub since they were opened here. Nothing was written.
    Conflict(Vec<String>),
    /// The request does not make sense, for example an empty commit message.
    Invalid(String),
    /// The person pressed Stop.
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(detail) => write!(f, "Could not reach GitHub: {detail}"),
            Self::Unauthorized => write!(f, "GitHub did not accept the token. Sign in again."),
            Self::Forbidden(detail) => write!(f, "GitHub refused: {detail}"),
            Self::NotFound(what) => write!(f, "Not found: {what}"),
            Self::RateLimited {
                reset_epoch: Some(epoch),
            } => {
                write!(
                    f,
                    "GitHub rate limit reached. It resets at Unix time {epoch}."
                )
            }
            Self::RateLimited { reset_epoch: None } => write!(f, "GitHub rate limit reached."),
            Self::Rejected(detail) => write!(f, "GitHub rejected the change: {detail}"),
            Self::Api { status, message } => write!(f, "GitHub error {status}: {message}"),
            Self::Decode(detail) => write!(f, "Unexpected answer from GitHub: {detail}"),
            Self::Conflict(paths) => write!(
                f,
                "Changed on GitHub since you opened {}: {}. Nothing was written.",
                if paths.len() == 1 { "it" } else { "them" },
                paths.join(", ")
            ),
            Self::Invalid(detail) => write!(f, "{detail}"),
            Self::Cancelled => write!(f, "Stopped."),
        }
    }
}

impl std::error::Error for Error {}

/// Shorthand used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
