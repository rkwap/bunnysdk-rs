//! The HTTP layer shared by every API client: auth, retries, error mapping.

use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

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

/// What a list endpoint's method returns. bunny.net list endpoints return
/// either a plain array or a `{ "Items": [...], ... }` page depending on the
/// `page` parameter, whatever their spec says, so responses are reshaped to
/// match the method's return type.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ListShape {
    /// `Vec<T>`: a page object is unwrapped to its items.
    Array,
    /// A page struct whose items field has this JSON name: a plain array is
    /// wrapped as `{ name: [...] }`.
    Page(&'static str),
}

fn normalize_list(value: Value, shape: ListShape) -> Value {
    match (shape, value) {
        (ListShape::Array, Value::Object(mut page)) => {
            let key = ["Items", "items"]
                .into_iter()
                .find(|k| page.get(*k).is_some_and(Value::is_array));
            match key {
                Some(key) => page.remove(key).unwrap_or_default(),
                None => Value::Object(page),
            }
        }
        (ListShape::Page(key), items @ Value::Array(_)) => {
            Value::Object(Map::from_iter([(key.to_owned(), items)]))
        }
        (_, other) => other,
    }
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
        // Keys pasted from files or env vars often carry stray whitespace.
        let access_key = access_key.trim();
        if access_key.is_empty() {
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

    /// Sends a request and returns status, content type and body.
    async fn read(&self, request: RequestBuilder) -> Result<(u16, String, Bytes)> {
        let response = self.send(request).await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        Ok((status, content_type, response.bytes().await?))
    }

    /// Sends a request and decodes a JSON response body.
    pub(crate) async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T> {
        let (status, content_type, body) = self.read(request).await?;
        decode_json(status, content_type, &body)
    }

    /// Like [`Transport::json`] for list endpoints; see [`ListShape`].
    pub(crate) async fn json_list<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        shape: ListShape,
    ) -> Result<T> {
        let (status, content_type, body) = self.read(request).await?;
        let value: Value = decode_json(status, content_type.clone(), &body)?;
        serde_json::from_value(normalize_list(value, shape))
            .map_err(|source| decode_error(status, content_type, &body, source))
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
    serde_json::from_slice(input).map_err(|source| decode_error(status, content_type, body, source))
}

fn decode_error(
    status: u16,
    content_type: String,
    body: &[u8],
    source: serde_json::Error,
) -> Error {
    // JSON bodies can carry credentials (zone passwords, library API keys),
    // so only non-JSON bodies, like a proxy's HTML page, are quoted.
    let looks_like_json = body
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| matches!(b, b'{' | b'['));
    let body = if looks_like_json {
        String::new()
    } else {
        truncate(&String::from_utf8_lossy(body), 200)
    };
    Error::Decode {
        status,
        content_type,
        body,
        source,
    }
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
    fn list_shapes_are_normalized() {
        let page = serde_json::json!({"Items": [1, 2], "CurrentPage": 1});
        assert_eq!(
            normalize_list(page.clone(), ListShape::Array),
            serde_json::json!([1, 2])
        );
        assert_eq!(
            normalize_list(serde_json::json!([1]), ListShape::Page("items")),
            serde_json::json!({"items": [1]})
        );
        // Values already in the right shape are untouched.
        assert_eq!(normalize_list(page.clone(), ListShape::Page("Items")), page);
        let no_items = serde_json::json!({"Other": 1});
        assert_eq!(normalize_list(no_items.clone(), ListShape::Array), no_items);
    }

    #[test]
    fn decode_errors_do_not_leak_json_bodies() {
        // A paginated object for a Vec is a plain decode error (no silent fallback).
        let err = decode_json::<Vec<u32>>(200, "application/json".into(), br#"{"Items":[]}"#)
            .unwrap_err();
        assert!(matches!(err, Error::Decode { status: 200, .. }));

        let secret = br#"{"Id":1,"Password":"hunter2","Length":"oops"}"#;
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Zone {
            #[serde(rename = "Length")]
            length: u32,
        }
        let err = decode_json::<Zone>(200, "application/json".into(), secret).unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
    }

    #[test]
    fn html_body_is_quoted_in_decode_error() {
        let err =
            decode_json::<Vec<u32>>(200, "text/html".into(), b"<html>login</html>").unwrap_err();
        assert!(matches!(err, Error::Decode { status: 200, .. }));
        assert!(err.to_string().contains("text/html"));
        assert!(err.to_string().contains("<html>login"));
    }

    #[test]
    fn api_key_is_trimmed() {
        let transport = Transport::new(" key \n", "https://x", Config::new()).unwrap();
        assert_eq!(transport.api_key(), "key");
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
            Transport::new("ke\ny", "https://x", Config::new()),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Transport::new("key", "not a url", Config::new()),
            Err(Error::InvalidInput(_))
        ));
    }
}
