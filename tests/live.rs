//! Smoke tests against the real bunny.net APIs.
//!
//! Ignored by default. Run with credentials for a dedicated test zone:
//!
//! ```sh
//! BUNNY_STORAGE_ZONE=... BUNNY_STORAGE_PASSWORD=... BUNNY_STORAGE_REGION=de \
//!   cargo test --test live -- --ignored
//! ```
//!
//! Storage tests only touch files under `bunnysdk-ci/` and delete them afterwards.

use std::time::{SystemTime, UNIX_EPOCH};

use bunnysdk::Config;
use bunnysdk::storage::{Region, StorageClient};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for live tests"))
}

#[tokio::test]
#[ignore = "live: needs BUNNY_STORAGE_* env vars"]
async fn storage_roundtrip() {
    let region: Region = std::env::var("BUNNY_STORAGE_REGION")
        .unwrap_or_else(|_| "de".into())
        .parse()
        .unwrap();
    let storage = StorageClient::with_config(
        env("BUNNY_STORAGE_ZONE"),
        &env("BUNNY_STORAGE_PASSWORD"),
        region,
        Config::new().max_retries(2),
    )
    .unwrap();

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = format!("bunnysdk-ci/{nanos}/");
    let file = format!("{dir}hello world.txt");
    let data = format!("hello from bunnysdk-rs {nanos}");

    storage.upload(&file, data.clone()).await.unwrap();

    // Checks return errors instead of panicking, so cleanup below always runs.
    let result: Result<(), String> = async {
        let listed = storage.list(&dir).await.map_err(|e| e.to_string())?;
        if !listed
            .iter()
            .any(|o| o.object_name == "hello world.txt" && !o.is_directory)
        {
            return Err(format!("uploaded file missing from listing: {listed:?}"));
        }
        let downloaded = storage.download(&file).await.map_err(|e| e.to_string())?;
        if downloaded != data.as_bytes() {
            return Err("downloaded content differs from upload".into());
        }
        match storage.download(&format!("{dir}missing.txt")).await {
            Err(e) if e.is_not_found() => Ok(()),
            other => Err(format!("expected 404 for missing file, got {other:?}")),
        }
    }
    .await;

    storage.delete(&dir).await.unwrap();
    result.unwrap();
}
