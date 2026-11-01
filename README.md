# bunnysdk-rs

Async Rust SDK for the [bunny.net](https://bunny.net) APIs.

- **Complete:** about 350 endpoints across 10 APIs, most generated from bunny.net's official OpenAPI specs.
- **Kept current:** a weekly job refreshes the specs, regenerates, tests, and opens a PR.
- **Typed errors:** bunny.net's five error formats become one `ApiError { status, message, field, validation_errors, .. }`.
- **Robust decoding:** enums have an `Other(..)` variant and unknown fields are ignored, so API additions don't break you.
- **Sensible defaults:** idle timeout, rustls TLS, opt-in retries for 429 / 5xx. Bring your own `reqwest::Client` if needed.
- **No secrets in logs:** requests are traced (method, path, status, time) without keys or bodies.

## APIs

| Module | API | Authenticates with | Source |
|---|---|---|---|
| `core` | Pull zones, DNS, storage zones, video libraries, billing, statistics, … | account API key | generated |
| `compute` | Edge Scripting | account API key | generated |
| `shield` | WAF, rate limits, bot detection, access lists | account API key | generated |
| `containers` | Magic Containers | account API key | generated |
| `database` | Managed libSQL databases | account API key | generated |
| `origin_errors` | Origin error logs | account API key | generated |
| `partner` | Reseller customers, pricing, billing | partner account API key | generated |
| `logging` | CDN request logs (v2 JSON, v1 raw) | account API key | hand-written |
| `stream` | Videos, collections, captions, live streams + resumable TUS uploads | video library API key | generated + hand-written |
| `storage` | Edge Storage files | storage zone password | hand-written |

Each module is a Cargo feature (`origin-errors` for `origin_errors`), all on by default.

## Install

```toml
[dependencies]
bunnysdk = { git = "https://github.com/rkwap/bunnysdk-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

# Or only what you use, for faster builds:
# bunnysdk = { git = "...", default-features = false, features = ["core", "storage"] }
```

Minimum supported Rust version: 1.88.

## Usage

### Core

```rust
use bunnysdk::core::{CoreClient, ListPullZonesQuery, PullZoneSettingsModel};

#[tokio::main]
async fn main() -> bunnysdk::Result<()> {
    let core = CoreClient::new("account-api-key")?;

    for zone in core.list_pull_zones(&ListPullZonesQuery::default()).await? {
        println!("{:?} {:?}", zone.id, zone.name);
    }

    // Updates send only the fields you set.
    let settings = PullZoneSettingsModel {
        origin_url: Some("https://origin.example.com".into()),
        ..Default::default()
    };
    core.update_pull_zone(123, &settings).await?;
    Ok(())
}
```

Generated types follow the API reference: fields the spec doesn't mark as required are `Option`s,
query parameters are a `...Query` struct (`&Default::default()` when you need none), and each
method's docs show its HTTP route.

Where the API behaves differently from its spec, the SDK smooths it over:

- List endpoints with a `page` parameter accept both a plain array and a `{ Items: [...] }` page,
  whichever the API sends for the page you ask for.
- Endpoints that can answer with or without a body (e.g. `200` JSON or `204`) return `Option<T>`.
- GET endpoints whose body the spec doesn't describe (e.g. PDF invoices) return the raw bytes.

Decimal amounts (prices, balances) are `f64`, which keeps about 15 significant digits.

### Errors

```rust
match core.get_pull_zone(123, &Default::default()).await {
    Ok(zone) => println!("{:?}", zone.name),
    Err(e) if e.is_not_found() => println!("no such pull zone"),
    Err(bunnysdk::Error::Api(e)) => println!("{} ({}): {:?}", e.message, e.status, e.field),
    Err(e) => return Err(e),
}
```

### Configuration

```rust
use std::time::Duration;
use bunnysdk::{Config, core::CoreClient};

let config = Config::new()
    .timeout(Duration::from_secs(60)) // idle timeout; default 30s
    .max_retries(3);                  // default 0
let core = CoreClient::with_config("account-api-key", config)?;
```

`Config` also takes `base_url`, `user_agent`, and `http_client` (your own `bunnysdk::reqwest::Client`).

### Edge Storage

```rust
use bunnysdk::storage::{Region, StorageClient};

let storage = StorageClient::new("my-zone", "zone-password", Region::NewYork)?;
storage.upload("docs/hello.txt", "hello").await?;   // sends a SHA-256 checksum
let files = storage.list("docs").await?;
let data = storage.download("docs/hello.txt").await?;
storage.delete("docs/").await?;                       // trailing slash: whole directory

// Large files: stream from disk / read in chunks.
let file = tokio::fs::File::open("video.mp4").await?;
storage.upload_with("videos/video.mp4", bunnysdk::reqwest::Body::from(file), Default::default()).await?;
let mut download = storage.download_stream("videos/video.mp4").await?;
while let Some(chunk) = download.chunk().await? { /* write chunk */ }
```

### Stream

```rust
use std::time::Duration;
use bunnysdk::stream::{CreateVideoModel, StreamClient, TusUpload};

let stream = StreamClient::new("library-api-key")?;
let video = stream
    .create_video(42, &CreateVideoModel { title: "Intro".into(), ..Default::default() })
    .await?;

// Resumable upload in 8 MiB chunks; a failed chunk resumes from the server's offset.
stream
    .upload_file_resumable(42, &video.guid, "intro.mp4", &TusUpload::new("Intro", "video/mp4"))
    .await?;

// Or presign, and let a browser upload directly with tus-js-client.
let auth = stream.tus_authorization(42, &video.guid, Duration::from_secs(3600));
// serde_json::to_string(&auth) -> {"endpoint", "libraryId", "videoId", "expires", "signature"}
```

### Logging

```rust
use bunnysdk::logging::{LogQuery, LoggingClient};

let logging = LoggingClient::new("account-api-key")?;
let page = logging
    .get_logs(1337, &LogQuery { status: Some("5xx".into()), ..Default::default() })
    .await?;
```

## How it's built

```
specs/*.json          official OpenAPI specs (vendored)
xtask/                cargo xtask update-specs | codegen
src/generated/*.rs    generated clients + types (do not edit)
src/{storage,logging,stream_upload}.rs   hand-written
src/{transport,error,config,macros}.rs   shared runtime
```

```sh
cargo xtask update-specs   # download the latest specs
cargo xtask codegen        # regenerate src/generated
```

Method names come from each operation's summary or operationId. When two operations would get the
same name, codegen stops and asks for an override in `xtask/src/main.rs`.

### CI

- **CI** (`ci.yml`): fmt, clippy, tests, docs, generated code up to date, each feature alone, MSRV,
  and live smoke tests when secrets are set.
- **Update specs** (`update-specs.yml`): weekly; opens a PR with new specs and regenerated code,
  reporting codegen and test results. Enable *Settings → Actions → General → Allow GitHub Actions
  to create and approve pull requests*.

### Live tests

Ignored by default; CI runs them when these repository secrets exist:

| Secret | Used by |
|---|---|
| `BUNNY_API_KEY` | read-only Core calls |
| `BUNNY_STORAGE_ZONE`, `BUNNY_STORAGE_PASSWORD`, `BUNNY_STORAGE_REGION` | storage round-trip in a **dedicated test zone**, under `bunnysdk-ci/` only |

```sh
BUNNY_API_KEY=... cargo test --test live -- --ignored
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
