use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, info};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Get base URL for server
pub fn server_base_url(server: &str) -> &'static str {
    match server {
        "cn" => "https://game.maj-soul.com",
        "en" | "jp" => "https://mahjongsoul.game.yo-star.com",
        _ => unreachable!("clap ValueEnum guarantees en|jp|cn"),
    }
}

/// Get path prefix for server (CN uses /1/, EN doesn't)
fn server_path_prefix(server: &str) -> &'static str {
    match server {
        "cn" => "/1",
        _ => "",
    }
}

#[derive(Debug, Deserialize)]
pub struct VersionInfo {
    pub version: String,
}

#[derive(Debug, Deserialize)]
struct Gateway {
    url: String,
}

#[derive(Debug, Deserialize)]
struct IpEntry {
    #[serde(default)]
    gateways: Vec<Gateway>,
}

#[derive(Debug, Deserialize)]
struct Config {
    ip: Vec<IpEntry>,
}

/// Route entry from /api/clientgate/routes response
#[derive(Debug, Deserialize)]
struct Route {
    domain: String,
    id: String,
}

/// Inner data from /api/clientgate/routes
#[derive(Debug, Deserialize)]
struct RoutesData {
    routes: Vec<Route>,
}

/// Routes response from /api/clientgate/routes
#[derive(Debug, Deserialize)]
struct RoutesResponse {
    data: RoutesData,
}

/// Discover gateway endpoint, version, and `route_id` for Majsoul server
///
/// Returns (endpoint, version, `route_id`) tuple needed for connection
pub async fn discover_gateway(client: &reqwest::Client, server: &str) -> Result<(String, String, String)> {
    let ms_host = server_base_url(server);
    let prefix = server_path_prefix(server);
    info!("Using {} server: {}", server, ms_host);

    // Step 1: Get version (check HTTP status before .json(), bail non-2xx).
    let version_url = format!("{ms_host}{prefix}/version.json");
    let version_info: VersionInfo = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let resp = client.get(&version_url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("version.json HTTP {status}");
        }
        resp.json().await.context("Failed to parse version.json")
    })
    .await
    .context("Timeout fetching version.json")??;

    let version = &version_info.version;
    // version.json reports e.g. "X.Y.w"; login/fetch expect "web-X.Y" — strip ".w", never forward verbatim.
    let version_clean = version.replace(".w", "");
    info!("Majsoul version: {}", version);

    // Step 2: Get config to find gateway URLs.
    let config_url = format!("{ms_host}{prefix}/v{version}/config.json");
    let config: Config = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let resp = client.get(&config_url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("config.json HTTP {status}");
        }
        resp.json().await.context("Failed to parse config.json")
    })
    .await
    .context("Timeout fetching config.json")??;

    // Collect all gateway base URLs (ips -> gateways) for failover.
    let gateway_bases: Vec<String> = config
        .ip
        .iter()
        .flat_map(|ip| ip.gateways.iter())
        .map(|g| g.url.trim_end_matches('/').to_string())
        .collect();
    if gateway_bases.is_empty() {
        anyhow::bail!("No gateway found in config");
    }

    // Step 3: Iterate gateways -> routes with fallback instead of .first().
    let mut last_err: Option<anyhow::Error> = None;
    for gateway_base in &gateway_bases {
        debug!("Gateway base URL: {}", gateway_base);
        let routes_url = format!(
            "{gateway_base}/api/clientgate/routes?platform=Web&version={version}"
        );
        let routes_response: Result<RoutesResponse> = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let resp = client.get(&routes_url).send().await?;
            let status = resp.status();
            if !status.is_success() {
                anyhow::bail!("routes HTTP {status}");
            }
            resp.json().await.context("Failed to parse routes response")
        })
        .await
        .context("Timeout fetching routes")
        .and_then(|r| r);
        let routes_response = match routes_response {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        if let Some(route) = routes_response.data.routes.first() {
            let route_id = route.id.clone();
            let route_domain = &route.domain;
            debug!("Route: domain={}, id={}", route_domain, route_id);
            let endpoint = format!("wss://{route_domain}/gateway");
            info!("Discovered gateway: {} (route_id: {})", endpoint, route_id);
            return Ok((endpoint, version_clean.clone(), route_id));
        }
        last_err = Some(anyhow::anyhow!("No routes found in response from {gateway_base}"));
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("No gateway/route available")))
}

/// Discover the gateway, connect, and log in with native credentials.
///
/// Single attempt, no retry: callers keep their own retry loops and call this
/// once per attempt. Returns the logged-in RPC handle plus the discovered
/// version string (`.w` suffix already stripped by [`discover_gateway`];
/// format as `web-{version}` for fetch calls).
pub async fn discover_and_connect(
    client: &reqwest::Client,
    server: &str,
    username: &str,
    password: &str,
) -> Result<(super::rpc::MajsoulRpc, String)> {
    let (endpoint, version, route_id) = discover_gateway(client, server).await?;
    // The gateway validates the WS Origin against its own host (see rpc.rs).
    let origin = super::rpc::origin_for_server(server);
    let rpc = super::rpc::MajsoulRpc::connect(&endpoint, origin).await?;
    rpc.login_native(username, password, &version, &route_id).await?;
    Ok((rpc, version))
}

/// Verdict for a failed `fetch_game_record`.
///
/// The 60s sleep and the bounded-3 version-retry counter live at the call
/// sites (loop shapes differ per caller), so this returns the verdict only.
/// Close semantics also stay at the call site (keep-rpc vs close-and-bail).
pub enum FetchOutcome {
    /// Version rejected (code 151): caller runs its bounded rediscovery flow
    /// inline (counter + 60s sleep), marks the uuid failed, and continues.
    Done,
    /// Generic/transient failure: caller marks the uuid error and continues.
    MarkAndContinue,
    /// Rate limited (code 103): caller finishes progress, closes the RPC
    /// handle per its own semantics, and returns the carried error.
    Abort(anyhow::Error),
}

/// Classify a `fetch_game_record` failure into a batch verdict.
///
/// `VersionMismatch` → [`FetchOutcome::Done`], `RateLimited` →
/// [`FetchOutcome::Abort`] (carrying the shared rate-limit message),
/// anything else (fatal code or unparsable message) →
/// [`FetchOutcome::MarkAndContinue`].
pub fn classify_outcome(e: &anyhow::Error) -> FetchOutcome {
    match super::rpc::classify_fetch_error(e) {
        Some(super::rpc::MajsoulError::VersionMismatch) => FetchOutcome::Done,
        Some(super::rpc::MajsoulError::RateLimited) => {
            FetchOutcome::Abort(anyhow::anyhow!(
                "Rate limited by Majsoul; lower your request rate and retry later."
            ))
        }
        _ => FetchOutcome::MarkAndContinue,
    }
}
