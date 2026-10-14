//! Hourly-window Koromo uuid harvester (ported from ~/tmp/majsoul-smoke/pool_harvest.py).
//!
//! Proven semantics, single IP:
//! - 24 fixed 1h JST windows per day; cursor pages within the hour only.
//! - Overlap cursor `oldest * 1000` (server floors/ceils to seconds; seen-set dedups).
//! - 10s minimum-range guard (server 400s `invalid_date_range` below ~10s).
//! - Cursor-stall break (park hour, never silent-loss).
//! - 429 body classifier: `x-cap-token-required`/CAPTCHA -> token path (re-solve);
//!   `Too many requests` (no cap text) -> IP backoff 60s.
//! - Auth behind one entrypoint: `KOROMO_API_KEY` env bypasses browser solves.
//! - Resume: SQLite seen-set + done day/modes + parked hours (retry queue).
//! - Jade tiers: pure fns on `GameRecord` (`tier_game`, `sort_key`).

use anyhow::{Context, Result};
use chrono::{FixedOffset, NaiveDate};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::LazyLock;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::{debug, info, warn};

use super::types::GameRecord;

/// Jade tier split (ported from `jade_tiers.py`). Throne (16) = all; Jade (12) by max level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JadeTier {
    Throne,
    JadeHigh, // Saint+ / Celestial (>= 10501)
    JadeMid,  // Master (10401..=10403)
    JadeLow,  // rest of mode 12
    Other,
}

/// Max player level in a game (rank signal from `PlayerRecord.level`).
pub fn max_level(game: &GameRecord) -> i32 {
    game.players
        .iter()
        .map(|p| p.level)
        .max()
        .unwrap_or_default()
}

/// Strength of one player: `grading_score` preferred, `score` fallback.
pub fn strength_of_level(game: &GameRecord, level: i32) -> i32 {
    game.players
        .iter()
        .filter(|p| p.level == level)
        .map(|p| p.grading_score.unwrap_or(p.score))
        .max()
        .unwrap_or_default()
}

/// Rank+strength sort key: (max level, max strength among top-level holders).
/// Celestial Lv10 sorts above Lv1 within `JadeHigh`; direct Rust port of `sort_key`.
pub fn sort_key(game: &GameRecord) -> (i32, i32) {
    let m = max_level(game);
    (m, strength_of_level(game, m))
}

/// Tier split: direct port of `tier_game`.
pub fn tier_game(game: &GameRecord) -> JadeTier {
    match game.mode_id {
        16 => JadeTier::Throne,
        12 => {
            let m = max_level(game);
            if m >= 10501 {
                JadeTier::JadeHigh
            } else if (10401..=10403).contains(&m) {
                JadeTier::JadeMid
            } else {
                JadeTier::JadeLow
            }
        }
        _ => JadeTier::Other,
    }
}

/// Tier label for journal rows (Jade re-sort pass reads this, no recompute).
pub fn tier_label(game: &GameRecord) -> &'static str {
    match tier_game(game) {
        JadeTier::Throne => "throne",
        JadeTier::JadeHigh => "jade-high",
        JadeTier::JadeMid => "jade-mid",
        JadeTier::JadeLow => "jade-low",
        JadeTier::Other => "other",
    }
}

/// One harvested record: raw game + provenance stamps + tier/sort key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StampedRecord {
    pub day: String,
    pub mode: i32,
    pub tier: String,
    pub rank: (i32, i32),
    #[serde(flatten)]
    pub game: GameRecord,
}

impl StampedRecord {
    pub fn stamp(day: String, mode: i32, game: GameRecord) -> Self {
        let tier = tier_label(&game).to_owned();
        let rank = sort_key(&game);
        Self {
            day,
            mode,
            tier,
            rank,
            game,
        }
    }
}

/// 429 classifier: token path needs a fresh solve, IP path needs backoff.
/// NOTE: `contact SAPikachu to obtain a key` is the TOKEN path (solve CAPTCHA),
/// not the IP limiter. True IP limit = express-rate-limit text without cap text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Token,
    Ip,
}

pub fn classify_429(body: &str) -> LimitKind {
    if body.contains("x-cap-token-required") || body.contains("CAPTCHA") {
        LimitKind::Token
    } else {
        LimitKind::Ip
    }
}
/// Sanitized 429 detail for error strings: bounded classification + body
/// length only. Raw bodies are never embedded (they may carry edge/IP text);
/// classification itself still runs on the raw body via `classify_429`.
fn sanitize_429_detail(body: &str, token_path: bool) -> String {
    let kind = if token_path {
        if body.contains("x-cap-token-required") {
            "cap-token-required"
        } else if body.contains("CAPTCHA") {
            "captcha"
        } else {
            "token-other"
        }
    } else if body.contains("Too many requests") {
        "rate-limited"
    } else {
        "ip-other"
    };
    format!("{kind} len={n}", n = body.len())
}

/// Numeric rate-limit hints from a 429 response. Allowlisted headers only,
/// integers only: `retry-after` (delay seconds; HTTP dates ignored) plus the
/// `x-ratelimit-*` / `ratelimit-*` limit/remaining/reset counters. Never raw
/// header text. Must run before the body is consumed. Evidence only: callers
/// MUST NOT use these to evade limits. Missing headers stay `None`.
#[derive(Debug, Clone, Copy, Default)]
struct RateHint {
    retry_after_s: Option<u64>,
    limit: Option<u64>,
    remaining: Option<u64>,
    reset: Option<u64>,
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

fn rate_hint(resp: &reqwest::Response) -> RateHint {
    let headers = resp.headers();
    RateHint {
        retry_after_s: header_u64(headers, "retry-after"),
        limit: header_u64(headers, "x-ratelimit-limit")
            .or_else(|| header_u64(headers, "ratelimit-limit")),
        remaining: header_u64(headers, "x-ratelimit-remaining")
            .or_else(|| header_u64(headers, "ratelimit-remaining")),
        reset: header_u64(headers, "x-ratelimit-reset")
            .or_else(|| header_u64(headers, "ratelimit-reset")),
    }
}

/// Numeric-only suffix for sanitized 429 error strings (`retry_after_s=..`
/// parts appear only when the server sent them).
fn rate_hint_suffix(hint: &RateHint) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(v) = hint.retry_after_s {
        let _ = write!(out, " retry_after_s={v}");
    }
    if let Some(v) = hint.limit {
        let _ = write!(out, " limit={v}");
    }
    if let Some(v) = hint.remaining {
        let _ = write!(out, " remaining={v}");
    }
    if let Some(v) = hint.reset {
        let _ = write!(out, " reset={v}");
    }
    out
}

/// Last-seen numeric rate hints (`u64::MAX` = never seen). Written on every
/// 429 that carries at least one allowlisted numeric header; surfaced at INFO
/// in the 30s `fetch interval` aggregate so no debug enabling or restart is
/// needed to read the limiter's current evidence.
static RATE_HINT_429S: AtomicU64 = AtomicU64::new(0);
static LAST_RETRY_AFTER_S: AtomicU64 = AtomicU64::new(u64::MAX);
static LAST_RATE_LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);
static LAST_RATE_REMAINING: AtomicU64 = AtomicU64::new(u64::MAX);
static LAST_RATE_RESET: AtomicU64 = AtomicU64::new(u64::MAX);

fn note_rate_hint(hint: &RateHint) {
    let mut seen = false;
    if let Some(v) = hint.retry_after_s {
        LAST_RETRY_AFTER_S.store(v, Ordering::Relaxed);
        seen = true;
    }
    if let Some(v) = hint.limit {
        LAST_RATE_LIMIT.store(v, Ordering::Relaxed);
        seen = true;
    }
    if let Some(v) = hint.remaining {
        LAST_RATE_REMAINING.store(v, Ordering::Relaxed);
        seen = true;
    }
    if let Some(v) = hint.reset {
        LAST_RATE_RESET.store(v, Ordering::Relaxed);
        seen = true;
    }
    if seen {
        RATE_HINT_429S.fetch_add(1, Ordering::Relaxed);
    }
}
/// Per-page outcome category. Fetch-lane identity comes from the caller's
/// worker/lane tracing span; auth-side aggregates stay global by page kind
/// (`games` vs `player`) so cardinality stays bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageCategory {
    Ok,
    Token429,
    Ip429,
    HttpErr,
    NetErr,
    DecodeErr,
}

/// Cumulative page-fetch outcomes for one page kind. Interval deltas are
/// derived by the 30s gate / reporters; only sums are stored, never a
/// cumulative average labeled as current.
struct PageStats {
    pages: AtomicU64,
    ok: AtomicU64,
    rows: AtomicU64,
    ms_total: AtomicU64,
    token429: AtomicU64,
    ip429: AtomicU64,
    http_err: AtomicU64,
    net_err: AtomicU64,
    decode_err: AtomicU64,
}

impl PageStats {
    const fn new() -> Self {
        Self {
            pages: AtomicU64::new(0),
            ok: AtomicU64::new(0),
            rows: AtomicU64::new(0),
            ms_total: AtomicU64::new(0),
            token429: AtomicU64::new(0),
            ip429: AtomicU64::new(0),
            http_err: AtomicU64::new(0),
            net_err: AtomicU64::new(0),
            decode_err: AtomicU64::new(0),
        }
    }

    fn record(&self, elapsed_ms: u64, rows: u64, category: PageCategory) {
        self.pages.fetch_add(1, Ordering::Relaxed);
        self.ms_total.fetch_add(elapsed_ms, Ordering::Relaxed);
        match category {
            PageCategory::Ok => {
                self.ok.fetch_add(1, Ordering::Relaxed);
                self.rows.fetch_add(rows, Ordering::Relaxed);
            }
            PageCategory::Token429 => {
                self.token429.fetch_add(1, Ordering::Relaxed);
            }
            PageCategory::Ip429 => {
                self.ip429.fetch_add(1, Ordering::Relaxed);
            }
            PageCategory::HttpErr => {
                self.http_err.fetch_add(1, Ordering::Relaxed);
            }
            PageCategory::NetErr => {
                self.net_err.fetch_add(1, Ordering::Relaxed);
            }
            PageCategory::DecodeErr => {
                self.decode_err.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

static GAMES_CHUNK_STATS: LazyLock<PageStats> = LazyLock::new(PageStats::new);
static PLAYER_PAGE_STATS: LazyLock<PageStats> = LazyLock::new(PageStats::new);

fn games_chunk_stats() -> &'static PageStats {
    &GAMES_CHUNK_STATS
}

fn player_page_stats() -> &'static PageStats {
    &PLAYER_PAGE_STATS
}


/// Cumulative player-history walk outcomes. Recorded once per finished walk
/// (complete or partial); the 30s gate reports interval deltas as rates.
struct HistoryStats {
    walks: AtomicU64,
    complete: AtomicU64,
    partial: AtomicU64,
    pages: AtomicU64,
    rows: AtomicU64,
    refresh_ok: AtomicU64,
    refresh_fail: AtomicU64,
    token_wait_ms: AtomicU64,
    req_ms: AtomicU64,
}

impl HistoryStats {
    const fn new() -> Self {
        Self {
            walks: AtomicU64::new(0),
            complete: AtomicU64::new(0),
            partial: AtomicU64::new(0),
            pages: AtomicU64::new(0),
            rows: AtomicU64::new(0),
            refresh_ok: AtomicU64::new(0),
            refresh_fail: AtomicU64::new(0),
            token_wait_ms: AtomicU64::new(0),
            req_ms: AtomicU64::new(0),
        }
    }
}

static HISTORY_STATS: LazyLock<HistoryStats> = LazyLock::new(HistoryStats::new);

fn history_stats() -> &'static HistoryStats {
    &HISTORY_STATS
}

/// Per-walk completion record: updates cumulative walk stats, emits one
/// `player history walk` debug line (lane context inherited from the caller's
/// worker/lane tracing span), then offers the 30s aggregate a chance to fire.
#[allow(clippy::too_many_arguments)]
fn log_history_walk(
    player_id: i64,
    mode: i32,
    pages: u64,
    rows: usize,
    complete: bool,
    end: &'static str,
    refresh_ok: u64,
    refresh_fail: u64,
    token_wait_ms: u64,
    req_ms: u64,
    elapsed_ms: u64,
) {
    let h = history_stats();
    h.walks.fetch_add(1, Ordering::Relaxed);
    if complete {
        h.complete.fetch_add(1, Ordering::Relaxed);
    } else {
        h.partial.fetch_add(1, Ordering::Relaxed);
    }
    h.pages.fetch_add(pages, Ordering::Relaxed);
    h.rows.fetch_add(rows as u64, Ordering::Relaxed);
    h.refresh_ok.fetch_add(refresh_ok, Ordering::Relaxed);
    h.refresh_fail.fetch_add(refresh_fail, Ordering::Relaxed);
    h.token_wait_ms.fetch_add(token_wait_ms, Ordering::Relaxed);
    h.req_ms.fetch_add(req_ms, Ordering::Relaxed);
    debug!(
        player = player_id,
        mode,
        pages,
        rows,
        complete,
        end,
        refresh_ok,
        refresh_fail,
        token_wait_ms,
        req_ms,
        elapsed_ms,
        "player history walk"
    );
    maybe_log_fetch_interval();
}

/// Baseline snapshot for the 30s fetch-interval gate: both page kinds, walk
/// totals and last-seen numeric rate hints. Lives behind a `try_lock` gate
/// so hot request paths pay one failed lock attempt when another thread is
/// logging. Deltas are interval-only; `last_*` rate values are point-in-time
/// (`u64::MAX` = no hint seen yet) and approximate under concurrency, like
/// all `Relaxed` snapshots here.
#[derive(Default, Clone, Copy)]
struct FetchLogBase {
    at: Option<Instant>,
    games_pages: u64,
    games_ok: u64,
    games_rows: u64,
    games_ms: u64,
    games_token429: u64,
    games_ip429: u64,
    games_http: u64,
    games_net: u64,
    games_decode: u64,
    player_pages: u64,
    player_ok: u64,
    player_rows: u64,
    player_ms: u64,
    player_token429: u64,
    player_ip429: u64,
    player_http: u64,
    player_net: u64,
    player_decode: u64,
    walks: u64,
    complete: u64,
    partial: u64,
    hpages: u64,
    hrows: u64,
    refresh_ok: u64,
    refresh_fail: u64,
    token_wait_ms: u64,
    req_ms: u64,
    hint_429s: u64,
    last_retry_after_s: u64,
    last_limit: u64,
    last_remaining: u64,
    last_reset: u64,
}

static FETCH_LOG_GATE: LazyLock<std::sync::Mutex<FetchLogBase>> =
    LazyLock::new(|| std::sync::Mutex::new(FetchLogBase::default()));
/// Interval per-second rate. Counts are 30s/60s interval deltas and never
/// approach `u32` range, so the bounded conversion keeps full `f64`
/// precision without a precision-loss cast.
fn interval_rate(count: u64, secs: f64) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / secs
}

/// 30s aggregated fetch interval. Emits one `fetch interval` info event with
/// interval counts and per-second rates (attempted/s, ok/s, rows/s), latency
/// as interval sum+count, split token-429 / ip-429 / http-err counters, and
/// last-seen numeric rate-limit hints. No detached task: record sites call
/// this; the `try_lock` gate lets at most one caller per 30s log. No debug
/// enabling needed for these metrics.
// Long by design: snapshot + interval deltas + one aggregate event must stay
// in a single verified shape so counts and baselines cannot drift apart.
#[allow(clippy::too_many_lines)]
fn maybe_log_fetch_interval() {
    let Ok(mut base) = FETCH_LOG_GATE.try_lock() else {
        return;
    };
    let now = Instant::now();
    if let Some(at) = base.at {
        if now.duration_since(at) < Duration::from_secs(30) {
            return;
        }
    }
    let g = games_chunk_stats();
    let p = player_page_stats();
    let h = history_stats();
    let snap = FetchLogBase {
        at: Some(now),
        games_pages: g.pages.load(Ordering::Relaxed),
        games_ok: g.ok.load(Ordering::Relaxed),
        games_rows: g.rows.load(Ordering::Relaxed),
        games_ms: g.ms_total.load(Ordering::Relaxed),
        games_token429: g.token429.load(Ordering::Relaxed),
        games_ip429: g.ip429.load(Ordering::Relaxed),
        games_http: g.http_err.load(Ordering::Relaxed),
        games_net: g.net_err.load(Ordering::Relaxed),
        games_decode: g.decode_err.load(Ordering::Relaxed),
        player_pages: p.pages.load(Ordering::Relaxed),
        player_ok: p.ok.load(Ordering::Relaxed),
        player_rows: p.rows.load(Ordering::Relaxed),
        player_ms: p.ms_total.load(Ordering::Relaxed),
        player_token429: p.token429.load(Ordering::Relaxed),
        player_ip429: p.ip429.load(Ordering::Relaxed),
        player_http: p.http_err.load(Ordering::Relaxed),
        player_net: p.net_err.load(Ordering::Relaxed),
        player_decode: p.decode_err.load(Ordering::Relaxed),
        walks: h.walks.load(Ordering::Relaxed),
        complete: h.complete.load(Ordering::Relaxed),
        partial: h.partial.load(Ordering::Relaxed),
        hpages: h.pages.load(Ordering::Relaxed),
        hrows: h.rows.load(Ordering::Relaxed),
        refresh_ok: h.refresh_ok.load(Ordering::Relaxed),
        refresh_fail: h.refresh_fail.load(Ordering::Relaxed),
        token_wait_ms: h.token_wait_ms.load(Ordering::Relaxed),
        req_ms: h.req_ms.load(Ordering::Relaxed),
        hint_429s: RATE_HINT_429S.load(Ordering::Relaxed),
        last_retry_after_s: LAST_RETRY_AFTER_S.load(Ordering::Relaxed),
        last_limit: LAST_RATE_LIMIT.load(Ordering::Relaxed),
        last_remaining: LAST_RATE_REMAINING.load(Ordering::Relaxed),
        last_reset: LAST_RATE_RESET.load(Ordering::Relaxed),
    };
    let interval_s = base
        .at
        .map_or(30.0, |at| now.duration_since(at).as_secs_f64().max(1.0));
    let first = base.at.is_none();
    let was = *base;
    let d = |cur: u64, prev: u64| cur.saturating_sub(prev);
    let g_pages = d(snap.games_pages, was.games_pages);
    let g_ok = d(snap.games_ok, was.games_ok);
    let g_rows = d(snap.games_rows, was.games_rows);
    let g_ms = d(snap.games_ms, was.games_ms);
    let g_token429 = d(snap.games_token429, was.games_token429);
    let g_ip429 = d(snap.games_ip429, was.games_ip429);
    let g_http = d(snap.games_http, was.games_http);
    let g_net = d(snap.games_net, was.games_net);
    let g_decode = d(snap.games_decode, was.games_decode);
    let p_pages = d(snap.player_pages, was.player_pages);
    let p_ok = d(snap.player_ok, was.player_ok);
    let p_rows = d(snap.player_rows, was.player_rows);
    let p_ms = d(snap.player_ms, was.player_ms);
    let p_token429 = d(snap.player_token429, was.player_token429);
    let p_ip429 = d(snap.player_ip429, was.player_ip429);
    let p_http = d(snap.player_http, was.player_http);
    let p_net = d(snap.player_net, was.player_net);
    let p_decode = d(snap.player_decode, was.player_decode);
    let walks = d(snap.walks, was.walks);
    let complete = d(snap.complete, was.complete);
    let partial = d(snap.partial, was.partial);
    let hpages = d(snap.hpages, was.hpages);
    let hrows = d(snap.hrows, was.hrows);
    let refresh_ok = d(snap.refresh_ok, was.refresh_ok);
    let refresh_fail = d(snap.refresh_fail, was.refresh_fail);
    let token_wait_ms = d(snap.token_wait_ms, was.token_wait_ms);
    let walk_req_ms = d(snap.req_ms, was.req_ms);
    let hint_429s = d(snap.hint_429s, was.hint_429s);
    let last_retry_after_s = snap.last_retry_after_s;
    let last_limit = snap.last_limit;
    let last_remaining = snap.last_remaining;
    let last_reset = snap.last_reset;
    *base = snap;
    drop(base);
    if first {
        return;
    }
    info!(
        interval_s,
        games_attempted = g_pages,
        games_ok = g_ok,
        games_rows = g_rows,
        games_token429 = g_token429,
        games_ip429 = g_ip429,
        games_http_err = g_http,
        games_net_err = g_net,
        games_decode_err = g_decode,
        games_req_ms_sum = g_ms,
        games_req_count = g_pages,
        games_attempted_s = interval_rate(g_pages, interval_s),
        games_ok_s = interval_rate(g_ok, interval_s),
        games_rows_s = interval_rate(g_rows, interval_s),
        player_attempted = p_pages,
        player_ok = p_ok,
        player_rows = p_rows,
        player_token429 = p_token429,
        player_ip429 = p_ip429,
        player_http_err = p_http,
        player_net_err = p_net,
        player_decode_err = p_decode,
        player_req_ms_sum = p_ms,
        player_req_count = p_pages,
        player_attempted_s = interval_rate(p_pages, interval_s),
        player_ok_s = interval_rate(p_ok, interval_s),
        player_rows_s = interval_rate(p_rows, interval_s),
        walks,
        walks_complete = complete,
        walks_partial = partial,
        walk_pages = hpages,
        walk_rows = hrows,
        refresh_ok,
        refresh_fail,
        walk_token_wait_ms_sum = token_wait_ms,
        walk_token_wait_count = walks,
        walk_req_ms_sum = walk_req_ms,
        rate_hint_429s = hint_429s,
        last_retry_after_s,
        last_rate_limit = last_limit,
        last_rate_remaining = last_remaining,
        last_rate_reset = last_reset,
        "fetch interval"
    );
}

/// Chromium net-error category: bounded enum words only. The raw
/// `error_text` is never logged (it may embed request-specific text).
fn net_err_category(text: &str) -> &'static str {
    let t = text.strip_prefix("net::").unwrap_or(text);
    if t.contains("TIMED_OUT") || t.contains("TIMEOUT") {
        "timeout"
    } else if t.contains("ABORTED") || t.contains("ABANDONED") {
        "aborted"
    } else if t.contains("RESET") {
        "reset"
    } else if t.contains("REFUSED") {
        "refused"
    } else if t.contains("DISCONNECT")
        || t.contains("CONNECTION_CLOSED")
        || t.contains("CONNECTION_BROKEN")
    {
        "conn-closed"
    } else if t.contains("DNS") || t.contains("HOST") || t.contains("NAME_NOT_RESOLVED") {
        "dns"
    } else if t.contains("CERT") || t.contains("SSL") {
        "tls"
    } else if t.contains("BLOCKED") {
        "blocked"
    } else if t.contains("FAILED") {
        "failed"
    } else {
        "other"
    }
}

/// JST midnight millis for a date (site buckets days in JST).
pub fn jst_day_start_ms(date: NaiveDate) -> Result<i64> {
    let jst = FixedOffset::east_opt(9 * 3600).context("valid JST offset")?;
    Ok(date
        .and_hms_opt(0, 0, 0)
        .context("valid midnight")?
        .and_local_timezone(jst)
        .unwrap()
        .timestamp_millis())
}

/// Hour-window bounds (ms) for hour `h` of a JST day.
pub fn hour_window(day_start_ms: i64, h: u32) -> (i64, i64) {
    let h_start = day_start_ms + i64::from(h) * 3_600_000;
    (h_start, h_start + 3_600_000 - 1)
}

/// Minimum valid range: server 400s `invalid_date_range` below ~10s (extApi.js).
pub const MIN_RANGE_MS: i64 = 10_000;

/// Token entry: (tab prefix, bearer, mirror).
pub type TokenEntry = (String, String, String);
/// Cross-process guard for runners sharing a solver; handles release on cancellation.
async fn lock_solver(endpoint: &str) -> Result<std::fs::File> {
    let mut endpoint_hash = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(endpoint, &mut endpoint_hash);
    let lock_path = std::env::temp_dir().join(format!(
        "tenhou-koromo-solver-{:016x}.lock",
        std::hash::Hasher::finish(&endpoint_hash)
    ));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening solver lock {}", lock_path.display()))?;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

/// Shared token pool with paced browser solves.
/// `KOROMO_API_KEY` set -> every tab serves the key, no solves at all.
pub struct AuthPool {
    tokens: Mutex<HashMap<String, TokenEntry>>,
    available: Notify,
    pace_ms: u64,
    brokered: bool,
    cdp_url: String,
    solvers: Vec<String>,
    solve_idx: Mutex<usize>,
    /// Hold through pacing and CDP work: sleeping alone lets callers herd tabs.
    solve_locks: Vec<Mutex<()>>,
    key: Option<String>,
    last_steal: Mutex<Vec<Instant>>,
    /// Browsers sharing an exit also share Cap's request budget.
    mint_exits: Vec<std::sync::Arc<Mutex<Instant>>>,
    /// Consecutive steal failures per solver slot; unhealthy solvers are skipped.
    solver_fails: Mutex<Vec<u32>>,
    /// Last successful 200-bearer per slot (for autoscale decisions).
    solver_ok_at: Mutex<Vec<Option<Instant>>>,
    /// Diagnostics (cumulative atomics; interval deltas via the 60s gate).
    /// Acquire waits: waiter gauge + served count + wait cycles + total wait.
    acquire_waiters: AtomicUsize,
    acquire_served: AtomicU64,
    acquire_cycles: AtomicU64,
    acquire_wait_ms: AtomicU64,
    /// Token take/give/drop/mint outcomes.
    take_hit: AtomicU64,
    take_miss: AtomicU64,
    give_ok: AtomicU64,
    drop_dead: AtomicU64,
    mint_ok: AtomicU64,
    mint_err: AtomicU64,
    key_ok: AtomicU64,
    pool_log_gate: std::sync::Mutex<PoolLogBase>,
}

/// Cancellation-safe acquire-wait gauge: dropping always releases the waiter
/// slot, including when the waiting future is cancelled.
struct AcquireWaitGuard<'a> {
    gauge: &'a AtomicUsize,
}

impl Drop for AcquireWaitGuard<'_> {
    fn drop(&mut self) {
        self.gauge.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Baseline snapshot for the 60s `auth pool interval` gate.
#[derive(Default, Clone, Copy)]
struct PoolLogBase {
    at: Option<Instant>,
    served: u64,
    cycles: u64,
    wait_ms: u64,
    hit: u64,
    miss: u64,
    give: u64,
    drop: u64,
    mint_ok: u64,
    mint_err: u64,
    key: u64,
 }

impl AuthPool {
    pub fn new(cdp_url: String) -> Self {
        // Explicit fleet selection is optional; otherwise honor the caller's CDP URL.
        let solvers: Vec<String> = std::env::var("KOROMO_SOLVERS").ok().map_or_else(
            || vec![cdp_url.clone()],
            |s| {
                s.split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect()
            },
        );
        let pace_ms = std::env::var("KOROMO_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(8000);
        let mut pool = Self::configured(cdp_url, solvers, pace_ms);
        pool.brokered = false;
        pool
    }

    pub fn configured(cdp_url: String, mut solvers: Vec<String>, pace_ms: u64) -> Self {
        if solvers.is_empty() { solvers.push(cdp_url.clone()); }
        let key = std::env::var("KOROMO_API_KEY").ok().filter(|k| !k.is_empty());
        let n = solvers.len().max(1);
        Self {
            tokens: Mutex::new(HashMap::new()),
            available: Notify::new(),
            pace_ms,
            brokered: true,
            cdp_url,
            solvers,
            solve_idx: Mutex::new(0),
            solve_locks: (0..n).map(|_| Mutex::new(())).collect(),
            key,
            mint_exits: Vec::new(),
            last_steal: Mutex::new(vec![
                Instant::now()
                    .checked_sub(Duration::from_secs(60))
                    .unwrap_or_else(Instant::now);
                n
            ]),
            solver_fails: Mutex::new(vec![0u32; n]),
            solver_ok_at: Mutex::new(vec![None; n]),
            acquire_waiters: AtomicUsize::new(0),
            acquire_served: AtomicU64::new(0),
            acquire_cycles: AtomicU64::new(0),
            acquire_wait_ms: AtomicU64::new(0),
            take_hit: AtomicU64::new(0),
            take_miss: AtomicU64::new(0),
            give_ok: AtomicU64::new(0),
            drop_dead: AtomicU64::new(0),
            mint_ok: AtomicU64::new(0),
            mint_err: AtomicU64::new(0),
            key_ok: AtomicU64::new(0),
            pool_log_gate: std::sync::Mutex::new(PoolLogBase::default()),
        }
    }

    pub(super) fn group_mint_exits(&mut self, exits: &[String]) {
        let mut groups = HashMap::new();
        self.mint_exits = exits.iter().map(|exit| {
            std::sync::Arc::clone(groups.entry(exit).or_insert_with(|| {
                std::sync::Arc::new(Mutex::new(Instant::now().checked_sub(Duration::from_secs(5)).unwrap_or_else(Instant::now)))
            }))
        }).collect();
    }

    async fn wait_mint_exit(&self, slot: usize) {
        if let Some(exit) = self.mint_exits.get(slot) {
            // Challenge and redeem share an IP budget: stagger token starts
            // across browsers, not just each browser's independent requests.
            let mut last = exit.lock().await;
            let delay = Duration::from_millis(1100).saturating_sub(last.elapsed());
            if !delay.is_zero() { tokio::time::sleep(delay).await; }
            *last = Instant::now();
        }
    }

    /// Solver fleet size (for autoscale decisions like prewarmer depth).
    pub fn solver_count(&self) -> usize {
        self.solvers.len()
    }

    /// Load banked tokens from a pool.json file (tab -> [bearer, mirror]).
    pub async fn load_file(&self, path: &Path) -> Result<usize> {
        let Ok(text) = tokio::fs::read_to_string(path).await else {
            return Ok(0);
        };
        let raw: HashMap<String, (String, String)> = serde_json::from_str(&text)?;
        let mut tokens = self.tokens.lock().await;
        for (tab, (bearer, mirror)) in raw {
            tokens.insert(tab.clone(), (tab, bearer, mirror));
        }
        Ok(tokens.len())
    }

    /// Rotate: pop one live token (round-robin-ish via arbitrary pick).
    pub async fn take(&self) -> Option<TokenEntry> {
        let mut tokens = self.tokens.lock().await;
        let Some(tab) = tokens.keys().next().cloned() else {
            self.take_miss.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        self.take_hit.fetch_add(1, Ordering::Relaxed);
        tokens.remove(&tab)
    }

    /// Return a token to the pool.
    pub async fn give(&self, entry: TokenEntry) {
        if self.key.is_some() { return; }
        self.tokens.lock().await.insert(entry.0.clone(), entry);
        self.give_ok.fetch_add(1, Ordering::Relaxed);
        self.available.notify_one();
    }

    /// Drop a dead token.
    pub async fn drop_tab(&self, tab: &str) {
        self.tokens.lock().await.remove(tab);
        self.drop_dead.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn len(&self) -> usize {
        self.tokens.lock().await.len()
    }

    /// Await ready authentication without occupying a browser solve slot.
    /// The service owns minting; consumers wake on returned/minted tokens.
    pub async fn acquire(&self, tab: &str, day: &str, mode: i32) -> Result<TokenEntry> {
        if self.key.is_some() { return self.ensure(tab, day, mode).await; }
        let start = Instant::now();
        self.acquire_waiters.fetch_add(1, Ordering::Relaxed);
        let _wait_guard = AcquireWaitGuard {
            gauge: &self.acquire_waiters,
        };
        let mut cycles = 0u64;
        loop {
            let notified = self.available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(entry) = self.take().await {
                self.acquire_served.fetch_add(1, Ordering::Relaxed);
                self.acquire_cycles.fetch_add(cycles, Ordering::Relaxed);
                self.acquire_wait_ms.fetch_add(
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                self.maybe_log_pool_interval();
                return Ok(entry);
            }
            cycles += 1;
            notified.await;
        }
    }

    /// Mint an exclusively owned token; only idle tokens belong in the pool.
    // Long by design: solver pick + pacing + steal + outcome counts must stay
    // in one verified order; splitting risks pacing/count drift between paths.
    #[allow(clippy::too_many_lines)]
    pub async fn ensure(&self, tab: &str, day: &str, mode: i32) -> Result<TokenEntry> {
        if let Some(key) = &self.key {
            // Accept raw or already-prefixed keys (a pasted "Bearer ..." must not
            // become "Bearer Bearer ..." and 401 the whole fleet on key day).
            let bare = key
                .strip_prefix("Bearer ")
                .or_else(|| key.strip_prefix("bearer "))
                .unwrap_or(key)
                .trim();
            self.key_ok.fetch_add(1, Ordering::Relaxed);
            self.maybe_log_pool_interval();
            return Ok((tab.to_owned(), format!("Bearer {bare}"), "https://5-data.amae-koromo.com/".to_owned()));
        }
        // Prefer an available solver before failure score: queueing behind a
        // healthy-but-busy Chrome otherwise strands idle fleet capacity.
        let (endpoint, slot, acquired) = {
            let mut idx = self.solve_idx.lock().await;
            let fails = self.solver_fails.lock().await;
            let n = self.solve_locks.len();
            let start = *idx % n;
            let mut best = start;
            let mut best_score = u32::MAX;
            let mut acquired = None;
            for k in 0..n {
                let s = (start + k) % n;
                let f = fails[s];
                if f <= best_score {
                    if let Ok(guard) = self.solve_locks[s].try_lock() {
                        best_score = f;
                        best = s;
                        acquired = Some(guard);
                        if f == 0 {
                            break;
                        }
                    }
                }
            }
            *idx = (best + 1) % n;
            let ep = self
                .solvers
                .get(best)
                .cloned()
                .unwrap_or_else(|| self.cdp_url.clone());
            (ep, best, acquired)
        };
        // One active solve per Chrome, not per worker. The file lock also covers
        // independent runners sharing an endpoint (q126 and recent Jade). Keep
        // it across the entire solve; dropping the handle releases it on errors
        // and cancellation without stale lock-file ownership or watchdog work.
        let queued_at = Instant::now();
        let _slot_guard = acquired
            .map_or_else(
                || futures::future::Either::Left(self.solve_locks[slot].lock()),
                |guard| futures::future::Either::Right(std::future::ready(guard)),
            )
            .await;
        let _process_guard = lock_solver(&endpoint).await?;
        info!(
            solver = endpoint,
            tab,
            phase = "slot_acquired",
            elapsed_ms = queued_at.elapsed().as_millis(),
            "solver phase"
        );
        // Pacing per solver (env KOROMO_PACE_MS, default 8000). No key = solves needed.
        let pace_ms = self.pace_ms;
        let wait = {
            let last = self.last_steal.lock().await;
            if slot < last.len() {
                Duration::from_millis(pace_ms).saturating_sub(last[slot].elapsed())
            } else {
                Duration::ZERO
            }
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        self.wait_mint_exit(slot).await;
        {
            let mut last = self.last_steal.lock().await;
            if slot < last.len() {
                last[slot] = Instant::now();
            }
        }
        match steal_bearer(&endpoint, tab, day, mode).await {
            Ok((bearer, mirror)) => {
                // Healthy: reset fails, stamp success.
                if slot < self.solver_fails.lock().await.len() {
                    self.solver_fails.lock().await[slot] = 0;
                }
                if slot < self.solver_ok_at.lock().await.len() {
                    self.solver_ok_at.lock().await[slot] = Some(Instant::now());
                }
                self.mint_ok.fetch_add(1, Ordering::Relaxed);
                self.maybe_log_pool_interval();
                let entry = (tab.to_owned(), bearer, mirror);
                Ok(entry)
            }
            Err(e) => {
                // Sick: count up; at 5 consecutive fails the picker skips the slot.
                // No in-process respawn: the watchdog is the SOLE pkill/rm owner.
                // Concurrent respawns from runs sharing solvers raced (4x in 1s
                // observed); a wedged-but-listening solver recovers via the
                // success-reset when the picker retries it.
                if slot < self.solver_fails.lock().await.len() {
                    let mut fails = self.solver_fails.lock().await;
                    fails[slot] = fails[slot].saturating_add(1);
                    if fails[slot] == 5 {
                        warn!("solver slot {slot} ({endpoint}): 5 fails, auto-skipping (watchdog owns respawn)");
                    }
                }
                self.mint_err.fetch_add(1, Ordering::Relaxed);
                self.maybe_log_pool_interval();
                Err(e)
            }
        }
    }

    /// 60s `auth pool interval` info: acquire waits (gauge, served, cycles,
    /// avg wait) plus take/give/mint/drop/key outcome deltas. No detached
    /// task: `acquire`/`ensure` completion paths call this; the `try_lock`
    /// gate lets at most one caller per 60s log.
    fn maybe_log_pool_interval(&self) {
        let Ok(mut base) = self.pool_log_gate.try_lock() else {
            return;
        };
        let now = Instant::now();
        if let Some(at) = base.at {
            if now.duration_since(at) < Duration::from_secs(60) {
                return;
            }
        }
        let snap = PoolLogBase {
            at: Some(now),
            served: self.acquire_served.load(Ordering::Relaxed),
            cycles: self.acquire_cycles.load(Ordering::Relaxed),
            wait_ms: self.acquire_wait_ms.load(Ordering::Relaxed),
            hit: self.take_hit.load(Ordering::Relaxed),
            miss: self.take_miss.load(Ordering::Relaxed),
            give: self.give_ok.load(Ordering::Relaxed),
            drop: self.drop_dead.load(Ordering::Relaxed),
            mint_ok: self.mint_ok.load(Ordering::Relaxed),
            mint_err: self.mint_err.load(Ordering::Relaxed),
            key: self.key_ok.load(Ordering::Relaxed),
        };
        let interval_s = base
            .at
            .map_or(60.0, |at| now.duration_since(at).as_secs_f64().max(1.0));
        let first = base.at.is_none();
        let was = *base;
        let d = |cur: u64, prev: u64| cur.saturating_sub(prev);
        let served = d(snap.served, was.served);
        let cycles = d(snap.cycles, was.cycles);
        let wait_ms = d(snap.wait_ms, was.wait_ms);
        let hit = d(snap.hit, was.hit);
        let miss = d(snap.miss, was.miss);
        let give = d(snap.give, was.give);
        let drop = d(snap.drop, was.drop);
        let mint_ok = d(snap.mint_ok, was.mint_ok);
        let mint_err = d(snap.mint_err, was.mint_err);
        let key = d(snap.key, was.key);
        let waiters = self.acquire_waiters.load(Ordering::Relaxed);
        *base = snap;
        std::mem::drop(base);
        if first {
            return;
        }
        info!(
            interval_s,
            acquire_waiters = waiters,
            acquires = served,
            acquire_waits = cycles,
            acquire_avg_wait_ms = wait_ms.checked_div(served).unwrap_or(0),
            take_hit = hit,
            take_miss = miss,
            give,
            drop,
            mint_ok,
            mint_err,
            key = key,
            "auth pool interval"
        );
    }
}


/// Steal the 200-winning bearer from one browser tab (mirrors `steal_tab.py`):
/// navigate to a `day` page, correlate `requestId` -> `Authorization` header, keep the
/// `bearer` whose response was 200 (the first bearer seen is often the stale one).
pub async fn steal_bearer(
    cdp_url: &str,
    tab_prefix: &str,
    day: &str,
    mode: i32,
) -> Result<(String, String)> {
    use chromiumoxide::browser::Browser;
    // AbortHandle is tied to the solve scope: detached CDP event pumps used to
    // survive setup errors and epoch cancellation, retaining connections.
    struct HandlerGuard(tokio::task::AbortHandle);
    impl Drop for HandlerGuard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let started = Instant::now();
    let (mut browser, mut handler) = Browser::connect(cdp_url.to_owned())
        .await
        .context("CDP connect")?;
    info!(
        solver = cdp_url,
        tab = tab_prefix,
        phase = "connect",
        elapsed_ms = started.elapsed().as_millis(),
        "solver phase"
    );
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let _handler_guard = HandlerGuard(handler_task.abort_handle());
    browser.fetch_targets().await.context("fetch targets")?;
    let page = browser
        .new_page("about:blank")
        .await
        .context("open solve tab")?;
    info!(
        solver = cdp_url,
        tab = tab_prefix,
        phase = "tab_ready",
        elapsed_ms = started.elapsed().as_millis(),
        "solver phase"
    );
    let result = steal_inner(&page, tab_prefix, day, mode).await;
    let close_result = page.close().await;
    info!(
        solver = cdp_url,
        tab = tab_prefix,
        phase = "finished",
        elapsed_ms = started.elapsed().as_millis(),
        success = result.is_ok(),
        tab_closed = close_result.is_ok(),
        "solver phase"
    );
    result
}

/// Post-open solve flow: Network.enable -> listeners -> goto -> capture loop.
// Keep the request, response and GPU completion streams together: ordering is
// part of capturing an accepted bearer, not independent helper operations.
#[allow(clippy::too_many_lines)]
async fn steal_inner(
    page: &chromiumoxide::page::Page,
    tab_prefix: &str,
    day: &str,
    mode: i32,
) -> Result<(String, String)> {
    use anyhow::Context;
    use chromiumoxide::cdp::browser_protocol::network::{
        EventLoadingFailed, EventRequestWillBeSent, EventResponseReceived,
    };
    #[cfg(feature = "gpu-pow")]
    use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
    #[cfg(feature = "gpu-pow")]
    use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EventBindingCalled};
    #[cfg(feature = "gpu-pow")]
    #[derive(Deserialize)]
    struct Task {
        id: u64,
        salt: String,
        target: String,
    }
    // Network domain MUST be enabled or no request/response events flow.
    page.execute(chromiumoxide::cdp::browser_protocol::network::EnableParams::default())
        .await
        .context("enable network")?;
    let mut req_events = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .context("listen requests")?;
    let mut resp_events = page
        .event_listener::<EventResponseReceived>()
        .await
        .context("listen responses")?;
    let mut failed_events = page
        .event_listener::<EventLoadingFailed>()
        .await
        .context("listen loading-failed")?;
    #[cfg(feature = "gpu-pow")]
    let mut gpu_events = page.event_listener::<EventBindingCalled>().await?;
    #[cfg(feature = "gpu-pow")]
    let gpu: Option<Arc<Mutex<super::cap_pow::Solver>>> = if std::env::var("KOROMO_GPU_POW").as_deref() == Ok("1") {
        static GPU: tokio::sync::OnceCell<Arc<Mutex<super::cap_pow::Solver>>> =
            tokio::sync::OnceCell::const_new();
        let solver = GPU
            .get_or_try_init(|| async {
                super::cap_pow::Solver::new()
                    .await
                    .map(|s| Arc::new(Mutex::new(s)))
            })
            .await?;
        page.execute(AddBindingParams::new("__koromoGpu")).await?;
        page.execute(AddScriptToEvaluateOnNewDocumentParams::new(include_str!(
            "cap_pow_bridge.js"
        )))
        .await?;
        Some(Arc::clone(solver))
    } else {
        None
    };
    #[cfg(feature = "gpu-pow")]
    let mut gpu_tasks: tokio::task::JoinSet<anyhow::Result<()>> = tokio::task::JoinSet::new();
    #[cfg(not(feature = "gpu-pow"))]
    {
        if std::env::var("KOROMO_GPU_POW").as_deref() == Ok("1") {
            anyhow::bail!("KOROMO_GPU_POW=1 requires `--features gpu-pow` build (wgpu); refusing to silently fall back to CPU");
        }
    }
    // No CDP binding subscription without the feature: a pending stream keeps the
    // `tokio::select!` GPU arms below disabled (`gpu.is_some()` is always false) with
    // zero GPU side work. (`tokio::select!` branches cannot carry `#[cfg]`; the GPU
    // work inside those arms is itself `#[cfg]`-gated.)
    #[cfg(not(feature = "gpu-pow"))]
    let gpu: Option<()> = None;
    #[cfg(not(feature = "gpu-pow"))]
    let mut gpu_events = futures::stream::pending::<()>();
    #[cfg(not(feature = "gpu-pow"))]
    let mut gpu_tasks: tokio::task::JoinSet<anyhow::Result<()>> = tokio::task::JoinSet::new();
    let navigation_started = Instant::now();
    page.goto(format!("https://amae-koromo.sapk.ch/{day}/{mode}"))
        .await
        .context("navigate day page")?;
    info!(
        tab = tab_prefix,
        phase = "navigated",
        elapsed_ms = navigation_started.elapsed().as_millis(),
        "solver phase"
    );
    // In-memory request correlation, bounded per solve: only games and
    // challenge/redeem ids are tracked (page assets are counted, not stored).
    // Values carry method + phase for failure reports; never URLs or headers.
    let mut cap_requests: HashMap<String, (Instant, String, &'static str)> = HashMap::new();
    let mut challenge_received: Option<Instant> = None;

    // Games requests WITH an Authorization bearer: (bearer, mirror, method).
    let mut pending: HashMap<String, (String, String, String)> = HashMap::new();
    // Games requests WITHOUT a bearer: (method, seen-at). Small by nature
    // (the page polls `/games` a handful of times per solve); still capped.
    let mut games_bare: HashMap<String, (String, Instant)> = HashMap::new();
    // Per-solve outcome counters for the timeout context + failure reports.
    let mut games_seen = 0u64;
    let mut games200_matched = 0u64;
    let mut games200_unmatched = 0u64;
    let mut games_non200 = 0u64;
    let mut games_failed = 0u64;
    let mut cap_failed = 0u64;
    let mut other_failed = 0u64;
    let mut last_games_status: Option<i64> = None;
    let mut last_cap: Option<(&'static str, i64)> = None;
    let mut last_net_err: Option<&'static str> = None;
    // 45s steal budget: healthy solves land in ~3s; hung tabs must not park workers.
    // Timeout context: last observed phases/counts only, never URLs/headers.
    // (Inlined at both bails: a closure would hold shared borrows while the
    // loop below mutates the same counters.)
    let deadline = Instant::now() + Duration::from_secs(45);
    let result = async {
        loop {
            let now = Instant::now();
            if now > deadline {
                let ctx = format!("games_seen={games_seen} matched={games200_matched} unmatched={games200_unmatched} non200={games_non200} games_failed={games_failed} cap_failed={cap_failed} other_failed={other_failed} last_games={last_games_status:?} last_cap={last_cap:?} last_net_err={last_net_err:?}");
                anyhow::bail!("no 200-bearer within 45s ({ctx})");
            }
            let remaining = deadline - now;
            tokio::select! {
                biased;
                () = tokio::time::sleep(remaining) => {
                    let ctx = format!("games_seen={games_seen} matched={games200_matched} unmatched={games200_unmatched} non200={games_non200} games_failed={games_failed} cap_failed={cap_failed} other_failed={other_failed} last_games={last_games_status:?} last_cap={last_cap:?} last_net_err={last_net_err:?}");
                    anyhow::bail!("no 200-bearer within 45s ({ctx})");
                }
                Some(joined) = gpu_tasks.join_next(), if !gpu_tasks.is_empty() => {
                    joined??;
                }
                ev = gpu_events.next(), if gpu.is_some() => {
                    #[cfg(feature = "gpu-pow")]
                    {
                        let Some(ev) = ev else { anyhow::bail!("GPU binding stream ended") };
                        if ev.name != "__koromoGpu" { continue; }
                        let task: Task = serde_json::from_str(&ev.payload)?;
                        let solver = Arc::clone(gpu.as_ref().context("GPU binding without solver")?);
                        let page = page.clone();
                        gpu_tasks.spawn(async move {
                            let id = task.id;
                            let started = Instant::now();
                            // Await the GPU permit before entering the blocking pool:
                            // cancelled tabs must not leave dozens of queued searches.
                            let solver = solver.lock_owned().await;
                            let result = tokio::task::spawn_blocking(move || {
                                solver.solve(&task.salt, &task.target)
                            }).await?;
                            let nonce = match result {
                                Ok(n) => { info!(phase = "gpu_pow", elapsed_ms = started.elapsed().as_millis(), "solver phase"); n.to_string() }
                                Err(e) => { warn!("GPU PoW failed; retaining widget CPU solver: {e}"); "null".to_owned() }
                            };
                            page.evaluate(format!("globalThis.__koromoGpuReply({id},{nonce})")).await?;
                            Ok::<(), anyhow::Error>(())
                        });
                    }
                    #[cfg(not(feature = "gpu-pow"))]
                    {
                        let _ = ev;
                        anyhow::bail!("GPU binding stream ended");
                    }
                }
                ev = req_events.next() => {
                    let Some(ev) = ev else { anyhow::bail!("request stream ended") };
                    let url = &ev.request.url;
                    // Method is a small bounded token (GET/POST/...); safe to keep.
                    let method = ev.request.method.clone();
                    if let Ok(parsed) = reqwest::Url::parse(url) {
                        let path = parsed.path();
                        if path.ends_with("/challenge") || path.ends_with("/redeem") {
                            let cap_phase: &'static str = if path.ends_with("/redeem") { "redeem" } else { "challenge" };
                            if cap_requests.len() < 64 {
                                cap_requests.insert(ev.request_id.inner().clone(), (Instant::now(), method.clone(), cap_phase));
                            }
                            if path.ends_with("/redeem") {
                                if let Some(start) = challenge_received.take() {
                                    info!(tab = tab_prefix, phase = "challenge_to_redeem", elapsed_ms = start.elapsed().as_millis(), "solver phase");
                                }
                            }
                        }
                    }
                    if url.contains("data.amae-koromo.com") && url.contains("/games") {
                        games_seen += 1;
                        let headers = ev.request.headers.inner();
                        let auth = headers
                            .get("Authorization")
                            .or_else(|| headers.get("authorization"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_owned();
                        if auth.contains("Bearer") {
                            let mirror = url.split("/api/").next().unwrap_or("").to_owned() + "/";
                            if pending.len() < 256 {
                                pending.insert(ev.request_id.inner().clone(), (auth, mirror, method));
                            }
                        } else if games_bare.len() < 256 {
                            games_bare.insert(ev.request_id.inner().clone(), (method, Instant::now()));
                        }
                    }
                }
                ev = resp_events.next() => {
                    let Some(ev) = ev else { anyhow::bail!("response stream ended") };
                    let url = &ev.response.url;
                    // Log phases only, never token-bearing paths or payloads.
                    if let Ok(parsed) = reqwest::Url::parse(url) {
                        let phase = [("/challenge", "challenge"), ("/redeem", "redeem")]
                            .into_iter().find_map(|(suffix, phase)| parsed.path().ends_with(suffix).then_some(phase));
                        if let Some(phase) = phase {
                            if phase == "challenge" && ev.response.status == 200 {
                                challenge_received = Some(Instant::now());
                            }
                            last_cap = Some((phase, ev.response.status));
                            let elapsed_ms = cap_requests.remove(ev.request_id.inner())
                                .map(|(start, _, _)| start.elapsed().as_millis());
                            info!(tab = tab_prefix, phase, status = ev.response.status, elapsed_ms = ?elapsed_ms, "solver phase");
                            // A rejected challenge/redeem cannot produce a bearer
                            // in this attempt. Release the Chrome slot immediately:
                            // waiting 45s after a 429 creates fleet-wide queue waves.
                            // Producers back off before retrying; browsers stay live
                            // so existing proxy exit rotation can recover them.
                            if ev.response.status == 403 {
                                anyhow::bail!("{phase} blocked (403): solver exit cannot mint here");
                            }
                            if ev.response.status == 429 {
                                anyhow::bail!("{phase} rate-limited (429): release solver slot");
                            }
                        }
                    }
                    let status = ev.response.status;
                    if url.contains("data.amae-koromo.com") && url.contains("/games") {
                        info!("{tab_prefix}: games -> {status}");
                        last_games_status = Some(status);
                        let cached = ev.response.from_disk_cache.unwrap_or(false);
                        let service_worker = ev.response.from_service_worker.unwrap_or(false);
                        if status == 200 {
                            if let Some((auth, mirror, _method)) = pending.remove(ev.request_id.inner()) {
                                games200_matched += 1;
                                games_bare.remove(ev.request_id.inner());
                                info!(tab = tab_prefix, phase = "accepted", elapsed_ms = navigation_started.elapsed().as_millis(), cached, service_worker, "solver phase");
                                return Ok((auth, mirror));
                            }
                            games200_unmatched += 1;
                            games_bare.remove(ev.request_id.inner());
                            debug!(tab = tab_prefix, phase = "games-unmatched-200", cached, service_worker, "solver phase");
                        } else {
                            games_non200 += 1;
                            games_bare.remove(ev.request_id.inner());
                            debug!(tab = tab_prefix, phase = "games-response", status, cached, service_worker, "solver phase");
                        }
                    }
                }
                ev = failed_events.next() => {
                    let Some(ev) = ev else { anyhow::bail!("loading-failed stream ended") };
                    let id = ev.request_id.inner();
                    let resource = ev.r#type.as_ref();
                    // Enum category only: the raw error text may embed
                    // request-specific text and is never logged.
                    let err_cat = net_err_category(&ev.error_text);
                    last_net_err = Some(err_cat);
                    let canceled = ev.canceled.unwrap_or(false);
                    let blocked = ev.blocked_reason.as_ref();
                    if let Some((_auth, _mirror, method)) = pending.remove(id) {
                        games_failed += 1;
                        games_bare.remove(id);
                        warn!(tab = tab_prefix, phase = "games-failed", err = err_cat, resource, method, auth_present = true, canceled, blocked = ?blocked, "solver phase");
                    } else if let Some((method, _)) = games_bare.remove(id) {
                        games_failed += 1;
                        warn!(tab = tab_prefix, phase = "games-failed", err = err_cat, resource, method, auth_present = false, canceled, blocked = ?blocked, "solver phase");
                    } else if let Some((_, method, cap_phase)) = cap_requests.remove(id) {
                        cap_failed += 1;
                        warn!(tab = tab_prefix, phase = "cap-failed", cap_phase, err = err_cat, resource, method, canceled, blocked = ?blocked, "solver phase");
                    } else {
                        other_failed += 1;
                    }
                }
            }
        }
    }
    .await;
    result
}

/// Fetch one `/games` chunk. Exact request shape the site itself uses.
pub async fn fetch_chunk(
    client: &reqwest::Client,
    mirror: &str,
    cursor_ms: i64,
    floor_ms: i64,
    mode: i32,
    auth: &str,
) -> Result<Vec<GameRecord>> {
    let started = Instant::now();
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ts = chrono::Utc::now().timestamp_millis();
    let url = format!(
        "{mirror}api/v2/pl4/games/{cursor_ms}/{floor_ms}?limit=500&descending=true&mode={mode}&cap_token_refreshed={ts}"
    );
    let resp = match client
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
        )
        .header("Referer", "https://amae-koromo.sapk.ch/")
        .header("Authorization", auth)
        .header("Cache-Control", "no-cache")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            games_chunk_stats().record(elapsed_ms(), 0, PageCategory::NetErr);
            debug!(kind = "games", category = "net-err", elapsed_ms = elapsed_ms(), "page fetch");
            return Err(e.without_url()).context("games chunk GET");
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let hint = rate_hint(&resp);
        let body = resp.text().await.unwrap_or_default();
        let token_path = classify_429(&body) == LimitKind::Token;
        let detail = sanitize_429_detail(&body, token_path);
        let suffix = rate_hint_suffix(&hint);
        note_rate_hint(&hint);
        let ms = elapsed_ms();
        if token_path {
            games_chunk_stats().record(ms, 0, PageCategory::Token429);
            debug!(kind = "games", category = "token-429", elapsed_ms = ms, retry_after_s = ?hint.retry_after_s, limit = ?hint.limit, remaining = ?hint.remaining, reset = ?hint.reset, "page fetch");
            anyhow::bail!("token-429: {detail}{suffix}");
        }
        games_chunk_stats().record(ms, 0, PageCategory::Ip429);
        debug!(kind = "games", category = "ip-429", elapsed_ms = ms, retry_after_s = ?hint.retry_after_s, limit = ?hint.limit, remaining = ?hint.remaining, reset = ?hint.reset, "page fetch");
        anyhow::bail!("ip-429: {detail}{suffix}");
    }
    if !status.is_success() {
        let code = status.as_u16();
        let ms = elapsed_ms();
        games_chunk_stats().record(ms, 0, PageCategory::HttpErr);
        debug!(kind = "games", category = "http-err", status = code, elapsed_ms = ms, "page fetch");
        anyhow::bail!("HTTP {status}");
    }
    match resp.json::<Vec<GameRecord>>().await {
        Ok(games) => {
            let ms = elapsed_ms();
            let rows = games.len() as u64;
            games_chunk_stats().record(ms, rows, PageCategory::Ok);
            debug!(kind = "games", category = "ok", rows, elapsed_ms = ms, "page fetch");
            maybe_log_fetch_interval();
            Ok(games)
        }
        Err(e) => {
            let ms = elapsed_ms();
            games_chunk_stats().record(ms, 0, PageCategory::DecodeErr);
            debug!(kind = "games", category = "decode-err", elapsed_ms = ms, "page fetch");
            Err(e.without_url().into())
        }
    }
}

/// Drain one hour window. Returns (games fetched, parked?).
/// Never silently drops: repeated token failure parks the hour for retry.
#[allow(clippy::too_many_arguments)]
pub async fn drain_hour(
    client: &reqwest::Client,
    pool: &AuthPool,
    seen: &Mutex<HashSet<String>>,
    dbx: &Mutex<HarvestDb>,
    day: &str,
    mode: i32,
    h: u32,
    h_start: i64,
    h_end: i64,
    out: &mut Vec<StampedRecord>,
) -> Result<(usize, bool)> {
    // Resume from a parked cursor when this hour was interrupted mid-drain.
    let mut cursor = dbx.lock().await.load_cursor(day, mode, h).unwrap_or(None).unwrap_or(h_end);
    // Skip-seen gate: per-HOUR deep state. Only break on all-seen chunks when
    // new code fully drained THIS hour before (legacy runs truncated days, so
    // day-level state would bake their holes in).
    let deep = dbx.lock().await.is_deep(day, mode, h);
    let mut fetched = 0usize;
    let mut fails = 0u32;
    loop {
        let prev = cursor;
        let identity = format!("hour-{day}-{mode}-{h}");
        let (tab, auth, mirror) = if pool.brokered {
            pool.acquire(&identity, day, mode).await?
        } else {
            match pool.take().await {
                Some(entry) => entry,
                None => pool.ensure(&identity, day, mode).await?,
            }
        };
        let chunk = match fetch_chunk(client, &mirror, cursor, h_start, mode, &auth).await {
            Ok(c) => {
                fails = 0;
                pool.give((tab, auth, mirror)).await;
                c
            }
            Err(e) if e.to_string().starts_with("token-429") => {
                // Dead token stays out of the pool; solve a fresh one for that tab.
                pool.drop_tab(&tab).await;
                if pool.len().await < 2 {
                    warn!("token pool low after drop; solves racing demand");
                }
                if !pool.brokered {
                    if let Ok(entry) = pool.ensure(&tab, day, mode).await {
                        pool.give(entry).await;
                    }
                }
                fails += 1;
                if fails > 8 {
                    warn!("{day}/{mode} h{h}: 8 token fails, parking");
                    return Ok((fetched, true));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(e) if e.to_string().starts_with("ip-429") => {
                pool.give((tab, auth, mirror)).await;
                warn!("{day}/{mode} h{h}: IP limit, backoff 60s");
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
            Err(e) => {
                pool.give((tab, auth, mirror)).await;
                return Err(e);
            }
        };
        if chunk.is_empty() {
            break;
        }
        fetched += chunk.len();
        // Skip-seen fast path: server returns newest-first, so a chunk that is
        // 100% already-seen means everything older in this hour is seen too.
        // Saves ~10x drain cost on already-covered ranges (no extra requests).
        let fresh = {
            let seen = seen.lock().await;
            chunk.iter().filter(|g| !seen.contains(&g.uuid)).count()
        };
        if fresh == 0 && deep {
            dbx.lock().await.clear_cursor(day, mode, h).ok();
            break;
        }
        // Oldest BEFORE pushing (borrow-safe): min startTime of this chunk.
        let oldest = chunk.iter().map(|g| g.start_time).min().unwrap_or(0);
        // Skip-seen fast path: server returns DESCENDING (newest first). A chunk
        // that is 100% already-seen means everything older is seen too — break
        // early instead of draining known history one chunk at a time.
        // GATE: only when this day/mode was fully drained by new code before
        // (`deep` flag). Legacy v1/v2 runs truncated days at 150 rows, so their
        // ranges contain unseen holes older than seen rows — must drain fully.
        let mut new_here = 0usize;
        {
            let mut seen = seen.lock().await;
            for g in chunk {
                if seen.insert(g.uuid.clone()) {
                    new_here += 1;
                    out.push(StampedRecord::stamp(day.to_owned(), mode, g));
                }
            }
        }
        if new_here == 0 && deep {
            break;
        }
        // Overlap into the window (server floors/ceils to seconds); seen-set dedups.
        cursor = oldest * 1000;
        // Persisting a cursor before journal commit can skip unwritten games.
        // Until the writer commits the hour, interruption safely replays it.
        if cursor <= h_start + MIN_RANGE_MS {
            break;
        }
        if cursor >= prev {
            warn!("{day}/{mode} h{h}: cursor stall, parking for retry");
            return Ok((fetched, true));
        }
    }
    dbx.lock().await.clear_cursor(day, mode, h).ok();
    Ok((fetched, false))
}

/// CLI config for `majsoul harvest`.
#[derive(Debug, Clone)]
pub struct HarvestConfig {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub modes: Vec<i32>,
    pub journal: PathBuf,
    pub db_path: PathBuf,
    pub workers: usize,
    pub cdp_url: String,
    pub pool_file: Option<PathBuf>,
    pub seed_files: Vec<PathBuf>,
    /// Door proxies for fetch rotation (one lane per door + direct).
    pub doors: Vec<String>,
}

impl HarvestConfig {
    /// Hour jobs for the whole range: (`day`, `mode`, `hour`, `day_start_ms`).
    pub fn hour_jobs(&self) -> Result<Vec<(String, i32, u32, i64)>> {
        let mut jobs = Vec::new();
        let mut d = self.start;
        while d <= self.end {
            let ds = jst_day_start_ms(d)?;
            for mode in &self.modes {
                for h in 0..24u32 {
                    jobs.push((d.format("%Y-%m-%d").to_string(), *mode, h, ds));
                }
            }
            d = d.succ_opt().context("date overflow")?;
        }
        Ok(jobs)
    }
}

/// Spawned support tasks must not outlive a cancelled harvest pass.
struct HarvestTaskGuard(Vec<tokio::task::AbortHandle>);

impl Drop for HarvestTaskGuard {
    fn drop(&mut self) {
        for task in &self.0 { task.abort(); }
    }
}

enum HarvestWrite {
    Record(StampedRecord),
    Hour(String, i32, u32, bool),
}

/// Run the harvest: hour-job queue + worker tasks sharing one pool/client.
/// Each worker drains whole hours via `drain_hour`; journal appends under a
/// mutex; done/parked checkpoint to SQLite at day completion.
/// Worker fan-out is one verified pipeline; splitting churns spawn sites.
#[allow(clippy::too_many_lines)]
pub async fn run_harvest(cfg: HarvestConfig) -> Result<usize> {
    let pool = Arc::new(AuthPool::new(cfg.cdp_url.clone()));
    run_harvest_with_pool(cfg, pool).await
}

/// Shared-service harvest: the caller owns authentication production.
#[allow(clippy::too_many_lines)]
pub async fn run_harvest_with_pool(cfg: HarvestConfig, pool: Arc<AuthPool>) -> Result<usize> {
    let jobs = cfg.hour_jobs()?;
    let jobs = filter_done(&cfg.db_path, jobs)?;
    if jobs.is_empty() { return Ok(0); }
    info!(
        "harvest: {} hour-jobs over {} days, modes {:?}, {} workers",
        jobs.len(),
        (cfg.end - cfg.start).num_days() + 1,
        cfg.modes,
        cfg.workers
    );
    let db = HarvestDb::open(&cfg.db_path)?;
    for seed in &cfg.seed_files {
        let n = db.seed_from_jsonl(seed);
        if n > 0 {
            info!("seeded {n} uuids from {}", seed.display());
        }
    }
    // Restart-safe: the journal itself is the primary resume source.
    // (Old runs never INSERTed into SQLite, so the journal holds the truth.)
    let n = db.seed_from_jsonl(&cfg.journal);
    if n > 0 {
        info!(
            "seeded {n} uuids from own journal {}",
            cfg.journal.display()
        );
    }
    // Finish what in-memory counting cannot: days fully deep-drained across
    // restarts are done NOW (deep hours filter at startup, so no single run
    // ever counts all 24). Unblocks completion guards on finite backfills.
    let mut days = Vec::new();
    let mut d = cfg.start;
    while d <= cfg.end {
        days.push(d.format("%Y-%m-%d").to_string());
        d = d.succ_opt().context("date overflow")?;
    }
    let reconciled = db.mark_done_where_all_deep(&days, &cfg.modes);
    if reconciled > 0 {
        info!("reconciled {reconciled} fully-drained days -> done");
    }
    let seen = Arc::new(Mutex::new(db.load_seen()?));
    info!("resume: {} banked uuids", seen.lock().await.len());
    // Shared checkpoint connection; unfinished hours replay until journal commit.
    let dbx = Arc::new(Mutex::new(db));

    // Fetch client pool: direct + one per door proxy. Workers rotate by index,
    // spreading per-IP fetch budget (the wall when solves are healthy).
    // NOTE: plain-http absolute-URI forwarding works through doors; https uses CONNECT.
    let mut clients = vec![Arc::new(crate::util::http_client()?)];
    for d in &cfg.doors {
        match reqwest::Proxy::all(d) {
            Ok(px) => match reqwest::Client::builder()
                .proxy(px)
                .timeout(Duration::from_secs(30))
                .build()
            {
                Ok(c) => clients.push(Arc::new(c)),
                Err(e) => warn!("door client {d} failed: {e:#}"),
            },
            Err(e) => warn!("door proxy {d} invalid: {e:#}"),
        }
    }
    info!(
        "fetch clients: {} lanes (direct + {} doors)",
        clients.len(),
        cfg.doors.len()
    );
    let clients = Arc::new(clients);
    if let Some(pf) = &cfg.pool_file {
        let n = pool.load_file(pf).await?;
        info!("pool: {n} banked tokens");
    }
    // Token pre-warmer: keep the pool topped up in the background so workers
    // never block on on-demand solves. Buffer scales with the solver fleet
    // (one round + margin), paced by ensure()'s per-solver limits.
    // Aborted at run end.
    let warm_pool = Arc::clone(&pool);
    let warm_depth = warm_pool.solver_count().saturating_mul(4).max(8);
    let warmer = if pool.brokered { None } else { Some(tokio::spawn(async move {
        // One producer per solver prevents a slow Chrome from stalling the
        // entire fleet. JoinSet owns children so aborting the warmer stops all.
        let mut producers = tokio::task::JoinSet::new();
        for producer in 0..warm_pool.solver_count().max(1) {
            let warm_pool = Arc::clone(&warm_pool);
            producers.spawn(async move {
                let mut n = 0u64;
                loop {
                    if warm_pool.len().await < warm_depth {
                        n += 1;
                        let tag = format!("prewarm-{producer}-{n}");
                        match warm_pool.ensure(&tag, "2025-06-01", 16).await {
                            Ok(entry) => warm_pool.give(entry).await,
                            Err(_) => tokio::time::sleep(Duration::from_secs(30)).await,
                        }
                    } else {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                }
            });
        }
        while producers.join_next().await.is_some() {}
    })) };

    // Shared job channel: Mutex<Receiver> hold is microseconds per recv vs
    // second/minute drains — contention is imaginary. Static stripes reintroduce
    // stragglers (slow hours pin one worker); shared queue work-steals instead.
    let (job_tx, job_rx) = tokio::sync::mpsc::channel::<(String, i32, u32, i64)>(4096);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (rec_tx, mut rec_rx) = tokio::sync::mpsc::channel::<HarvestWrite>(4096);
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<(String, i32, u32, bool)>(1024);
    let sem = Arc::new(Semaphore::new(cfg.workers));
    // Checkpoint task: owns a HarvestDb connection, marks parked hours immediately
    // and day/modes done once all 24 hours complete. Survives restarts via SQLite.
    let db_path_ckpt = cfg.db_path.clone();
    let checkpoint = tokio::spawn(async move {
        let dbc = match HarvestDb::open(&db_path_ckpt) {
            Ok(d) => d,
            Err(e) => {
                warn!("checkpoint db open failed: {e:#}");
                return;
            }
        };
        let mut hours_done: std::collections::HashMap<(String, i32), u32> =
            std::collections::HashMap::new();
        while let Some((day, mode, h, parked)) = done_rx.recv().await {
            if parked {
                dbc.mark_parked(&day, mode, h);
            } else {
                dbc.clear_parked(&day, mode, h);
                let c = hours_done.entry((day.clone(), mode)).or_insert(0);
                *c += 1;
                if *c >= 24 {
                    dbc.mark_done(&day, mode);
                    info!("checkpoint: {day}/{mode} complete");
                }
            }
        }
    });

    // Journal writer: single task owns the file handle + a HarvestDb connection.
    // Batch-INSERTs seen uuids every 500 so restarts resume from SQLite, not just the journal.
    let journal_path = cfg.journal.clone();
    let db_path_writer = cfg.db_path.clone();
    let writer_done = done_tx.clone();
    let mut writer = tokio::spawn(async move {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal_path)
            .await?;
        let dbw = HarvestDb::open(&db_path_writer)?;
        let mut n = 0usize;
        let mut pending: Vec<String> = Vec::with_capacity(500);
        while let Some(rec) = rec_rx.recv().await {
            let rec = match rec {
                HarvestWrite::Record(rec) => rec,
                HarvestWrite::Hour(day, mode, hour, parked) => {
                    f.flush().await?;
                    dbw.mark_seen(&pending);
                    pending.clear();
                    if !parked { dbw.mark_deep(&day, mode, hour); }
                    writer_done.send((day, mode, hour, parked)).await.context("harvest checkpoint receiver")?;
                    continue;
                }
            };
            let line = serde_json::to_string(&rec)? + "\n";
            f.write_all(line.as_bytes()).await?;
            pending.push(rec.game.uuid.clone());
            n += 1;
            if n.is_multiple_of(500) {
                f.flush().await?;
                dbw.mark_seen(&pending);
                pending.clear();
            }
        }
        f.flush().await?;
        if !pending.is_empty() {
            dbw.mark_seen(&pending);
        }
        Ok::<usize, anyhow::Error>(n)
    });

    // Speed reporter: rows/min + rows/sec + pool/token health every 60s.
    // (Diagnoses stalls like frozen journals with alive workers.)
    let stats_seen = Arc::clone(&seen);
    let stats_pool = Arc::clone(&pool);
    let reporter = tokio::spawn(async move {
        let t0 = Instant::now();
        let n0 = stats_seen.lock().await.len();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let n = stats_seen.lock().await.len();
            let ptokens = stats_pool.len().await;
            let dt = t0.elapsed().as_secs_f64().max(1.0);
            let fresh = f64::from(u32::try_from(n.saturating_sub(n0)).unwrap_or(u32::MAX));
            let per_min = fresh / dt * 60.0;
            info!(
                "speed: {} new uuids total, {:.1}/s overall ({:.0}/min), pool={} tokens",
                n.saturating_sub(n0),
                fresh / dt,
                per_min,
                ptokens,
            );
        }
    });

    // Feed hour jobs into the shared queue (4096 buffer absorbs bursts).
    let feeder = {
        let job_tx = job_tx.clone();
        tokio::spawn(async move {
            for j in jobs {
                if job_tx.send(j).await.is_err() {
                    break;
                }
            }
        })
    };
    let mut support = HarvestTaskGuard(vec![writer.abort_handle(), checkpoint.abort_handle(), reporter.abort_handle(), feeder.abort_handle()]);
    if let Some(warmer) = &warmer { support.0.push(warmer.abort_handle()); }
    drop(job_tx);

    // Workers: each pulls hour jobs, drains via shared pool, forwards records.
    let mut handles = tokio::task::JoinSet::new();
    for wid in 0..cfg.workers {
        let rx = Arc::clone(&job_rx);
        let tx = rec_tx.clone();
        let done_tx = done_tx.clone();
        let client = Arc::clone(&clients[wid % clients.len()]);
        let pool = Arc::clone(&pool);
        let seen = Arc::clone(&seen);
        let dbx = Arc::clone(&dbx);
        let sem = Arc::clone(&sem);
        handles.spawn(async move {
            loop {
                let job = { rx.lock().await.recv().await };
                let Some((day, mode, h, ds)) = job else { break };
                let Ok(_permit) = sem.acquire().await else {
                    break;
                };
                // Pre-check deep: covered hours skip with ZERO requests (no token burn).
                // Still report completion so day-level mark_done can fire.
                if dbx.lock().await.is_deep(&day, mode, h) {
                    let _ = done_tx.send((day.clone(), mode, h, false)).await;
                    continue;
                }
                let (h_start, h_end) = hour_window(ds, h);
                let mut out = Vec::new();
                match drain_hour(
                    &client, &pool, &seen, &dbx, &day, mode, h, h_start, h_end, &mut out,
                )
                .await
                {
                    Ok((_, parked)) => {
                        if parked {
                            warn!("{day}/{mode} h{h}: parked for retry");
                        }
                        for rec in out {
                            if tx.send(HarvestWrite::Record(rec)).await.is_err() { return; }
                        }
                        if tx.send(HarvestWrite::Hour(day.clone(), mode, h, parked)).await.is_err() { return; }
                    }
                    Err(e) => warn!("{day}/{mode} h{h}: {e:#}"),
                }
            }
        });
    }
    drop(rec_tx);
    drop(done_tx);
    // Observe disk failures while workers are active, not after their joins.
    // JoinSet aborts sibling workers on cancellation or an early writer error.
    let result = loop {
        tokio::select! {
            completed = &mut writer => break completed.context("harvest writer task")?,
            worker = handles.join_next(), if !handles.is_empty() => {
                if let Some(Err(error)) = worker { break Err(error.into()); }
            }
        }
    };
    handles.abort_all();
    while handles.join_next().await.is_some() {}
    feeder.abort();
    reporter.abort();
    if let Some(warmer) = warmer { warmer.abort(); }
    checkpoint.await.context("harvest checkpoint task")?;
    let n = result?;
    Ok(n)
}
/// Drop hour-jobs already checkpointed (restart-safe): whole done day/modes,
/// plus individually deep-drained hours. Deep hours need zero requests.
/// Parked hours are FORCE-KEPT (retry priority): they completed nothing.
fn filter_done(
    db_path: &Path,
    jobs: Vec<(String, i32, u32, i64)>,
) -> Result<Vec<(String, i32, u32, i64)>> {
    let db = HarvestDb::open(db_path)?;
    let done = db.load_done()?;
    let deep = db.load_deep()?;
    let parked = db.load_parked()?;
    if done.is_empty() && deep.is_empty() {
        return Ok(jobs);
    }
    let kept: Vec<_> = jobs
        .into_iter()
        .filter(|(day, mode, h, _)| {
            parked.contains(&(day.clone(), *mode, *h))
                || (!done.contains(&(day.clone(), *mode))
                    && !deep.contains(&(day.clone(), *mode, *h)))
        })
        .collect();
    info!(
        "checkpoint: skipped {} done day/modes + {} deep hours ({} parked retry)",
        done.len(),
        deep.len(),
        parked.len()
    );
    Ok(kept)
}

impl HarvestDb {
    /// Load all parked (day, mode, hour) triples for force-retry.
    pub fn load_parked(&self) -> Result<HashSet<(String, i32, u32)>> {
        let mut stmt = self.conn.prepare("SELECT day, mode, hour FROM parked")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i32>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }
}

impl HarvestDb {
    /// Load all deep-drained (day, mode, hour) triples for job filtering.
    pub fn load_deep(&self) -> Result<HashSet<(String, i32, u32)>> {
        let mut stmt = self.conn.prepare("SELECT day, mode, hour FROM deep")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i32>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }
}
/// Resolve one short uuid to its full paipu id via Koromo `view_game`.
/// Unauthenticated 301: `GET {mirror}api/v2/pl4/view_game/2/{mode}/{short}/{account}`
/// -> `Location: ...?paipu={FULL}`. No CAP, no token. Returns the paipu id.
/// Proven 10/10 at ~1/s sequential; bulk headroom unknown — callers pace + backoff.
/// Resolve trying every witness account (any seated player works).
pub async fn resolve_paipu_any(
    client: &reqwest::Client,
    mirror: &str,
    mode: i32,
    short_uuid: &str,
    account_ids: &[i64],
) -> Result<String> {
    // NOTE: caller must pass a no-redirect client: the 301 Location HOLDS the
    // paipu id, and following it yields a 200 HTML page with nothing to parse.
    // Mirror rotation: each host may carry its own limiter budget.
    // Stagger start host by uuid hash so N lanes don't all hammer 5-data first.
    let hosts: Vec<String> = if mirror.is_empty() {
        let off = short_uuid
            .bytes()
            .fold(0usize, |a, b| a.wrapping_add(b as usize))
            % MIRRORS.len();
        (0..MIRRORS.len())
            .map(|i| MIRRORS[(off + i) % MIRRORS.len()].to_owned())
            .collect()
    } else {
        vec![mirror.to_owned()]
    };
    let mut last_err = String::from("no mirror tried");
    let mut saw_429 = false;
    // Attempt budget: 4 witnesses x 4 zones x 3 mirrors = 48 worst case.
    // Cap at 8 rotations (own-zone + mirrors first); 429 bails immediately below.
    let mut tries = 0u32;
    for account_id in account_ids {
        // Zone per witness (record.ts getMaskedRecordLink + zone.ts getAccountZone):
        // prefix = accountId >> 23; 0-6 = CN zone 1, 7-12 = JP zone 2, 13-15 = Intl zone 3.
        // Hardcoded /2/ 404s CN witnesses and burns retries; try own zone first.
        let prefix = account_id >> 23;
        let own_zone = if (0..=6).contains(&prefix) {
            1
        } else if (13..=15).contains(&prefix) {
            3
        } else {
            2
        };
        let zones = [own_zone, 1, 2, 3];
        for host in &hosts {
            for zone in zones {
                tries += 1;
                if tries > 8 {
                    if saw_429 {
                        anyhow::bail!("429 exhausted ({last_err})");
                    }
                    anyhow::bail!("view_game: attempt budget spent ({last_err})");
                }
                let url =
                    format!("{host}api/v2/pl4/view_game/{zone}/{mode}/{short_uuid}/{account_id}");
                let resp = match client
                    .get(&url)
                    .header(
                        "User-Agent",
                        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
                    )
                    .header("Referer", "https://amae-koromo.sapk.ch/")
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = format!("GET {host}: {e:#}");
                        continue;
                    }
                };
                if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    // Rotate, don't bail: a transient 429 on one host must not forfeit
                    // the other mirrors/witnesses/zones. Remember 429 for lane backoff.
                    saw_429 = true;
                    last_err = format!("429 at {host}");
                    // Mark host burned for this uuid by skipping its remaining combos:
                    // simplest correct signal is to continue (next host/zone/witness).
                    // True per-host-twice detection lives in the lane backoff counters.
                    continue;
                }
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let paipu = loc.split("paipu=").nth(1).unwrap_or("").to_owned();
                if paipu.is_empty() {
                    last_err = format!("no paipu in redirect ({})", resp.status());
                    continue;
                }
                return Ok(paipu);
            }
        }
    }
    if saw_429 {
        anyhow::bail!("429 exhausted ({last_err})");
    }
    anyhow::bail!("view_game: {last_err}")
}

/// Koromo data mirrors (`api.ts` `DATA_MIRRORS` + `pl4` suffix). Limiter may be per-host:
/// rotate to spread budget. 2.data has no DNS; keep the three live ones.
pub const MIRRORS: [&str; 3] = [
    "https://5-data.amae-koromo.com/",
    "https://1.data.amae-koromo.com/",
    "https://4.data.amae-koromo.com/",
];

/// Bulk resolver: short records -> paipu ids, concurrent workers, paced.
/// Input: journal lines with `uuid` + `players[].accountId` (+ `day`/`mode` stamps).
/// Output: appended JSONL `{uuid, paipu, day, mode}`. Resume: skips uuids in `done_set`.
/// Rate discipline: semaphore-bounded concurrency + 429 backoff (unknown limiter —
/// ramp 100 -> 1k before blasting; this fn is the throttle point).
/// Build a `reqwest` client routing through one `OxAlpha` door (`HTTP CONNECT` proxy).
/// Each door = distinct Mullvad exit IP = independent `view_game` budget.
/// `None` proxy = direct (home IP lane).
pub fn door_client(proxy: Option<&str>) -> Result<reqwest::Client> {
    // 30s total timeout like the main client: a hung Tor stall must fail the
    // request, not wedge a lane worker forever.
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
    if let Some(p) = proxy {
        b = b.proxy(reqwest::Proxy::all(p).context("door proxy")?);
    }
    b.build().context("door client")
}

/// Multi-lane resolver: split batch across doors (home + Mullvad exits),
/// one worker stream per lane. Each lane paces itself; budgets are per-IP.
/// `doors`: e.g. `[None, Some("http://127.0.0.1:9181"), ...]` — lane per entry.
/// Returns merged (paipu, uuid, day, mode) rows.
// Long by design: lane spawn + stream fan-out + adaptive state in one verified shape.
#[allow(clippy::too_many_lines)]
pub async fn resolve_multi_lane(
    doors: &[Option<String>],
    cfg: &ResolveConfig,
    batch: Vec<ShortRecord>,
) -> Vec<(String, String, String, i32)> {
    if doors.is_empty() {
        return Vec::new();
    }
    // GLOBAL work-stealing queue: all lanes pull from one deque, so fast lanes
    // eat more and no straggler holds a pre-striped slice hostage.
    // Home lane (index 0, direct) + every door proxy lane after it.
    let queue: std::sync::Arc<tokio::sync::Mutex<std::collections::VecDeque<ShortRecord>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(batch.into()));
    // Per-lane adaptive state: (delay_ms, clean_streak, ok_total, err_total).
    // Healthy lanes (high ok rate) get picked first; flagged lanes starve out.
    let mut lane_state = Vec::new();
    for _ in 0..doors.len().max(1) {
        lane_state.push(std::sync::Arc::new(tokio::sync::Mutex::new((
            cfg.delay_ms.max(250),
            0u32,
            0u64,
            0u64,
        ))));
    }
    // GLOBAL attempts map: poison uuids park across ALL lanes, not per-lane.
    let attempts: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, u32>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    // Streams scale with workers (fixed 40 default): 40 workers / 9 lanes
    // = 4 streams per lane; lean 2 workers / 1 lane stays 2. Clamped so
    // a huge worker count can't melt a single door IP (backoff governs).
    // Computed here (sync context): lane tasks are 'static futures and
    // can't borrow cfg/doors.
    let streams_per_lane = (cfg.workers / doors.len().max(1)).clamp(1, 8);
    let mut handles = Vec::new();
    for (lane, proxy) in doors.iter().enumerate() {
        let proxy = proxy.clone();
        let attempts = std::sync::Arc::clone(&attempts);
        // Empty mirror = rotate all three hosts per request; adaptive pacing lives
        // in shared lane_state (delay, clean). Global queue => no straggler slices.
        let shared = std::sync::Arc::clone(&lane_state[lane]);
        let queue = std::sync::Arc::clone(&queue);
        let attempts = std::sync::Arc::clone(&attempts);
        handles.push(tokio::spawn(async move {
            // Stagger INSIDE the task: all lanes spawn immediately, first request
            // per lane is delayed 2s apiece (lane8 awake in 16s, not 120s).
            // view_game 301s are light + park-and-continue/backoff governs bursts.
            tokio::time::sleep(Duration::from_secs(u64::try_from(lane).unwrap_or(0) * 2)).await;
            let client = match door_client(proxy.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    warn!("lane{lane}: bad door proxy, skipping lane: {e:#}");
                    return Vec::new();
                }
            };
            // Park-and-continue: 429s park the uuid instantly (no 60s stall);
            // after the queue drains, one cooldown then a retry pass over parked.
            // Lanes never idle on throttle — they flow on fresh uuids.
            // streams_per_lane scales with --workers (Copy usize, moved in).
            let mut streams = Vec::new();
            for _ in 0..streams_per_lane {
                let client = client.clone();
                let shared = std::sync::Arc::clone(&shared);
                let queue = std::sync::Arc::clone(&queue);
                let attempts = std::sync::Arc::clone(&attempts);
                streams.push(tokio::spawn(async move {
                    let mut out = Vec::new();
                    loop {
                        let rec = { queue.lock().await.pop_front() };
                        let Some(rec) = rec else { break };
                        let delay = { shared.lock().await.0 };
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        match resolve_paipu_any(&client, "", rec.mode, &rec.uuid, &rec.account_ids)
                            .await
                        {
                            Ok(paipu) => {
                                out.push((paipu, rec.uuid.clone(), rec.day.clone(), rec.mode));
                                let mut s = shared.lock().await;
                                s.2 += 1;
                                s.1 += 1;
                                if s.1 >= 20 && s.0 > 250 {
                                    s.1 = 0;
                                    s.0 = (s.0 / 2).max(250);
                                }
                            }
                            Err(e) if e.to_string().contains("429") => {
                                // Requeue BACK with cap: poison uuids park instead of spinning.
                                let n = {
                                    let mut a = attempts.lock().await;
                                    let c = a.entry(rec.uuid.clone()).or_insert(0);
                                    *c += 1;
                                    *c
                                };
                                if n > 10 {
                                    warn!("resolve {}: 10x429, parking for retry pass", rec.uuid);
                                    continue;
                                }
                                queue.lock().await.push_back(rec.clone());
                                let mut s = shared.lock().await;
                                s.3 += 1;
                                s.0 = (s.0 * 2).min(30_000);
                                s.1 = 0;
                                warn!("lane{lane}: 429, requeued {}, delay -> {}ms", rec.uuid, s.0);
                                drop(s);
                                // No 60s sleep: delay doubling already throttles, and
                                // sleeping parks the stream while fresh uuids wait.
                            }
                            Err(e) => {
                                warn!("resolve {}: {e:#}", rec.uuid);
                            }
                        }
                    }
                    out
                }));
            }
            let mut out = Vec::new();
            for s in streams {
                if let Ok(rows) = s.await {
                    out.extend(rows);
                }
            }
            out
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        if let Ok(rows) = h.await {
            out.extend(rows);
        }
    }
    out
}

/// Player-history bulk leg: ONE `player_records` call returns up to 500 games
/// WITH full uuids directly (main.rs:959 pattern), vs 500 `view_game` calls.
/// Same CAP bearer auth as `/games` (fleet steals landing 200s). This is the
/// 100x request-count win: unique players << total games.
/// Endpoint: `{mirror}api/v2/pl4/player_records/{pid}/{end_ms}/{start_ms}?mode={m}&limit=500&descending=true`
/// Fetch ONE `player_records` page (caller drives the cursor).
async fn fetch_player_page(
    client: &reqwest::Client,
    mirror: &str,
    player_id: i64,
    mode: i32,
    end_ms: i64,
    auth: &str,
) -> Result<Vec<GameRecord>> {
    // Earliest collection: 2019-08-23 Jade/Throne South (site front page).
    let started = Instant::now();
    let elapsed_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let url = format!(
        "{mirror}api/v2/pl4/player_records/{player_id}/{end_ms}/1262304000000?mode={mode}&limit=500&descending=true"
    );
    let resp = match client
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
        )
        .header("Referer", "https://amae-koromo.sapk.ch/")
        .header("Authorization", auth)
        .header("Cache-Control", "no-cache")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            player_page_stats().record(elapsed_ms(), 0, PageCategory::NetErr);
            debug!(kind = "player", category = "net-err", elapsed_ms = elapsed_ms(), "page fetch");
            return Err(e.without_url()).context("player_records GET");
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let hint = rate_hint(&resp);
        let body = resp.text().await.unwrap_or_default();
        let token_path = classify_429(&body) == LimitKind::Token;
        let detail = sanitize_429_detail(&body, token_path);
        let suffix = rate_hint_suffix(&hint);
        note_rate_hint(&hint);
        let ms = elapsed_ms();
        if token_path {
            player_page_stats().record(ms, 0, PageCategory::Token429);
            debug!(kind = "player", category = "token-429", elapsed_ms = ms, retry_after_s = ?hint.retry_after_s, limit = ?hint.limit, remaining = ?hint.remaining, reset = ?hint.reset, "page fetch");
            anyhow::bail!("token-429: {detail}{suffix}");
        }
        player_page_stats().record(ms, 0, PageCategory::Ip429);
        debug!(kind = "player", category = "ip-429", elapsed_ms = ms, retry_after_s = ?hint.retry_after_s, limit = ?hint.limit, remaining = ?hint.remaining, reset = ?hint.reset, "page fetch");
        anyhow::bail!("ip-429: {detail}{suffix}");
    }
    if !status.is_success() {
        let code = status.as_u16();
        let ms = elapsed_ms();
        player_page_stats().record(ms, 0, PageCategory::HttpErr);
        debug!(kind = "player", category = "http-err", status = code, elapsed_ms = ms, "page fetch");
        anyhow::bail!("HTTP {status}");
    }
    match resp.json::<Vec<GameRecord>>().await {
        Ok(games) => {
            let ms = elapsed_ms();
            let rows = games.len() as u64;
            player_page_stats().record(ms, rows, PageCategory::Ok);
            debug!(kind = "player", category = "ok", rows, elapsed_ms = ms, "page fetch");
            Ok(games)
        }
        Err(e) => {
            let ms = elapsed_ms();
            player_page_stats().record(ms, 0, PageCategory::DecodeErr);
            debug!(kind = "player", category = "decode-err", elapsed_ms = ms, "page fetch");
            Err(e.without_url().into())
        }
    }
}

/// Shared fetch context: connection + token pool + solve identity.
/// Bundled so `fetch_player_records` stays under the arg limit.
pub struct FetchCtx<'a> {
    pub client: &'a reqwest::Client,
    pub pool: &'a AuthPool,
    pub tab: &'a str,
    pub day: &'a str,
}

// Long by design: the page/refresh/completion branches share one token walk
// whose give-back policy must stay identical on every exit; splitting risks
// leaking or double-returning the bearer on one path.
#[allow(clippy::too_many_lines)]
pub async fn fetch_player_records(
    ctx: &FetchCtx<'_>,
    mirror: &str,
    player_id: i64,
    mode: i32,
    auth: &str,
    start_cursor: Option<i64>,
) -> Result<(Vec<GameRecord>, bool)> {
    // The site pages by startTime and finishes only on an empty response.
    // A short rendered page is not proof that the player index is exhausted.
    // Token economy: ONE token walks all pages; a fresh solve is minted ONLY
    // when the current token 429s mid-pagination (budgets die after 2-3
    // replays). The old shape minted a solve PER PAGE — 3x burn on veterans
    // and guaranteed truncation during solve droughts.
    // Returns (games, complete): partial pages still bank (never discard —
    // veterans are the core of Jade history); callers requeue incompletes at a
    // resume cursor derived from banked rows (no dupe re-banks).
    // Own the bearer through the walk. Return only the survivor on clean
    // completion; rejected originals and failed refreshes never reenter pool.
    let mut all = Vec::new();
    let mut end_ms = start_cursor.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let mut cur_auth = auth.to_owned();
    let mut cur_mirror = mirror.to_owned();
    let mut cur_tab = ctx.tab.to_owned();
    let walk_started = Instant::now();
    let walk_elapsed_ms = || u64::try_from(walk_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut pages = 0u64;
    let mut refresh_ok = 0u64;
    let mut refresh_fail = 0u64;
    let mut token_wait_ms = 0u64;
    let mut req_ms = 0u64;
    loop {
        let req_started = Instant::now();
        let first = fetch_player_page(
            ctx.client,
            &cur_mirror,
            player_id,
            mode,
            end_ms,
            &cur_auth,
        )
        .await;
        req_ms = req_ms.saturating_add(u64::try_from(req_started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let page = match first
        {
            Ok(p) => {
                pages += 1;
                p
            }
            Err(e) if e.to_string().contains("token-429") => {
                // Consume an already minted idle token before queueing browser
                // work. Prewarmers otherwise fill the pool while active walks
                // wait for redundant fresh solves behind the same Chrome locks.
                let wait_started = Instant::now();
                let refreshed = if ctx.pool.brokered {
                    ctx.pool.acquire(ctx.tab, ctx.day, mode).await
                } else {
                    match ctx.pool.take().await {
                        Some(entry) => Ok(entry),
                        None => ctx.pool.ensure(ctx.tab, ctx.day, mode).await,
                    }
                };
                token_wait_ms = token_wait_ms.saturating_add(u64::try_from(wait_started.elapsed().as_millis()).unwrap_or(u64::MAX));
                match refreshed {
                    Ok((tab, a, m)) => {
                        refresh_ok += 1;
                        cur_tab = tab;
                        cur_auth = a;
                        cur_mirror = m;
                    }
                    Err(e2) => {
                        refresh_fail += 1;
                        debug!(
                            "player {player_id}/{mode}: re-solve failed at cursor {end_ms}: {e2:#}"
                        );
                        let rows = all.len();
                        log_history_walk(player_id, mode, pages, rows, false, "refresh-failed", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
                        return Ok((all, false));
                    }
                }
                let retry_started = Instant::now();
                let retry = fetch_player_page(ctx.client, &cur_mirror, player_id, mode, end_ms, &cur_auth)
                    .await;
                req_ms = req_ms.saturating_add(u64::try_from(retry_started.elapsed().as_millis()).unwrap_or(u64::MAX));
                match retry
                {
                    Ok(p) => {
                        pages += 1;
                        p
                    }
                    Err(e2) => {
                        debug!("player {player_id}/{mode}: page failed at cursor {end_ms} after {} games: {e2:#}", all.len());
                        if !e2.to_string().contains("token-429") {
                            ctx.pool.give((cur_tab, cur_auth, cur_mirror)).await;
                        }
                        let rows = all.len();
                        log_history_walk(player_id, mode, pages, rows, false, "page-failed", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
                        return Ok((all, false));
                    }
                }
            }
            Err(e) => {
                debug!("player {player_id}/{mode}: page failed at cursor {end_ms} after {} games: {e:#}", all.len());
                ctx.pool.give((cur_tab, cur_auth, cur_mirror)).await;
                let rows = all.len();
                log_history_walk(player_id, mode, pages, rows, false, "page-failed", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
                return Ok((all, false));
            }
        };
        if page.is_empty() {
            ctx.pool
                .give((cur_tab, cur_auth, cur_mirror))
                .await;
            let rows = all.len();
            log_history_walk(player_id, mode, pages, rows, true, "empty", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
            return Ok((all, true));
        }
        // Match the site's startTime index, not the game's finishing time.
        if let Some(oldest) = page.iter().map(|g| g.start_time).min() {
            end_ms = oldest * 1000 - 1;
        } else {
            ctx.pool
                .give((cur_tab, cur_auth, cur_mirror))
                .await;
            let rows = all.len();
            log_history_walk(player_id, mode, pages, rows, true, "no-timestamp", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
            return Ok((all, true));
        }
        all.extend(page);
        // Bank a bounded slice instead of retaining an entire veteran history
        // while more requests or token waits run. The durable partial cursor
        // lets the scheduler continue without claiming exhaustion.
        if pages >= 4 {
            ctx.pool.give((cur_tab, cur_auth, cur_mirror)).await;
            let rows = all.len();
            log_history_walk(player_id, mode, pages, rows, false, "checkpoint", refresh_ok, refresh_fail, token_wait_ms, req_ms, walk_elapsed_ms());
            return Ok((all, false));
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}


pub struct ResolveConfig {
    /// Mirror base, e.g. `https://5-data.amae-koromo.com/`.
    pub mirror: String,
    /// Max concurrent `view_game` requests.
    pub workers: usize,
    /// Delay between requests per worker (ms). Start ~500, tune by 429 rate.
    pub delay_ms: u64,
}

impl Default for ResolveConfig {
    fn default() -> Self {
        Self {
            mirror: "https://5-data.amae-koromo.com/".to_owned(),
            workers: 16,
            delay_ms: 500,
        }
    }
}

/// One resolvable record: short `uuid` + ALL witness accounts + provenance.
/// `view_game` needs (`short`, `accountId`); any seated player works, so a failed
/// witness retries against the other three before the uuid is declared dead.
#[derive(Debug, Clone)]
pub struct ShortRecord {
    pub uuid: String,
    pub account_ids: Vec<i64>,
    pub mode: i32,
    pub day: String,
}

/// Parse a journal line into a `ShortRecord` (Rust `day`/`mode` or legacy `_day`/`_mode`).
pub fn parse_short(v: &serde_json::Value) -> Option<ShortRecord> {
    let uuid = v.get("uuid")?.as_str()?.to_owned();
    let mode = v
        .get("modeId")
        .or_else(|| v.get("mode"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|m| i32::try_from(m).ok())?;
    let day = v
        .get("day")
        .or_else(|| v.get("_day"))
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_owned();
    let players = v.get("players")?.as_array()?;
    let mut account_ids = Vec::new();
    for p in players {
        if let Some(id) = p.get("accountId").and_then(serde_json::Value::as_i64) {
            if !account_ids.contains(&id) {
                account_ids.push(id);
            }
        }
    }
    if account_ids.is_empty() {
        return None;
    }
    Some(ShortRecord {
        uuid,
        account_ids,
        mode,
        day,
    })
}

/// Resolve a batch: returns (paipu, uuid, day, mode) rows. 429s back off 60s and retry.
pub async fn resolve_batch(
    client: &reqwest::Client,
    cfg: &ResolveConfig,
    batch: Vec<ShortRecord>,
) -> Vec<(String, String, String, i32)> {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let sem = Arc::new(Semaphore::new(cfg.workers.max(1)));
    let mut handles = Vec::new();
    for rec in batch {
        let client = client.clone();
        let mirror = cfg.mirror.clone();
        let sem = Arc::clone(&sem);
        let delay = cfg.delay_ms;
        handles.push(tokio::spawn(async move {
            let mut tries = 0u32;
            loop {
                // Acquire per attempt: never hold the permit across sleeps/backoff,
                // or a few 429s wedge every worker for a minute.
                let Ok(permit) = sem.acquire().await else {
                    return None;
                };
                let r = resolve_paipu_any(&client, &mirror, rec.mode, &rec.uuid, &rec.account_ids)
                    .await;
                drop(permit);
                match r {
                    Ok(paipu) => {
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        return Some((paipu, rec.uuid.clone(), rec.day.clone(), rec.mode));
                    }
                    Err(e) if e.to_string().contains("429") && tries < 5 => {
                        tries += 1;
                        warn!("resolve {}: 429, backoff 60s ({tries})", rec.uuid);
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                    Err(e) => {
                        warn!("resolve {}: {e:#}", rec.uuid);
                        return None;
                    }
                }
            }
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        if let Ok(Some(row)) = h.await {
            out.push(row);
        }
    }
    out
}

/// Harvest SQLite state: done day/modes + parked hours + seen uuids.
pub struct HarvestDb {
    conn: rusqlite::Connection,
}

impl HarvestDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = rusqlite::Connection::open(path)?;
        // 3 connections share this file (run + writer + checkpoint): WAL + busy
        // timeout so concurrent batch INSERTs wait instead of SQLITE_BUSY-failing.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS seen(uuid TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS done(day TEXT, mode INTEGER, PRIMARY KEY(day, mode));
             CREATE TABLE IF NOT EXISTS parked(day TEXT, mode INTEGER, hour INTEGER,
                 PRIMARY KEY(day, mode, hour));
             CREATE TABLE IF NOT EXISTS deep(day TEXT, mode INTEGER, hour INTEGER,
                 PRIMARY KEY(day, mode, hour));
             CREATE TABLE IF NOT EXISTS cursors(day TEXT, mode INTEGER, hour INTEGER, cursor_ms INTEGER,
                 PRIMARY KEY(day, mode, hour));",
        )?;
        Ok(Self { conn })
    }

    /// Seed seen-set from legacy JSONL journals (pool/harvest/harvest-may).
    /// Append-only; restarts never re-harvest banked uuids. Single transaction
    /// + prepared INSERT OR IGNORE: restarts stay fast on 300k+ line journals.
    pub fn seed_from_jsonl(&self, path: &Path) -> usize {
        use std::io::BufRead as _;
        let Ok(file) = std::fs::File::open(path) else {
            return 0;
        };
        let mut n = 0usize;
        // Transaction batches fsyncs; prepared statement avoids re-parsing SQL
        // per row (300k+ rows on every restart otherwise).
        let Ok(tx) = self.conn.unchecked_transaction() else {
            return 0;
        };
        {
            let Ok(mut stmt) = tx.prepare("INSERT OR IGNORE INTO seen(uuid) VALUES (?1)") else {
                return 0;
            };
            for line in std::io::BufReader::new(file).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => { warn!(path = %path.display(), %error, "journal seed read failed"); return 0; }
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if let Some(u) = v.get("uuid").and_then(|u| u.as_str()) {
                    stmt.execute([u]).ok();
                    n += 1;
                }
            }
        }
        tx.commit().ok();
        n
    }

    pub fn load_seen(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT uuid FROM seen")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }

    /// Batch-persist uuids (writer calls every 500). Append-only, idempotent.
    pub fn mark_seen(&self, uuids: &[String]) {
        for u in uuids {
            if let Err(e) = self
                .conn
                .execute("INSERT OR IGNORE INTO seen(uuid) VALUES (?1)", [u])
            {
                warn!("mark_seen {u}: {e:#}");
            }
        }
    }

    /// Checkpoint a completed day/mode (skipped on restart).
    pub fn mark_done(&self, day: &str, mode: i32) {
        if let Err(e) = self.conn.execute(
            "INSERT OR IGNORE INTO done(day, mode) VALUES (?1, ?2)",
            rusqlite::params![day, mode],
        ) {
            warn!("mark_done {day}/{mode}: {e:#}");
        }
    }

    /// Reconcile restarts: days whose 24 hours are all deep-drained are done,
    /// even if no single process ever counted all 24 (deep hours are filtered
    /// at startup, so in-memory counting can never complete them — `done`
    /// would freeze and finite backfills relaunch forever).
    pub fn mark_done_where_all_deep(&self, days: &[String], modes: &[i32]) -> usize {
        let mut n = 0usize;
        for day in days {
            for m in modes {
                let c: i64 = self
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM deep WHERE day=?1 AND mode=?2",
                        rusqlite::params![day, m],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if c >= 24 {
                    self.mark_done(day, *m);
                    n += 1;
                }
            }
        }
        n
    }

    /// Queue a parked hour for retry (surfaces in `failed_hours` runs).
    pub fn mark_parked(&self, day: &str, mode: i32, hour: u32) {
        if let Err(e) = self.conn.execute(
            "INSERT OR IGNORE INTO parked(day, mode, hour) VALUES (?1, ?2, ?3)",
            rusqlite::params![day, mode, hour],
        ) {
            warn!("mark_parked {day}/{mode} h{hour}: {e:#}");
        }
    }

    pub fn load_cursor(&self, day: &str, mode: i32, hour: u32) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT cursor_ms FROM cursors WHERE day=?1 AND mode=?2 AND hour=?3")?;
        let mut rows =
            stmt.query_map(rusqlite::params![day, mode, hour], |r| r.get::<_, i64>(0))?;
        if let Some(r) = rows.next() {
            return Ok(Some(r?));
        }
        Ok(None)
    }


    pub fn clear_cursor(&self, day: &str, mode: i32, hour: u32) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM cursors WHERE day=?1 AND mode=?2 AND hour=?3",
                rusqlite::params![day, mode, hour],
            )
            .map(|_| ())
            .map_err(anyhow::Error::from)
    }

    /// Clear a stale parked mark once the hour completes clean on retry.
    pub fn clear_parked(&self, day: &str, mode: i32, hour: u32) {
        if let Err(e) = self.conn.execute(
            "DELETE FROM parked WHERE day=?1 AND mode=?2 AND hour=?3",
            rusqlite::params![day, mode, hour],
        ) {
            warn!("clear_parked {day}/{mode} h{hour}: {e:#}");
        }
    }

    /// Mark one hour deep-drained (skip-seen fires here on future runs).
    pub fn mark_deep(&self, day: &str, mode: i32, hour: u32) {
        if let Err(e) = self.conn.execute(
            "INSERT OR IGNORE INTO deep(day, mode, hour) VALUES (?1, ?2, ?3)",
            rusqlite::params![day, mode, hour],
        ) {
            warn!("mark_deep {day}/{mode} h{hour}: {e:#}");
        }
    }

    /// Hour-level deep check (skip-seen gate).
    pub fn is_deep(&self, day: &str, mode: i32, hour: u32) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM deep WHERE day=?1 AND mode=?2 AND hour=?3",
                rusqlite::params![day, mode, hour],
                |_| Ok(true),
            )
            .unwrap_or(false)
    }

    pub fn load_done(&self) -> Result<HashSet<(String, i32)>> {
        let mut stmt = self.conn.prepare("SELECT day, mode FROM done")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)))?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::PlayerRecord;
    use super::*;

    #[tokio::test]
    async fn history_short_page_continues_by_start_time_until_empty() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mirror = format!("http://{}/", listener.local_addr().unwrap());
        let mut first = mk(12, &[]);
        first.uuid = "first".into();
        first.start_time = 200;
        first.end_time = Some(250);
        let mut second = first.clone();
        second.uuid = "second".into();
        second.start_time = 100;
        second.end_time = Some(150);
        let bodies = [serde_json::to_string(&vec![first]).unwrap(), serde_json::to_string(&vec![second]).unwrap(), "[]".into()];
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut paths = Vec::new();
            for body in bodies {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "history did not request continuation");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = [0; 8192];
                let n = stream.read(&mut request).unwrap();
                paths.push(String::from_utf8_lossy(&request[..n]).lines().next().unwrap().to_owned());
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            paths
        });
        let pool = AuthPool::new("unused".into());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let ctx = FetchCtx { client: &client, pool: &pool, tab: "test", day: "20260101" };
        let (games, complete) = fetch_player_records(&ctx, &mirror, 1, 12, "test-only", Some(300_000)).await.unwrap();
        let paths = server.join().unwrap();
        assert!(complete);
        assert_eq!(games.iter().map(|game| game.uuid.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        assert!(paths[1].contains("/199999/"));
        assert!(paths[2].contains("/99999/"));
    }


    #[tokio::test]
    async fn solver_lock_serializes_and_releases_after_cancel() {
        let endpoint = format!("test-solver-{}", uuid::Uuid::new_v4());
        let held = lock_solver(&endpoint).await.unwrap();
        let waiting_endpoint = endpoint.clone();
        let waiting = tokio::spawn(async move { lock_solver(&waiting_endpoint).await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!waiting.is_finished(), "same solver must not overlap");
        // A different solver remains usable while this one is occupied.
        let other = lock_solver(&format!("{endpoint}-other")).await.unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        drop(held);
        let acquired = tokio::time::timeout(Duration::from_secs(1), lock_solver(&endpoint))
            .await
            .expect("cancelled waiter must not retain ownership")
            .unwrap();
        drop(acquired);
        drop(other);
    }

    #[tokio::test]
    async fn available_solver_is_used_when_healthy_solver_is_busy() {
        let mut pool = AuthPool::new("http://127.0.0.1:1".to_owned());
        pool.key = None;
        pool.solvers = vec!["http://127.0.0.1:1".to_owned(); 2];
        pool.solve_locks = vec![Mutex::new(()), Mutex::new(())];
        pool.solver_fails = Mutex::new(vec![0, 3]);
        pool.solver_ok_at = Mutex::new(vec![None; 2]);
        pool.last_steal = Mutex::new(vec![
            Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap();
            2
        ]);
        let _busy = pool.solve_locks[0].lock().await;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            pool.ensure("scheduler-test", "2025-01-01", 16),
        )
        .await;
        assert!(result
            .expect("idle slot must not queue behind busy healthy slot")
            .is_err());
        assert_eq!(*pool.solver_fails.lock().await, vec![0, 4]);
    }

    fn mk(mode: i32, levels: &[i32]) -> GameRecord {
        GameRecord {
            uuid: "x".to_owned(),
            mode_id: mode,
            start_time: 0,
            end_time: None,
            players: levels
                .iter()
                .map(|l| PlayerRecord {
                    account_id: 1,
                    nickname: String::new(),
                    level: *l,
                    score: 0,
                    grading_score: None,
                })
                .collect(),
        }
    }

    #[test]
    fn tier_split_matches_prototype() {
        assert_eq!(tier_game(&mk(16, &[10401])), JadeTier::Throne);
        assert_eq!(tier_game(&mk(12, &[10603, 10501])), JadeTier::JadeHigh);
        assert_eq!(tier_game(&mk(12, &[10402])), JadeTier::JadeMid);
        assert_eq!(tier_game(&mk(12, &[10303])), JadeTier::JadeLow);
        assert_eq!(tier_game(&mk(9, &[10601])), JadeTier::Other);
    }

    #[test]
    fn sort_key_prefers_level_then_strength() {
        let hi = GameRecord {
            players: vec![PlayerRecord {
                account_id: 1,
                nickname: String::new(),
                level: 10610,
                score: 50,
                grading_score: None,
            }],
            ..mk(12, &[])
        };
        let lo = GameRecord {
            players: vec![PlayerRecord {
                account_id: 1,
                nickname: String::new(),
                level: 10601,
                score: 100,
                grading_score: None,
            }],
            ..mk(12, &[])
        };
        // Lv10 > Lv1 at any score.
        assert!(sort_key(&hi) > sort_key(&lo));
        // Same level: stronger score wins.
        let a = mk(12, &[]);
        let mut b = mk(12, &[]);
        b.players = vec![
            PlayerRecord {
                account_id: 1,
                nickname: String::new(),
                level: 10603,
                score: 200,
                grading_score: None,
            },
            PlayerRecord {
                account_id: 2,
                nickname: String::new(),
                level: 10603,
                score: 900,
                grading_score: None,
            },
        ];
        let _ = a;
        assert_eq!(sort_key(&b), (10603, 900));
    }

    #[test]
    fn classifier_reads_cap_first() {
        // "contact SAPikachu to obtain a key" is the TOKEN path, not IP.
        assert_eq!(
            classify_429(
                "[x-cap-token-required] CAPTCHA required, please contact SAPikachu to obtain a key"
            ),
            LimitKind::Token
        );
        assert_eq!(
            classify_429("Too many requests, please contact SAPikachu for bulk data"),
            LimitKind::Ip
        );
    }

    #[test]
    fn hour_windows_cover_day() {
        let d = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        let ds = jst_day_start_ms(d).unwrap();
        let (s0, e0) = hour_window(ds, 0);
        let (s23, e23) = hour_window(ds, 23);
        assert_eq!(s0, ds);
        assert_eq!(e23, ds + 86_400_000 - 1);
        assert_eq!(e0 + 1, s0 + 3_600_000);
        let _ = s23;
    }
}
