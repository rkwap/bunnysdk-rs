//! Live tests against the real bunny.net APIs.
//!
//! Ignored by default. Each test skips itself (printing why) when its
//! environment variables are missing:
//!
//! | Variable | Used for |
//! |---|---|
//! | `BUNNY_API_KEY` | account API key: all APIs except Partner (read-only calls) |
//! | `BUNNY_TEST_PULL_ZONE_ID` | Logging, Origin Errors (default: the first pull zone) |
//! | `BUNNY_TEST_STORAGE_ZONE_ID` | Storage write test: files under `bunnysdk-ci/`, deleted after |
//! | `BUNNY_TEST_VIDEO_LIBRARY_ID` | Stream write test: creates and deletes test videos |
//! | `BUNNY_PARTNER_API_KEY` | Partner API (partner accounts only) |
//!
//! ```sh
//! BUNNY_API_KEY=... cargo test --test live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every call is checked and reported as `ok` or `FAIL`; a test fails at the
//! end if any call did. Values are never printed: some responses contain
//! credentials (storage zone passwords, library API keys).

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bunnysdk::Config;
use bunnysdk::core::CoreClient;
use bunnysdk::storage::{Region, StorageClient};
use bunnysdk::stream::{CreateVideoModel, StreamClient, TusUpload, UploadVideoQuery};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn id_var(name: &str) -> Option<i64> {
    var(name).map(|v| {
        v.trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a number"))
    })
}

/// Returns the variable's value, or skips the test.
macro_rules! require {
    ($name:expr) => {
        match var($name) {
            Some(value) => value,
            None => {
                println!("skip: {} is not set", $name);
                return;
            }
        }
    };
}

fn config() -> Config {
    Config::new()
        .max_retries(3)
        .timeout(Duration::from_secs(60))
}

fn core_client(key: &str) -> CoreClient {
    CoreClient::with_config(key, config()).unwrap()
}

/// Runs calls, prints `ok`/`FAIL` for each, and fails the test at the end.
struct Checks {
    api: &'static str,
    failures: Vec<String>,
}

impl Checks {
    fn new(api: &'static str) -> Self {
        Self {
            api,
            failures: Vec::new(),
        }
    }

    async fn run<T>(
        &mut self,
        what: &str,
        call: impl Future<Output = bunnysdk::Result<T>>,
    ) -> Option<T> {
        match call.await {
            Ok(value) => {
                println!("ok   {}: {what}", self.api);
                Some(value)
            }
            Err(e) => {
                println!("FAIL {}: {what}: {e}", self.api);
                self.failures.push(format!("{what}: {e}"));
                None
            }
        }
    }

    /// Like [`Checks::run`], but a 404 (feature not enabled on the account) is fine.
    async fn run_allow_404<T>(
        &mut self,
        what: &str,
        call: impl Future<Output = bunnysdk::Result<T>>,
    ) -> Option<T> {
        match call.await {
            Err(e) if e.is_not_found() => {
                println!("ok   {}: {what} (404: {e})", self.api);
                None
            }
            other => self.run(what, async { other }).await,
        }
    }

    fn check(&mut self, what: &str, ok: bool) {
        if ok {
            println!("ok   {}: {what}", self.api);
        } else {
            println!("FAIL {}: {what}", self.api);
            self.failures.push(what.to_owned());
        }
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} live check(s) failed for {}:\n  {}",
            self.failures.len(),
            self.api,
            self.failures.join("\n  ")
        );
    }
}

/// Today's UTC date as (year, month, day).
fn today() -> (i64, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn core_read_only() {
    let key = require!("BUNNY_API_KEY");
    let core = core_client(&key);
    let mut c = Checks::new("core");

    if let Some(countries) = c.run("get_country_list", core.get_country_list()).await {
        c.check("country list is not empty", !countries.is_empty());
    }
    c.run("region_list", core.region_list()).await;
    c.run("get_billing_details", core.get_billing_details())
        .await;
    c.run("get_statistics", core.get_statistics(&Default::default()))
        .await;
    c.run("list_api_keys", core.list_api_keys(&Default::default()))
        .await;

    let zones = c
        .run("list_pull_zones", core.list_pull_zones(&Default::default()))
        .await;
    if let Some(id) = zones.and_then(|z| z.into_iter().find_map(|z| z.id)) {
        let query = bunnysdk::core::GetPullZoneQuery {
            include_certificate: Some(true),
        };
        c.run("get_pull_zone", core.get_pull_zone(id, &query)).await;
    }
    let paged = bunnysdk::core::ListPullZonesQuery {
        page: Some(1),
        per_page: Some(5),
        ..Default::default()
    };
    c.run("list_pull_zones (page 1)", core.list_pull_zones(&paged))
        .await;

    let zones = c
        .run(
            "list_storage_zones",
            core.list_storage_zones(&Default::default()),
        )
        .await;
    if let Some(id) = zones.and_then(|z| z.into_iter().find_map(|z| z.id)) {
        c.run("get_storage_zone", core.get_storage_zone(id)).await;
    }

    let zones = c
        .run("list_dns_zones", core.list_dns_zones(&Default::default()))
        .await;
    if let Some(id) = zones.and_then(|z| z.items?.into_iter().find_map(|z| z.id)) {
        c.run("get_dns_zone", core.get_dns_zone(id, &Default::default()))
            .await;
    }

    let libraries = c
        .run(
            "list_video_libraries",
            core.list_video_libraries(&Default::default()),
        )
        .await;
    if let Some(id) = libraries.and_then(|l| l.items?.into_iter().find_map(|l| l.id)) {
        c.run("get_video_library", core.get_video_library(id)).await;
    }

    let err = core.get_pull_zone(i64::MAX, &Default::default()).await;
    c.check(
        "unknown pull zone is a 4xx error",
        err.is_err_and(|e| e.status().is_some_and(|s| (400..500).contains(&s))),
    );
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn compute_read_only() {
    let key = require!("BUNNY_API_KEY");
    let compute = bunnysdk::compute::ComputeClient::with_config(&key, config()).unwrap();
    let mut c = Checks::new("compute");
    let scripts = c
        .run(
            "list_edge_scripts",
            compute.list_edge_scripts(&Default::default()),
        )
        .await;
    if let Some(id) = scripts.and_then(|s| s.items?.into_iter().find_map(|s| s.id)) {
        c.run("get_edge_script", compute.get_edge_script(id)).await;
        c.run("get_active_release", compute.get_active_release(id))
            .await;
    }
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn shield_read_only() {
    let key = require!("BUNNY_API_KEY");
    let shield = bunnysdk::shield::ShieldClient::with_config(&key, config()).unwrap();
    let mut c = Checks::new("shield");
    c.run("get_waf_enum_mappings", shield.get_waf_enum_mappings())
        .await;
    c.run(
        "get_shield_zones_pull_zone_mapping",
        shield.get_shield_zones_pull_zone_mapping(),
    )
    .await;
    let zones = c
        .run(
            "get_shield_zone_configurations",
            shield.get_shield_zone_configurations(&Default::default()),
        )
        .await;
    if let Some(id) = zones.and_then(|z| z.data?.into_iter().find_map(|z| z.shield_zone_id)) {
        c.run(
            "get_shield_zone_configuration",
            shield.get_shield_zone_configuration(i32::try_from(id).expect("zone ID fits i32")),
        )
        .await;
        c.run(
            "get_rate_limits",
            shield.get_rate_limits(id, &Default::default()),
        )
        .await;
        c.run(
            "get_custom_waf_rules",
            shield.get_custom_waf_rules(id, &Default::default()),
        )
        .await;
    }
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn containers_read_only() {
    let key = require!("BUNNY_API_KEY");
    let mc = bunnysdk::containers::ContainersClient::with_config(&key, config()).unwrap();
    let mut c = Checks::new("containers");
    c.run("get_limits", mc.get_limits()).await;
    c.run("list_regions", mc.list_regions(&Default::default()))
        .await;
    c.run("list_container_registries", mc.list_container_registries())
        .await;
    let apps = c
        .run(
            "list_applications",
            mc.list_applications(&Default::default()),
        )
        .await;
    if let Some(id) = apps.and_then(|a| a.items?.into_iter().find_map(|a| a.id)) {
        c.run("get_application", mc.get_application(&id)).await;
    }
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn database_read_only() {
    let key = require!("BUNNY_API_KEY");
    let db = bunnysdk::database::DatabaseClient::with_config(&key, config()).unwrap();
    let mut c = Checks::new("database");
    c.run("get_config", db.get_config()).await;
    c.run("get_limits", db.get_limits()).await;
    let list = c
        .run("list_databases", db.list_databases(&Default::default()))
        .await;
    if let Some(id) = list.and_then(|l| l.databases.into_iter().next().map(|d| d.id)) {
        c.run("get_database", db.get_database(&id)).await;
    }
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY"]
async fn logging_and_origin_errors() {
    let key = require!("BUNNY_API_KEY");
    let mut c = Checks::new("logging");
    let pull_zone = match id_var("BUNNY_TEST_PULL_ZONE_ID") {
        Some(id) => Some(id),
        None => core_client(&key)
            .list_pull_zones(&Default::default())
            .await
            .ok()
            .and_then(|z| z.into_iter().find_map(|z| z.id)),
    };
    let Some(pull_zone) = pull_zone else {
        println!("skip: no pull zone on the account and BUNNY_TEST_PULL_ZONE_ID is not set");
        return;
    };
    let (year, month, day) = today();

    let logging = bunnysdk::logging::LoggingClient::with_config(&key, config()).unwrap();
    let query = bunnysdk::logging::LogQuery {
        limit: Some(5),
        ..Default::default()
    };
    c.run_allow_404("get_logs (v2)", logging.get_logs(pull_zone, &query))
        .await;
    let date = format!("{month:02}-{day:02}-{:02}", year % 100);
    let raw = bunnysdk::logging::RawLogQuery {
        end: Some(5),
        ..Default::default()
    };
    c.run_allow_404(
        "get_raw_logs (v1)",
        logging.get_raw_logs(pull_zone, &date, &raw),
    )
    .await;

    let origin = bunnysdk::origin_errors::OriginErrorsClient::with_config(&key, config()).unwrap();
    let date = format!("{month:02}-{day:02}-{year}");
    c.run_allow_404(
        "origin_errors::get_origin_errors",
        origin.get_origin_errors(pull_zone, &date),
    )
    .await;
    c.finish();
}

#[tokio::test]
#[ignore = "live: needs BUNNY_PARTNER_API_KEY"]
async fn partner_read_only() {
    let key = require!("BUNNY_PARTNER_API_KEY");
    let partner = bunnysdk::partner::PartnerClient::with_config(&key, config()).unwrap();
    let mut c = Checks::new("partner");
    c.run("get_partner_overview", partner.get_partner_overview())
        .await;
    c.run("get_partner_pricing", partner.get_partner_pricing())
        .await;
    c.run("get_contract", partner.get_contract()).await;
    c.run("list_customers", partner.list_customers()).await;
    c.finish();
}

/// Creates, uploads (simple and resumable), reads and deletes test videos.
#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY and BUNNY_TEST_VIDEO_LIBRARY_ID"]
async fn stream_roundtrip() {
    let key = require!("BUNNY_API_KEY");
    let library_id = require!("BUNNY_TEST_VIDEO_LIBRARY_ID");
    let library_id: i64 = library_id.trim().parse().expect("a numeric library ID");
    let mut c = Checks::new("stream");

    let library = c
        .run(
            "core::get_video_library",
            core_client(&key).get_video_library(library_id),
        )
        .await;
    let Some(library_key) = library.and_then(|l| l.api_key) else {
        c.check("video library has an API key", false);
        return c.finish();
    };
    let stream = StreamClient::with_config(&library_key, config()).unwrap();

    c.run(
        "list_videos",
        stream.list_videos(library_id, &Default::default()),
    )
    .await;
    c.run(
        "get_collection_list",
        stream.get_collection_list(library_id, &Default::default()),
    )
    .await;
    c.run_allow_404(
        "list_live_streams",
        stream.list_live_streams(library_id, &Default::default()),
    )
    .await;

    let title = format!("bunnysdk-ci {}", unique());
    let body = CreateVideoModel {
        title: title.clone(),
        ..Default::default()
    };

    // Simple upload. The bytes aren't a real video, so encoding will fail
    // later; the upload itself must succeed.
    if let Some(video) = c
        .run("create_video", stream.create_video(library_id, &body))
        .await
    {
        let data = vec![0u8; 64 * 1024];
        c.run(
            "upload_video",
            stream.upload_video(library_id, &video.guid, &UploadVideoQuery::default(), data),
        )
        .await;
        c.run("get_video", stream.get_video(library_id, &video.guid))
            .await;
        c.run("delete_video", stream.delete_video(library_id, &video.guid))
            .await;
    }

    // Resumable upload in two chunks.
    if let Some(video) = c
        .run(
            "create_video (resumable)",
            stream.create_video(library_id, &body),
        )
        .await
    {
        let data = vec![0u8; 3 * 1024 * 1024];
        let options = TusUpload::new(&title, "video/mp4").chunk_size(2 * 1024 * 1024);
        c.run(
            "upload_resumable",
            stream.upload_resumable(
                library_id,
                &video.guid,
                &data[..],
                data.len() as u64,
                &options,
            ),
        )
        .await;
        c.run(
            "delete_video (resumable)",
            stream.delete_video(library_id, &video.guid),
        )
        .await;
    }
    c.finish();
}

/// Uploads, lists, downloads and deletes files under `bunnysdk-ci/`.
#[tokio::test]
#[ignore = "live: needs BUNNY_API_KEY and BUNNY_TEST_STORAGE_ZONE_ID"]
async fn storage_roundtrip() {
    let key = require!("BUNNY_API_KEY");
    let zone_id = require!("BUNNY_TEST_STORAGE_ZONE_ID");
    let zone_id: i64 = zone_id.trim().parse().expect("a numeric storage zone ID");
    let mut c = Checks::new("storage");

    let Some(zone) = c
        .run(
            "core::get_storage_zone",
            core_client(&key).get_storage_zone(zone_id),
        )
        .await
    else {
        return c.finish();
    };
    let (Some(name), Some(password)) = (zone.name, zone.password) else {
        c.check("storage zone has a name and password", false);
        return c.finish();
    };
    // The zone's own hostname is authoritative; the region is a fallback.
    let mut storage_config = config();
    if let Some(host) = zone.storage_hostname.filter(|h| !h.is_empty()) {
        storage_config = storage_config.base_url(format!("https://{host}"));
    }
    let region = zone
        .region
        .and_then(|r| r.parse::<Region>().ok())
        .unwrap_or_default();
    let storage = StorageClient::with_config(name, &password, region, storage_config).unwrap();

    let dir = format!("bunnysdk-ci/{}/", unique());
    let file = format!("{dir}hello world.txt");
    let data = format!("hello from bunnysdk-rs {}", unique());

    c.run("upload", storage.upload(&file, data.clone())).await;
    if let Some(listed) = c.run("list", storage.list(&dir)).await {
        c.check(
            "uploaded file is listed",
            listed
                .iter()
                .any(|o| o.object_name == "hello world.txt" && !o.is_directory),
        );
    }
    if let Some(downloaded) = c.run("download", storage.download(&file)).await {
        c.check("downloaded content matches", downloaded == data.as_bytes());
    }
    if let Some(mut download) = c
        .run("download_stream", storage.download_stream(&file))
        .await
    {
        let mut received = Vec::new();
        while let Ok(Some(chunk)) = download.chunk().await {
            received.extend_from_slice(&chunk);
        }
        c.check("streamed content matches", received == data.as_bytes());
    }
    let missing = storage.download(&format!("{dir}missing.txt")).await;
    c.check(
        "missing file is a 404",
        missing.is_err_and(|e| e.is_not_found()),
    );
    c.run("delete directory", storage.delete(&dir)).await;
    c.finish();
}

#[test]
fn today_is_a_valid_date() {
    let (year, month, day) = today();
    assert!(year >= 2025);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
}
