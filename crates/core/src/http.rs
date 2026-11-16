//! The one seam between this crate and the network.

use std::future::Future;
use std::pin::Pin;

/// A boxed future that does not have to be `Send`: the browser is one thread.
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// HTTP methods the GitHub calls in this crate use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Patch,
}

impl Method {
    /// The method as it appears on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
        }
    }
}

/// A request to the GitHub REST API. `path` starts with `/` and is relative
/// to `https://api.github.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub body: Option<String>,
}

/// What came back. Rate-limit numbers come from GitHub's response headers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub rate_limit_remaining: Option<u64>,
    pub rate_limit_reset: Option<u64>,
}

/// Sends requests. The app implements this with `fetch`; tests and the demo
/// use [`crate::fake::FakeGitHub`].
pub trait Http {
    /// Sends one request. `Err` means no HTTP answer arrived at all.
    fn send(&self, request: Request) -> LocalBoxFuture<'_, Result<Response, String>>;
}
