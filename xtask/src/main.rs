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
    /// `(url, path prefix)`: operations under the prefix that another copy
    /// of the spec documents but `url` lacks are merged in, with the schemas
    /// they use. `url` wins on conflicts. Scoped to a prefix so endpoints
    /// bunny.net removes from `url` are not brought back.
    pub merge: &'static [(&'static str, &'static str)],
    pub naming: Naming,
    /// Method names chosen by hand: `("METHOD /path", "name")`.
    pub overrides: &'static [(&'static str, &'static str)],
    /// Rust code appended to the generated module.
    pub extra: &'static str,
}

/// The official specs listed at <https://bunny.net/docs/llms.txt> ("OpenAPI Specs").
/// Storage and Logging are hand-written, so they are not listed: Storage needs
/// streaming, and the Logging spec does not describe the v1 response body.
pub const SPECS: &[Spec] = &[
    Spec {
        name: "core",
        url: "https://core-api-public-docs.b-cdn.net/docs/v3/public.json",
        title: "Core Platform API (pull zones, DNS, storage zones, video libraries, billing, ...)",
        client: "CoreClient",
        key: "your account API key",
        merge: &[],
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
        merge: &[],
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
        merge: &[],
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
        // The docs-site copy still documents the live streaming endpoints.
        merge: &[(
            "https://bunny.net/docs/api-reference/stream/openapi.json",
            "/library/{libraryId}/live",
        )],
        naming: Naming::Summary,
        overrides: &[
            ("GET /OEmbed", "get_oembed_data"),
            ("GET /library/{libraryId}/live", "list_live_streams"),
            ("POST /library/{libraryId}/live", "create_live_stream"),
            (
                "GET /library/{libraryId}/live/{streamId}",
                "get_live_stream",
            ),
            (
                "GET /library/{libraryId}/live/{streamId}/bitrate-history",
                "get_live_stream_bitrate_history",
            ),
            (
                "GET /library/{libraryId}/live/{streamId}/current-bitrate",
                "get_live_stream_current_bitrate",
            ),
            (
                "POST /library/{libraryId}/live/{streamId}/thumbnail",
                "set_live_stream_thumbnail",
            ),
        ],
        extra: "pub use crate::stream_upload::{TusAuthorization, TusUpload};\n",
    },
    Spec {
        name: "containers",
        url: "https://api-mc.opsbunny.net/docs/public/swagger.json",
        title: "Magic Containers API",
        client: "ContainersClient",
        key: "your account API key",
        merge: &[],
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
        merge: &[],
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
        merge: &[],
        naming: Naming::Summary,
        overrides: &[("GET /{pullZoneId}/{dateTime}", "get_origin_errors")],
        extra: "",
    },
    Spec {
        name: "partner",
        url: "https://core-api-public-docs.b-cdn.net/docs/v3/PartnerPortal.json",
        title: "Partner API (manage reseller customers, pricing and billing)",
        client: "PartnerClient",
        key: "your partner account API key",
        merge: &[],
        naming: Naming::Summary,
        overrides: &[],
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
        let mut json = download(spec.url)?;
        for (url, prefix) in spec.merge {
            merge(&mut json, &download(url)?, prefix);
            println!("{} <+ {url}", spec.name);
        }
        let mut text = serde_json::to_string_pretty(&json)?;
        text.push('\n');
        std::fs::write(dir.join(format!("{}.json", spec.name)), text)?;
        println!("{} <- {}", spec.name, spec.url);
    }
    Ok(())
}

fn download(url: &str) -> Result<serde_json::Value> {
    let output = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "--max-time", "60", url])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "downloading {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("{url} is not valid JSON: {e}").into())
}

/// Adds the operations under `prefix` that `other` has and `base` lacks,
/// plus the component schemas they use (transitively).
fn merge(base: &mut serde_json::Value, other: &serde_json::Value, prefix: &str) {
    use serde_json::Value;
    let mut needed = Vec::new();
    if let Some(Value::Object(paths)) = other.get("paths")
        && let Some(Value::Object(target)) = base.get_mut("paths")
    {
        for (path, item) in paths.iter().filter(|(p, _)| p.starts_with(prefix)) {
            let Value::Object(ops) = item else { continue };
            let entry = target
                .entry(path.clone())
                .or_insert_with(|| Value::Object(Default::default()));
            let Value::Object(entry) = entry else {
                continue;
            };
            for (method, op) in ops {
                if !entry.contains_key(method) {
                    schema_refs(op, &mut needed);
                    entry.insert(method.clone(), op.clone());
                }
            }
        }
    }
    while let Some(name) = needed.pop() {
        let Some(schema) = other.pointer(&format!("/components/schemas/{name}")) else {
            continue;
        };
        let Some(Value::Object(target)) = base.pointer_mut("/components/schemas") else {
            return;
        };
        if !target.contains_key(&name) {
            schema_refs(schema, &mut needed);
            target.insert(name, schema.clone());
        }
    }
}

/// Names of the component schemas `value` refers to.
fn schema_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                match v
                    .as_str()
                    .and_then(|r| r.strip_prefix("#/components/schemas/"))
                {
                    Some(name) if key == "$ref" => out.push(name.to_owned()),
                    _ => schema_refs(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| schema_refs(v, out)),
        _ => {}
    }
}
