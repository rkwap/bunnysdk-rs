use serde::Deserialize;
use serde_json::Value;

/// Shorthand for `Result<T, bunnysdk::Error>`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong when calling a bunny.net API.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The API answered with a non-success HTTP status.
    #[error(transparent)]
    Api(Box<ApiError>),

    /// The request could not be sent or the response could not be read
    /// (DNS, TLS, connection reset, timeout, ...).
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// The API answered successfully, but the body was not the expected JSON.
    /// A non-JSON content type usually means a proxy or captive portal
    /// answered instead of bunny.net.
    #[error(
        "failed to decode response (HTTP {status}, content-type {content_type:?}): {source}; body starts with: {body}"
    )]
    Decode {
        status: u16,
        content_type: String,
        body: String,
        #[source]
        source: serde_json::Error,
    },

    /// An argument or setting was rejected before any request was sent.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Reading local data failed (e.g. the file being uploaded).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The HTTP status code, if a response was received.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api(e) => Some(e.status),
            Error::Http(e) => e.status().map(|s| s.as_u16()),
            Error::Decode { status, .. } => Some(*status),
            Error::InvalidInput(_) | Error::Io(_) => None,
        }
    }

    /// True for HTTP 404.
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// The API error details, if this is an [`Error::Api`].
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Error::Api(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Error::Api(Box::new(e))
    }
}

/// A non-success response from a bunny.net API, normalised across the
/// different error formats the APIs use.
#[derive(Debug, Clone, thiserror::Error)]
#[error("bunny.net API error (HTTP {status}): {message}")]
#[non_exhaustive]
pub struct ApiError {
    /// HTTP status code.
    pub status: u16,
    /// Human-readable message from the API, or a generic one for empty bodies.
    pub message: String,
    /// Machine-readable error key (Core API `ErrorKey`), when given.
    pub error_key: Option<String>,
    /// The request field the error refers to (Core API `Field`), when given.
    pub field: Option<String>,
    /// Field-level validation errors (RFC 7807 `errors`), when given.
    pub validation_errors: Vec<ValidationError>,
    /// The raw response body, truncated to a few KB.
    pub body: String,
}

/// One field-level validation error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct ValidationError {
    pub field: Option<String>,
    pub message: String,
}

const MAX_BODY: usize = 4096;

/// Cuts `text` to at most `max` bytes on a character boundary.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl ApiError {
    /// An error with just a status and message.
    #[cfg_attr(not(feature = "stream"), allow(dead_code))]
    pub(crate) fn new(status: u16, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
            error_key: None,
            field: None,
            validation_errors: Vec::new(),
            body: String::new(),
        }
    }

    /// Builds an error from a non-success response body.
    ///
    /// bunny.net APIs use several error formats; the first match wins:
    /// - RFC 7807 `{ title, detail, errors[] }` (Magic Containers, Shield)
    /// - `{ ErrorKey, Field, Message }` (Core, Compute, Storage `{ HttpCode, Message }`)
    /// - `{ success, message, statusCode }` (Stream)
    /// - `{ error: { code, message, details[] } }` (Logging)
    ///
    /// Ported from the official bunny.net TypeScript client (MIT, BunnyWay d.o.o.).
    pub(crate) fn from_body(status: u16, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(body);
        let mut err = ApiError {
            status,
            message: String::new(),
            error_key: None,
            field: None,
            validation_errors: Vec::new(),
            body: truncate(&text, MAX_BODY),
        };

        let json: Option<Value> = serde_json::from_slice(body).ok();
        // Some endpoints wrap the error object in an array.
        let obj = match &json {
            Some(Value::Array(items)) => items.first(),
            other => other.as_ref(),
        };

        if let Some(obj) = obj {
            let s = |key: &str| text_field(obj, key);
            let logging = obj.get("error").filter(|e| e.is_object());
            if let Some(msg) = logging.and_then(|e| text_field(e, "message")) {
                // Logging API: { error: { code, message, details: [..] } }
                let inner = logging.expect("checked above");
                err.message = msg;
                err.error_key = text_field(inner, "code");
                err.validation_errors = inner
                    .get("details")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|d| ValidationError {
                        field: None,
                        message: d.to_owned(),
                    })
                    .collect();
            } else if let Some(msg) = s("detail").or_else(|| s("title")) {
                err.message = msg;
                if let Some(list) = obj.get("errors") {
                    err.validation_errors =
                        serde_json::from_value(list.clone()).unwrap_or_default();
                }
            } else if let Some(msg) = s("Message") {
                err.message = msg;
                err.field = s("Field");
                err.error_key = s("ErrorKey");
            } else if let Some(msg) = s("message") {
                err.message = msg;
            }
        } else {
            // Short plain-text bodies are usually a readable message; HTML pages are not.
            let trimmed = text.trim();
            if !trimmed.is_empty() && trimmed.len() <= 300 && !trimmed.starts_with('<') {
                err.message = trimmed.to_owned();
            }
        }

        if err.message.is_empty() {
            err.message = match status {
                401 => "Unauthorized. Check your API key.",
                403 => "Forbidden. You don't have permission for this action.",
                404 => "Not found.",
                409 => "Conflict. The resource already exists or is in use.",
                429 => "Too many requests.",
                500 => "Internal server error.",
                _ => "Request failed.",
            }
            .to_owned();
        }
        err
    }
}

/// A non-empty string field of a JSON object.
fn text_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_format() {
        let e = ApiError::from_body(
            400,
            br#"{"error":{"code":"invalid_request","message":"One or more query parameters are invalid.","details":["limit must be between 1 and 10000."]}}"#,
        );
        assert_eq!(e.message, "One or more query parameters are invalid.");
        assert_eq!(e.error_key.as_deref(), Some("invalid_request"));
        assert_eq!(
            e.validation_errors[0].message,
            "limit must be between 1 and 10000."
        );
    }

    #[test]
    fn core_format() {
        let e = ApiError::from_body(
            400,
            br#"{"ErrorKey":"pullzone.name_taken","Field":"Name","Message":"The name is already taken."}"#,
        );
        assert_eq!(e.message, "The name is already taken.");
        assert_eq!(e.field.as_deref(), Some("Name"));
        assert_eq!(e.error_key.as_deref(), Some("pullzone.name_taken"));
    }

    #[test]
    fn storage_format() {
        let e = ApiError::from_body(404, br#"{"HttpCode":404,"Message":"Object Not Found"}"#);
        assert_eq!(e.message, "Object Not Found");
    }

    #[test]
    fn storage_array_format() {
        let e = ApiError::from_body(404, br#"[{"HttpCode":404,"Message":"Object Not Found"}]"#);
        assert_eq!(e.message, "Object Not Found");
    }

    #[test]
    fn rfc7807_format() {
        let e = ApiError::from_body(
            422,
            br#"{"title":"Validation failed","status":422,"detail":"Invalid app","errors":[{"field":"name","message":"required"},{"message":"bad"}]}"#,
        );
        assert_eq!(e.message, "Invalid app");
        assert_eq!(e.validation_errors.len(), 2);
        assert_eq!(e.validation_errors[0].field.as_deref(), Some("name"));
        assert_eq!(e.validation_errors[0].message, "required");
        assert_eq!(e.validation_errors[1].field, None);
    }

    #[test]
    fn rfc7807_title_only() {
        let e = ApiError::from_body(404, br#"{"title":"Not Found","status":404}"#);
        assert_eq!(e.message, "Not Found");
    }

    #[test]
    fn stream_format() {
        let e = ApiError::from_body(
            400,
            br#"{"success":false,"message":"Video not found","statusCode":400}"#,
        );
        assert_eq!(e.message, "Video not found");
    }

    #[test]
    fn empty_body_uses_status_message() {
        assert_eq!(
            ApiError::from_body(401, b"").message,
            "Unauthorized. Check your API key."
        );
        assert_eq!(ApiError::from_body(418, b"").message, "Request failed.");
    }

    #[test]
    fn plain_text_and_html() {
        assert_eq!(
            ApiError::from_body(400, b"Bad thing\n").message,
            "Bad thing"
        );
        let html = ApiError::from_body(502, b"<html><body>Bad Gateway</body></html>");
        assert_eq!(html.message, "Request failed.");
        assert!(html.body.contains("Bad Gateway"));
    }

    #[test]
    fn body_is_truncated_on_char_boundary() {
        let body = "é".repeat(5000);
        let e = ApiError::from_body(500, body.as_bytes());
        assert!(e.body.len() <= MAX_BODY + "…".len());
        assert!(e.body.ends_with('…'));
    }

    #[test]
    fn error_helpers() {
        let e: Error = ApiError::from_body(404, b"").into();
        assert!(e.is_not_found());
        assert_eq!(e.status(), Some(404));
        assert_eq!(e.api().unwrap().message, "Not found.");
        assert_eq!(e.to_string(), "bunny.net API error (HTTP 404): Not found.");
    }
}
