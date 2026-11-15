//! The parts of the IDE that do not draw anything.
//!
//! Everything here is plain Rust with no browser or GPUI dependency, so it is
//! tested natively with `cargo test`. The app crate supplies two things: an
//! [`http::Http`] implementation that calls `fetch`, and somewhere to keep the
//! JSON that [`workspace::Workspace::export_drafts`] produces.

// Every fallible function returns `error::Error`, whose variants are documented
// once on the type; repeating them under each function would add nothing.
#![allow(clippy::missing_errors_doc)]

pub mod diff;
pub mod error;
pub mod fake;
pub mod github;
pub mod http;
pub mod palette;
pub mod path;
pub mod python;
pub mod screen;
pub mod shell;

pub mod workspace;

pub use error::Error;
