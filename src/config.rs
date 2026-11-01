use std::time::Duration;

/// Settings shared by every API client.
///
/// ```
/// use std::time::Duration;
/// let config = bunnysdk::Config::new()
///     .timeout(Duration::from_secs(60))
///     .max_retries(2);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    pub(crate) base_url: Option<String>,
    pub(crate) http_client: Option<reqwest::Client>,
    pub(crate) user_agent: String,
    pub(crate) timeout: Duration,
    pub(crate) max_retries: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: None,
            http_client: None,
            user_agent: concat!("bunnysdk-rs/", env!("CARGO_PKG_VERSION")).to_owned(),
            timeout: Duration::from_secs(30),
            max_retries: 0,
        }
    }
}

impl Config {
    /// Same as [`Config::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the API base URL (for proxies, staging, or tests).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Uses your own `reqwest::Client` (connection pool, proxy, TLS settings).
    ///
    /// The SDK then leaves timeouts to that client: [`Config::timeout`] only
    /// applies to the client the SDK builds itself.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Sets the `User-Agent` header. Defaults to `bunnysdk-rs/<version>`.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Fails a request when the connection stays silent this long.
    /// Defaults to 30 seconds.
    ///
    /// The timer restarts on every chunk received, so large uploads and
    /// downloads still work while slow or hung connections are cut off.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Retries failed requests up to `n` times. Defaults to 0 (off).
    ///
    /// Retried: HTTP 429 (honouring `Retry-After`), and for idempotent
    /// methods (GET, HEAD, PUT, DELETE) HTTP 502/503/504, connection errors
    /// and timeouts. Streaming request bodies are never retried.
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }
}
