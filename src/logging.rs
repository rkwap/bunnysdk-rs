//! CDN request logs (<https://bunny.net/docs/cdn/logging>), kept for 3 days.
//!
//! bunny.net publishes no OpenAPI spec for this API, so this module is
//! hand-written from the documentation.
//!
//! ```no_run
//! # async fn run() -> bunnysdk::Result<()> {
//! use bunnysdk::logging::{LogQuery, LoggingClient};
//!
//! let logging = LoggingClient::new("account-api-key")?;
//! let query = LogQuery {
//!     status: Some("4xx,5xx".into()),
//!     limit: Some(100),
//!     ..Default::default()
//! };
//! let page = logging.get_logs(1337, &query).await?;
//! for entry in &page.data {
//!     println!("{} {} {}", entry.timestamp, entry.status_code, entry.url);
//! }
//! # Ok(()) }
//! ```

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::Result;
use crate::transport::{Transport, segment};

/// Default API base URL.
pub const BASE_URL: &str = "https://logging.bunnycdn.com";

/// Client for the bunny.net Logging API.
///
/// Authenticates with your account API key. Cheap to clone; clones share the
/// connection pool.
#[derive(Debug, Clone)]
pub struct LoggingClient {
    transport: Transport,
}

impl LoggingClient {
    /// Creates a client authenticated with your account API key.
    pub fn new(api_key: &str) -> Result<Self> {
        Self::with_config(api_key, Config::default())
    }

    /// Like [`LoggingClient::new`], with custom settings.
    pub fn with_config(api_key: &str, config: Config) -> Result<Self> {
        Ok(Self {
            transport: Transport::new(api_key, BASE_URL, config)?,
        })
    }

    /// Structured request logs for a pull zone (Logging API v2).
    ///
    /// Page through results by raising `offset` by `limit` while
    /// `pagination.has_more` is true. Rate limit: 30 requests / 10 s per pull zone.
    ///
    /// `GET /v2/pullzones/{pullZoneId}/logs`
    pub async fn get_logs(&self, pull_zone_id: i64, query: &LogQuery) -> Result<LogPage> {
        let path = format!("/v2/pullzones/{}/logs", segment(pull_zone_id));
        let request = self
            .transport
            .request_with_query(Method::GET, &path, query)?;
        self.transport.json(request).await
    }

    /// Raw pipe-separated log lines for one day (legacy Logging API v1).
    ///
    /// `date` is formatted `MM-DD-YY`, e.g. `03-07-26`. Each line is
    /// `cache status|status|timestamp ms|bytes|pull zone id|ip|referer|url|edge location|user agent|request id|country`.
    ///
    /// `GET /{MM-DD-YY}/{pullZoneId}.log`
    pub async fn get_raw_logs(
        &self,
        pull_zone_id: i64,
        date: &str,
        query: &RawLogQuery,
    ) -> Result<String> {
        let path = format!("/{}/{}.log", segment(date), segment(pull_zone_id));
        let request = self
            .transport
            .request_with_query(Method::GET, &path, query)?;
        self.transport.text(request).await
    }
}

/// Filters for [`LoggingClient::get_logs`]. All fields are optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogQuery {
    /// Inclusive start, ISO 8601 UTC (e.g. `2026-05-08T00:00:00Z`). Defaults to `to` minus 24h.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Exclusive end, ISO 8601 UTC. Defaults to now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Comma list of exact codes or classes, e.g. `404,5xx`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Comma list, e.g. `HIT,MISS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_status: Option<String>,
    /// Comma list of ISO 3166 alpha-2 codes, e.g. `EE,DE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ip: Option<String>,
    /// Case-insensitive substring of host + path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent_contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referer_contains: Option<String>,
    /// Space-separated free-text tokens; a row matches if any token matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Include edge-to-origin-shield requests. Defaults to false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_origin_shield: Option<bool>,
    /// Entries per page: default 100, max 10000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// `asc` or `desc` (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// One page of log entries.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct LogPage {
    pub data: Vec<LogEntry>,
    pub pagination: LogPagination,
    /// The query as the server understood it.
    pub query: serde_json::Value,
}

/// Paging state of a [`LogPage`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct LogPagination {
    pub offset: u64,
    pub limit: u64,
    pub returned: u64,
    pub has_more: bool,
}

/// One logged request. Absent values are `None` or empty.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct LogEntry {
    /// ISO 8601 UTC.
    pub timestamp: String,
    pub pull_zone_id: i64,
    pub request_id: String,
    /// `HIT`, `MISS`, `BYPASS`, `REVALIDATED`, `STALE`, `UPDATING` or `-`.
    pub cache_status: String,
    pub status_code: u16,
    pub bytes_sent: u64,
    /// Anonymised unless disabled in the pull zone settings.
    pub remote_ip: String,
    pub country_code: String,
    pub edge_location: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    pub url: String,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub ja4_fingerprint: Option<String>,
    pub asn: Option<i64>,
    pub asn_organization: Option<String>,
    /// Extended logging only.
    pub body_bytes_sent: Option<u64>,
    /// Extended logging only.
    pub content_range: Option<String>,
    /// Extended logging only.
    pub authorization_header: Option<String>,
}

/// Filters for [`LoggingClient::get_raw_logs`]. All fields are optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RawLogQuery {
    /// Lines to skip from the beginning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    /// End index of lines to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    /// `asc` or `desc` (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Status classes, e.g. `4,5` (default `2,3,4,5`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Only lines containing this string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Return logs as a downloadable file (server default: true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<bool>,
}
