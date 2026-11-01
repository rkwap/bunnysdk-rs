//! Maintenance tasks: `cargo xtask update-specs` and `cargo xtask codegen`.

mod codegen;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// How generated method names are derived.
#[derive(Clone, Copy)]
pub enum Naming {
    /// From the operation `summary` ("List DNS Zones" -> `list_dns_zones`).
    Summary,
    /// From the `operationId` ("DeployApplication" -> `deploy_application`).
    OperationId,
}

/// An official bunny.net OpenAPI spec and the module generated from it.
pub struct Spec {
    /// File name in `specs/` and Rust module name.
    pub name: &'static str,
    pub url: &'static str,
    /// Human-readable API name for docs.
    pub title: &'static str,
    /// Client struct the generated methods belong to.
    pub client: &'static str,
    /// Which key the client authenticates with.
    pub key: &'static str,
    pub naming: Naming,
    /// Method names chosen by hand: `("METHOD /path", "name")`.
    pub overrides: &'static [(&'static str, &'static str)],
    /// Rust code appended to the generated module.
    pub extra: &'static str,
}

/// Same sources as the official bunny.net TypeScript client (BunnyWay/cli).
/// Storage and Logging are hand-written, so they are not listed.
pub const SPECS: &[Spec] = &[
    Spec {
        name: "core",
        url: "https://core-api-public-docs.b-cdn.net/docs/v3/public.json",
        title: "Core Platform API (pull zones, DNS, storage zones, video libraries, billing, ...)",
        client: "CoreClient",
        key: "your account API key",
        naming: Naming::Summary,
        overrides: &[
            ("GET /dnszone/{id}/export", "export_dns_zone"),
            ("GET /user/audit/{date}", "get_user_audit_log"),
            (
                "POST /videolibrary/{id}/addAllowedReferrer",
                "add_video_library_allowed_referer",
            ),
            (
                "POST /videolibrary/{id}/removeAllowedReferrer",
                "remove_video_library_allowed_referer",
            ),
            (
                "POST /videolibrary/{id}/addBlockedReferrer",
                "add_video_library_blocked_referer",
            ),
            (
                "POST /videolibrary/{id}/removeBlockedReferrer",
                "remove_video_library_blocked_referer",
            ),
            (
                "GET /v1/pricing/{source}/{resourceId}",
                "get_price_estimation",
            ),
            (
                "POST /loadbalancer/{loadBalancerId}/origingroup",
                "add_load_balancer_origin_group",
            ),
            (
                "POST /loadbalancer/{loadBalancerId}/origingroup/{id}",
                "update_load_balancer_origin_group",
            ),
            (
                "DELETE /loadbalancer/{loadBalancerId}/origingroup/{id}",
                "delete_load_balancer_origin_group",
            ),
            (
                "GET /loadbalancer/{loadBalancerId}/pullzones",
                "get_load_balancer_pull_zones",
            ),
            (
                "GET /loadbalancer/statistics",
                "get_load_balancer_account_statistics",
            ),
            (
                "GET /loadbalancer/{loadBalancerId}/usage",
                "get_load_balancer_usage",
            ),
            ("POST /dnszone/records/scan", "trigger_dns_record_scan"),
            (
                "GET /dnszone/{zoneId}/records/scan",
                "get_latest_dns_record_scan",
            ),
        ],
        extra: "",
    },
    Spec {
        name: "compute",
        url: "https://core-api-public-docs.b-cdn.net/docs/v3/compute.json",
        title: "Edge Scripting API",
        client: "ComputeClient",
        key: "your account API key",
        naming: Naming::Summary,
        overrides: &[(
            "POST /compute/script/{id}/publish/{uuid}",
            "publish_release_by_uuid",
        )],
        extra: "",
    },
    Spec {
        name: "shield",
        url: "https://api.bunny.net/shield/docs/v1/swagger.json",
        title: "Shield API (WAF, rate limiting, bot detection, access lists)",
        client: "ShieldClient",
        key: "your account API key",
        naming: Naming::OperationId,
        overrides: &[
            (
                "PATCH /shield/waf/custom-rule/{id}",
                "patch_custom_waf_rule",
            ),
            ("GET /shield/ddos/enums", "get_ddos_enums"),
        ],
        extra: "",
    },
    Spec {
        name: "stream",
        url: "https://video.bunnycdn.com/openapi/bunnynet-video-api.public.json",
        title: "Stream API (video libraries, videos, collections)",
        client: "StreamClient",
        key: "the video library's API key (not the account key)",
        naming: Naming::Summary,
        overrides: &[("GET /OEmbed", "get_oembed_data")],
        extra: "pub use crate::stream_upload::{TusAuthorization, TusUpload};\n",
    },
    Spec {
        name: "containers",
        url: "https://api-mc.opsbunny.net/docs/public/swagger.json",
        title: "Magic Containers API",
        client: "ContainersClient",
        key: "your account API key",
        naming: Naming::OperationId,
        overrides: &[],
        extra: "",
    },
    Spec {
        name: "database",
        url: "https://api.bunny.net/database/docs/private/api.json",
        title: "Database API (managed libSQL databases)",
        client: "DatabaseClient",
        key: "your account API key",
        naming: Naming::Summary,
        overrides: &[
            ("GET /v1/groups", "list_groups"),
            ("POST /v1/groups", "create_group"),
            ("GET /v1/groups/{group_id}", "get_group"),
            ("DELETE /v1/groups/{group_id}", "delete_group"),
            ("PATCH /v1/groups/{group_id}", "update_group"),
            ("GET /v1/groups/{group_id}/stats", "get_group_statistics"),
            (
                "GET /v1/groups/{group_id}/aggregated_usage",
                "get_group_aggregated_usage",
            ),
            (
                "POST /v1/groups/{group_id}/auth/generate",
                "generate_group_token",
            ),
            (
                "POST /v1/groups/{group_id}/auth/invalidate",
                "invalidate_group_tokens",
            ),
            ("GET /v1/databases", "list_databases_v1"),
            ("POST /v1/databases", "create_database_v1"),
            ("GET /v1/databases/{db_id}", "get_database_v1"),
            ("DELETE /v1/databases/{db_id}", "delete_database_v1"),
            (
                "POST /v1/databases/{db_id}/list_versions",
                "list_database_versions",
            ),
            ("POST /v1/databases/{db_id}/restore", "restore_database"),
            ("POST /v1/databases/{db_id}/fork", "fork_database"),
            (
                "POST /v1/databases/{db_id}/auth/tokens",
                "generate_database_token_v1",
            ),
            (
                "POST /v1/databases/{db_id}/auth/invalidate",
                "invalidate_database_tokens_v1",
            ),
            ("POST /v1/live/live_db", "live_database_metrics"),
            ("POST /v1/live/live_group", "live_group_metrics"),
            ("GET /v1/config", "get_config"),
            ("GET /v1/config/optimal", "get_optimal_config"),
            (
                "GET /v1/config/optimal_single",
                "get_optimal_single_region_config",
            ),
            ("GET /v1/config/limits", "get_limits"),
            ("GET /v2/databases", "list_databases"),
            ("POST /v2/databases", "create_database"),
            ("GET /v2/databases/{db_id}", "get_database"),
            ("DELETE /v2/databases/{db_id}", "delete_database"),
            ("PATCH /v2/databases/{db_id}", "update_database"),
            ("GET /v2/databases/{db_id}/usage", "get_database_usage"),
            ("GET /v2/databases/active_usage", "get_active_usage"),
            (
                "GET /v2/databases/{db_id}/statistics",
                "get_database_statistics",
            ),
            (
                "PUT /v2/databases/{db_id}/auth/generate",
                "generate_database_token",
            ),
            (
                "POST /v2/databases/{db_id}/auth/revoke",
                "invalidate_database_tokens",
            ),
        ],
        extra: "",
    },
    Spec {
        name: "origin_errors",
        url: "https://bunny.net/docs/api-reference/origin-errors/openapi.json",
        title: "Origin Errors API",
        client: "OriginErrorsClient",
        key: "your account API key",
        naming: Naming::Summary,
        overrides: &[("GET /{pullZoneId}/{dateTime}", "get_origin_errors")],
        extra: "",
    },
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "update-specs" => update_specs(),
        "codegen" => codegen::run(&root()),
        _ => Err("usage: cargo xtask <update-specs|codegen>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The repository root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is inside the repository")
        .to_path_buf()
}

/// Downloads every spec and stores it pretty-printed, so updates diff cleanly.
fn update_specs() -> Result {
    let dir = root().join("specs");
    std::fs::create_dir_all(&dir)?;
    for spec in SPECS {
        let output = Command::new("curl")
            .args(["-fsSL", "--retry", "3", "--max-time", "60", spec.url])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "downloading {}: {}",
                spec.url,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("{} is not valid JSON: {e}", spec.url))?;
        let mut text = serde_json::to_string_pretty(&json)?;
        text.push('\n');
        std::fs::write(dir.join(format!("{}.json", spec.name)), text)?;
        println!("{} <- {}", spec.name, spec.url);
    }
    Ok(())
}
