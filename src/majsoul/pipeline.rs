//! Persistent campaign owner: browser lifecycle, shared authentication, harvest,
//! continuous history resolution, and dashboard run under one Rust supervisor.
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::task::JoinSet;
use tracing::{info, warn};
use super::koromo_harvest::{AuthPool, HarvestConfig, run_harvest_with_pool};

#[derive(Clone, Deserialize, Serialize)]
pub struct Solver { pub port: u16, pub proxy: Option<String> }
#[derive(Clone, Deserialize, Serialize)]
pub struct Campaign {
    pub name: String, pub start: String, pub end: Option<String>, pub mode: i32,
    pub journal: String, pub state: String, pub workers: usize,
}
/// Browser processes are external unless users explicitly request supervision.
#[derive(Clone, Deserialize, Serialize)]
pub struct BrowserLaunch {
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub wrapper: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub data_dir: PathBuf,
    #[serde(default)]
    pub solvers: Vec<Solver>,
    #[serde(default)]
    pub doors: Vec<String>,
    #[serde(default)]
    pub campaigns: Vec<Campaign>,
    #[serde(default)]
    pub inputs: Vec<String>,
    pub fetch_workers: usize, pub pace_ms: u64, pub token_depth: usize,
    #[serde(default)]
    pub dashboard_port: Option<u16>,
    pub scan_secs: u64,
    #[serde(default)]
    pub browser_launch: Option<BrowserLaunch>,
    #[serde(default)]
    pub derive_paipu: bool,
    #[serde(default)]
    pub resolve_histories: bool,
    pub pool_file: Option<PathBuf>,
}

fn today() -> String {
    (chrono::Utc::now() + chrono::Duration::hours(9)).format("%Y-%m-%d").to_string()
}

async fn ready(client: &reqwest::Client, port: u16) -> bool {
    client.get(format!("http://127.0.0.1:{port}/json/version"))
        .send().await.is_ok_and(|r| r.status().is_success())
}

async fn solver_supervisor(config: Config) -> Result<()> {
    use std::process::Stdio;
    let launch = config.browser_launch.as_ref().context("browser supervision is not configured")?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(4)).build()?;
    let mut children: std::collections::HashMap<u16, tokio::process::Child> = std::collections::HashMap::new();
    loop {
        for solver in &config.solvers {
            if ready(&client, solver.port).await { continue; }
            if let Some(child) = children.get_mut(&solver.port) {
                if child.try_wait()?.is_none() { continue; }
            }
            let profile = config.data_dir.join(format!("solver-{}", solver.port));
            let log = std::fs::OpenOptions::new().create(true).append(true)
                .open(config.data_dir.join(format!("solver-{}.log", solver.port)))?;
            let mut command = if let Some(wrapper) = launch.wrapper.first() {
                let mut command = tokio::process::Command::new(wrapper);
                command.args(&launch.wrapper[1..]).arg(&launch.executable);
                command
            } else { tokio::process::Command::new(&launch.executable) };
            command.args(&launch.args).envs(&launch.env)
                .arg(format!("--remote-debugging-port={}", solver.port))
                .arg(format!("--user-data-dir={}", profile.display()));
            if let Some(proxy) = &solver.proxy { command.arg(format!("--proxy-server={proxy}")); }
            command.arg("about:blank").kill_on_drop(true)
                .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
            children.insert(solver.port, command.spawn()?);
            info!(port = solver.port, "pipeline browser launched");
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

fn dashboard_config(config: &Config) -> super::dashboard::DashboardConfig {
    let mut dashboard = super::dashboard::DashboardConfig::new(config.data_dir.clone());
    dashboard.solver_ports = config.solvers.iter().map(|s| s.port).collect();
    dashboard.campaigns = config.campaigns.iter().map(|c| super::dashboard::CampaignDef {
        name: c.name.clone(), start: c.start.clone(), end: c.end.clone(), mode: c.mode,
        state_db: PathBuf::from(&c.state),
    }).collect();
    dashboard.journals = config.campaigns.iter().map(|c| super::dashboard::JournalDef::new(c.name.clone(), PathBuf::from(&c.journal))).collect();
    if config.resolve_histories {
        dashboard.journals.push(super::dashboard::JournalDef { name: "Full games".into(), path: "fulls.jsonl".into(), universe: false, stage: true });
        dashboard.fulls_journal = Some("Full games".into());
    }
    if config.derive_paipu {
        dashboard.journals.push(super::dashboard::JournalDef { name: "Paipu IDs".into(), path: "paipu.jsonl".into(), universe: false, stage: true });
        dashboard.paipu_journal = Some("Paipu IDs".into());
    }
    dashboard
}

pub async fn run(path: PathBuf) -> Result<()> {
    let config: Config = serde_json::from_slice(&tokio::fs::read(&path).await?)
        .context("pipeline config")?;
    anyhow::ensure!(!config.solvers.is_empty() || std::env::var_os("KOROMO_API_KEY").is_some(), "configure solver endpoints or KOROMO_API_KEY");
    anyhow::ensure!(config.fetch_workers > 0 && config.token_depth > 0 && config.scan_secs > 0, "pipeline concurrency/depth/scan must be nonzero");
    anyhow::ensure!(!config.campaigns.is_empty() || config.resolve_histories, "enable a campaign or history resolution");
    anyhow::ensure!(!config.derive_paipu || config.resolve_histories, "paipu derivation requires history resolution");
    tokio::fs::create_dir_all(&config.data_dir).await?;
    // An advisory file lock prevents a second service from creating another writer.
    let ownership = std::fs::OpenOptions::new().create(true).truncate(false).write(true)
        .open(config.data_dir.join("pipeline.lock"))?;
    ownership.try_lock().context("another pipeline owns this directory")?;
    info!(pid = std::process::id(), version = env!("CARGO_PKG_VERSION"),
        solvers = config.solvers.len(), fetch_workers = config.fetch_workers,
        fetch_lanes = config.doors.len() + 1, campaigns = config.campaigns.len(),
        token_depth = config.token_depth, pace_ms = config.pace_ms,
        scan_secs = config.scan_secs, "pipeline started");
    // Slot/group numbers support correlation without printing proxy URLs,
    // which may carry credentials in future configurations.
    let mut exit_groups = std::collections::HashMap::new();
    for (slot, solver) in config.solvers.iter().enumerate() {
        let next = exit_groups.len();
        let exit_group = *exit_groups.entry(solver.proxy.as_deref().unwrap_or("direct")).or_insert(next);
        info!(slot, port = solver.port, exit_group, proxied = solver.proxy.is_some(), "pipeline solver assignment");
    }
    let endpoints = config.solvers.iter().map(|s| format!("http://127.0.0.1:{}", s.port)).collect::<Vec<_>>();
    let mut pool = AuthPool::configured(endpoints.first().cloned().unwrap_or_default(), endpoints, config.pace_ms);
    let exits = config.solvers.iter().map(|s| s.proxy.clone().unwrap_or_else(|| "direct".to_owned())).collect::<Vec<_>>();
    pool.group_mint_exits(&exits);
    let pool = Arc::new(pool);
    if let Some(path) = &config.pool_file { pool.load_file(path).await?; }
    let mut tasks = JoinSet::new();
    if config.browser_launch.is_some() {
        let browser_config = config.clone();
        tasks.spawn(async move { solver_supervisor(browser_config).await });
    }
    for (producer, _) in config.solvers.iter().enumerate() {
        let pool = Arc::clone(&pool);
        let depth = config.token_depth;
        tasks.spawn(async move {
            let mut sequence = 0u64;
            loop {
                if std::env::var_os("KOROMO_API_KEY").is_some() {
                    std::future::pending::<()>().await;
                }
                if pool.len().await >= depth { tokio::time::sleep(Duration::from_millis(100)).await; continue; }
                sequence += 1;
                // A completed JST day avoids waiting for today's first indexed game.
                let mint_day = (chrono::Utc::now() + chrono::Duration::hours(9) - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
                match pool.ensure(&format!("service-{producer}-{sequence}"), &mint_day, 16).await {
                    Ok(entry) => pool.give(entry).await,
                    Err(error) => { warn!(producer, error = %error, "pipeline mint failed"); tokio::time::sleep(Duration::from_secs(5)).await; }
                }
            }
        });
    }
    let mut journal_owners = std::collections::HashMap::new();
    for campaign in config.campaigns.clone() {
        let pool = Arc::clone(&pool);
        let root = config.data_dir.clone();
        let doors = config.doors.clone();
        let journal_owner = Arc::clone(journal_owners.entry(campaign.journal.clone()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))));
        tasks.spawn(async move {
            loop {
                let pass_started = std::time::Instant::now();
                let journal_guard = journal_owner.lock().await;
                info!(campaign = campaign.name, journal_wait_ms = pass_started.elapsed().as_millis(),
                    workers = campaign.workers, mode = campaign.mode, "pipeline campaign pass started");
                let start = NaiveDate::parse_from_str(&campaign.start, "%Y-%m-%d")?;
                let end = NaiveDate::parse_from_str(campaign.end.as_deref().unwrap_or(&today()), "%Y-%m-%d")?;
                let cfg = HarvestConfig { start, end, modes: vec![campaign.mode], journal: root.join(&campaign.journal), db_path: root.join(&campaign.state), workers: campaign.workers, cdp_url: String::new(), pool_file: None, seed_files: Vec::new(), doors: doors.clone() };
                match run_harvest_with_pool(cfg, Arc::clone(&pool)).await {
                    Ok(rows) => info!(campaign = campaign.name, rows, elapsed_ms = pass_started.elapsed().as_millis(), "pipeline campaign pass finished"),
                    Err(error) => warn!(campaign = campaign.name, error = %error, elapsed_ms = pass_started.elapsed().as_millis(), "pipeline campaign failed"),
                }
                drop(journal_guard);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }
    if config.resolve_histories {
        let full_config = super::full_pipeline::Config {
            inputs: config.inputs.iter().map(|p| config.data_dir.join(p)).collect(),
            output: config.data_dir.join("fulls.jsonl"),
            paipu: config.derive_paipu.then(|| config.data_dir.join("paipu.jsonl")),
            workers: config.fetch_workers, doors: config.doors.clone(), scan_secs: config.scan_secs, batch_limit: None,
        };
        let full_pool = Arc::clone(&pool);
        tasks.spawn(async move { super::full_pipeline::run(full_config, full_pool).await });
    }
    if let Some(port) = config.dashboard_port {
        let dashboard = dashboard_config(&config);
        tasks.spawn(async move { super::dashboard::run_with_dashboard_config(port, dashboard).await });
    }
    match tasks.join_next().await.context("pipeline tasks missing")? {
        Ok(Ok(())) => anyhow::bail!("pipeline task exited unexpectedly"),
        Ok(Err(error)) => Err(error.context("pipeline task failed")),
        Err(error) => Err(error.into()),
    }
}
