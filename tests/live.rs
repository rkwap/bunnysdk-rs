//! Smoke tests against the real bunny.net APIs.
//!
//! Ignored by default. Run with credentials (a dedicated test account or zone):
//!
//! ```sh
//! BUNNY_API_KEY=... cargo test --test live core_ -- --ignored
//! BUNNY_STORAGE_ZONE=... BUNNY_STORAGE_PASSWORD=... BUNNY_STORAGE_REGION=de \
//!   cargo test --test live storage_ -- --ignored
//! ```
//!
//! Core tests only read. Storage tests only touch files under `bunnysdk-ci/`
//! and delete them afterwards.

use std::time::{SystemTime, UNIX_EPOCH};

use bunnysdk::Config;
use bunnysdk::core::CoreClient;
use bunnysdk::storage::{Region, StorageClient};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for live tests"))
}

/// Read-only calls that decode the account's real data with the generated types.
#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn core_read_only() {
    let core =
        CoreClient::with_config(&env("BUNNY_API_KEY"), Config::new().max_retries(2)).unwrap();

    let countries = core.get_country_list().await.unwrap();
    assert!(!countries.is_empty(), "country list is empty");
    assert!(!core.region_list().await.unwrap().is_empty());

    core.list_pull_zones(&Default::default()).await.unwrap();
    core.list_storage_zones(&Default::default()).await.unwrap();
    core.list_dns_zones(&Default::default()).await.unwrap();
    core.list_video_libraries(&Default::default())
        .await
        .unwrap();

    let err = core
        .get_pull_zone(i64::MAX, &Default::default())
        .await
        .unwrap_err();
    assert!(
        err.status().is_some_and(|s| (400..500).contains(&s)),
        "{err:?}"
    );
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
