//! Resumable video uploads for Bunny Stream over the TUS protocol
//! (<https://bunny.net/docs/stream/tus-resumable-uploads>).
//!
//! Two ways to use it:
//! - [`StreamClient::upload_file_resumable`] / [`StreamClient::upload_resumable`]
//!   upload from your server in chunks, resuming from the server's offset when
//!   a chunk fails.
//! - [`StreamClient::tus_authorization`] presigns an upload so a browser or
//!   mobile app can upload directly with any TUS client, without your API key.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use reqwest::header::{CONTENT_TYPE, HeaderMap, LOCATION};
use reqwest::{Method, RequestBuilder, Url};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{ApiError, Error, Result};
use crate::stream::StreamClient;

const TUS_VERSION: &str = "1.0.0";
const DEFAULT_CHUNK_SIZE: usize = 8 * 1024 * 1024;
const DEFAULT_VALID_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// Attempts per chunk before giving up.
const CHUNK_ATTEMPTS: u32 = 5;

/// Presigned credentials for uploading one video with any TUS client
/// (e.g. tus-js-client in a browser), without exposing your API key.
///
/// Serializes to camelCase JSON, ready to return from your own endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TusAuthorization {
    /// The TUS endpoint to upload to.
    pub endpoint: String,
    pub library_id: i64,
    pub video_id: String,
    /// UNIX time (seconds) after which the signature is rejected.
    pub expires: u64,
    /// Hex SHA-256 of `library_id + api_key + expires + video_id`.
    pub signature: String,
}

impl TusAuthorization {
    /// The headers a TUS client must send with every request.
    pub fn headers(&self) -> [(&'static str, String); 4] {
        [
            ("AuthorizationSignature", self.signature.clone()),
            ("AuthorizationExpire", self.expires.to_string()),
            ("LibraryId", self.library_id.to_string()),
            ("VideoId", self.video_id.clone()),
        ]
    }
}

/// Options for a resumable upload.
#[derive(Debug, Clone)]
pub struct TusUpload {
    title: String,
    file_type: String,
    collection: Option<String>,
    thumbnail_time: Option<u64>,
    chunk_size: usize,
    valid_for: Duration,
}

impl TusUpload {
    /// `title` is the video's title; `file_type` its MIME type, e.g. `video/mp4`.
    pub fn new(title: impl Into<String>, file_type: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            file_type: file_type.into(),
            collection: None,
            thumbnail_time: None,
            chunk_size: DEFAULT_CHUNK_SIZE,
            valid_for: DEFAULT_VALID_FOR,
        }
    }

    /// Adds the video to a collection (by collection ID).
    pub fn collection(mut self, collection_id: impl Into<String>) -> Self {
        self.collection = Some(collection_id.into());
        self
    }

    /// Time in milliseconds to take the main thumbnail from.
    pub fn thumbnail_time(mut self, millis: u64) -> Self {
        self.thumbnail_time = Some(millis);
        self
    }

    /// Bytes sent per request. Defaults to 8 MiB. One chunk is held in memory
    /// at a time so it can be re-sent if a request fails.
    pub fn chunk_size(mut self, bytes: usize) -> Self {
        self.chunk_size = bytes.max(1);
        self
    }

    /// How long the upload signature stays valid. Defaults to 24 hours.
    pub fn valid_for(mut self, duration: Duration) -> Self {
        self.valid_for = duration;
        self
    }

    /// The `Upload-Metadata` header: `key base64(value)` pairs.
    fn metadata(&self) -> String {
        let thumbnail_time = self.thumbnail_time.map(|t| t.to_string());
        [
            ("filetype", Some(&self.file_type)),
            ("title", Some(&self.title)),
            ("collection", self.collection.as_ref()),
            ("thumbnailTime", thumbnail_time.as_ref()),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|v| format!("{key} {}", BASE64.encode(v))))
        .collect::<Vec<_>>()
        .join(",")
    }
}

impl StreamClient {
    /// Presigns a TUS upload of `video_id` (create it first with
    /// [`StreamClient::create_video`]). Hand the result to a browser or mobile
    /// TUS client; your API key stays on the server.
    pub fn tus_authorization(
        &self,
        library_id: i64,
        video_id: &str,
        valid_for: Duration,
    ) -> TusAuthorization {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let expires = (now + valid_for).as_secs();
        TusAuthorization {
            endpoint: format!("{}/tusupload", self.transport.base_url()),
            library_id,
            video_id: video_id.to_owned(),
            expires,
            signature: sign(library_id, self.transport.api_key(), expires, video_id),
        }
    }

    /// Uploads a local video file to `video_id` (create it first with
    /// [`StreamClient::create_video`]) in resumable chunks.
    pub async fn upload_file_resumable(
        &self,
        library_id: i64,
        video_id: &str,
        path: impl AsRef<Path>,
        options: &TusUpload,
    ) -> Result<()> {
        let file = tokio::fs::File::open(path).await?;
        let size = file.metadata().await?.len();
        self.upload_resumable(library_id, video_id, file, size, options)
            .await
    }

    /// Uploads `size` bytes from `reader` to `video_id` in resumable chunks.
    /// A failed chunk is retried from the offset the server reports.
    pub async fn upload_resumable<R: AsyncRead + Unpin>(
        &self,
        library_id: i64,
        video_id: &str,
        mut reader: R,
        size: u64,
        options: &TusUpload,
    ) -> Result<()> {
        let auth = self.tus_authorization(library_id, video_id, options.valid_for);
        let endpoint = Url::parse(&auth.endpoint)
            .map_err(|e| Error::InvalidInput(format!("invalid TUS endpoint: {e}")))?;

        let create = self
            .tus_request(Method::POST, endpoint.clone(), &auth)
            .header("Upload-Length", size)
            .header("Upload-Metadata", options.metadata());
        let response = self.transport.send(create).await?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ApiError::new(
                    response.status().as_u16(),
                    "TUS server did not return an upload URL",
                )
            })?;
        let upload_url = endpoint
            .join(location)
            .map_err(|e| Error::InvalidInput(format!("invalid TUS upload URL: {e}")))?;

        let mut offset = 0;
        while offset < size {
            let want = (size - offset).min(options.chunk_size as u64);
            let mut chunk = Vec::with_capacity(want as usize);
            (&mut reader).take(want).read_to_end(&mut chunk).await?;
            if (chunk.len() as u64) < want {
                return Err(Error::InvalidInput(format!(
                    "upload source ended after {} of {size} bytes",
                    offset + chunk.len() as u64
                )));
            }
            offset = self
                .send_chunk(&upload_url, &auth, offset, Bytes::from(chunk))
                .await?;
            tracing::debug!(video_id, offset, size, "TUS chunk uploaded");
        }
        Ok(())
    }

    /// Sends the chunk that starts at `offset`; returns the offset after it.
    async fn send_chunk(
        &self,
        url: &Url,
        auth: &TusAuthorization,
        offset: u64,
        chunk: Bytes,
    ) -> Result<u64> {
        let end = offset + chunk.len() as u64;
        let mut from = offset;
        let mut attempt = 0;
        loop {
            let request = self
                .tus_request(Method::PATCH, url.clone(), auth)
                .header("Upload-Offset", from)
                .header(CONTENT_TYPE, "application/offset+octet-stream")
                .body(chunk.slice((from - offset) as usize..));
            let error = match self.transport.send(request).await {
                Ok(response) => match upload_offset(response.headers()) {
                    Some(reached) if reached == end => return Ok(end),
                    // The server took only part of the chunk; send the rest.
                    Some(reached) if reached > from && reached < end => {
                        from = reached;
                        continue;
                    }
                    other => Error::from(ApiError::new(
                        response.status().as_u16(),
                        format!("TUS server reported offset {other:?}, expected {end}"),
                    )),
                },
                Err(e) => e,
            };

            attempt += 1;
            if attempt >= CHUNK_ATTEMPTS || !is_retryable(&error) {
                return Err(error);
            }
            let wait = Duration::from_secs(1 << (attempt - 1));
            tracing::warn!(%error, attempt, ?wait, "TUS chunk failed, resuming");
            tokio::time::sleep(wait).await;

            // Ask the server how much of the chunk it has.
            from = self.server_offset(url, auth).await?;
            if from == end {
                return Ok(end);
            }
            if !(offset..end).contains(&from) {
                return Err(ApiError::new(
                    409,
                    format!(
                        "TUS server offset {from} is outside the current chunk {offset}..{end}"
                    ),
                )
                .into());
            }
        }
    }

    async fn server_offset(&self, url: &Url, auth: &TusAuthorization) -> Result<u64> {
        let response = self
            .transport
            .send(self.tus_request(Method::HEAD, url.clone(), auth))
            .await?;
        upload_offset(response.headers()).ok_or_else(|| {
            ApiError::new(
                response.status().as_u16(),
                "TUS server did not report an upload offset",
            )
            .into()
        })
    }

    fn tus_request(&self, method: Method, url: Url, auth: &TusAuthorization) -> RequestBuilder {
        auth.headers().into_iter().fold(
            self.transport
                .request_without_key(method, url)
                .header("Tus-Resumable", TUS_VERSION),
            |request, (name, value)| request.header(name, value),
        )
    }
}

/// Hex SHA-256 of `library_id + api_key + expires + video_id`.
fn sign(library_id: i64, api_key: &str, expires: u64, video_id: &str) -> String {
    Sha256::digest(format!("{library_id}{api_key}{expires}{video_id}"))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn upload_offset(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("Upload-Offset")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Network failures, conflicts (offset mismatch), locks, rate limits and server errors.
fn is_retryable(error: &Error) -> bool {
    match error {
        Error::Http(_) => true,
        Error::Api(e) => matches!(e.status, 409 | 423 | 429 | 500..=599),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_bunny_formula() {
        // sha256("123" + "secret-key" + "1700000000" + "abc-def")
        assert_eq!(
            sign(123, "secret-key", 1_700_000_000, "abc-def"),
            "6319d29159c2e1916e11c8d0449cd9301bdc2e22a7580ee271c6d193d4bc8ee5"
        );
    }

    #[test]
    fn metadata_is_base64_encoded() {
        let options = TusUpload::new("My video", "video/mp4")
            .collection("c1")
            .thumbnail_time(1500);
        assert_eq!(
            options.metadata(),
            "filetype dmlkZW8vbXA0,title TXkgdmlkZW8=,collection YzE=,thumbnailTime MTUwMA=="
        );
    }
}
