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
    let uuid_owned = uuid.to_string();

    let result = timeout(Duration::from_secs(120), async {
        // Wait for app to be ready
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let check = page.evaluate(r#"typeof app !== 'undefined' && app.NetMgr && app.NetMgr.Lobby"#).await;
            if let Ok(val) = check {
                if val.value().and_then(|v| v.as_bool()) == Some(true) {
                    break;
                }
            }
        }

        info!("Game loaded, fetching record...");

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

    // Navigate to Majsoul
    let page = browser
        .new_page(url)
        .await
        .context("Failed to open Majsoul page")?;

    info!("Waiting for login... (timeout: 5 minutes)");
    info!("Login in the browser, then wait for token capture.");

    // Poll localStorage for ssssoooodd token
    let server_owned = server.to_string();
    let token = timeout(Duration::from_secs(300), async {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Debug: dump all localStorage keys on first iteration
            static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DUMPED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                if let Ok(keys) = page.evaluate(r#"JSON.stringify(Object.keys(localStorage))"#).await {
                    if let Some(k) = keys.value().and_then(|v| v.as_str()) {
                        debug!("localStorage keys: {}", k);
                    }
                }
                // Also dump values for relevant keys
                if let Ok(all) = page.evaluate(r#"JSON.stringify({
                    ssssoooodd: localStorage.getItem('ssssoooodd'),
                    lq_uid: localStorage.getItem('lq_uid'),
                    access_token: localStorage.getItem('access_token'),
                    account_id: localStorage.getItem('account_id')
                })"#).await {
                    if let Some(v) = all.value().and_then(|v| v.as_str()) {
                        info!("localStorage auth data: {}", v);
                    }
                }
            }

            // Try to get token from localStorage
            let result = page.evaluate(r#"localStorage.getItem('ssssoooodd')"#).await;

            if let Ok(value) = result {
                if let Some(token_str) = value.value().and_then(|v| v.as_str()) {
                    if token_str.len() == 36 && token_str.contains('-') {
                        info!("Found access token in localStorage");
                        return CachedToken {
                            access_token: token_str.to_string(),
                            uid: 0,
                            captured_at: chrono::Utc::now().timestamp(),
                            server: server_owned.clone(),
                        };
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
