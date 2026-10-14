//! Realtime pipeline dashboard.
//!
//! Serves one self-contained page plus JSON stats/history feeds on localhost.
//! All journals, campaigns, solver probes, and disk paths come from
//! [`DashboardConfig`]; this module hardcodes no campaigns, dates, filenames,
//! or host paths. The sampler tails journal sizes (cheap) and only counts
//! appended bytes, so a 2s refresh never re-reads gigabytes. Totals across
//! input journals are an approximation (uuid overlap is not recomputed); the
//! UI labels them as such.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

fn default_true() -> bool {
    true
}

/// One tailed input/output journal, as configured by the caller.
///
/// Relative `path`s resolve against [`DashboardConfig::data_dir`], matching
/// the pipeline convention of joining campaign-relative paths onto its root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalDef {
    /// Display name; also the history-series label and the lookup key for
    /// [`DashboardConfig::paipu_journal`] / `fulls_journal`.
    pub name: String,
    /// Journal file; relative paths resolve against the dashboard root.
    pub path: PathBuf,
    /// Include row counts in the `universe_approx` denominator.
    #[serde(default = "default_true")]
    pub universe: bool,
    /// Show in `stages` and track a history series for throughput graphs.
    #[serde(default = "default_true")]
    pub stage: bool,
}

impl JournalDef {
    /// Journal shown in stages/history and counted in the universe estimate.
    pub fn new(name: impl Into<String>, path: PathBuf) -> Self {
        Self { name: name.into(), path, universe: true, stage: true }
    }
}

/// One harvest campaign with a known day range: done-days from its state DB
/// is ground truth, unlike row sums (journals overlap and formats differ).
///
/// `start`/`end` accept `YYYYMMDD` or `YYYY-MM-DD`. A missing or empty `end`
/// means the campaign runs through today (JST).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CampaignDef {
    pub name: String,
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    pub mode: i32,
    /// Campaign state DB (`done` table); relative paths resolve against
    /// [`DashboardConfig::data_dir`].
    pub state_db: PathBuf,
}

/// Dashboard configuration: arbitrary user-configured campaigns/journals
/// drive the page. No field defaults to a specific user's data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Disk root that relative journal/state/history/progress paths join onto.
    pub data_dir: PathBuf,
    /// Tailed journals, in display/history order.
    #[serde(default)]
    pub journals: Vec<JournalDef>,
    /// Harvest campaigns with known day ranges.
    #[serde(default)]
    pub campaigns: Vec<CampaignDef>,
    /// Solver fleet to probe (`/json/version`); empty disables probing.
    #[serde(default)]
    pub solver_ports: Vec<u16>,
    /// Journal name feeding the headline paipu count/rate.
    #[serde(default)]
    pub paipu_journal: Option<String>,
    /// Journal name feeding the headline full-games count/rate.
    #[serde(default)]
    pub fulls_journal: Option<String>,
    /// Disk snapshot restoring graphs across restarts (`None` = memory only).
    /// Relative paths resolve against `data_dir`.
    #[serde(default)]
    pub history_path: Option<PathBuf>,
    /// `pipeline-progress.json` overlay for deduplicated estimates
    /// (`None` = row-sum progress only). Relative paths resolve against
    /// `data_dir`.
    #[serde(default)]
    pub progress_path: Option<PathBuf>,
}

impl DashboardConfig {
    /// Empty generic config rooted at `data_dir`: no journals, campaigns, or
    /// solver probes. History stays memory-only unless `history_path` is set;
    /// the progress overlay defaults to `pipeline-progress.json` under the root.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            journals: Vec::new(),
            campaigns: Vec::new(),
            solver_ports: Vec::new(),
            paipu_journal: None,
            fulls_journal: None,
            history_path: None,
            progress_path: Some(PathBuf::from("pipeline-progress.json")),
        }
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.data_dir.join(path)
        }
    }
}

const SAMPLE_EVERY: Duration = Duration::from_secs(2);
const RATE_WINDOW: Duration = Duration::from_secs(30);
/// Per-cycle read cap so first convergence never blocks the loop on gigabytes.
const MAX_TAIL_BYTES: u64 = 64 << 20;

/// One tailed journal: harvest stage, fulls, or paipu output.
struct Journal {
    name: String,
    path: PathBuf,
    universe: bool,
    stage: bool,
    count: u64,
    scanned_to: u64,
    converged: bool,
    samples: VecDeque<(Instant, u64)>,
    live_offset: Option<u64>,
    live_count: u64,
}

impl Journal {
    fn new(def: &JournalDef, path: PathBuf) -> Self {
        Self {
            name: def.name.clone(),
            path,
            universe: def.universe,
            stage: def.stage,
            count: 0,
            scanned_to: 0,
            converged: false,
            samples: VecDeque::new(),
            live_offset: None,
            live_count: 0,
        }
    }

    /// Count newly appended lines; a shrunk file means rotation, recount fully.
    // Single-pass newline scan over small tail buffers; a bytecount dep is heavier than the loop.
    #[allow(clippy::naive_bytecount)]
    async fn sample(&mut self) -> Result<()> {
        let size = tokio::fs::metadata(&self.path).await.map_or(0, |m| m.len());
        // Observe only bytes appended since startup for rates. Recounting old
        // rows must neither fabricate throughput nor hide real live output.
        if let Some(offset) = self.live_offset.filter(|offset| *offset <= size) {
            if size > offset {
                let mut file = tokio::fs::File::open(&self.path).await?;
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut buffer = [0u8; 4096];
                let mut remaining = size - offset;
                while remaining > 0 {
                    let length = usize::try_from(remaining.min(4096))?;
                    file.read_exact(&mut buffer[..length]).await?;
                    self.live_count += u64::try_from(buffer[..length].iter().filter(|byte| **byte == b'\n').count())?;
                    remaining -= u64::try_from(length)?;
                }
            }
        }
        self.live_offset = Some(size);
        if size < self.scanned_to {
            self.count = 0;
            self.scanned_to = 0;
            self.converged = false;
        }
        let mut end = size;
        if end - self.scanned_to > MAX_TAIL_BYTES {
            end = self.scanned_to + MAX_TAIL_BYTES;
        }
        if end > self.scanned_to {
            let mut f = tokio::fs::File::open(&self.path).await?;
            f.seek(std::io::SeekFrom::Start(self.scanned_to)).await?;
            let mut buf = vec![0u8; usize::try_from(end - self.scanned_to)?];
            f.read_exact(&mut buf).await?;
            self.count += u64::try_from(buf.iter().filter(|b| **b == b'\n').count())?;
            self.scanned_to = end;
        }
        let now = Instant::now();
        self.samples.push_back((now, self.live_count));
        while self.samples.front().is_some_and(|s| now - s.0 > RATE_WINDOW) {
            self.samples.pop_front();
        }
        Ok(())
    }

    /// Live appended rows/sec; zero until two timed samples exist.
    fn rate(&self) -> f64 {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        let dt = (last.0 - first.0).as_secs_f64();
        if dt < 1.0 {
            return 0.0;
        }
        let grown = last.1.saturating_sub(first.1);
        f64::from(u32::try_from(grown).unwrap_or(u32::MAX)) / dt
    }
}

#[derive(Serialize, Clone)]
struct Stage {
    name: String,
    count: u64,
    rate: f64,
}

#[derive(Serialize, Clone)]
struct Stats {
    uptime_secs: u64,
    paipu_count: u64,
    paipu_rate: f64,
    fulls_count: u64,
    fulls_rate: f64,
    stages: Vec<Stage>,
    universe_approx: u64,
    progress_pct: f64,
    eta_secs: Option<u64>,
    solvers_alive: usize,
    solvers_headless: usize,
    solvers_total: usize,
    campaigns: Vec<Campaign>,
    camp_done: u64,
    camp_total: u64,
    camp_pct: f64,
    camp_eta_secs: Option<u64>,
    remaining_estimate: Option<u64>,
    estimated_total: Option<u64>,
    rate_ready: bool,
}

/// One history point per sampler tick (2s); 1800 points cover an hour.
/// Series order follows the configured stage journals; values are parallel
/// to the `/api/history` series names.
const HISTORY_MAX: usize = 1800;

#[derive(Serialize, serde::Deserialize, Clone)]
struct HistPoint {
    t: u64,
    v: Vec<f32>,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Points younger than an hour survive dashboard restarts; older ones age out.
/// `None` path means memory only. Points from a previous journal layout
/// (different series length) are dropped.
fn load_history(path: Option<&Path>, series_len: usize) -> VecDeque<HistPoint> {
    let text = path.map_or_else(String::new, |p| std::fs::read_to_string(p).unwrap_or_default());
    let mut points: VecDeque<HistPoint> = serde_json::from_str(&text).unwrap_or_default();
    let cutoff = now_unix().saturating_sub(3600);
    points.retain(|p| p.t >= cutoff && p.v.len() == series_len);
    while points.len() > HISTORY_MAX {
        points.pop_front();
    }
    points
}

#[derive(Serialize, Clone)]
struct Campaign {
    name: String,
    done: u64,
    total: u64,
}

/// Today in the API's JST calendar; open-ended campaigns run through it.
fn today_jst() -> chrono::NaiveDate {
    (chrono::Utc::now() + chrono::Duration::hours(9)).date_naive()
}

fn parse_day(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y%m%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .ok()
}

fn campaign_days(start: &str, end: Option<&str>) -> u64 {
    let Some(a) = parse_day(start) else {
        return 0;
    };
    let Some(b) = end.filter(|s| !s.is_empty()).map_or_else(|| Some(today_jst()), parse_day) else {
        return 0;
    };
    u64::try_from(b.signed_duration_since(a).num_days() + 1).unwrap_or(0)
}

fn campaign_done(db: &Path, mode: i32) -> u64 {
    rusqlite::Connection::open(db)
        .and_then(|c| {
            c.query_row("SELECT COUNT(*) FROM done WHERE mode=?", [mode], |r| r.get::<_, i64>(0))
        })
        .map_or(0, |n| u64::try_from(n).unwrap_or(0))
}

/// Resolved campaign: precomputed day-range total plus its state DB path.
struct ResolvedCampaign {
    name: String,
    total: u64,
    db: PathBuf,
    mode: i32,
}

struct State {
    started: Instant,
    journals: Vec<Journal>,
    /// Stage journal names in history-series order.
    series: Vec<String>,
    campaigns: Vec<ResolvedCampaign>,
    paipu_journal: Option<String>,
    fulls_journal: Option<String>,
    solvers_alive: usize,
    solvers_headless: usize,
    solver_ports: Vec<u16>,
    solver_checked_at: Instant,
    history: VecDeque<HistPoint>,
    history_path: Option<PathBuf>,
    tick: u64,
    camp_samples: VecDeque<(Instant, u64)>,
    progress_path: Option<PathBuf>,
}
impl State {
    fn new(config: &DashboardConfig) -> Self {
        let journals: Vec<Journal> = config
            .journals
            .iter()
            .map(|def| Journal::new(def, config.resolve(&def.path)))
            .collect();
        let series: Vec<String> = config
            .journals
            .iter()
            .filter(|def| def.stage)
            .map(|def| def.name.clone())
            .collect();
        let campaigns: Vec<ResolvedCampaign> = config
            .campaigns
            .iter()
            .map(|def| ResolvedCampaign {
                name: def.name.clone(),
                total: campaign_days(&def.start, def.end.as_deref()),
                db: config.resolve(&def.state_db),
                mode: def.mode,
            })
            .collect();
        // Bind owned snapshot paths before loading history through them.
        let history_path = config.history_path.as_ref().map(|p| config.resolve(p));
        let progress_path = config.progress_path.as_ref().map(|p| config.resolve(p));
        let history = load_history(history_path.as_deref(), series.len());
        Self {
            started: Instant::now(),
            journals,
            series,
            campaigns,
            paipu_journal: config.paipu_journal.clone(),
            fulls_journal: config.fulls_journal.clone(),
            solvers_alive: 0,
            solvers_headless: 0,
            solver_ports: config.solver_ports.clone(),
            solver_checked_at: Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now),
            history,
            history_path,
            tick: 0,
            camp_samples: VecDeque::new(),
            progress_path,
        }
    }

    fn get(&self, name: &str) -> (u64, f64) {
        self.journals.iter().find(|j| j.name == name).map_or((0, 0.0), |j| (j.count, j.rate()))
    }

    /// Total banked campaign days; the sampler retains these for the ETA rate.
    fn campaign_done_total(&self) -> u64 {
        self.campaigns
            .iter()
            .map(|c| campaign_done(&c.db, c.mode).min(c.total))
            .sum()
    }

    // ETA seconds: truncation toward zero is intended; the value is guarded non-negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn snapshot(&self) -> Stats {
        let (fulls_count, fulls_rate) =
            self.fulls_journal.as_deref().map_or((0, 0.0), |name| self.get(name));
        let (paipu_count, paipu_rate) =
            self.paipu_journal.as_deref().map_or((0, 0.0), |name| self.get(name));
        let mut universe = 0u64;
        let mut stages = Vec::new();
        for j in &self.journals {
            if j.universe {
                universe += j.count;
            }
            if j.stage {
                stages.push(Stage { name: j.name.clone(), count: j.count, rate: j.rate() });
            }
        }
        let progress_pct = if universe > 0 {
            f64::from(u32::try_from(fulls_count.min(universe)).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(universe).unwrap_or(u32::MAX))
                * 100.0
        } else {
            0.0
        };
        // Campaign truth: done-days per state DB over known ranges. The row-sum
        // universe above only counts seen rows; days are the real finish line.
        let mut campaigns = Vec::new();
        let mut camp_done = 0u64;
        let mut camp_total = 0u64;
        for c in &self.campaigns {
            let done = campaign_done(&c.db, c.mode).min(c.total);
            camp_done += done;
            camp_total += c.total;
            campaigns.push(Campaign { name: c.name.clone(), done, total: c.total });
        }
        let camp_pct = if camp_total > 0 {
            f64::from(u32::try_from(camp_done).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(camp_total).unwrap_or(u32::MAX))
                * 100.0
        } else {
            0.0
        };
        let camp_eta_secs = self.camp_eta(camp_done, camp_total);
        // The pipeline deduplicates campaign witnesses against full UUIDs.
        // Extrapolate unharvested days from observed games/day; this is an
        // estimate, never a row-sum pretending to be exact campaign coverage.
        let live = self
            .progress_path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let fresh = live.as_ref().filter(|value| value["updated_at"].as_u64()
            .is_some_and(|updated| now_unix().saturating_sub(updated) < 90));
        let target = fresh.and_then(|value| value["target_games"].as_u64());
        let missing = fresh.and_then(|value| value["remaining_known"].as_u64());
        let estimated_total = target.filter(|_| camp_done > 0)
            .map(|games| games.saturating_mul(camp_total).checked_div(camp_done).unwrap_or(games).max(games));
        let remaining_estimate = estimated_total.zip(target).zip(missing)
            .map(|((total, seen), left)| total.saturating_sub(seen).saturating_add(left));
        let progress_pct = estimated_total.zip(remaining_estimate)
            .filter(|(total, _)| *total > 0)
            .map_or(progress_pct, |(total, left)| 100.0 * f64::from(u32::try_from(total.saturating_sub(left)).unwrap_or(u32::MAX)) / f64::from(u32::try_from(total).unwrap_or(u32::MAX)));
        let rate_ready = fresh.is_some_and(|value| value["rate_ready"].as_bool() == Some(true));
        let paipu_rate = fresh.and_then(|value| value["paipu_rate"].as_f64()).unwrap_or(paipu_rate);
        let eta_secs = remaining_estimate.filter(|_| rate_ready && paipu_rate > 0.0)
            .map(|left| (f64::from(u32::try_from(left).unwrap_or(u32::MAX)) / paipu_rate) as u64)
            .map(|output_eta| output_eta.max(camp_eta_secs.unwrap_or(0)));
        Stats {
            uptime_secs: self.started.elapsed().as_secs(),
            paipu_count,
            paipu_rate,
            fulls_count,
            fulls_rate,
            stages,
            universe_approx: universe,
            progress_pct,
            eta_secs,
            solvers_alive: self.solvers_alive,
            solvers_headless: self.solvers_headless,
            solvers_total: self.solver_ports.len(),
            campaigns,
            camp_done,
            camp_total,
            camp_pct,
            camp_eta_secs,
            remaining_estimate,
            estimated_total,
            rate_ready,
        }
    }

    /// Days-per-second from retained samples; ETA over remaining campaign days.
    // ETA seconds: truncation toward zero is intended; the value is guarded non-negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn camp_eta(&self, done: u64, total: u64) -> Option<u64> {
        let (Some(first), Some(last)) = (self.camp_samples.front(), self.camp_samples.back())
        else {
            return None;
        };
        let dt = (last.0 - first.0).as_secs_f64();
        if dt < 60.0 || total <= done {
            return None;
        }
        let rate = f64::from(u32::try_from(last.1.saturating_sub(first.1)).unwrap_or(u32::MAX)) / dt;
        if rate <= 0.0 {
            return None;
        }
        Some((f64::from(u32::try_from(total - done).unwrap_or(u32::MAX)) / rate) as u64)
    }
}

/// Minimal blocking HTTP GET over localhost; avoids client-pool questions.
async fn fetch_version(port: u16) -> Option<(bool, bool)> {
    let mut stream = tokio::time::timeout(
        Duration::from_millis(1200),
        tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")),
    )
    .await
    .ok()?
    .ok()?;
    // DevTools answers 500 unless Host names this exact loopback endpoint, and
    // it ignores `Connection: close`, so read exactly Content-Length bytes.
    let req = format!("GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 8192 {
            return None;
        }
        let n = tokio::time::timeout(Duration::from_millis(1200), stream.read(&mut byte)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        head.extend_from_slice(&byte);
    }
    let head_text = String::from_utf8_lossy(&head);
    if !head_text.starts_with("HTTP/1.1 200") && !head_text.starts_with("HTTP/1.0 200") {
        return None;
    }
    let len: usize = head_text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
    })?;
    let mut body = vec![0u8; len];
    tokio::time::timeout(Duration::from_millis(1500), stream.read_exact(&mut body)).await.ok()?.ok()?;
    let body = String::from_utf8_lossy(&body);
    Some((true, body.contains("HeadlessChrome")))
}

// History stores f32 (display resolution, halves snapshot bytes); precision loss is intended.
#[allow(clippy::cast_possible_truncation)]
async fn sampler(state: Arc<Mutex<State>>) {
    loop {
        {
            let mut s = state.lock().await;
            for j in &mut s.journals {
                if let Err(e) = j.sample().await {
                    let path = j.path.display();
                    tracing::warn!("dashboard: {path}: {e:#}");
                }
            }
            if s.solver_checked_at.elapsed() > Duration::from_secs(5) {
                s.solver_checked_at = Instant::now();
                let ports = s.solver_ports.clone();
                let mut alive = 0usize;
                let mut headless = 0usize;
                for port in ports {
                    if let Some((_, h)) = fetch_version(port).await {
                        alive += 1;
                        headless += usize::from(h);
                    }
                }
                s.solvers_alive = alive;
                s.solvers_headless = headless;
            }
            s.tick += 1;
            // One point per tick keeps a full hour under 1800 entries; the disk
            // snapshot (every ~minute) restores graphs across restarts too.
            let series = s.series.clone();
            let mut v = vec![0f32; series.len()];
            for (i, name) in series.iter().enumerate() {
                v[i] = s.get(name).1 as f32;
            }
            // The progress overlay measures deduplicated output better than a
            // journal recount; headline the paipu series with it when known.
            if let Some(name) = s.paipu_journal.clone() {
                if let Some(i) = series.iter().position(|n| n == &name) {
                    v[i] = s.snapshot().paipu_rate as f32;
                }
            }
            s.history.push_back(HistPoint { t: now_unix(), v });
            while s.history.len() > HISTORY_MAX {
                s.history.pop_front();
            }
            // Campaign-day samples back the finish-line ETA (cheap integers).
            let camp_done = s.campaign_done_total();
            s.camp_samples.push_back((Instant::now(), camp_done));
            while s.camp_samples.front().is_some_and(|c| c.0.elapsed() > Duration::from_secs(3600)) {
                s.camp_samples.pop_front();
            }
            if s.tick.is_multiple_of(30) {
                if let Some(path) = s.history_path.clone() {
                    if let Ok(json) = serde_json::to_vec(&s.history) {
                        if let Err(e) = tokio::fs::write(&path, &json).await {
                            tracing::warn!("dashboard history save: {e:#}");
                        }
                    }
                }
            }
        }
        tokio::time::sleep(SAMPLE_EVERY).await;
    }
}

async fn handle(mut stream: tokio::net::TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let mut head = vec![0u8; 0];
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16_384 {
            break;
        }
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&byte);
    }
    let line = String::from_utf8_lossy(&head);
    let path = line.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/");
    let (status, mime, body) = match path {
        "/" => ("200 OK", "text/html; charset=utf-8", include_str!("dashboard.html").as_bytes().to_vec()),
        "/api/stats" => {
            let json = serde_json::to_vec(&state.lock().await.snapshot())?;
            ("200 OK", "application/json", json)
        }
        "/api/history" => {
            // Downsample to ~360 points so a fresh page load stays light.
            let guard = state.lock().await;
            let step = guard.history.len().div_ceil(360);
            let points: Vec<&HistPoint> = guard.history.iter().step_by(step.max(1)).collect();
            let json = serde_json::json!({"series": guard.series, "points": points});
            ("200 OK", "application/json", serde_json::to_vec(&json)?)
        }
        _ => ("404 Not Found", "text/plain", b"nope".to_vec()),
    };
    let len = body.len();
    let header = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {mime}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&body).await?;
    Ok(())
}

/// Serve the dashboard forever on `127.0.0.1:port` from an explicit
/// [`DashboardConfig`]. Relative journal/state/history/progress paths resolve
/// against `config.data_dir`; an empty solver list disables fleet probing.
pub async fn run_with_dashboard_config(port: u16, config: DashboardConfig) -> Result<()> {
    let state = Arc::new(Mutex::new(State::new(&config)));
    let mut tasks = tokio::task::JoinSet::new();
    let sampler_state = Arc::clone(&state);
    tasks.spawn(async move { sampler(sampler_state).await });
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .context("bind dashboard")?;
    tracing::info!("dashboard: http://127.0.0.1:{port}/");
    loop {
        let (stream, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            finished = tasks.join_next() => {
                if let Some(Err(error)) = finished { return Err(error.into()); }
                continue;
            }
        };
        let state = Arc::clone(&state);
        tasks.spawn(async move {
            // Probes that connect and vanish (readiness checks) are not news.
            match handle(stream, state).await {
                Err(e) if e.to_string().contains("Broken pipe") => {}
                Err(e) => tracing::warn!("dashboard conn: {e:#}"),
                Ok(()) => {}
            }
        });
    }
}
