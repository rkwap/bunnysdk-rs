//! Edge Storage API: upload, download, list and delete files in a storage zone.
//!
//! Authenticate with the storage zone **password**, not your account API key.
//!
//! ```no_run
//! # async fn run() -> bunnysdk::Result<()> {
//! use bunnysdk::storage::{Region, StorageClient};
//!
//! let storage = StorageClient::new("my-zone", "zone-password", Region::NewYork)?;
//! storage.upload("images/logo.png", std::fs::read("logo.png").unwrap()).await?;
//! for object in storage.list("images").await? {
//!     println!("{} ({} bytes)", object.object_name, object.length);
//! }
//! let data = storage.download("images/logo.png").await?;
//! storage.delete("images/").await?; // trailing slash: delete the directory recursively
//! # Ok(()) }
//! ```

use std::fmt;
use std::str::FromStr;

use bytes::Bytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Body, Method};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::transport::Transport;

/// Characters left as-is in a path segment (RFC 3986 unreserved).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The primary region of a storage zone, which decides the API hostname.
///
/// Find it on the storage zone's **Access** page in the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Region {
    /// Frankfurt / Falkenstein, DE (`storage.bunnycdn.com`).
    #[default]
    Frankfurt,
    London,
    NewYork,
    LosAngeles,
    Singapore,
    Stockholm,
    SaoPaulo,
    Johannesburg,
    Sydney,
}

impl Region {
    const ALL: [Region; 9] = [
        Region::Frankfurt,
        Region::London,
        Region::NewYork,
        Region::LosAngeles,
        Region::Singapore,
        Region::Stockholm,
        Region::SaoPaulo,
        Region::Johannesburg,
        Region::Sydney,
    ];

    /// The bunny.net region code: `de`, `uk`, `ny`, `la`, `sg`, `se`, `br`, `jh`, `syd`.
    pub fn code(self) -> &'static str {
        match self {
            Region::Frankfurt => "de",
            Region::London => "uk",
            Region::NewYork => "ny",
            Region::LosAngeles => "la",
            Region::Singapore => "sg",
            Region::Stockholm => "se",
            Region::SaoPaulo => "br",
            Region::Johannesburg => "jh",
            Region::Sydney => "syd",
        }
    }

    /// The API base URL for this region.
    pub fn base_url(self) -> &'static str {
        match self {
            Region::Frankfurt => "https://storage.bunnycdn.com",
            Region::London => "https://uk.storage.bunnycdn.com",
            Region::NewYork => "https://ny.storage.bunnycdn.com",
            Region::LosAngeles => "https://la.storage.bunnycdn.com",
            Region::Singapore => "https://sg.storage.bunnycdn.com",
            Region::Stockholm => "https://se.storage.bunnycdn.com",
            Region::SaoPaulo => "https://br.storage.bunnycdn.com",
            Region::Johannesburg => "https://jh.storage.bunnycdn.com",
            Region::Sydney => "https://syd.storage.bunnycdn.com",
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Parses a region code such as `ny` or `DE` (case-insensitive).
impl FromStr for Region {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        Region::ALL
            .into_iter()
            .find(|r| r.code().eq_ignore_ascii_case(s))
            .ok_or_else(|| Error::InvalidInput(format!("unknown storage region {s:?}")))
    }
}

/// A file or directory in a storage zone.
///
/// Timestamps are kept as the API sends them (UTC, e.g. `2017-03-10T03:06:48.203`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
#[non_exhaustive]
pub struct StorageObject {
    pub guid: String,
    pub storage_zone_name: String,
    /// Directory holding the object, e.g. `/my-zone/images/`.
    pub path: String,
    /// File or directory name.
    pub object_name: String,
    /// Size in bytes.
    pub length: u64,
    pub last_changed: String,
    pub is_directory: bool,
    pub server_id: i64,
    pub user_id: String,
    pub date_created: String,
    pub storage_zone_id: i64,
    /// Upper-case hex SHA-256 of the content (files only).
    pub checksum: Option<String>,
    pub content_type: Option<String>,
    pub replicated_zones: Option<String>,
}

/// Optional settings for [`StorageClient::upload_with`].
#[derive(Debug, Clone, Default)]
pub struct UploadOptions {
    content_type: Option<String>,
    checksum: Option<String>,
}

impl UploadOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the MIME type stored with the file and served by the CDN.
    /// By default bunny.net detects it from the file name.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// Hex SHA-256 of the content (see [`checksum`]). The server rejects the
    /// upload if the received data does not match.
    pub fn checksum(mut self, sha256_hex: impl Into<String>) -> Self {
        self.checksum = Some(sha256_hex.into().to_ascii_uppercase());
        self
    }
}

/// Upper-case hex SHA-256 of `data`, the format the Storage API expects.
pub fn checksum(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// A download whose body is read chunk by chunk, for files too big for memory.
#[derive(Debug)]
pub struct Download {
    response: reqwest::Response,
}

impl Download {
    /// Total size in bytes, if the server sent it.
    pub fn content_length(&self) -> Option<u64> {
        self.response.content_length()
    }

    /// The next chunk of the file, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        Ok(self.response.chunk().await?)
    }
}

/// Client for one storage zone.
///
/// Cheap to clone; clones share the connection pool.
#[derive(Debug, Clone)]
pub struct StorageClient {
    transport: Transport,
    zone: String,
}

impl StorageClient {
    /// Creates a client for `zone` in `region`, authenticated with the zone password.
    pub fn new(zone: impl Into<String>, password: &str, region: Region) -> Result<Self> {
        Self::with_config(zone, password, region, Config::default())
    }

    /// Like [`StorageClient::new`], with custom settings.
    /// [`Config::base_url`] overrides the region's hostname.
    pub fn with_config(
        zone: impl Into<String>,
        password: &str,
        region: Region,
        config: Config,
    ) -> Result<Self> {
        let zone = zone.into();
        if zone.is_empty() || zone.contains('/') {
            return Err(Error::InvalidInput(format!(
                "invalid storage zone name {zone:?}"
            )));
        }
        Ok(Self {
            transport: Transport::new(password, region.base_url(), config)?,
            zone,
        })
    }

    /// The storage zone name.
    pub fn zone(&self) -> &str {
        &self.zone
    }

    /// Lists the files and directories in `dir`. Use `""` or `"/"` for the zone root.
    pub async fn list(&self, dir: &str) -> Result<Vec<StorageObject>> {
        let path = self.url_path(&format!("{dir}/"), true)?;
        self.transport
            .json(self.transport.request(Method::GET, &path))
            .await
    }

    /// Downloads a whole file into memory.
    pub async fn download(&self, path: &str) -> Result<Bytes> {
        let path = self.file_path(path)?;
        self.transport
            .bytes(self.transport.request(Method::GET, &path))
            .await
    }

    /// Starts a download to be read chunk by chunk with [`Download::chunk`].
    pub async fn download_stream(&self, path: &str) -> Result<Download> {
        let path = self.file_path(path)?;
        let response = self
            .transport
            .send(self.transport.request(Method::GET, &path))
            .await?;
        Ok(Download { response })
    }

    /// Uploads `data` to `path`, replacing any existing file. Missing
    /// directories are created. A SHA-256 checksum is sent so the server
    /// rejects corrupted uploads.
    pub async fn upload(&self, path: &str, data: impl Into<Bytes>) -> Result<()> {
        let data = data.into();
        let options = UploadOptions::new().checksum(checksum(&data));
        self.upload_with(path, data, options).await
    }

    /// Uploads any request body, e.g. a `tokio::fs::File` streamed from disk
    /// (`reqwest::Body::from(file)`). Streamed bodies are never retried.
    pub async fn upload_with(
        &self,
        path: &str,
        body: impl Into<Body>,
        options: UploadOptions,
    ) -> Result<()> {
        let path = self.file_path(path)?;
        let mut request = self.transport.request(Method::PUT, &path).body(body);
        // Without a Content-Type, bunny.net detects the type from the file name.
        if let Some(content_type) = options.content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        if let Some(sum) = options.checksum {
            request = request.header("Checksum", sum);
        }
        self.transport.empty(request).await
    }

    /// Deletes a file, or a whole directory (recursively) when `path` ends with `/`.
    /// Deleting the zone root is refused.
    pub async fn delete(&self, path: &str) -> Result<()> {
        let path = self.url_path(path, false)?;
        self.transport
            .empty(self.transport.request(Method::DELETE, &path))
            .await
    }

    /// URL path for a file: must name something, and must not end with `/`.
    fn file_path(&self, path: &str) -> Result<String> {
        if path.ends_with('/') {
            return Err(Error::InvalidInput(format!(
                "{path:?} is a directory, expected a file path"
            )));
        }
        self.url_path(path, false)
    }

    /// Builds `/{zone}/{segments...}`, percent-encoding each segment and
    /// keeping a trailing `/` for directories.
    fn url_path(&self, path: &str, allow_root: bool) -> Result<String> {
        let mut out = format!("/{}", utf8_percent_encode(&self.zone, SEGMENT));
        let mut segments = 0;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            if segment == "." || segment == ".." {
                return Err(Error::InvalidInput(format!(
                    "path {path:?} must not contain '.' or '..'"
                )));
            }
            out.push('/');
            out.extend(utf8_percent_encode(segment, SEGMENT));
            segments += 1;
        }
        if segments == 0 && !allow_root {
            return Err(Error::InvalidInput(
                "path must not be empty or the zone root".into(),
            ));
        }
        if segments == 0 || path.ends_with('/') {
            out.push('/');
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> StorageClient {
        StorageClient::new("my zone", "pw", Region::Frankfurt).unwrap()
    }

    #[test]
    fn builds_paths() {
        let c = client();
        assert_eq!(c.url_path("/", true).unwrap(), "/my%20zone/");
        assert_eq!(c.url_path("a//b/", true).unwrap(), "/my%20zone/a/b/");
        assert_eq!(
            c.file_path("a/b c#?.txt").unwrap(),
            "/my%20zone/a/b%20c%23%3F.txt"
        );
        assert_eq!(c.file_path("é.txt").unwrap(), "/my%20zone/%C3%A9.txt");
        assert_eq!(c.url_path("dir/", false).unwrap(), "/my%20zone/dir/");
    }

    #[test]
    fn rejects_unsafe_paths() {
        let c = client();
        assert!(c.url_path("", false).is_err());
        assert!(c.url_path("/", false).is_err());
        assert!(c.url_path("a/../b", true).is_err());
        assert!(c.url_path("./a", true).is_err());
        assert!(c.file_path("dir/").is_err());
    }

    #[test]
    fn debug_output_hides_password() {
        let c = StorageClient::new("zone", "super-secret-pw", Region::Frankfurt).unwrap();
        assert!(!format!("{c:?}").contains("super-secret-pw"));
    }

    #[test]
    fn rejects_bad_zone() {
        assert!(StorageClient::new("", "pw", Region::Frankfurt).is_err());
        assert!(StorageClient::new("a/b", "pw", Region::Frankfurt).is_err());
    }

    #[test]
    fn parses_regions() {
        assert_eq!("NY".parse::<Region>().unwrap(), Region::NewYork);
        assert_eq!(" syd ".parse::<Region>().unwrap(), Region::Sydney);
        assert!("xx".parse::<Region>().is_err());
        for r in Region::ALL {
            assert_eq!(r.code().parse::<Region>().unwrap(), r);
        }
    }

    #[test]
    fn checksum_is_uppercase_sha256() {
        assert_eq!(
            checksum(b"hello"),
            "2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824"
        );
    }
}
