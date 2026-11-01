# bunnysdk-rs

Async Rust SDK for the [bunny.net](https://bunny.net) APIs.

- Typed errors: every API error format becomes one `ApiError { status, message, field, validation_errors, .. }`
- Sensible defaults: idle timeout, rustls TLS, opt-in retries for 429 / 5xx
- Bring your own `reqwest::Client` if you need proxies or custom TLS
- No secrets in logs: requests are traced (method, path, status, time) without keys or bodies

## Status

| API | Module | Status |
|---|---|---|
| Edge Storage | `storage` | ✅ |
| Core (pull zones, DNS, storage zones, …) | — | planned (generated from the official OpenAPI spec) |
| Stream | — | planned |
| Shield, Edge Scripting, Magic Containers, Database, Origin Errors | — | planned |

## Usage

```toml
[dependencies]
bunnysdk = { git = "https://github.com/rkwap/bunnysdk-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use bunnysdk::storage::{Region, StorageClient};

#[tokio::main]
async fn main() -> bunnysdk::Result<()> {
    // Storage uses the storage zone password, not the account API key.
    let storage = StorageClient::new("my-zone", "zone-password", Region::NewYork)?;

    storage.upload("docs/hello.txt", "hello").await?;   // sends a SHA-256 checksum
    for object in storage.list("docs").await? {
        println!("{} ({} bytes)", object.object_name, object.length);
    }

    match storage.download("docs/missing.txt").await {
        Ok(bytes) => println!("{} bytes", bytes.len()),
        Err(e) if e.is_not_found() => println!("not found"),
        Err(e) => return Err(e),
    }

    storage.delete("docs/").await?; // trailing slash deletes the directory
    Ok(())
}
```

### Configuration

```rust
use std::time::Duration;
use bunnysdk::{Config, storage::{Region, StorageClient}};

let config = Config::new()
    .timeout(Duration::from_secs(60)) // idle timeout; default 30s
    .max_retries(3);                  // default 0
let storage = StorageClient::with_config("my-zone", "zone-password", Region::Frankfurt, config)?;
```

### Large files

```rust
// Stream an upload from disk.
let file = tokio::fs::File::open("video.mp4").await?;
storage.upload_with("videos/video.mp4", bunnysdk::reqwest::Body::from(file), Default::default()).await?;

// Stream a download.
let mut download = storage.download_stream("videos/video.mp4").await?;
while let Some(chunk) = download.chunk().await? {
    // write chunk
}
```

## Development

```sh
cargo test                                   # unit + mock-server tests
cargo test --test live -- --ignored          # live tests, needs the env vars below
```

Live tests need a **dedicated test storage zone** and only touch files under `bunnysdk-ci/`:

| Variable | Value |
|---|---|
| `BUNNY_STORAGE_ZONE` | storage zone name |
| `BUNNY_STORAGE_PASSWORD` | storage zone password |
| `BUNNY_STORAGE_REGION` | region code: `de`, `uk`, `ny`, `la`, `sg`, `se`, `br`, `jh`, `syd` (default `de`) |

In CI, add them as repository secrets; the live job is skipped when they are missing.

Minimum supported Rust version: 1.88.

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
