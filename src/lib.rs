//! Async Rust SDK for the [bunny.net](https://bunny.net) APIs.
//!
//! Each API has its own client, because each uses its own key and hostname:
//!
//! | Module | API | Key |
//! |---|---|---|
//! | [`core`] | Pull zones, DNS, storage zones, video libraries, billing, ... | account API key |
//! | [`compute`] | Edge Scripting | account API key |
//! | [`shield`] | WAF, rate limiting, bot detection, access lists | account API key |
//! | [`containers`] | Magic Containers | account API key |
//! | [`database`] | Managed libSQL databases | account API key |
//! | [`logging`] | CDN request logs | account API key |
//! | [`origin_errors`] | CDN origin error logs | account API key |
//! | [`partner`] | Reseller customers, pricing, billing | partner account API key |
//! | [`stream`] | Videos, collections, live streams, resumable uploads | video library API key |
//! | [`storage`] | Edge Storage files | storage zone password |
//!
//! Every module is behind a Cargo feature of the same name (`origin-errors`
//! for [`origin_errors`]); all are enabled by default.
//!
//! All clients share [`Config`] (timeouts, retries, custom HTTP client) and
//! return [`Error`], which normalises the different error formats of the
//! bunny.net APIs into [`ApiError`].
//!
//! ```no_run
//! # async fn run() -> bunnysdk::Result<()> {
//! use bunnysdk::core::{CoreClient, ListPullZonesQuery};
//!
//! let core = CoreClient::new("account-api-key")?;
//! let zones = core.list_pull_zones(&ListPullZonesQuery::default()).await?;
//! for zone in zones {
//!     println!("{:?}", zone.name);
//! }
//!
//! match core.get_pull_zone(123, &Default::default()).await {
//!     Ok(zone) => println!("{:?}", zone.name),
//!     Err(e) if e.is_not_found() => println!("no such pull zone"),
//!     Err(e) => return Err(e),
//! }
//! # Ok(()) }
//! ```
//!
//! Most modules are generated from bunny.net's official OpenAPI specs, so
//! their types and methods follow the API reference at
//! <https://docs.bunny.net/reference>. Fields the spec does not mark as
//! required are `Option`s, and enums have an `Other(..)` variant for values
//! added after this SDK was released. Decimal amounts are `f64` (about 15
//! significant digits).
//!
//! Requests are logged with [`tracing`](https://docs.rs/tracing) at debug
//! level (method, path, status, duration). Keys and bodies are never logged.

// Helpers in these modules go unused when some API features are disabled.
#[macro_use]
#[allow(unused_macros, dead_code)]
mod macros;
#[allow(dead_code)]
mod transport;

mod config;
mod error;

#[cfg(feature = "compute")]
#[path = "generated/compute.rs"]
pub mod compute;
#[cfg(feature = "containers")]
#[path = "generated/containers.rs"]
pub mod containers;
#[cfg(feature = "core")]
#[path = "generated/core.rs"]
pub mod core;
#[cfg(feature = "database")]
#[path = "generated/database.rs"]
pub mod database;
#[cfg(feature = "logging")]
pub mod logging;
#[cfg(feature = "origin-errors")]
#[path = "generated/origin_errors.rs"]
pub mod origin_errors;
#[cfg(feature = "partner")]
#[path = "generated/partner.rs"]
pub mod partner;
#[cfg(feature = "shield")]
#[path = "generated/shield.rs"]
pub mod shield;
#[cfg(feature = "storage")]
pub mod storage;
#[cfg(feature = "stream")]
#[path = "generated/stream.rs"]
pub mod stream;
#[cfg(feature = "stream")]
mod stream_upload;

pub use config::Config;
pub use error::{ApiError, Error, Result, ValidationError};

/// Lower-case hex SHA-256 of `data`.
#[cfg(any(feature = "storage", feature = "stream"))]
fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The `reqwest` version this SDK uses, for building a client to pass to
/// [`Config::http_client`] or a [`reqwest::Body`] to upload.
pub use reqwest;
