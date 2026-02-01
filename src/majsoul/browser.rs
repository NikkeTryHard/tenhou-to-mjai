//! Browser automation for Majsoul token capture via Chrome DevTools Protocol.

use anyhow::{Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnable, EventWebSocketFrameReceived,
};
use futures::StreamExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// Path to cached token file
fn token_cache_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .context("Could not find config directory")?
        .join("tenhou-scraper");
    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("majsoul-token.json"))
}

/// Cached token data
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct CachedToken {
    pub access_token: String,
    pub uid: u64,
    pub captured_at: i64,
    #[serde(default = "default_server")]
    pub server: String,
}

fn default_server() -> String {
    "en".to_string()
}

impl CachedToken {
    /// Load cached token from disk
    pub fn load() -> Result<Option<Self>> {
        let path = token_cache_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let token: CachedToken = serde_json::from_str(&data)?;
        Ok(Some(token))
    }

    /// Save token to disk
    pub fn save(&self) -> Result<()> {
        let path = token_cache_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        info!("Token saved to {:?}", path);
        Ok(())
    }
}

/// Server URLs for different regions
pub fn server_url(server: &str) -> &'static str {
    match server {
        "cn" => "https://game.maj-soul.com/1/",
        "en" | "jp" => "https://mahjongsoul.game.yo-star.com/",
        _ => "https://mahjongsoul.game.yo-star.com/",
    }
}

/// Launch Chrome with optimized flags for fast startup
async fn launch_browser(
) -> Result<(
    Browser,
    impl futures::Stream<Item = Result<(), chromiumoxide::error::CdpError>>,
)> {
    // Check if Chrome is available
    if std::process::Command::new("google-chrome")
        .arg("--version")
        .output()
        .is_err()
        && std::process::Command::new("chromium")
            .arg("--version")
            .output()
            .is_err()
        && std::process::Command::new("chromium-browser")
            .arg("--version")
            .output()
            .is_err()
    {
        anyhow::bail!(
            "Chrome/Chromium not found. Please install Chrome or Chromium.\n\
             Ubuntu: sudo apt install chromium-browser\n\
             Arch: sudo pacman -S chromium"
        );
    }

    // Use a unique temp dir each time to avoid conflicts with running Chrome
    let user_data_dir = std::env::temp_dir()
        .join(format!("majsoul-auth-{}", std::process::id()));

    // Try to find chromium first, fall back to chrome
    let chrome_path = ["chromium", "chromium-browser", "google-chrome", "chrome"]
        .iter()
        .find_map(|name| {
            std::process::Command::new("which")
                .arg(name)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        });

    let mut builder = BrowserConfig::builder()
        .no_sandbox()
        .with_head()
        .disable_default_args()
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--exclude-switches=enable-automation")
        .arg("--disable-dev-shm-usage")
        .arg("--no-first-run")
        .arg("--disable-features=Translate,OptimizationHints,MediaRouter")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .window_size(1280, 800);

    if let Some(path) = chrome_path {
        info!("Using browser: {}", path);
        builder = builder.chrome_executable(path);
    }

    let config = builder
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build browser config: {}", e))?;

    let (browser, handler) = Browser::launch(config)
        .await
        .context("Failed to launch Chrome. Is Chrome/Chromium installed?")?;

    Ok((browser, handler))
}

/// Fetch a game record using the browser's authenticated session
pub async fn fetch_game_record_via_browser(
    server: &str,
    uuid: &str,
) -> Result<Vec<u8>> {
    let url = server_url(server);
    info!("Launching browser to fetch game record: {}", uuid);

    let (browser, mut handler) = launch_browser().await?;

    let handler_task = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    let page = browser
        .new_page(url)
        .await
        .context("Failed to open Majsoul page")?;

    // Wait for game to load and check if logged in
    info!("Waiting for game to initialize...");
    info!("Please login in the browser if not already logged in.");
    let uuid_owned = uuid.to_string();

    let result = timeout(Duration::from_secs(300), async {
        // Wait for lobby to be ready
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Check if we're in the lobby (uiscript.UI_Lobby visible or app ready)
            let check = page.evaluate(r#"
                (function() {
                    // Check multiple indicators of being in lobby
                    if (typeof app === 'undefined') return false;
                    if (!app.NetMgr) return false;
                    // Check if Lobby RPC is available
                    if (app.NetMgr.Lobby && typeof app.NetMgr.Lobby.fetchGameRecord === 'function') {
                        return true;
                    }
                    // Alternative: check if we have account data
                    if (app.account_data && app.account_data.account_id) {
                        return true;
                    }
                    return false;
                })()
            "#).await;

            if let Ok(val) = check {
                if val.value().and_then(|v| v.as_bool()) == Some(true) {
                    info!("Lobby detected!");
                    break;
                }
            }

            // Debug: show what's available
            let debug = page.evaluate(r#"
                JSON.stringify({
                    hasApp: typeof app !== 'undefined',
                    hasNetMgr: typeof app !== 'undefined' && !!app.NetMgr,
                    hasLobby: typeof app !== 'undefined' && app.NetMgr && !!app.NetMgr.Lobby,
                    hasFetch: typeof app !== 'undefined' && app.NetMgr && app.NetMgr.Lobby && typeof app.NetMgr.Lobby.fetchGameRecord === 'function',
                    token: localStorage.getItem('ssssoooodd') ? 'exists' : 'none'
                })
            "#).await;
            if let Ok(d) = debug {
                if let Some(s) = d.value().and_then(|v| v.as_str()) {
                    debug!("App state: {}", s);
                }
            }
        }

        info!("Fetching record...");

        // Call fetchGameRecord
        let script = format!(
            r#"
            new Promise((resolve, reject) => {{
                const uuid = "{}";
                app.NetMgr.Lobby.fetchGameRecord({{ game_uuid: uuid }}, (err, res) => {{
                    if (err) {{
                        reject(JSON.stringify(err));
                    }} else {{
                        // Convert protobuf to base64
                        const bytes = res.data || res.data_url;
                        if (bytes instanceof Uint8Array) {{
                            resolve(btoa(String.fromCharCode.apply(null, bytes)));
                        }} else {{
                            resolve(JSON.stringify(res));
                        }}
                    }}
                }});
            }})
            "#,
            uuid_owned
        );

        let result = page.evaluate(script).await
            .context("Failed to call fetchGameRecord")?;

        if let Some(data) = result.value().and_then(|v| v.as_str()) {
            Ok(data.to_string())
        } else {
            anyhow::bail!("No data returned from fetchGameRecord")
        }
    })
    .await
    .context("Timeout fetching game record")??;

    drop(page);
    drop(browser);
    handler_task.abort();

    // Decode base64 if needed
    if result.starts_with('{') {
        // JSON response
        Ok(result.into_bytes())
    } else {
        // Base64 encoded protobuf
        base64::engine::general_purpose::STANDARD
            .decode(&result)
            .context("Failed to decode base64 response")
    }
}

/// Capture access token by grabbing localStorage after login completes
/// Also intercepts WebSocket to see actual auth flow
pub async fn capture_token_interactive(server: &str) -> Result<CachedToken> {
    let url = server_url(server);
    info!("Launching Chrome for Majsoul authentication ({} server)...", server);
    info!("URL: {}", url);

    let (browser, mut handler) = launch_browser().await?;

    // Drive the browser connection in background
    let handler_task = tokio::spawn(async move {
        while let Some(result) = handler.next().await {
            if result.is_err() {
                break;
            }
        }
    });

    // Inject JS hook BEFORE navigation to intercept token the moment it's set
    let inject_script = r#"
        (function() {
            // Hook localStorage.setItem to capture ssssoooodd the moment it's set
            const originalSetItem = localStorage.setItem.bind(localStorage);
            localStorage.setItem = function(key, value) {
                if (key === 'ssssoooodd') {
                    window._capturedLiqiToken = value;
                    console.log('[HOOK] Captured ssssoooodd on setItem:', value);
                }
                return originalSetItem(key, value);
            };
            console.log('[HOOK] localStorage.setItem interceptor installed');
        })();
    "#;

    // Create page but don't navigate yet
    let page = browser
        .new_page("about:blank")
        .await
        .context("Failed to create page")?;

    // Add script to run on every new document (before page scripts)
    use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
    page.execute(AddScriptToEvaluateOnNewDocumentParams::new(inject_script.to_string()))
        .await
        .context("Failed to inject interceptor script")?;
    info!("Injected WebSocket interceptor for liqi_access_token capture");

    // Clear localStorage and cookies to force fresh login (otherwise auto-login skips oauth2Auth)
    use chromiumoxide::cdp::browser_protocol::storage::ClearDataForOriginParams;
    let clear_params = ClearDataForOriginParams::new(
        url.trim_end_matches('/').to_string(),
        "cookies,local_storage".to_string(),
    );
    if let Err(e) = page.execute(clear_params).await {
        warn!("Failed to clear storage (non-fatal): {}", e);
    }
    info!("Cleared cookies/localStorage to force fresh login");

    // NOW navigate to Majsoul - hook will be active
    page.goto(url).await.context("Failed to navigate to Majsoul")?;

    // Enable network interception to see WebSocket frames
    page.execute(NetworkEnable::default()).await?;

    info!("Waiting for login... (timeout: 5 minutes)");
    info!("Login in the browser, then wait for token capture.");

    // Poll for token - check window._capturedLiqiToken (CN) or localStorage (EN/JP)
    let server_owned = server.to_string();
    let mut attempts = 0;
    let token = timeout(Duration::from_secs(300), async {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempts += 1;

            // Check for captured liqi_access_token from WebSocket hook (CN/TW/HK)
            if let Ok(captured) = page.evaluate(r#"window._capturedLiqiToken || null"#).await {
                if let Some(liqi_token) = captured.value().and_then(|v| v.as_str()) {
                    if liqi_token.len() == 36 && liqi_token.contains('-') {
                        info!("Captured liqi_access_token from WebSocket hook!");
                        return CachedToken {
                            access_token: liqi_token.to_string(),
                            uid: 0,
                            captured_at: chrono::Utc::now().timestamp(),
                            server: server_owned.clone(),
                        };
                    }
                }
            }

            // Debug: dump localStorage on first iteration
            static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DUMPED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                // Dump ALL localStorage keys and values for debugging
                if let Ok(all) = page.evaluate(r#"JSON.stringify(localStorage)"#).await {
                    if let Some(v) = all.value().and_then(|v| v.as_str()) {
                        info!("ALL localStorage: {}", v);
                    }
                }
            }

            // Try to get token from localStorage
            // For CN: capture _pre_id_token (raw Google JWT) - ssssoooodd gets consumed by browser
            let (token_key, token_type) = if server_owned == "cn" {
                ("_pre_id_token", "jwt")
            } else {
                ("ssssoooodd", "uuid")
            };

            let script = format!(r#"localStorage.getItem('{}')"#, token_key);
            let result = page.evaluate(script.as_str()).await;

            if let Ok(value) = result {
                if let Some(token_str) = value.value().and_then(|v| v.as_str()) {
                    let valid = if token_type == "jwt" {
                        token_str.starts_with("eyJ") && token_str.len() > 100
                    } else {
                        token_str.len() == 36 && token_str.contains('-')
                    };

                    if valid {
                        // Also check for lq_uid to ensure account is fully loaded
                        let uid: u64 = if let Ok(uid_val) = page.evaluate(r#"localStorage.getItem('lq_uid')"#).await {
                            uid_val.value()
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0)
                        } else {
                            0
                        };

                        if uid > 0 {
                            info!("Found access token with uid {} in localStorage", uid);
                            return CachedToken {
                                access_token: token_str.to_string(),
                                uid,
                                captured_at: chrono::Utc::now().timestamp(),
                                server: server_owned.clone(),
                            };
                        } else if attempts > 5 {
                            // Fallback: CN server may not set lq_uid, accept token anyway after 10 seconds
                            info!("Found access token (no lq_uid after {} attempts, using anyway)", attempts);
                            return CachedToken {
                                access_token: token_str.to_string(),
                                uid: 0,
                                captured_at: chrono::Utc::now().timestamp(),
                                server: server_owned.clone(),
                            };
                        } else {
                            debug!("Token found but lq_uid not set yet, waiting... (attempt {})", attempts);
                        }
                    }
                }
            }
        }
    })
    .await
    .context("Timeout waiting for login")?;

    // Cleanup
    drop(page);
    drop(browser);
    handler_task.abort();

    // Save to cache
    token.save()?;
    info!("Token captured and cached successfully!");

    Ok(token)
}

/// Try to extract access_token from response payload
/// Looks for UUID-like strings in field 2 (access_token field in oauth2Auth response)
fn try_extract_oauth2_token(data: &[u8], server: &str) -> Option<CachedToken> {
    // The wrapper has: field 1 = name (empty in responses), field 2 = payload
    // We need to get field 2, then look for field 2 inside that (access_token)

    let (_, payload) = decode_wrapper(data).ok()?;

    // Look for field 2 in the inner payload (access_token)
    let access_token = extract_string_field(&payload, 2)?;

    // Must look like a UUID (36 chars with dashes)
    if access_token.len() == 36 && access_token.chars().filter(|c| *c == '-').count() == 4 {
        debug!("Found potential access_token: {}", access_token);
        return Some(CachedToken {
            access_token,
            uid: 0,
            captured_at: chrono::Utc::now().timestamp(),
            server: server.to_string(),
        });
    }

    None
}

/// Simple protobuf wrapper decoder (matches rpc.rs wrapper module)
fn decode_wrapper(buf: &[u8]) -> Result<(String, Vec<u8>)> {
    let mut pos = 0;
    let mut name = String::new();
    let mut data = Vec::new();

    while pos < buf.len() {
        if pos >= buf.len() {
            break;
        }
        let tag = buf[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type != 2 {
            if wire_type == 0 {
                while pos < buf.len() && buf[pos] & 0x80 != 0 {
                    pos += 1;
                }
                pos += 1;
            }
            continue;
        }

        let (len, bytes_read) = decode_varint(&buf[pos..])?;
        pos += bytes_read;
        let end = pos + len as usize;
        if end > buf.len() {
            anyhow::bail!("Buffer overflow");
        }

        match field_num {
            1 => name = String::from_utf8_lossy(&buf[pos..end]).to_string(),
            2 => data = buf[pos..end].to_vec(),
            _ => {}
        }
        pos = end;
    }
    Ok((name, data))
}

fn decode_varint(buf: &[u8]) -> Result<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0;
    let mut pos = 0;
    loop {
        if pos >= buf.len() {
            anyhow::bail!("Unexpected end in varint");
        }
        let byte = buf[pos];
        pos += 1;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok((value, pos))
}

fn extract_string_field(data: &[u8], target_field: u8) -> Option<String> {
    let mut pos = 0;
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type == 2 {
            let (len, bytes_read) = decode_varint(&data[pos..]).ok()?;
            pos += bytes_read;
            let end = pos + len as usize;
            if end > data.len() {
                return None;
            }
            if field_num == target_field {
                return Some(String::from_utf8_lossy(&data[pos..end]).to_string());
            }
            pos = end;
        } else if wire_type == 0 {
            while pos < data.len() && data[pos] & 0x80 != 0 {
                pos += 1;
            }
            pos += 1;
        } else {
            return None;
        }
    }
    None
}
