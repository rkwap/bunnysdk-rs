//! The HTTP layer shared by every API client: auth, retries, error mapping.

use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::Config;
use crate::error::{ApiError, Error, Result, truncate};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(10);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Characters left as-is in a URL path segment (RFC 3986 unreserved).
pub(crate) const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encodes a value for use as one URL path segment.
pub(crate) fn segment(value: impl std::fmt::Display) -> String {
    utf8_percent_encode(&value.to_string(), SEGMENT).to_string()
}

/// Turns a serializable query struct into `key=value` pairs.
/// `None` fields are skipped and arrays become repeated keys.
fn query_pairs<Q: Serialize>(query: &Q) -> Result<Vec<(String, String)>> {
    let value = serde_json::to_value(query)
        .map_err(|e| Error::InvalidInput(format!("invalid query parameters: {e}")))?;
    let Value::Object(map) = value else {
        return Err(Error::InvalidInput(
            "query parameters must be a struct".into(),
        ));
    };
    let scalar = |v: Value| match v {
        Value::String(s) => s,
        other => other.to_string(),
    };
    let mut pairs = Vec::new();
    for (key, value) in map {
        match value {
            Value::Null => {}
            Value::Array(items) => {
                pairs.extend(items.into_iter().map(|item| (key.clone(), scalar(item))));
            }
            other => pairs.push((key, scalar(other))),
        }
    }
    Ok(pairs)
}

#[derive(Debug, Clone)]
pub(crate) struct Transport {
    http: reqwest::Client,
    base_url: String,
    access_key: HeaderValue,
    user_agent: HeaderValue,
    max_retries: u32,
}

impl Transport {
    pub(crate) fn new(access_key: &str, default_base_url: &str, config: Config) -> Result<Self> {
        if access_key.trim().is_empty() {
            return Err(Error::InvalidInput("API key must not be empty".into()));
        }
        let mut access_key = HeaderValue::from_str(access_key)
            .map_err(|_| Error::InvalidInput("API key contains invalid characters".into()))?;
        access_key.set_sensitive(true);

        let user_agent = HeaderValue::from_str(&config.user_agent)
            .map_err(|_| Error::InvalidInput("user agent contains invalid characters".into()))?;

        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or(default_base_url)
            .trim_end_matches('/')
            .to_owned();
        reqwest::Url::parse(&base_url)
            .map_err(|e| Error::InvalidInput(format!("invalid base URL {base_url:?}: {e}")))?;

        let http = match config.http_client {
            Some(client) => client,
            None => reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(config.timeout)
                .build()?,
        };

        Ok(Self {
            http,
            base_url,
            access_key,
            user_agent,
            max_retries: config.max_retries,
        })
    }

    /// Starts a request to `path` (which must begin with `/`), with auth headers set.
    pub(crate) fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.authorize(
            self.http
                .request(method, format!("{}{}", self.base_url, path)),
        )
    }

    /// Like [`Transport::request`], with query parameters from a serializable struct.
    pub(crate) fn request_with_query<Q: Serialize>(
        &self,
        method: Method,
        path: &str,
        query: &Q,
    ) -> Result<RequestBuilder> {
        let mut url = Url::parse(&format!("{}{}", self.base_url, path))
            .map_err(|e| Error::InvalidInput(format!("invalid request path {path:?}: {e}")))?;
        let pairs = query_pairs(query)?;
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
        Ok(self.authorize(self.http.request(method, url)))
    }

    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header("AccessKey", self.access_key.clone())
            .header(USER_AGENT, self.user_agent.clone())
    }

    /// Starts a request to an absolute URL without the `AccessKey` header
    /// (for endpoints that authenticate differently, like TUS uploads).
    pub(crate) fn request_without_key(&self, method: Method, url: Url) -> RequestBuilder {
        self.http
            .request(method, url)
            .header(USER_AGENT, self.user_agent.clone())
    }

    /// The API key, for computing signatures. Never log it.
    pub(crate) fn api_key(&self) -> &str {
        self.access_key.to_str().unwrap_or_default()
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Sends a request, retrying when allowed. Non-success statuses become [`Error::Api`].
    pub(crate) async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let mut next = Some(request.build()?);
        let mut attempt = 0;
        loop {
            let request = next.take().expect("request is set before each attempt");
            let spare = if attempt < self.max_retries {
                request.try_clone()
            } else {
                None
            };
            let method = request.method().clone();
            let path = request.url().path().to_owned();
            let started = Instant::now();

            let result = self.http.execute(request).await;

            if spare.is_some()
                && let Some(wait) = retry_delay(&method, &result, attempt)
            {
                tracing::warn!(%method, path, attempt, ?wait, "retrying bunny.net request");
                tokio::time::sleep(wait).await;
                next = spare;
                attempt += 1;
                continue;
            }

            let response = result?;
            let status = response.status();
            tracing::debug!(
                %method,
                path,
                status = status.as_u16(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bunny.net request"
            );
            if status.is_success() {
                return Ok(response);
            }
            let body = response.bytes().await.unwrap_or_default();
            return Err(ApiError::from_body(status.as_u16(), &body).into());
        }
    }

    /// Sends a request and decodes a JSON response body.
    pub(crate) async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let response = self.send(request).await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response.bytes().await?;
        decode_json(status, content_type, &body)
    }

    /// Sends a request and ignores the response body.
    pub(crate) async fn empty(&self, request: RequestBuilder) -> Result<()> {
        // Reading the body lets the connection be reused.
        self.send(request).await?.bytes().await?;
        Ok(())
    }

    /// Sends a request and returns the raw response body.
    pub(crate) async fn bytes(&self, request: RequestBuilder) -> Result<Bytes> {
        Ok(self.send(request).await?.bytes().await?)
    }

    /// Sends a request and returns the response body as text.
    pub(crate) async fn text(&self, request: RequestBuilder) -> Result<String> {
        let body = self.bytes(request).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

fn decode_json<T: DeserializeOwned>(status: u16, content_type: String, body: &[u8]) -> Result<T> {
    // An empty body decodes like JSON `null`, so `()` and `Option<T>` work.
    let input: &[u8] = if body.iter().all(u8::is_ascii_whitespace) {
        b"null"
    } else {
        body
    };
    serde_json::from_slice(input).or_else(|source| {
        // Core API list endpoints are documented as returning an array, but
        // return `{ "Items": [...], ... }` when a page number is given.
        if let Ok(Value::Object(mut page)) = serde_json::from_slice::<Value>(input)
            && let Some(items @ Value::Array(_)) = page.remove("Items")
            && let Ok(decoded) = serde_json::from_value(items)
        {
            return Ok(decoded);
        }
        Err(Error::Decode {
            status,
            content_type,
            body: truncate(&String::from_utf8_lossy(body), 200),
            source,
        })
    })
}

fn is_idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
    )
}

/// How long to wait before retrying, or `None` if the result must not be retried.
fn retry_delay(
    method: &Method,
    result: &reqwest::Result<Response>,
    attempt: u32,
) -> Option<Duration> {
    match result {
        // The request was rejected before processing, so any method is safe to retry.
        Ok(r) if r.status() == StatusCode::TOO_MANY_REQUESTS => {
            Some(retry_after(r).unwrap_or_else(|| backoff(attempt)))
        }
        Ok(r)
            if is_idempotent(method)
                && matches!(
                    r.status(),
                    StatusCode::BAD_GATEWAY
                        | StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::GATEWAY_TIMEOUT
                ) =>
        {
            Some(retry_after(r).unwrap_or_else(|| backoff(attempt)))
        }
        Err(e) if is_idempotent(method) && (e.is_connect() || e.is_timeout()) => {
            Some(backoff(attempt))
        }
        _ => None,
    }
}

/// Parses a `Retry-After` header given in seconds.
fn retry_after(response: &Response) -> Option<Duration> {
    let secs: u64 = response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// 0.5s, 1s, 2s, 4s, ... capped at 10s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500)
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(10), MAX_BACKOFF);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn empty_body_decodes_as_null() {
        let unit: () = decode_json(200, String::new(), b"").unwrap();
        assert_eq!(unit, ());
        let none: Option<u32> = decode_json(204, String::new(), b"  ").unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn paginated_object_decodes_as_list() {
        let body = br#"{"Items":[1,2],"CurrentPage":1,"TotalItems":2,"HasMoreItems":false}"#;
        let items: Vec<u32> = decode_json(200, "application/json".into(), body).unwrap();
        assert_eq!(items, [1, 2]);
    }

    #[test]
    fn html_body_is_a_decode_error() {
        let err =
            decode_json::<Vec<u32>>(200, "text/html".into(), b"<html>login</html>").unwrap_err();
        assert!(matches!(err, Error::Decode { status: 200, .. }));
        assert!(err.to_string().contains("text/html"));
    }

    #[test]
    fn query_pairs_skip_none_and_repeat_arrays() {
        #[derive(Serialize)]
        struct Q {
            page: Option<i32>,
            search: Option<String>,
            #[serde(rename = "Tags")]
            tags: Vec<String>,
            flag: bool,
        }
        let mut pairs = query_pairs(&Q {
            page: Some(2),
            search: None,
            tags: vec!["a".into(), "b c".into()],
            flag: true,
        })
        .unwrap();
        pairs.sort();
        let expect = [
            ("Tags", "a"),
            ("Tags", "b c"),
            ("flag", "true"),
            ("page", "2"),
        ];
        assert_eq!(
            pairs,
            expect.map(|(k, v)| (k.to_owned(), v.to_owned())).to_vec()
        );
    }

    #[test]
    fn segment_encodes_reserved_characters() {
        assert_eq!(segment("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(segment(42), "42");
    }

    #[test]
    fn rejects_bad_settings() {
        assert!(matches!(
            Transport::new("", "https://x", Config::new()),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Transport::new("key\n", "https://x", Config::new()),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Transport::new("key", "not a url", Config::new()),
            Err(Error::InvalidInput(_))
        ));
    }
}
