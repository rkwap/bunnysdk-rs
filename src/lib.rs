//! Async Rust SDK for the [bunny.net](https://bunny.net) APIs.
//!
//! Each API has its own client, because each uses its own key and hostname:
//!
//! | Module | API | Key |
//! |---|---|---|
//! | [`storage`] | Edge Storage | storage zone password |
//!
//! All clients share [`Config`] (timeouts, retries, custom HTTP client) and
//! return [`Error`], which normalises the different error formats of the
//! bunny.net APIs into [`ApiError`].
//!
//! ```no_run
//! # async fn run() -> bunnysdk::Result<()> {
//! use bunnysdk::storage::{Region, StorageClient};
//!
//! let storage = StorageClient::new("my-zone", "zone-password", Region::Frankfurt)?;
//! match storage.download("missing.txt").await {
//!     Ok(bytes) => println!("{} bytes", bytes.len()),
//!     Err(e) if e.is_not_found() => println!("no such file"),
//!     Err(e) => return Err(e),
//! }
//! # Ok(()) }
//! ```
//!
//! Requests are logged with [`tracing`](https://docs.rs/tracing) at debug
//! level (method, path, status, duration). Keys and bodies are never logged.

mod config;
mod error;
mod transport;

pub mod storage;

pub use config::Config;
pub use error::{ApiError, Error, Result, ValidationError};

/// The `reqwest` version this SDK uses, for building a client to pass to
/// [`Config::http_client`] or a [`reqwest::Body`] to upload.
pub use reqwest;
