//! Browser automation for Majsoul phantom resolution via Chrome DevTools Protocol.

use anyhow::{Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// Cached localStorage for session persistence (the real auth data)
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct CachedSession {
    pub local_storage: std::collections::HashMap<String, String>,
    pub cookies: Vec<serde_json::Value>,
    pub server: String,
    pub saved_at: i64,
}

/// Path to cached session file
fn session_cache_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .context("Could not find config directory")?
        .join("tenhou-scraper");
    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("majsoul-session.json"))
}

#[allow(dead_code)]
impl CachedSession {
    /// Load cached session from disk
    pub fn load() -> Result<Option<Self>> {
        let path = session_cache_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let session: CachedSession = serde_json::from_str(&data)?;
        Ok(Some(session))
    }

    /// Save session to disk
    pub fn save(&self) -> Result<()> {
        let path = session_cache_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        info!("Session saved to {:?} ({} localStorage keys, {} cookies)",
              path, self.local_storage.len(), self.cookies.len());
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

/// Profile type for browser launch
#[allow(dead_code)]
pub enum BrowserProfile {
    /// Interactive headed browser for login
    Interactive,
    /// Headless scraper (separate profile)
    Headless,
}

/// Launch Chrome with optimized flags for fast startup
/// Uses PERSISTENT profile so you stay logged in
async fn launch_browser_with_profile(
    profile: BrowserProfile,
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

    // Use separate profile directories for interactive vs headless
    let profile_name = match profile {
        BrowserProfile::Interactive => "majsoul-chrome-profile",
        BrowserProfile::Headless => "majsoul-headless-profile",
    };
    let user_data_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("tenhou-scraper")
        .join(profile_name);
    std::fs::create_dir_all(&user_data_dir).ok();

    let is_headless = matches!(profile, BrowserProfile::Headless);
    info!("Using {} profile: {:?}", if is_headless { "headless" } else { "interactive" }, user_data_dir);

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
        .disable_default_args()
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--exclude-switches=enable-automation")
        .arg("--disable-dev-shm-usage")
        .arg("--no-first-run")
        .arg("--disable-features=Translate,OptimizationHints,MediaRouter")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .window_size(1280, 800);

    // Headed for interactive, headless for scraping
    if is_headless {
        builder = builder.arg("--headless=new");
    } else {
        builder = builder.with_head();
    }

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

/// Backward-compatible launch (interactive headed)
#[allow(dead_code)]
async fn launch_browser(
) -> Result<(
    Browser,
    impl futures::Stream<Item = Result<(), chromiumoxide::error::CdpError>>,
)> {
    launch_browser_with_profile(BrowserProfile::Interactive).await
}

/// Export full session (localStorage + cookies) from browser
#[allow(dead_code)]
pub async fn export_session_from_page(page: &chromiumoxide::Page, server: &str) -> Result<CachedSession> {
    use chromiumoxide::cdp::browser_protocol::network::GetCookiesParams;

    // Get localStorage (this is where the auth tokens live)
    let ls_result = page.evaluate(r#"JSON.stringify(localStorage)"#).await
        .context("Failed to get localStorage")?;

    let local_storage: std::collections::HashMap<String, String> = ls_result
        .value()
        .and_then(|v| v.as_str())
        .map(|s| serde_json::from_str(s).unwrap_or_default())
        .unwrap_or_default();

    info!("Captured localStorage keys: {:?}", local_storage.keys().collect::<Vec<_>>());

    // Get cookies too
    let cookies_response = page.execute(GetCookiesParams::default()).await
        .context("Failed to get cookies")?;

    let cookies: Vec<serde_json::Value> = cookies_response.cookies
        .iter()
        .map(|c| serde_json::json!({
            "name": c.name,
            "value": c.value,
            "domain": c.domain,
            "path": c.path,
            "expires": c.expires,
            "httpOnly": c.http_only,
            "secure": c.secure,
        }))
        .collect();

    let session = CachedSession {
        local_storage,
        cookies,
        server: server.to_string(),
        saved_at: chrono::Utc::now().timestamp(),
    };

    session.save()?;
    Ok(session)
}

/// Import full session (localStorage + cookies) into browser
#[allow(dead_code)]
pub async fn import_session_to_page(page: &chromiumoxide::Page, session: &CachedSession) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::network::{SetCookieParams, CookieSameSite};

    // Import localStorage - this is the critical part for auth
    let ls_json = serde_json::to_string(&session.local_storage)?;
    let script = format!(r#"
        (function() {{
            const data = {};
            for (const [key, value] of Object.entries(data)) {{
                localStorage.setItem(key, value);
            }}
            return Object.keys(data).length;
        }})()
    "#, ls_json);

    let result = page.evaluate(script.as_str()).await?;
    let count = result.value().and_then(|v| v.as_i64()).unwrap_or(0);
    info!("Imported {} localStorage keys", count);

    // Import cookies
    for cookie in &session.cookies {
        let name = cookie["name"].as_str().unwrap_or_default();
        let value = cookie["value"].as_str().unwrap_or_default();
        let domain = cookie["domain"].as_str().unwrap_or_default();
        let path = cookie["path"].as_str().unwrap_or("/");
        let secure = cookie["secure"].as_bool().unwrap_or(false);
        let http_only = cookie["httpOnly"].as_bool().unwrap_or(false);

        let mut params = SetCookieParams::new(name.to_string(), value.to_string());
        params.domain = Some(domain.to_string());
        params.path = Some(path.to_string());
        params.secure = Some(secure);
        params.http_only = Some(http_only);
        params.same_site = Some(CookieSameSite::None);

        if let Err(e) = page.execute(params).await {
            debug!("Failed to set cookie {}: {}", name, e);
        }
    }

    info!("Imported {} cookies", session.cookies.len());
    Ok(())
}

/// Fetch multiple game records using ONE browser session (kept alive)
/// Browser opens, waits for lobby, fetches all UUIDs, then closes
#[allow(dead_code)]
pub async fn fetch_game_records_batch(
    server: &str,
    uuids: &[String],
    delay_ms: u64,
) -> Result<Vec<(String, Result<Vec<u8>>)>> {
    let url = server_url(server);
    info!("Launching browser for batch download ({} records)", uuids.len());

    let (browser, mut handler) = launch_browser_with_profile(BrowserProfile::Interactive).await?;

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

    // Wait for lobby to be ready (user may need to login/dismiss dialogs)
    info!("Waiting for lobby... (login if needed, dismiss any popups)");
    let lobby_ready = timeout(Duration::from_secs(300), async {
        let mut attempt = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempt += 1;

            // Auto-close Yostar Account Binding dialog if present
            let _ = page.evaluate(r#"
                (function() {
                    // Check for Yostar Account Binding dialog and close it
                    const bindingText = document.querySelector('span.ellipsis');
                    if (bindingText && bindingText.textContent.includes('Yostar Account Binding')) {
                        const closeBtn = document.querySelector('div.btn.close');
                        if (closeBtn) {
                            closeBtn.click();
                            return 'closed';
                        }
                    }
                    return 'not-found';
                })()
            "#).await;

            // Use AlphaJong's detection method:
            // - GameMgr.Inst.login_loading_end = lobby fully loaded
            // - view.DesktopMgr.Inst.active = in a game
            let check = page.evaluate(r#"
                (function() {
                    try {
                        const lobbyReady = (typeof GameMgr !== 'undefined' && GameMgr.Inst && GameMgr.Inst.login_loading_end === true);
                        const inGame = (typeof view !== 'undefined' && view.DesktopMgr && view.DesktopMgr.Inst && view.DesktopMgr.Inst.active === true);
                        const hasRooms = (typeof cfg !== 'undefined' && cfg.desktop && cfg.desktop.matchmode);

                        let status = 'loading';
                        if (lobbyReady) status = 'ready';
                        else if (inGame) status = 'in_game';

                        return JSON.stringify({
                            status: status,
                            lobbyReady: lobbyReady,
                            inGame: inGame,
                            hasRooms: !!hasRooms,
                            url: window.location.href.substring(0, 50)
                        });
                    } catch(e) {
                        return JSON.stringify({error: e.toString()});
                    }
                })()
            "#).await;

            if let Ok(val) = check {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    info!("Lobby check #{}: {}", attempt, json_str);
                    // Ready when GameMgr.Inst.login_loading_end is true
                    if json_str.contains("\"lobbyReady\":true") || json_str.contains("\"inGame\":true") {
                        return Ok::<(), anyhow::Error>(());
                    }
                }
            } else {
                info!("Lobby check #{}: evaluate failed", attempt);
            }
        }
    })
    .await;

    if lobby_ready.is_err() {
        drop(page);
        drop(browser);
        handler_task.abort();
        anyhow::bail!("Timeout waiting for lobby");
    }

    info!("Lobby ready! Starting batch download...");

    // If no UUIDs provided, fetch recent games from the server
    let uuids_to_fetch = if uuids.is_empty() {
        info!("No UUIDs provided, fetching recent game list from server...");

        // First, check what methods are available on NetAgent
        let methods_check = page.evaluate(r#"
            (function() {
                if (!app || !app.NetAgent) return JSON.stringify({error: 'no NetAgent'});

                // Try to find Lobby service methods
                const info = {
                    hasNetAgent: true,
                    sendReq2Lobby: typeof app.NetAgent.sendReq2Lobby === 'function'
                };

                // Check if there's a proto definition we can inspect
                if (typeof net !== 'undefined' && net.ProtobufMgr) {
                    info.hasProtobufMgr = true;
                }

                return JSON.stringify(info);
            })()
        "#).await;
        if let Ok(val) = methods_check {
            if let Some(s) = val.value().and_then(|v| v.as_str()) {
                info!("NetAgent check: {}", s);
            }
        }

        // Try fetching YOUR game history instead (this definitely works)
        let list_result = page.evaluate(r#"
            new Promise((resolve, reject) => {
                if (!app || !app.NetAgent || !app.NetAgent.sendReq2Lobby) {
                    reject('NetAgent not available');
                    return;
                }

                // Try fetchGameRecordList with different params
                // Also try fetchGameRecord to see if it at least responds
                app.NetAgent.sendReq2Lobby('Lobby', 'fetchGameRecordList', {
                    start: 0,
                    count: 30,
                    type: 0  // 0 = all types, or try specific room
                }, (err, res) => {
                    if (err) {
                        // Try alternative: fetch account's own game list
                        app.NetAgent.sendReq2Lobby('Lobby', 'fetchGameRecordListByAccountId', {
                            account_id: GameMgr.Inst.account_id,
                            start: 0,
                            count: 30,
                            type: 0
                        }, (err2, res2) => {
                            if (err2) {
                                resolve(JSON.stringify({
                                    error1: err,
                                    error2: err2,
                                    account_id: GameMgr.Inst.account_id
                                }));
                            } else {
                                const records = res2.record_list || res2.recordList || [];
                                const uuids = records.map(r => r.uuid || r.game_uuid);
                                resolve(JSON.stringify({
                                    source: 'fetchGameRecordListByAccountId',
                                    uuids: uuids,
                                    count: records.length
                                }));
                            }
                        });
                        return;
                    }

                    // Debug: log response keys
                    const keys = Object.keys(res || {});
                    const records = res.record_list || res.recordList || res.records || res.list || [];
                    const uuids = records.map(r => r.uuid || r.game_uuid || r.id);

                    resolve(JSON.stringify({
                        source: 'fetchGameRecordList',
                        keys: keys,
                        uuids: uuids,
                        count: records.length,
                        raw: JSON.stringify(res).substring(0, 300)
                    }));
                });
            })
        "#).await;

        match list_result {
            Ok(val) => {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    info!("Game list response: {}", json_str);
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(uuids) = parsed.get("uuids").and_then(|v| v.as_array()) {
                            let fetched: Vec<String> = uuids.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            info!("Parsed {} game UUIDs from server", fetched.len());
                            fetched
                        } else {
                            warn!("No uuids in response");
                            uuids.to_vec()
                        }
                    } else {
                        warn!("Failed to parse response as JSON");
                        uuids.to_vec()
                    }
                } else {
                    warn!("No value in response");
                    uuids.to_vec()
                }
            }
            Err(e) => {
                warn!("Failed to fetch game list: {}", e);
                uuids.to_vec()
            }
        }
    } else {
        uuids.to_vec()
    };

    if uuids_to_fetch.is_empty() {
        info!("No games to download");
        drop(page);
        drop(browser);
        handler_task.abort();
        return Ok(Vec::new());
    }

    // Debug: deep search for fetchGameRecord in all global objects
    let api_check = page.evaluate(r#"
        (function() {
            const found = [];
            // Search all window properties for fetchGameRecord
            function searchObj(obj, path, depth) {
                if (depth > 3 || !obj) return;
                try {
                    for (const key of Object.keys(obj)) {
                        if (key === 'fetchGameRecord' && typeof obj[key] === 'function') {
                            found.push(path + '.' + key);
                        }
                        if (depth < 3 && obj[key] && typeof obj[key] === 'object') {
                            searchObj(obj[key], path + '.' + key, depth + 1);
                        }
                    }
                } catch(e) {}
            }
            // Search common namespaces
            if (typeof GameMgr !== 'undefined' && GameMgr.Inst) {
                searchObj(GameMgr.Inst, 'GameMgr.Inst', 0);
            }
            if (typeof app !== 'undefined') searchObj(app, 'app', 0);
            if (typeof view !== 'undefined') searchObj(view, 'view', 0);
            if (typeof uiscript !== 'undefined') searchObj(uiscript, 'uiscript', 0);
            if (typeof net !== 'undefined') searchObj(net, 'net', 0);

            // Also dump GameMgr.Inst prototype chain
            let instProto = [];
            if (typeof GameMgr !== 'undefined' && GameMgr.Inst) {
                let proto = Object.getPrototypeOf(GameMgr.Inst);
                if (proto) {
                    instProto = Object.getOwnPropertyNames(proto).filter(n => n.includes('fetch') || n.includes('record') || n.includes('Record'));
                }
            }

            return JSON.stringify({found, instProto}, null, 2);
        })()
    "#).await;
    if let Ok(val) = api_check {
        if let Some(s) = val.value().and_then(|v| v.as_str()) {
            info!("API search: {}", s);
        }
    }

    let mut results = Vec::new();

    for (i, uuid) in uuids_to_fetch.iter().enumerate() {
        info!("[{}/{}] Fetching {}...", i + 1, uuids.len(), uuid);

        // Use correct API: app.NetAgent.sendReq2Lobby (discovered via AlphaJong)
        let script = format!(
            r#"
            new Promise((resolve, reject) => {{
                const uuid = "{}";

                if (typeof app === 'undefined' || !app.NetAgent || !app.NetAgent.sendReq2Lobby) {{
                    reject('app.NetAgent.sendReq2Lobby not available');
                    return;
                }}

                app.NetAgent.sendReq2Lobby('Lobby', 'fetchGameRecord', {{ game_uuid: uuid }}, (err, res) => {{
                    if (err) {{
                        reject(JSON.stringify(err));
                        return;
                    }}

                    // Handle response - data is protobuf bytes, data_url is CDN fallback
                    if (res.data && res.data.length > 0) {{
                        // Direct protobuf data
                        if (res.data instanceof Uint8Array) {{
                            resolve(btoa(String.fromCharCode.apply(null, res.data)));
                        }} else {{
                            resolve(JSON.stringify(res));
                        }}
                    }} else if (res.data_url) {{
                        // Fetch from CDN URL (older records)
                        fetch(res.data_url)
                            .then(r => r.arrayBuffer())
                            .then(buf => {{
                                const bytes = new Uint8Array(buf);
                                resolve(btoa(String.fromCharCode.apply(null, bytes)));
                            }})
                            .catch(e => reject('CDN fetch failed: ' + e.toString()));
                    }} else {{
                        reject('No data or data_url in response: ' + JSON.stringify(res));
                    }}
                }});
            }})
            "#,
            uuid
        );

        let fetch_result = timeout(Duration::from_secs(30), page.evaluate(script.as_str())).await;

        let result = match fetch_result {
            Ok(Ok(val)) => {
                if let Some(data) = val.value().and_then(|v| v.as_str()) {
                    if data.starts_with('{') {
                        Ok(data.as_bytes().to_vec())
                    } else {
                        base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .context("Failed to decode base64")
                    }
                } else {
                    Err(anyhow::anyhow!("No data returned"))
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("Fetch error: {}", e)),
            Err(_) => Err(anyhow::anyhow!("Timeout")),
        };

        results.push((uuid.clone(), result));

        if delay_ms > 0 && i < uuids.len() - 1 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    info!("Batch download complete. Closing browser...");
    drop(page);
    drop(browser);
    handler_task.abort();

    Ok(results)
}

/// Resolve phantom UUIDs (short UUIDs without full_uuid) via browser injection
/// Returns Vec<(short_uuid, full_uuid)> for successfully resolved games
pub async fn resolve_phantom_uuids(
    server: &str,
    short_uuids: &[String],
    delay_ms: u64,
) -> Result<Vec<(String, String)>> {
    if short_uuids.is_empty() {
        return Ok(Vec::new());
    }

    let url = server_url(server);
    info!("Launching browser to resolve {} phantom UUIDs", short_uuids.len());

    let (browser, mut handler) = launch_browser_with_profile(BrowserProfile::Interactive).await?;

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

    // Wait for lobby to be ready (user may need to login/dismiss dialogs)
    info!("Waiting for lobby... (login if needed, dismiss any popups)");
    let lobby_ready = timeout(Duration::from_secs(300), async {
        let mut attempt = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            attempt += 1;

            // Auto-close Yostar Account Binding dialog if present
            let _ = page.evaluate(r#"
                (function() {
                    const bindingText = document.querySelector('span.ellipsis');
                    if (bindingText && bindingText.textContent.includes('Yostar Account Binding')) {
                        const closeBtn = document.querySelector('div.btn.close');
                        if (closeBtn) {
                            closeBtn.click();
                            return 'closed';
                        }
                    }
                    return 'not-found';
                })()
            "#).await;

            // Check if lobby is ready using GameMgr.Inst.login_loading_end
            let check = page.evaluate(r#"
                (function() {
                    try {
                        const lobbyReady = (typeof GameMgr !== 'undefined' && GameMgr.Inst && GameMgr.Inst.login_loading_end === true);
                        const inGame = (typeof view !== 'undefined' && view.DesktopMgr && view.DesktopMgr.Inst && view.DesktopMgr.Inst.active === true);

                        return JSON.stringify({
                            lobbyReady: lobbyReady,
                            inGame: inGame
                        });
                    } catch(e) {
                        return JSON.stringify({error: e.toString()});
                    }
                })()
            "#).await;

            if let Ok(val) = check {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    if attempt % 5 == 0 {
                        info!("Lobby check #{}: {}", attempt, json_str);
                    }
                    if json_str.contains("\"lobbyReady\":true") || json_str.contains("\"inGame\":true") {
                        return Ok::<(), anyhow::Error>(());
                    }
                }
            }
        }
    })
    .await;

    if lobby_ready.is_err() {
        drop(page);
        drop(browser);
        handler_task.abort();
        anyhow::bail!("Timeout waiting for lobby");
    }

    info!("Lobby ready! Starting phantom UUID resolution...");

    let mut results = Vec::new();

    for (i, short_uuid) in short_uuids.iter().enumerate() {
        info!("[{}/{}] Resolving {}...", i + 1, short_uuids.len(), short_uuid);

        // Use app.NetAgent.sendReq2Lobby to fetch game record and extract full UUID from head.uuid
        let script = format!(
            r#"
            new Promise((resolve, reject) => {{
                const uuid = "{}";
                const timeoutId = setTimeout(() => reject('Timeout after 30s'), 30000);

                if (typeof app === 'undefined' || !app.NetAgent || !app.NetAgent.sendReq2Lobby) {{
                    clearTimeout(timeoutId);
                    reject('app.NetAgent.sendReq2Lobby not available');
                    return;
                }}

                app.NetAgent.sendReq2Lobby('Lobby', 'fetchGameRecord', {{ game_uuid: uuid }}, (err, res) => {{
                    clearTimeout(timeoutId);
                    if (err) {{
                        reject(JSON.stringify(err));
                        return;
                    }}

                    // Extract full_uuid from res.head.uuid
                    if (res && res.head && res.head.uuid) {{
                        resolve(JSON.stringify({{
                            success: true,
                            full_uuid: res.head.uuid
                        }}));
                    }} else {{
                        reject('No head.uuid in response: ' + JSON.stringify(Object.keys(res || {{}})));
                    }}
                }});
            }})
            "#,
            short_uuid
        );

        let fetch_result = timeout(Duration::from_secs(30), page.evaluate(script.as_str())).await;

        match fetch_result {
            Ok(Ok(val)) => {
                if let Some(json_str) = val.value().and_then(|v| v.as_str()) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                        if let Some(full_uuid) = parsed.get("full_uuid").and_then(|v| v.as_str()) {
                            info!("  Resolved: {} -> {}", short_uuid, full_uuid);
                            results.push((short_uuid.clone(), full_uuid.to_string()));
                        } else {
                            warn!("  Failed to extract full_uuid from response: {}", json_str);
                        }
                    } else {
                        warn!("  Failed to parse response as JSON: {}", json_str);
                    }
                } else {
                    warn!("  No value returned for {}", short_uuid);
                }
            }
            Ok(Err(e)) => {
                warn!("  Fetch error for {}: {}", short_uuid, e);
            }
            Err(_) => {
                warn!("  Timeout for {}", short_uuid);
            }
        }

        if delay_ms > 0 && i < short_uuids.len() - 1 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    info!("Phantom UUID resolution complete. Resolved {}/{}", results.len(), short_uuids.len());
    drop(page);
    drop(browser);
    handler_task.abort();

    Ok(results)
}

/// Fetch a game record using the browser's authenticated session
#[allow(dead_code)]
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
