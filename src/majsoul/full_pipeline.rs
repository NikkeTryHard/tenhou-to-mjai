//! Long-lived full-uuid pipeline (no shell/epochs).
//!
//! One scheduler + N workers + one writer alive for the whole `run()`.
//! Scheduler unions input tails, workers acquire then run the exhaustive
//! history fetch, writer dedups/appends and acks only after flush.
//! Bounded channels, worker receiver behind `Mutex`, `JoinSet` propagation
//! where dropping `run()` aborts children.
//!
//! Compatibility (must not drift):
//! - completed sidecar: `output.with_extension("completed.jsonl")` rows
//!   `{"_completed_player": <i64>, "modeId": <i32>}`.
//! - partial sidecar: `output.with_extension("partial.jsonl")` rows
//!   `{"accountId": <i64>, "mode": <i32>, "cursor": <i64>, "fails": <u32>}`
//!   with `cursor == -1` meaning "restart fresh" (legacy + empty first page).
//! - paipu derive rows: `{"uuid", "paipu": "<uuid>_a<accountId>", "day",
//!   "mode", "startTime"}` with `day` from `day`/`_day` else JST
//!   (`startTime` + 9h), first witness wins. Matches `derive-paipu`/`EmitIds`.
//! - malformed lines and torn tails are skipped, never fatal. Incremental input
//!   scans stream tails locally so a concurrent append mid-read is held for
//!   the next scan.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Mutex, mpsc};
use tracing::{Instrument as _, info, warn};
use super::koromo_harvest::{AuthPool, FetchCtx, fetch_player_records};
use super::types::GameRecord;

const JOB_CAP: usize = 32;
const EVENT_CAP: usize = 4096;
const ACK_CAP: usize = 1024;
/// Latest durable cursor per walker: replay keeps the last row per key.
type PartialCursorMap = HashMap<(i64, i32), (Option<i64>, u32)>;
/// Full UUID dedup remains exact. Compressed IDs affect priority only: a rare
/// collision cannot suppress a history walk or discard a full/paipu record.
type BankedUuids = Arc<std::sync::RwLock<Coverage>>;

#[derive(Default)]
struct Coverage {
    fulls: HashSet<String>,
    covered: HashSet<u64>,
    witnesses: HashMap<u64, Vec<(i64, i32)>>,
    scores: HashMap<(i64, i32), u32>,
    target_games: u64,
}

impl Coverage {
    fn len(&self) -> usize { self.fulls.len() }

    fn bank(&mut self, game: &GameRecord) -> bool {
        if let Some(id) = compressed_game_id(&game.uuid, game.start_time) {
            self.covered.insert(id);
            if let Some(players) = self.witnesses.remove(&id) {
                for key in players {
                    if let Some(score) = self.scores.get_mut(&key) {
                        *score = score.saturating_sub(1);
                    }
                }
            }
        }
        self.fulls.insert(game.uuid.clone())
    }

    fn witness(&mut self, id: u64, players: Vec<(i64, i32)>) {
        self.target_games += 1;
        if !self.covered.contains(&id) {
            for key in &players { *self.scores.entry(*key).or_default() += 1; }
            self.witnesses.insert(id, players);
        }
    }
}

/// Koromo couchStorage.js: BE32(startTime) || first32bits(UUID), base62.
/// Return its integer representation directly; no temporary encoded string.
fn compressed_game_id(uuid: &str, start: i64) -> Option<u64> {
    if uuid.len() != 36 && uuid.len() != 43 { return None; }
    let hex = if uuid.as_bytes().get(6) == Some(&b'-') { uuid.get(7..15)? } else { uuid.get(..8)? };
    Some((u64::from(u32::try_from(start).ok()?) << 32) | u64::from(u32::from_str_radix(hex, 16).ok()?))
}

fn short_game_id(uuid: &str) -> Option<u64> {
    uuid.bytes().try_fold(0u64, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'A'..=b'Z' => byte - b'A' + 10,
            b'a'..=b'z' => byte - b'a' + 36,
            _ => return None,
        };
        value.checked_mul(62)?.checked_add(u64::from(digit))
    })
}
// ---------------------------------------------------------------------------
// Public config/entrypoint.
// ---------------------------------------------------------------------------
/// Fulls pipeline config.
#[derive(Debug, Clone)]
pub struct Config {
    /// Input shorts journals (repeatable; unioned, deduped by uuid).
    pub inputs: Vec<PathBuf>,
    /// Output full-rows journal (`GameRecord` JSON per line).
    pub output: PathBuf,
    /// Inline paipu derive output (`None` = fulls only).
    pub paipu: Option<PathBuf>,
    /// Concurrent player fetches (`0` = steady 40, matching legacy `auto_workers`).
    pub workers: usize,
    /// Extra door proxies for fetches (repeatable). Home lane always present.
    pub doors: Vec<String>,
    /// Rescan inputs every N secs (idle cadence; productive dispatch is immediate).
    pub scan_secs: u64,
    /// Max new witnesses enqueued per scan (`None` = all outstanding).
    /// Leftovers stay in `known` and enqueue on later scans, so every witness
    /// eventually runs unless completed.
    pub batch_limit: Option<usize>,
}

/// Continuous fulls pipeline. Returns only on error; caller cancellation aborts
/// all children (`JoinSet` drop => abort, no orphan loops). Queues, workers,
/// writer and the shared pool stay alive across scans (no per-epoch rebuild).
#[allow(clippy::too_many_lines)]
pub async fn run(config: Config, pool: Arc<AuthPool>) -> anyhow::Result<()> {
    let workers_n = if config.workers == 0 { 40 } else { config.workers };
    let scan_secs = config.scan_secs.max(1);
    // Writer day stamp identity for token acquire (same shape as legacy today).
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();

    // Fetch lanes: home direct + one per door proxy. Failures warn and skip;
    // workers rotate `clients[wid % len]` so a dead door never parks the fleet.
    let mut clients = vec![crate::util::http_client()?];
    for d in &config.doors {
        match reqwest::Proxy::all(d) {
            Ok(px) => {
                match reqwest::Client::builder()
                    .proxy(px)
                    .timeout(Duration::from_secs(30))
                    .build()
                {
                    Ok(c) => clients.push(c),
                    Err(e) => warn!("fulls: door {d} client failed: {e:#}"),
                }
            }
            Err(e) => warn!("fulls: proxy {d} invalid: {e:#}"),
        }
    }
    let lanes = clients.len();
    info!("fulls: {lanes} fetch lanes, {workers_n} workers");
    let clients = Arc::new(clients);

    // Streaming bootstrap (bounded BufRead, never `read_to_string` on journals).
    let banked = load_full_uuids(&config.output)?;
    let paipu_done = config.paipu.as_ref().map_or(Ok(HashSet::new()), |p| load_paipu_uuids(p))?;
    let completed_path = completed_path_for(&config.output);
    let partial_path = partial_path_for(&config.output);
    let completed = load_completed(&completed_path)?;
    let partials_raw = load_partials(&partial_path)?;
    let banked_n = banked.len();
    let banked: BankedUuids = std::sync::Arc::new(std::sync::RwLock::new(banked));
    info!(banked = banked_n, paipu = paipu_done.len(), completed = completed.len(), partials = partials_raw.len(), "fulls bootstrap");

    // A short dispatch horizon keeps coverage priority fresh; bounded events
    // retain backpressure without discarding games or completion markers.
    let (job_tx, job_rx) = mpsc::channel::<Job>(JOB_CAP);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (event_tx, event_rx) = mpsc::channel::<WriterEvent>(EVENT_CAP);
    let (ack_tx, ack_rx) = mpsc::channel::<Ack>(ACK_CAP);
    let stats = Arc::new(Stats::default());

    // Per-fetch-lane aggregates (home + one per door). Sized here so every
    // `wid % clients.len()` has exactly one slot; workers and the scheduler
    // share it, cardinality stays at the lane count (~10), never per game.
    let lane_stats: Arc<Vec<LaneStats>> =
        Arc::new((0..clients.len()).map(|_| LaneStats::default()).collect());

    let mut set = tokio::task::JoinSet::new();

    // Writer (single).
    {
        let stats = Arc::clone(&stats);
        let banked = Arc::clone(&banked);
        set.spawn(writer_task(
            event_rx,
            ack_tx,
            config.output.clone(),
            config.paipu.clone(),
            banked,
            paipu_done,
            stats,
        ));
    }

    // Scheduler (single owner of queue state).
    {
        let stats = Arc::clone(&stats);
        let pool = Arc::clone(&pool);
        let banked = Arc::clone(&banked);
        let lane_stats = Arc::clone(&lane_stats);
        set.spawn(scheduler_task(
            config.inputs.clone(),
            job_tx,
            ack_rx,
            completed,
            partials_raw,
            config.batch_limit,
            scan_secs,
            pool,
            banked,
            stats,
            lane_stats,
        ));
    }

    // Workers.
    for wid in 0..workers_n {
        let stats = Arc::clone(&stats);
        let pool = Arc::clone(&pool);
        let clients = Arc::clone(&clients);
        let lane_stats = Arc::clone(&lane_stats);
        set.spawn(worker_task(
            wid,
            Arc::clone(&job_rx),
            event_tx.clone(),
            pool,
            clients,
            day.clone(),
            stats,
            lane_stats,
        ));
    }
    // `run` holds no sender: when the scheduler exits, jobs close, workers
    // drain, events close, the writer drains. Continuous service never hits
    // this cascade except via error/cancellation.
    drop(event_tx);

    // Reliable propagation: the first exiting/failing task aborts the rest.
    // Continuous tasks never return `Ok` while healthy; any `Ok` return is an
    // unexpected early exit and surfaces as an error (no silent idle loops).
    let mut first_error: Option<anyhow::Error> = None;
    if let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {
                warn!("fulls: task exited early, aborting pipeline");
                first_error = Some(anyhow::anyhow!("fulls task exited early"));
            }
            Ok(Err(e)) => {
                warn!("fulls: task failed: {e:#}, aborting pipeline");
                first_error = Some(e);
            }
            Err(join_err) if join_err.is_cancelled() => {}
            Err(join_err) => {
                warn!("fulls: task join failed: {join_err}, aborting pipeline");
                first_error = Some(anyhow::anyhow!("fulls join: {join_err}"));
            }
        }
        set.abort_all();
    }
    // Drain aborted children without orphan loops.
    while set.join_next().await.is_some() {}
    if let Some(e) = first_error {
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed events / acks / jobs.
// ---------------------------------------------------------------------------

/// One unit of player-history work. `resume_at` preserves the paginator cursor
/// across partial retries; `fails` counts consecutive incompletes for bounded
/// cooldown (never a poison-drop threshold).
#[derive(Debug, Clone, Copy)]
struct Job {
    account_id: i64,
    mode: i32,
    resume_at: Option<i64>,
    fails: u32,
}

/// Worker -> writer events. Buffered game rows precede their terminal
/// Complete/Partial from the same worker (per-sender FIFO); the writer flushes
/// preceding games+paipu before committing the terminal record, so the ack
/// below always means durable.
#[derive(Debug)]
enum WriterEvent {
    Game(GameRecord),
    Complete { account_id: i64, mode: i32 },
    Partial {
        account_id: i64,
        mode: i32,
        cursor: Option<i64>,
        fails: u32,
    },
}

/// Writer -> scheduler acks, sent only after the terminal record is flushed to
/// its sidecar. Scheduler mutates `completed`/`partials`/`in_flight` on ack,
/// never on dispatch (acks skip only actually finished walkers).
#[derive(Debug, Clone, Copy)]
enum Ack {
    Completed { account_id: i64, mode: i32 },
    Partial {
        account_id: i64,
        mode: i32,
        cursor: Option<i64>,
        fails: u32,
    },
}

// ---------------------------------------------------------------------------
// Shared counters (atomics; scheduler logs every 30s, secrets never logged).
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Stats {
    // Cumulative outcome counters (monotonic per process; the scheduler log
    // derives per-interval deltas/rates from these over actual elapsed time).
    new_fulls: AtomicUsize,
    duplicates: AtomicUsize,
    completed_walks: AtomicUsize,
    partial_walks: AtomicUsize,
    paipu_new: AtomicUsize,
    returned_rows: AtomicUsize,
    // True short histories ONLY: complete walk that returned zero rows.
    // Zero-row incompletes (first-page failures, IP429) go to
    // `zero_row_partials` below and never pollute this counter.
    empty_histories: AtomicUsize,
    zero_row_partials: AtomicUsize,
    fetch_errs: AtomicUsize,
    // Fetch outcomes: `fetch_count`/`fetch_ms_total` cover the fetch phase
    // only (includes any mid-walk token refresh inside the history walk).
    // The initial pool acquire is tracked separately below.
    fetch_count: AtomicU64,
    fetch_ms_total: AtomicU64,
    token_acquire_count: AtomicU64,
    token_acquire_ms_total: AtomicU64,
    // Worker-side writer backpressure stalls (event channel send waits).
    writer_send_count: AtomicU64,
    writer_send_wait_ms_total: AtomicU64,
    // Live worker-state gauges (RAII guards in `worker_task`; exact even
    // under cancellation since guards decrement on drop).
    workers_active: AtomicUsize,
    workers_token_wait: AtomicUsize,
    workers_fetching: AtomicUsize,
    workers_send_wait: AtomicUsize,
    // Writer-side aggregates (backlog sampled per received event).
    writer_backlog: AtomicUsize,
    writer_flush_count: AtomicU64,
    writer_flush_ms_total: AtomicU64,
    writer_batch_rows: AtomicU64,
    // Scheduler scan aggregates + last-scan snapshot (counted from the
    // single scan's own candidate walk; no extra coverage passes).
    scan_count: AtomicU64,
    scan_ms_total: AtomicU64,
    scan_last_ms: AtomicU64,
    scan_last_candidates: AtomicUsize,
    scan_last_positive: AtomicUsize,
    scan_last_best: AtomicUsize,
    scan_last_resumed: AtomicUsize,
}

/// Saturating millisecond clock readout for interval accumulators.
fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Cancellation-safe gauge guard: increments on entry, decrements on drop
/// (including task-abort unwinding), so live-worker states stay exact.
struct CounterGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> CounterGuard<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for CounterGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Per-scan observable demand snapshot (no extra coverage passes).
#[derive(Debug, Default, Clone, Copy)]
struct ScanReport {
    elapsed_ms: u64,
    candidates: usize,
    positive: usize,
    best: u32,
    resumed: usize,
}

fn record_scan(stats: &Stats, report: &ScanReport) {
    stats.scan_count.fetch_add(1, Ordering::Relaxed);
    stats.scan_ms_total.fetch_add(report.elapsed_ms, Ordering::Relaxed);
    stats.scan_last_ms.store(report.elapsed_ms, Ordering::Relaxed);
    stats
        .scan_last_candidates
        .store(report.candidates, Ordering::Relaxed);
    stats
        .scan_last_positive
        .store(report.positive, Ordering::Relaxed);
    stats
        .scan_last_best
        .store(report.best as usize, Ordering::Relaxed);
    stats.scan_last_resumed.store(report.resumed, Ordering::Relaxed);
}

/// Previous-tick cumulative snapshot for actual-elapsed interval deltas.
#[derive(Default)]
struct IntervalPrev {
    at: Option<Instant>,
    new_fulls: usize,
    duplicates: usize,
    completed_walks: usize,
    partial_walks: usize,
    paipu_new: usize,
    returned_rows: usize,
    empty_histories: usize,
    zero_row_partials: usize,
    fetch_errs: usize,
    fetch_count: u64,
    fetch_ms_total: u64,
    token_acquire_count: u64,
    token_acquire_ms_total: u64,
    writer_send_count: u64,
    writer_send_ms_total: u64,
}

/// Per-second rate over the actual measured interval (never an assumed 30s).
fn interval_rate(delta: u64, dt_s: f64) -> f64 {
    if dt_s > 0.0 { f64::from(u32::try_from(delta).unwrap_or(u32::MAX)) / dt_s } else { 0.0 }
}

/// Per-fetch-lane aggregates. Lanes are fixed at startup (home + one per
/// door proxy) and `lane = wid % lanes.len()` never changes, so cardinality
/// stays bounded no matter the worker count and offline grouping by worker
/// stays stable. Lets the per-lane 30s rows correlate throughput with the
/// auth side's per-request diagnostics (which cannot know the lane without
/// a context API change); the `fulls_fetch` span below carries the same
/// lane id on every auth event inside the walk.
/// Every job performs exactly one initial acquire, so the per-lane acquire
/// attempt count is `fetch_ok + fetch_err`.
#[derive(Debug, Default)]
struct LaneStats {
    fetch_ok: AtomicU64,
    fetch_err: AtomicU64,
    fetch_ms_total: AtomicU64,
    returned_rows: AtomicU64,
    empty_histories: AtomicU64,
    zero_row_partials: AtomicU64,
    complete_walks: AtomicU64,
    partial_walks: AtomicU64,
    token_acquire_ms_total: AtomicU64,
    writer_send_count: AtomicU64,
    writer_send_wait_ms_total: AtomicU64,
}

/// Previous-tick per-lane snapshot for actual-elapsed interval deltas.
#[derive(Default)]
struct LanePrev {
    ok: u64,
    err: u64,
    rows: u64,
    empty: u64,
    zero_row_partials: u64,
    complete: u64,
    partial: u64,
    acquire_ms: u64,
    fetch_ms: u64,
    send_count: u64,
    send_ms: u64,
}


// ---------------------------------------------------------------------------
// Paths / small pure helpers.
// ---------------------------------------------------------------------------

fn completed_path_for(output: &Path) -> PathBuf {
    output.with_extension("completed.jsonl")
}

fn partial_path_for(output: &Path) -> PathBuf {
    output.with_extension("partial.jsonl")
}

/// Resume at the oldest banked startTime, minus one millisecond, matching
/// the site's player-history index. Empty rows restart the current cursor.
fn resume_cursor(recs: &[GameRecord]) -> Option<i64> {
    recs.iter()
        .map(|g| g.start_time)
        .min()
        .map(|t| t * 1000 - 1)
}

/// Bounded retry cooldown from consecutive `fails`. Caps at 5m so partials
/// retry promptly but never spin hot; failures never poison-drop.
fn retry_cooldown(fails: u32) -> Duration {
    let secs = 30u64.checked_shl(fails.min(4)).unwrap_or(300).min(300);
    Duration::from_secs(secs.max(15))
}


// ---------------------------------------------------------------------------
// Streaming bootstrap loaders (bounded `BufRead`, malformed-tolerant).
// ---------------------------------------------------------------------------

/// Stream uuids from a full-rows journal (`uuid` per row). Missing file =>
/// empty. Malformed/torn lines skipped (counted for the startup log).
fn load_full_uuids(path: &Path) -> anyhow::Result<Coverage> {
    let mut out = Coverage::default();
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    let mut bad = 0usize;
    for line in std::io::BufReader::with_capacity(64 * 1024, f).lines() {
        let line = line?;
        // Torn tail (concurrent append cut mid-line) fails parse: retryable
        // next scan via incremental tails, never fatal here.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            bad += 1;
            continue;
        };
        if let Some(uuid) = v.get("uuid").and_then(serde_json::Value::as_str) {
            if let Some(start) = v.get("startTime").and_then(serde_json::Value::as_i64) {
                if let Some(id) = compressed_game_id(uuid, start) { out.covered.insert(id); }
            }
            out.fulls.insert(uuid.to_owned());
        } else {
            bad += 1;
        }
    }
    if bad > 0 {
        info!(path = %path.display(), bad, "fulls: bootstrap skipped malformed lines");
    }
    Ok(out)
}

/// Stream uuids from a paipu journal. Separate set from fulls: a full-row
/// dedup hit must never substitute for paipu dedup (independent journals,
/// independent resume).
fn load_paipu_uuids(path: &Path) -> anyhow::Result<HashSet<String>> {
    let mut out = HashSet::new();
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    let mut bad = 0usize;
    for line in std::io::BufReader::with_capacity(64 * 1024, f).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            bad += 1;
            continue;
        };
        if let Some(uuid) = v.get("uuid").and_then(serde_json::Value::as_str) {
            out.insert(uuid.to_owned());
        } else {
            bad += 1;
        }
    }
    if bad > 0 {
        info!(path = %path.display(), bad, "fulls: bootstrap paipu skipped malformed");
    }
    Ok(out)
}

/// Replay completed sidecar: any row marks `(player, mode)` covered. Existing
/// completions are respected across restarts; only acked completions add more.
fn load_completed(path: &Path) -> anyhow::Result<HashSet<(i64, i32)>> {
    let mut out = HashSet::new();
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for line in std::io::BufReader::with_capacity(32 * 1024, f).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let (Some(id), Some(mode)) = (
            v.get("_completed_player").and_then(serde_json::Value::as_i64),
            v.get("modeId")
                .and_then(serde_json::Value::as_i64)
                .and_then(|m| i32::try_from(m).ok()),
        ) {
            out.insert((id, mode));
        }
    }
    Ok(out)
}

/// Replay partial sidecar append-only: latest row per `(player, mode)` wins
/// (never truncate until safe). `cursor -1`/missing = fresh. No fails-cap
/// drop: every key stays retryable with its preserved cursor.
fn load_partials(path: &Path) -> anyhow::Result<PartialCursorMap> {
    let mut out: PartialCursorMap = HashMap::new();
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for line in std::io::BufReader::with_capacity(32 * 1024, f).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let (Some(id), Some(m)) = (
            v.get("accountId").and_then(serde_json::Value::as_i64),
            v.get("mode").and_then(serde_json::Value::as_i64),
        ) else {
            continue;
        };
        let Ok(mode) = i32::try_from(m) else {
            continue;
        };
        let cursor = v.get("cursor").and_then(serde_json::Value::as_i64).filter(|c| *c > 0);
        let fails = v
            .get("fails")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |x| u32::try_from(x).unwrap_or(u32::MAX));
        out.insert((id, mode), (cursor, fails));
    }
    Ok(out)
}

// Bring `BufRead::lines` into scope for the loaders above.
use std::io::BufRead as _;

// ---------------------------------------------------------------------------
// Scheduler: incremental known/freq, pending + in_flight, acked completions.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
async fn scheduler_task(
    inputs: Vec<PathBuf>,
    job_tx: mpsc::Sender<Job>,
    mut ack_rx: mpsc::Receiver<Ack>,
    completed_seed: HashSet<(i64, i32)>,
    partials_seed: PartialCursorMap,
    batch_limit: Option<usize>,
    scan_secs: u64,
    pool: Arc<AuthPool>,
    banked: BankedUuids,
    stats: Arc<Stats>,
    lanes: Arc<Vec<LaneStats>>,
) -> anyhow::Result<()> {
    // Incremental union state persists for the whole service (tails only).
    let mut tail_offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut tail_seen: HashSet<String> = HashSet::new();
    let mut known: HashSet<(i64, i32)> = HashSet::new();
    let mut freq: HashMap<(i64, i32), u32> = HashMap::new();

    let mut pending: VecDeque<Job> = VecDeque::new();
    let mut pending_set: HashSet<(i64, i32)> = HashSet::new();
    let mut in_flight: HashSet<(i64, i32)> = HashSet::new();
    let mut completed: HashSet<(i64, i32)> = completed_seed;
    // Cursor/fails + earliest retry instant per partial walker.
    let now = Instant::now();
    let mut partials: HashMap<(i64, i32), (Option<i64>, u32, Instant)> = partials_seed
        .into_iter()
        .map(|(k, (c, f))| (k, (c, f, now)))
        .collect();
    // Previous-tick snapshots for actual-elapsed interval deltas/rates.
    let mut prev = IntervalPrev::default();
    let mut lane_prev: Vec<LanePrev> = (0..lanes.len()).map(|_| LanePrev::default()).collect();

    let mut scan_tick = tokio::time::interval(Duration::from_secs(scan_secs));
    scan_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut log_tick = tokio::time::interval(Duration::from_secs(30));
    log_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Old short-page completions can contradict witnessed games absent from
    // the full journal. Reconcile once at startup, not every scan: repeatedly
    // reopening an exhausted index would hammer histories the server lacks.
    union_tails_streaming(&inputs, &mut tail_offsets, &mut tail_seen, &mut known, &mut freq, &banked)?;
    {
        let coverage = banked.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = completed.len();
        completed.retain(|key| {
            let keep = coverage.scores.get(key).copied().unwrap_or(0) == 0;
            if !keep { partials.remove(key); }
            keep
        });
        info!(reopened = before - completed.len(), "reconciled contradictory history completions");
    }

    record_scan(
        &stats,
        &scan_and_enqueue(
            &inputs,
            &mut tail_offsets,
            &mut tail_seen,
            &mut known,
            &mut freq,
            &banked,
            &mut pending,
            &mut pending_set,
            &in_flight,
            &completed,
            &partials,
            batch_limit,
        )?,
    );

    loop {
        tokio::select! {
            _ = scan_tick.tick() => {
                record_scan(
                    &stats,
                    &scan_and_enqueue(
                        &inputs,
                        &mut tail_offsets,
                        &mut tail_seen,
                        &mut known,
                        &mut freq,
                        &banked,
                        &mut pending,
                        &mut pending_set,
                        &in_flight,
                        &completed,
                        &partials,
                        batch_limit,
                    )?,
                );
            }
            ack = ack_rx.recv() => {
                let Some(ack) = ack else {
                    anyhow::bail!("fulls scheduler: ack channel closed (writer gone)");
                };
                match ack {
                    Ack::Completed { account_id, mode } => {
                        in_flight.remove(&(account_id, mode));
                        pending_set.remove(&(account_id, mode));
                        completed.insert((account_id, mode));
                        partials.remove(&(account_id, mode));
                    }
                    Ack::Partial { account_id, mode, cursor, fails } => {
                        in_flight.remove(&(account_id, mode));
                        pending_set.remove(&(account_id, mode));
                        // Persisted by the writer before this ack; retry after
                        // a bounded cooldown with the same cursor (never drop).
                        let ready = Instant::now() + retry_cooldown(fails);
                        partials.insert((account_id, mode), (cursor, fails, ready));
                    }
                }
            }
            permit = job_tx.reserve(), if !pending.is_empty() => {
                let permit = permit?;
                let Some(job) = pending.pop_front() else { continue };
                pending_set.remove(&(job.account_id, job.mode));
                // In-flight from dispatch until ack; crash/restart replays the
                // durable sidecars, so no ack is ever skipped spuriously.
                in_flight.insert((job.account_id, job.mode));
                permit.send(job);
            }
            _ = log_tick.tick() => {
                // Actual elapsed interval: the first tick fires immediately,
                // so rates stay zero until a real interval has passed.
                let now_tick = Instant::now();
                let dt_s = prev.at.map_or(0.0, |t| now_tick.duration_since(t).as_secs_f64());
                let dt_s = if dt_s < 0.5 { 0.0 } else { dt_s };
                let tokens = pool.len().await;
                let uncovered_games = banked.read().unwrap_or_else(std::sync::PoisonError::into_inner).witnesses.len();
                // Queued jobs = buffered in the bounded job channel (32),
                // distinct from the scheduler-side ready `pending` horizon.
                let jobs_queued = job_tx.max_capacity().saturating_sub(job_tx.capacity());
                let cum_avg = |ms: u64, n: u64| ms.checked_div(n).unwrap_or(0);
                // Cumulative counters (monotonic per process).
                let new_fulls = stats.new_fulls.load(Ordering::Relaxed);
                let dup = stats.duplicates.load(Ordering::Relaxed);
                let done = stats.completed_walks.load(Ordering::Relaxed);
                let partial_done = stats.partial_walks.load(Ordering::Relaxed);
                let paipu = stats.paipu_new.load(Ordering::Relaxed);
                let rows = stats.returned_rows.load(Ordering::Relaxed);
                let empty = stats.empty_histories.load(Ordering::Relaxed);
                let zerorow = stats.zero_row_partials.load(Ordering::Relaxed);
                let errs = stats.fetch_errs.load(Ordering::Relaxed);
                let n_fetch = stats.fetch_count.load(Ordering::Relaxed);
                let fetch_ms = stats.fetch_ms_total.load(Ordering::Relaxed);
                let n_acquire = stats.token_acquire_count.load(Ordering::Relaxed);
                let acquire_ms = stats.token_acquire_ms_total.load(Ordering::Relaxed);
                let n_send = stats.writer_send_count.load(Ordering::Relaxed);
                let send_ms = stats.writer_send_wait_ms_total.load(Ordering::Relaxed);
                // Interval deltas over the ACTUAL elapsed time.
                let d = |cur: usize, was: usize| cur.saturating_sub(was) as u64;
                let du = |cur: u64, was: u64| cur.saturating_sub(was);
                let new_fulls_d = d(new_fulls, prev.new_fulls);
                let dup_d = d(dup, prev.duplicates);
                let done_d = d(done, prev.completed_walks);
                let partial_d = d(partial_done, prev.partial_walks);
                let paipu_d = d(paipu, prev.paipu_new);
                let rows_d = d(rows, prev.returned_rows);
                let empty_d = d(empty, prev.empty_histories);
                let zerorow_d = d(zerorow, prev.zero_row_partials);
                let errs_d = d(errs, prev.fetch_errs);
                let n_fetch_d = du(n_fetch, prev.fetch_count);
                let fetch_ms_d = du(fetch_ms, prev.fetch_ms_total);
                let acquire_ms_d = du(acquire_ms, prev.token_acquire_ms_total);
                let n_acquire_d = du(n_acquire, prev.token_acquire_count);
                let send_ms_d = du(send_ms, prev.writer_send_ms_total);
                let n_send_d = du(n_send, prev.writer_send_count);
                let flush_n = stats.writer_flush_count.load(Ordering::Relaxed);
                let flush_ms = stats.writer_flush_ms_total.load(Ordering::Relaxed);
                let batch_rows = stats.writer_batch_rows.load(Ordering::Relaxed);
                let scan_n = stats.scan_count.load(Ordering::Relaxed);
                let scan_ms = stats.scan_ms_total.load(Ordering::Relaxed);
                // Expose campaign-game coverage independently of overlapping
                // journals and unrelated games found in player histories.
                let coverage = banked.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                let live = serde_json::json!({
                    "updated_at": chrono::Utc::now().timestamp(),
                    "target_games": coverage.target_games,
                    "remaining_known": coverage.witnesses.len(),
                    "paipu_rate": interval_rate(paipu_d, dt_s),
                    "rate_ready": dt_s > 0.0,
                });
                drop(coverage);
                if let Some(root) = inputs.first().and_then(|path| path.parent()) {
                    if let Err(error) = std::fs::write(root.join("pipeline-progress.json"), live.to_string()) {
                        warn!(%error, "writing dashboard progress");
                    }
                }

                info!(
                    dt_s,
                    queue = pending.len(),
                    jobs_queued,
                    inflight = in_flight.len(),
                    workers_active = stats.workers_active.load(Ordering::Relaxed),
                    workers_token_wait = stats.workers_token_wait.load(Ordering::Relaxed),
                    workers_fetching = stats.workers_fetching.load(Ordering::Relaxed),
                    workers_send_wait = stats.workers_send_wait.load(Ordering::Relaxed),
                    tokens,
                    uncovered_games,
                    known = known.len(),
                    completed = completed.len(),
                    cursors = partials.len(),
                    new_fulls,
                    new_fulls_delta = new_fulls_d,
                    new_fulls_rate = interval_rate(new_fulls_d, dt_s),
                    duplicates = dup,
                    duplicates_delta = dup_d,
                    duplicates_rate = interval_rate(dup_d, dt_s),
                    completed_walks = done,
                    completed_walks_delta = done_d,
                    completed_walks_rate = interval_rate(done_d, dt_s),
                    partial_walks = partial_done,
                    partial_walks_delta = partial_d,
                    partial_walks_rate = interval_rate(partial_d, dt_s),
                    paipu_new = paipu,
                    paipu_new_delta = paipu_d,
                    paipu_new_rate = interval_rate(paipu_d, dt_s),
                    returned_rows = rows,
                    returned_rows_delta = rows_d,
                    returned_rows_rate = interval_rate(rows_d, dt_s),
                    empty_histories = empty,
                    empty_histories_delta = empty_d,
                    empty_histories_rate = interval_rate(empty_d, dt_s),
                    zero_row_partials = zerorow,
                    zero_row_partials_delta = zerorow_d,
                    zero_row_partials_rate = interval_rate(zerorow_d, dt_s),
                    fetch_errs = errs,
                    fetch_errs_delta = errs_d,
                    fetch_errs_rate = interval_rate(errs_d, dt_s),
                    fetch_avg_ms = cum_avg(fetch_ms, n_fetch),
                    fetch_n_interval = n_fetch_d,
                    fetch_phase_avg_ms_interval = cum_avg(fetch_ms_d, n_fetch_d),
                    token_acquire_avg_ms = cum_avg(acquire_ms, n_acquire),
                    token_acquire_avg_ms_interval = cum_avg(acquire_ms_d, n_acquire_d),
                    send_wait_avg_ms = cum_avg(send_ms, n_send),
                    send_wait_avg_ms_interval = cum_avg(send_ms_d, n_send_d),
                    writer_backlog = stats.writer_backlog.load(Ordering::Relaxed),
                    writer_flush_count = flush_n,
                    writer_flush_avg_ms = cum_avg(flush_ms, flush_n),
                    writer_batch_rows_avg = cum_avg(batch_rows, flush_n),
                    scan_last_ms = stats.scan_last_ms.load(Ordering::Relaxed),
                    scan_candidates = stats.scan_last_candidates.load(Ordering::Relaxed),
                    scan_positive = stats.scan_last_positive.load(Ordering::Relaxed),
                    scan_best = stats.scan_last_best.load(Ordering::Relaxed),
                    scan_resumed = stats.scan_last_resumed.load(Ordering::Relaxed),
                    scan_fresh = stats.scan_last_candidates.load(Ordering::Relaxed).saturating_sub(stats.scan_last_resumed.load(Ordering::Relaxed)),
                    scan_avg_ms = cum_avg(scan_ms, scan_n),
                    "fulls progress"
                );
                // One row per fetch lane per tick (~10 rows): interval history
                // outcomes (walks/rows/empty/zero-row/partial) plus phase waits,
                // so an offline lane is visible without per-game spam. `fetch_avg`
                // covers the walk INCLUDING mid-walk token refresh.
                for (i, lane) in lanes.iter().enumerate() {
                    let ok = lane.fetch_ok.load(Ordering::Relaxed);
                    let lerr = lane.fetch_err.load(Ordering::Relaxed);
                    let lrows = lane.returned_rows.load(Ordering::Relaxed);
                    let lempty = lane.empty_histories.load(Ordering::Relaxed);
                    let lzerorow = lane.zero_row_partials.load(Ordering::Relaxed);
                    let lcomplete = lane.complete_walks.load(Ordering::Relaxed);
                    let lpartial = lane.partial_walks.load(Ordering::Relaxed);
                    let lacquire_ms = lane.token_acquire_ms_total.load(Ordering::Relaxed);
                    let lfetch_ms = lane.fetch_ms_total.load(Ordering::Relaxed);
                    let lsend_n = lane.writer_send_count.load(Ordering::Relaxed);
                    let lsend_ms = lane.writer_send_wait_ms_total.load(Ordering::Relaxed);
                    let was = &lane_prev[i];
                    let ok_d = du(ok, was.ok);
                    let lane_errors = du(lerr, was.err);
                    let rows_d = du(lrows, was.rows);
                    let empty_d = du(lempty, was.empty);
                    let zero_d = du(lzerorow, was.zero_row_partials);
                    let lane_completions = du(lcomplete, was.complete);
                    let partial_d = du(lpartial, was.partial);
                    let acquire_d = du(lacquire_ms, was.acquire_ms);
                    let fetch_d = du(lfetch_ms, was.fetch_ms);
                    let send_n_d = du(lsend_n, was.send_count);
                    let send_d = du(lsend_ms, was.send_ms);
                    info!(
                        lane = i,
                        ok = ok_d,
                        ok_rate = interval_rate(ok_d, dt_s),
                        err = lane_errors,
                        err_rate = interval_rate(lane_errors, dt_s),
                        rows = rows_d,
                        rows_rate = interval_rate(rows_d, dt_s),
                        empty = empty_d,
                        zero_row = zero_d,
                        complete = lane_completions,
                        partial = partial_d,
                        acquire_avg_ms = cum_avg(acquire_d, ok_d.saturating_add(lane_errors)),
                        fetch_avg_ms = cum_avg(fetch_d, ok_d),
                        send_avg_ms = cum_avg(send_d, send_n_d),
                        cum_ok = ok,
                        cum_err = lerr,
                        "fulls lane"
                    );
                    lane_prev[i] = LanePrev {
                        ok,
                        err: lerr,
                        rows: lrows,
                        empty: lempty,
                        zero_row_partials: lzerorow,
                        complete: lcomplete,
                        partial: lpartial,
                        acquire_ms: lacquire_ms,
                        fetch_ms: lfetch_ms,
                        send_count: lsend_n,
                        send_ms: lsend_ms,
                    };
                }
                prev = IntervalPrev {
                    at: Some(now_tick),
                    new_fulls,
                    duplicates: dup,
                    completed_walks: done,
                    partial_walks: partial_done,
                    paipu_new: paipu,
                    returned_rows: rows,
                    empty_histories: empty,
                    zero_row_partials: zerorow,
                    fetch_errs: errs,
                    fetch_count: n_fetch,
                    fetch_ms_total: fetch_ms,
                    token_acquire_count: n_acquire,
                    token_acquire_ms_total: acquire_ms,
                    writer_send_count: n_send,
                    writer_send_ms_total: send_ms,
                };
            }
        }
    }
}

/// Streaming tail union with the same witness semantics as the shared helper
/// but without buffering whole journals. Per path it seeks to the stored
/// offset (reset when shrunk), reads newline-terminated rows via `read_until`,
/// holds a trailing unterminated tail for the next scan, parses each complete
/// row immediately into uuid dedup plus `known`/`freq`, and advances the
/// offset only past full lines. Missing inputs are treated as not-yet-present.
fn union_tails_streaming(
    paths: &[PathBuf],
    offsets: &mut HashMap<PathBuf, u64>,
    seen_games: &mut HashSet<String>,
    known: &mut HashSet<(i64, i32)>,
    freq: &mut HashMap<(i64, i32), u32>,
    banked: &BankedUuids,
) -> anyhow::Result<()> {
    use std::io::{BufRead as _, Seek as _};
    for path in paths {
        let mut off = offsets.get(path).copied().unwrap_or(0);
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let len = file.metadata()?.len();
        if len < off {
            off = 0;
        }
        file.seek(std::io::SeekFrom::Start(off))?;
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        let mut consumed: u64 = 0;
        loop {
            let mut buf = Vec::new();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            if !buf.ends_with(b"\n") {
                break;
            }
            consumed += n as u64;
            let Ok(text) = std::str::from_utf8(&buf).map(|s| s.trim_end_matches(['\n', '\r'])) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let Some(uuid) = v
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .filter(|u| !u.is_empty())
            else {
                continue;
            };
            if !seen_games.insert(uuid.to_owned()) {
                continue;
            }
            let id = v.get("startTime").and_then(serde_json::Value::as_i64)
                .and_then(|start| compressed_game_id(uuid, start))
                .or_else(|| short_game_id(uuid));
            let mut witnesses = Vec::with_capacity(4);
            let mode = v
                .get("modeId")
                .or_else(|| v.get("mode"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|m| i32::try_from(m).ok())
                .unwrap_or(16);
            if let Some(players) = v.get("players").and_then(|p| p.as_array()) {
                for p in players {
                    if let Some(id) = p.get("accountId").and_then(serde_json::Value::as_i64) {
                        known.insert((id, mode));
                        *freq.entry((id, mode)).or_insert(0) += 1;
                        if !witnesses.contains(&(id, mode)) { witnesses.push((id, mode)); }
                    }
                }
            }
            if let Some(id) = id {
                let mut coverage = banked.write().unwrap_or_else(std::sync::PoisonError::into_inner);
                coverage.witness(id, witnesses);
            }
        }
        offsets.insert(path.clone(), off + consumed);
    }
    Ok(())
}
/// Bounded pending queue cap: dispatch stays responsive without holding the
/// whole witness set in memory. Leftovers remain in `known` for later scans.
const PENDING_CAP: usize = 128;
/// One incremental scan: stream input tails, then enqueue outstanding
/// witnesses uncovered-first. Candidates exclude completed, pending, in-flight,
/// and cooling partials. Always sorted by descending uncovered appearances,
/// then total frequency, then key, so `None` limits stay deterministic and a
/// witness with many already-banked appearances yields to one with genuinely
/// uncovered games; leftovers remain in `known`, so every witness still runs.
#[allow(clippy::too_many_arguments)]
fn scan_and_enqueue(
    inputs: &[PathBuf],
    tail_offsets: &mut HashMap<PathBuf, u64>,
    tail_seen: &mut HashSet<String>,
    known: &mut HashSet<(i64, i32)>,
    freq: &mut HashMap<(i64, i32), u32>,
    banked: &BankedUuids,
    pending: &mut VecDeque<Job>,
    pending_set: &mut HashSet<(i64, i32)>,
    in_flight: &HashSet<(i64, i32)>,
    completed: &HashSet<(i64, i32)>,
    partials: &HashMap<(i64, i32), (Option<i64>, u32, Instant)>,
    batch_limit: Option<usize>,
) -> anyhow::Result<ScanReport> {
    let scan_started = Instant::now();
    union_tails_streaming(inputs, tail_offsets, tail_seen, known, freq, banked)?;
    // Rebuild the short ready horizon, so newly covered games cannot leave
    // stale high scores committed in a multi-minute FIFO backlog.
    pending.clear();
    pending_set.clear();
    let coverage = banked.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    let mut candidates: Vec<(i64, i32)> = Vec::new();
    for key in known.iter().copied() {
        if completed.contains(&key) || pending_set.contains(&key) || in_flight.contains(&key) {
            continue;
        }
        if let Some((_, _, ready)) = partials.get(&key) {
            if *ready > now {
                continue;
            }
        }
        candidates.push(key);
    }
    if candidates.is_empty() {
        return Ok(ScanReport {
            elapsed_ms: ms_since(scan_started),
            ..ScanReport::default()
        });
    }
    candidates.sort_unstable_by(|a, b| {
        coverage.scores
            .get(b)
            .copied()
            .unwrap_or(0)
            .cmp(&coverage.scores.get(a).copied().unwrap_or(0))
            .then_with(|| {
                freq.get(b)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&freq.get(a).copied().unwrap_or(0))
            })
            .then_with(|| a.cmp(b))
    });
    if let Some(lim) = batch_limit {
        if lim > 0 && candidates.len() > lim {
            candidates.truncate(lim);
        }
    }
    let room = PENDING_CAP.saturating_sub(pending.len());
    if candidates.len() > room {
        candidates.truncate(room);
    }
    // Demand snapshot from this same candidate walk: no extra coverage
    // pass. Counts reflect the horizon actually enqueued (post cap):
    // positive vs zero coverage score, best score, and partial-resume vs
    // fresh cursors, so dispatch capacity consumption is visible.
    let mut positive = 0usize;
    let mut best = 0u32;
    let mut resumed = 0usize;
    for key in &candidates {
        let s = coverage.scores.get(key).copied().unwrap_or(0);
        if s > 0 {
            positive += 1;
        }
        best = best.max(s);
        if partials.get(key).is_some_and(|(c, _, _)| c.is_some()) {
            resumed += 1;
        }
    }
    let report = ScanReport {
        elapsed_ms: ms_since(scan_started),
        candidates: candidates.len(),
        positive,
        best,
        resumed,
    };
    if room == 0 {
        return Ok(report);
    }
    for (pid, mode) in candidates {
        if pending_set.contains(&(pid, mode)) || in_flight.contains(&(pid, mode)) {
            continue;
        }
        let (cursor, fails) = partials
            .get(&(pid, mode))
            .map_or((None, 0), |(c, f, _)| (*c, *f));
        pending.push_back(Job {
            account_id: pid,
            mode,
            resume_at: cursor,
            fails,
        });
        pending_set.insert((pid, mode));
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Workers: acquire token, exhaustive `fetch_player_records`, typed emits.
// ---------------------------------------------------------------------------

/// Timed writer-event send: measures worker-side backpressure stalls while
/// holding the send-wait gauge (RAII, cancellation-safe).
async fn send_event(
    tx: &mpsc::Sender<WriterEvent>,
    ev: WriterEvent,
    stats: &Stats,
    lane: &LaneStats,
    wid: usize,
) -> anyhow::Result<()> {
    let _wait = CounterGuard::enter(&stats.workers_send_wait);
    let t = Instant::now();
    let r = tx.send(ev).await;
    let ms = ms_since(t);
    stats.writer_send_count.fetch_add(1, Ordering::Relaxed);
    stats
        .writer_send_wait_ms_total
        .fetch_add(ms, Ordering::Relaxed);
    lane.writer_send_count.fetch_add(1, Ordering::Relaxed);
    lane.writer_send_wait_ms_total.fetch_add(ms, Ordering::Relaxed);
    r.map_err(|_| anyhow::anyhow!("fulls worker {wid}: writer gone"))?;
    Ok(())
}

// Long by design: phased acquire/fetch/send timing plus per-lane accounting
// in one verified job loop; splitting churns the gauge guards for no gain.
#[allow(clippy::too_many_lines)]
 #[allow(clippy::too_many_arguments)]
 async fn worker_task(
    wid: usize,
    job_rx: Arc<Mutex<mpsc::Receiver<Job>>>,
    event_tx: mpsc::Sender<WriterEvent>,
    pool: Arc<AuthPool>,
    clients: Arc<Vec<reqwest::Client>>,
    day: String,
    stats: Arc<Stats>,
    lanes: Arc<Vec<LaneStats>>,
) -> anyhow::Result<()> {
    if clients.is_empty() {
        anyhow::bail!("fulls worker {wid}: no fetch clients");
    }
    // Stable lane identity for this worker (clients are fixed at startup);
    // offline grouping by `wid` recovers the lane, and the `fulls_fetch`
    // span below carries it on every auth event inside the walk.
    let lane_idx = wid % clients.len();
    let lane = &lanes[lane_idx];
    let client = &clients[lane_idx];
    loop {
        let job = { job_rx.lock().await.recv().await };
        let Some(job) = job else {
            // Scheduler closed (shutdown/error cascade). Drain path only.
            return Ok(());
        };
        // Live-job gauge for the whole handling below (cancellation-safe).
        let _active = CounterGuard::enter(&stats.workers_active);
        // Per-job span: worker id + lane + mode only, NEVER the player id.
        // Wraps the token acquire and the history walk so the auth side's
        // request category/latency and warn diagnostics inherit lane
        // correlation without any context API change.
        let span = tracing::info_span!("fulls_fetch", worker = wid, lane = lane_idx, mode = job.mode);
        // Per-job tab identity for the token acquire (pool keys by tab; the
        // key-bypass path mints `Bearer <key>` without browser solves).
        // `acquire` never solves: it waits on idle-pool notification.
        let account_id = job.account_id;
        let mode = job.mode;
        let tab = format!("fulls-{account_id}-{mode}");
        let acquire_started = Instant::now();
        let token_wait = CounterGuard::enter(&stats.workers_token_wait);
        let acquired = pool.acquire(&tab, &day, job.mode).instrument(span.clone()).await;
        // Measure before the guard drops so cancellation still releases it.
        let acquire_ms = ms_since(acquire_started);
        drop(token_wait);
        stats.token_acquire_count.fetch_add(1, Ordering::Relaxed);
        stats
            .token_acquire_ms_total
            .fetch_add(acquire_ms, Ordering::Relaxed);
        lane.token_acquire_ms_total.fetch_add(acquire_ms, Ordering::Relaxed);
        let (tab_owned, auth, mirror) = match acquired {
            Ok(entry) => entry,
            Err(e) => {
                stats.fetch_errs.fetch_add(1, Ordering::Relaxed);
                lane.fetch_err.fetch_add(1, Ordering::Relaxed);
                lane.partial_walks.fetch_add(1, Ordering::Relaxed);
                warn!("fulls worker {wid}: acquire {tab} failed: {e:#}");
                // Preserve cursor across acquire failures; cooldown applies on
                // the scheduler side after the writer durably appends this.
                let fails = job.fails.saturating_add(1);
                send_event(
                    &event_tx,
                    WriterEvent::Partial {
                        account_id: job.account_id,
                        mode: job.mode,
                        cursor: job.resume_at,
                        fails,
                    },
                    &stats,
                    lane,
                    wid,
                )
                .await?;
                continue;
            }
        };
        let ctx = FetchCtx {
            client,
            pool: &pool,
            tab: &tab_owned,
            day: &day,
        };
        // Fetch phase: the whole exhaustive walk, INCLUDING any mid-walk
        // token refresh the auth layer performs on 429s (labeled as such in
        // the log; never confused with the initial acquire above).
        let fetch_started = Instant::now();
        let fetching = CounterGuard::enter(&stats.workers_fetching);
        let fetch_res = fetch_player_records(&ctx, &mirror, job.account_id, job.mode, &auth, job.resume_at)
            .instrument(span)
            .await;
        let fetch_ms = ms_since(fetch_started);
        drop(fetching);
        match fetch_res {
            Ok((recs, complete)) => {
                stats.fetch_count.fetch_add(1, Ordering::Relaxed);
                stats.fetch_ms_total.fetch_add(fetch_ms, Ordering::Relaxed);
                lane.fetch_ok.fetch_add(1, Ordering::Relaxed);
                lane.fetch_ms_total.fetch_add(fetch_ms, Ordering::Relaxed);
                stats.returned_rows.fetch_add(recs.len(), Ordering::Relaxed);
                lane.returned_rows.fetch_add(u64::try_from(recs.len()).unwrap_or(u64::MAX), Ordering::Relaxed);
                if recs.is_empty() {
                    if complete {
                        stats.empty_histories.fetch_add(1, Ordering::Relaxed);
                        lane.empty_histories.fetch_add(1, Ordering::Relaxed);
                    } else {
                        stats.zero_row_partials.fetch_add(1, Ordering::Relaxed);
                        lane.zero_row_partials.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if complete {
                    lane.complete_walks.fetch_add(1, Ordering::Relaxed);
                } else {
                    lane.partial_walks.fetch_add(1, Ordering::Relaxed);
                }
                let resume = if complete { None } else { resume_cursor(&recs).or(job.resume_at) };
                let fails = if complete { job.fails } else { job.fails.saturating_add(1) };
                // Token ownership stays with the history fetch; workers never recycle entries.
                for g in recs {
                    send_event(&event_tx, WriterEvent::Game(g), &stats, lane, wid).await?;
                }
                if complete {
                    send_event(
                        &event_tx,
                        WriterEvent::Complete {
                            account_id: job.account_id,
                            mode: job.mode,
                        },
                        &stats,
                        lane,
                        wid,
                    )
                    .await?;
                } else {
                    send_event(
                        &event_tx,
                        WriterEvent::Partial {
                            account_id: job.account_id,
                            mode: job.mode,
                            cursor: resume,
                            fails,
                        },
                        &stats,
                        lane,
                        wid,
                    )
                    .await?;
                }
            }
            Err(e) => {
                stats.fetch_errs.fetch_add(1, Ordering::Relaxed);
                lane.fetch_err.fetch_add(1, Ordering::Relaxed);
                lane.partial_walks.fetch_add(1, Ordering::Relaxed);
                let account_id = job.account_id;
                let mode = job.mode;
                warn!("fulls worker {wid}: player {account_id}/{mode}: {e:#}");
                let fails = job.fails.saturating_add(1);
                send_event(
                    &event_tx,
                    WriterEvent::Partial {
                        account_id: job.account_id,
                        mode: job.mode,
                        cursor: job.resume_at,
                        fails,
                    },
                    &stats,
                    lane,
                    wid,
                )
                .await?;
            }
        }
        // No fixed sleeps: pacing lives inside the pool/token path, and the
        // bounded job channel already applies backpressure.
    }
}

// ---------------------------------------------------------------------------
// Writer: single owner of output + sidecars, dedup sets, durable acks.
// ---------------------------------------------------------------------------

/// Timed flush of the buffered game journals. Counts flushes, flush latency,
/// and rows per flush for the 30s log; terminal records always flush before
/// their ack regardless of the periodic bound.
async fn flush_game_buffers(
    out: &mut tokio::io::BufWriter<tokio::fs::File>,
    paipu_out: &mut Option<tokio::io::BufWriter<tokio::fs::File>>,
    since_flush: &mut usize,
    stats: &Stats,
) -> anyhow::Result<()> {
    let t = Instant::now();
    out.flush().await?;
    if let Some(pf) = paipu_out.as_mut() {
        pf.flush().await?;
    }
    stats.writer_flush_count.fetch_add(1, Ordering::Relaxed);
    stats.writer_flush_ms_total.fetch_add(ms_since(t), Ordering::Relaxed);
    stats.writer_batch_rows.fetch_add(*since_flush as u64, Ordering::Relaxed);
    *since_flush = 0;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn writer_task(
    mut event_rx: mpsc::Receiver<WriterEvent>,
    ack_tx: mpsc::Sender<Ack>,
    output: PathBuf,
    paipu: Option<PathBuf>,
    banked: BankedUuids,
    mut paipu_done: HashSet<String>,
    stats: Arc<Stats>,
) -> anyhow::Result<()> {
    let completed_path = completed_path_for(&output);
    let partial_path = partial_path_for(&output);

    let mut out = tokio::io::BufWriter::with_capacity(
        256 * 1024,
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)
            .await?,
    );
    let mut paipu_out: Option<tokio::io::BufWriter<tokio::fs::File>> = None;
    if let Some(p) = &paipu {
        paipu_out = Some(tokio::io::BufWriter::with_capacity(
            64 * 1024,
            tokio::fs::OpenOptions::new().create(true).append(true).open(p).await?,
        ));
    }
    let mut completed_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&completed_path)
        .await?;
    let mut partial_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial_path)
        .await?;

    // Periodic flush bound for buffered games (completions/partials always
    // flush before their ack regardless of this counter).
    let mut since_flush = 0usize;

    while let Some(ev) = event_rx.recv().await {
        // Exact buffered-event backlog for the 30s log (bounded: one number,
        // sampled here, never per-game log spam).
        stats.writer_backlog.store(event_rx.len(), Ordering::Relaxed);
        match ev {
            WriterEvent::Game(g) => {
                let uuid = g.uuid.clone();
                let is_new = match banked.write() {
                    Ok(mut guard) => guard.bank(&g),
                    Err(poison) => poison.into_inner().bank(&g),
                };
                if is_new {
                    let v = serde_json::to_value(&g)?;
                    out.write_all((serde_json::to_string(&v)? + "\n").as_bytes()).await?;
                    stats.new_fulls.fetch_add(1, Ordering::Relaxed);
                    since_flush += 1;
                } else {
                    stats.duplicates.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(pf) = paipu_out.as_mut() {
                    if !g.players.is_empty() && paipu_done.insert(uuid.clone()) {
                        if let Some(row) = super::paipu::row(&g) {
                            pf.write_all((row.to_string() + "\n").as_bytes()).await?;
                            stats.paipu_new.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                if since_flush >= 200 {
                    flush_game_buffers(&mut out, &mut paipu_out, &mut since_flush, &stats).await?;
                }
            }
            WriterEvent::Complete { account_id, mode } => {
                // Commit preceding games+paipu before the completion record;
                // crash recovery may replay, never skip.
                flush_game_buffers(&mut out, &mut paipu_out, &mut since_flush, &stats).await?;
                let row = serde_json::json!({"_completed_player": account_id, "modeId": mode});
                completed_file.write_all((row.to_string() + "\n").as_bytes()).await?;
                completed_file.flush().await?;
                stats.completed_walks.fetch_add(1, Ordering::Relaxed);
                ack_tx
                    .send(Ack::Completed { account_id, mode })
                    .await
                    .map_err(|_| anyhow::anyhow!("fulls writer: scheduler gone"))?;
            }
            WriterEvent::Partial {
                account_id,
                mode,
                cursor,
                fails,
            } => {
                // persisted cursor matches durable rows; then the cursor row
                // itself is flushed before the ack (append-only, latest wins
                // on replay; never truncated, never poison-dropped).
                flush_game_buffers(&mut out, &mut paipu_out, &mut since_flush, &stats).await?;
                let c = cursor.unwrap_or(-1);
                let row =
                    serde_json::json!({"accountId": account_id, "mode": mode, "cursor": c, "fails": fails});
                partial_file.write_all((row.to_string() + "\n").as_bytes()).await?;
                partial_file.flush().await?;
                stats.partial_walks.fetch_add(1, Ordering::Relaxed);
                ack_tx
                    .send(Ack::Partial {
                        account_id,
                        mode,
                        cursor,
                        fails,
                    })
                    .await
                    .map_err(|_| anyhow::anyhow!("fulls writer: scheduler gone"))?;
            }
        }
    }

    out.flush().await?;
    if let Some(pf) = paipu_out.as_mut() {
        pf.flush().await?;
    }
    completed_file.flush().await?;
    partial_file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    fn test_dir(name: &str) -> PathBuf {
        let base = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("tmp");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("fulls-test-{}-{nanos}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create isolated test dir");
        dir
    }
    fn test_game(uuid: &str, mode: i32, start: i64, account: i64) -> GameRecord {
        GameRecord {
            uuid: uuid.to_owned(),
            mode_id: mode,
            start_time: start,
            end_time: Some(start + 60),
            players: vec![super::super::types::PlayerRecord {
                account_id: account,
                nickname: "t".to_owned(),
                level: 10400,
                score: 25000,
                grading_score: None,
            }],
        }
    }
    #[tokio::test]
    async fn writer_backfills_paipu_on_duplicate_full() {
        let dir = test_dir("backfill");
        let out = dir.join("fulls.jsonl");
        let paipu = dir.join("paipu.jsonl");
        let (event_tx, event_rx) = mpsc::channel(16);
        let (ack_tx, mut ack_rx) = mpsc::channel(16);
        let stats = Arc::new(Stats::default());
        let mut seed = HashSet::new();
        seed.insert("u1".to_owned());
        let banked: BankedUuids = Arc::new(std::sync::RwLock::new(Coverage { fulls: seed, ..Coverage::default() }));
        let handle = tokio::spawn(writer_task(
            event_rx,
            ack_tx,
            out.clone(),
            Some(paipu.clone()),
            banked,
            HashSet::new(),
            Arc::clone(&stats),
        ));
        event_tx
            .send(WriterEvent::Game(test_game("u1", 16, 1_700_000_000, 111)))
            .await
            .expect("send duplicate game");
        event_tx
            .send(WriterEvent::Complete {
                account_id: 111,
                mode: 16,
            })
            .await
            .expect("send complete");
        let ack = tokio::time::timeout(Duration::from_secs(5), ack_rx.recv())
            .await
            .expect("ack timeout")
            .expect("writer closed");
        assert!(matches!(
            ack,
            Ack::Completed {
                account_id: 111,
                mode: 16
            }
        ));
        drop(event_tx);
        handle.await.expect("join").expect("writer ok");
        let fulls = std::fs::read_to_string(&out).unwrap_or_default();
        assert!(
            !fulls.contains("u1"),
            "duplicate full must not append again: {fulls}"
        );
        let paipu_text = std::fs::read_to_string(&paipu).expect("paipu readable after ack");
        assert!(paipu_text.contains("u1"), "missing paipu backfill: {paipu_text}");
        assert!(paipu_text.contains("u1_a88554176"), "wrong paipu id: {paipu_text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn completion_ack_means_game_and_paipu_readable() {
        let dir = test_dir("durable");
        let out = dir.join("fulls.jsonl");
        let paipu = dir.join("paipu.jsonl");
        let completed = completed_path_for(&out);
        let (event_tx, event_rx) = mpsc::channel(16);
        let (ack_tx, mut ack_rx) = mpsc::channel(16);
        let stats = Arc::new(Stats::default());
        let handle = tokio::spawn(writer_task(
            event_rx,
            ack_tx,
            out.clone(),
            Some(paipu.clone()),
            BankedUuids::default(),
            HashSet::new(),
            Arc::clone(&stats),
        ));
        event_tx
            .send(WriterEvent::Game(test_game("u2", 12, 1_700_000_100, 222)))
            .await
            .expect("send game");
        event_tx
            .send(WriterEvent::Complete {
                account_id: 222,
                mode: 12,
            })
            .await
            .expect("send complete");
        let ack = tokio::time::timeout(Duration::from_secs(5), ack_rx.recv())
            .await
            .expect("ack timeout")
            .expect("writer closed");
        assert!(matches!(
            ack,
            Ack::Completed {
                account_id: 222,
                mode: 12
            }
        ));
        drop(event_tx);
        handle.await.expect("join").expect("writer ok");
        let fulls = std::fs::read_to_string(&out).expect("game readable after ack");
        assert!(fulls.contains("u2"), "game missing after ack: {fulls}");
        let paipu_text = std::fs::read_to_string(&paipu).expect("paipu readable after ack");
        assert!(paipu_text.contains("u2_a88562103"), "paipu missing after ack: {paipu_text}");
        let done = std::fs::read_to_string(&completed).expect("completed readable after ack");
        assert!(done.contains("_completed_player"), "completion missing: {done}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn partial_replay_keeps_latest_cursor() {
        let dir = test_dir("partial");
        let path = dir.join("fulls.partial.jsonl");
        let mut f = std::fs::File::create(&path).expect("create partial");
        writeln!(f, "{{\"accountId\":7,\"mode\":16,\"cursor\":100,\"fails\":0}}").expect("write");
        writeln!(f, "not json").expect("write");
        writeln!(f, "{{\"accountId\":7,\"mode\":16,\"cursor\":200,\"fails\":1}}").expect("write");
        writeln!(f, "{{\"accountId\":8,\"mode\":12,\"cursor\":-1,\"fails\":0}}").expect("write");
        drop(f);
        let m = load_partials(&path).expect("replay");
        assert_eq!(m.get(&(7, 16)), Some(&(Some(200), 1)));
        assert_eq!(m.get(&(8, 12)), Some(&(None, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn empty_partial_keeps_prior_cursor() {
        assert_eq!(resume_cursor(&[]), None);
        assert_eq!(resume_cursor(&[]).or(Some(99)), Some(99));
        let g = test_game("u9", 16, 1_700_000_000, 333);
        let computed = resume_cursor(&[g]);
        assert_eq!(computed, Some(1_700_000_000 * 1000 - 1));
        assert_eq!(computed.or(Some(99)), computed);
    }
    #[test]
    fn scheduler_dense_first_and_cooldown_fair() {
        let mut offsets = HashMap::new();
        let mut seen = HashSet::new();
        let mut known = HashSet::from([(1, 16), (2, 16), (3, 16)]);
        let mut freq = HashMap::from([((1, 16), 10), ((2, 16), 100), ((3, 16), 1)]);
        let banked: BankedUuids = BankedUuids::default();
        let mut pending = VecDeque::new();
        let mut pending_set = HashSet::new();
        let in_flight = HashSet::new();
        let completed = HashSet::new();
        let mut partials = HashMap::from([(
            (3, 16),
            (Some(50), 0, Instant::now() + Duration::from_secs(300)),
        )]);
        scan_and_enqueue(
            &[],
            &mut offsets,
            &mut seen,
            &mut known,
            &mut freq,
            &banked,
            &mut pending,
            &mut pending_set,
            &in_flight,
            &completed,
            &partials,
            Some(1),
        )
        .expect("scan");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].account_id, 2);
        partials.insert((3, 16), (Some(50), 0, Instant::now()));
        scan_and_enqueue(
            &[],
            &mut offsets,
            &mut seen,
            &mut known,
            &mut freq,
            &banked,
            &mut pending,
            &mut pending_set,
            &in_flight,
            &completed,
            &partials,
            None,
        )
        .expect("scan");
        assert!(pending_set.contains(&(3, 16)));
    }
    #[test]
    fn real_short_identity_and_later_banking_reprioritize_pending() {
        let dir = test_dir("priority-real");
        let input = dir.join("shorts.jsonl");
        let full = "230111-ee5063d5-5ad5-4e52-86be-2343669ed6c6";
        let start = 1_673_370_670;
        let game = test_game(full, 12, start, 1);
        assert_eq!(compressed_game_id(full, start), short_game_id("8YurmJzXzCH"));
        let mut f = std::fs::File::create(&input).expect("input");
        writeln!(f, "{{\"uuid\":\"8YurmJzXzCH\",\"modeId\":12,\"startTime\":{start},\"players\":[{{\"accountId\":1}},{{\"accountId\":3}}]}}").expect("row");
        writeln!(f, "{{\"uuid\":\"8YuqhQEGgvK\",\"modeId\":12,\"players\":[{{\"accountId\":2}}]}}").expect("row");
        drop(f);
        let banked = BankedUuids::default();
        let mut offsets = HashMap::new();
        let mut seen = HashSet::new();
        let mut known = HashSet::new();
        let mut freq = HashMap::new();
        let mut pending = VecDeque::new();
        let mut pending_set = HashSet::new();
        let in_flight = HashSet::new();
        let completed = HashSet::new();
        let partials = HashMap::new();
        scan_and_enqueue(&[input], &mut offsets, &mut seen, &mut known, &mut freq,
            &banked, &mut pending, &mut pending_set, &in_flight, &completed, &partials, None).expect("scan");
        assert_eq!(pending.front().expect("job").account_id, 1);
        // A different witness supplies the same full game after the scan.
        // Both seated players lose their missing score, not just the walker.
        banked.write().expect("coverage").bank(&game);
        scan_and_enqueue(&[], &mut offsets, &mut seen, &mut known, &mut freq,
            &banked, &mut pending, &mut pending_set, &in_flight, &completed, &partials, None).expect("rescore");
        assert_eq!(pending.front().expect("uncovered job").account_id, 2);
        assert!(pending_set.contains(&(1, 12)), "covered witnesses remain exhaustive");
        assert!(pending_set.contains(&(3, 12)), "other seated witness retained");
        // Bootstrap must recognize the same short ID without a network lookup.
        let journal = dir.join("fulls.jsonl");
        std::fs::write(&journal, serde_json::to_string(&game).expect("serialize") + "\n").expect("journal");
        let mut restored = load_full_uuids(&journal).expect("bootstrap");
        restored.witness(short_game_id("8YurmJzXzCH").expect("short"), vec![(9, 12)]);
        assert!(!restored.scores.contains_key(&(9, 12)), "banked short must not score missing");
        // Collision in the compressed prefix must never deduplicate full UUIDs.
        let collision = test_game("230111-ee5063d5-0000-0000-0000-000000000000", 12, start, 4);
        assert!(restored.bank(&collision));
        assert_eq!(restored.fulls.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn playerless_then_witnessed_backfills_paipu() {
        let dir = test_dir("playerless");
        let out = dir.join("fulls.jsonl");
        let paipu = dir.join("paipu.jsonl");
        let (event_tx, event_rx) = mpsc::channel(16);
        let (ack_tx, mut ack_rx) = mpsc::channel(16);
        let stats = Arc::new(Stats::default());
        let handle = tokio::spawn(writer_task(
            event_rx,
            ack_tx,
            out.clone(),
            Some(paipu.clone()),
            BankedUuids::default(),
            HashSet::new(),
            Arc::clone(&stats),
        ));
        let mut bare = test_game("u3", 16, 1_700_000_200, 0);
        bare.players.clear();
        event_tx
            .send(WriterEvent::Game(bare))
            .await
            .expect("send player-less game");
        event_tx
            .send(WriterEvent::Game(test_game("u3", 16, 1_700_000_200, 444)))
            .await
            .expect("send witnessed game");
        event_tx
            .send(WriterEvent::Complete {
                account_id: 444,
                mode: 16,
            })
            .await
            .expect("send complete");
        let ack = tokio::time::timeout(Duration::from_secs(5), ack_rx.recv())
            .await
            .expect("ack timeout")
            .expect("writer closed");
        assert!(matches!(
            ack,
            Ack::Completed {
                account_id: 444,
                mode: 16
            }
        ));
        drop(event_tx);
        handle.await.expect("join").expect("writer ok");
        let fulls = std::fs::read_to_string(&out).expect("fulls readable after ack");
        assert_eq!(fulls.lines().filter(|l| l.contains("u3")).count(), 1);
        let paipu_text = std::fs::read_to_string(&paipu).expect("paipu readable after ack");
        assert!(
            paipu_text.contains("u3_a88560553"),
            "witnessed paipu missing after player-less row: {paipu_text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
