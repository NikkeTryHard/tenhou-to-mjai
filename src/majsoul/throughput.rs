//! Unique-throughput probe for append-only JSONL journals.
//!
//! Portable port of `majsoul-smoke/observe-unique.mjs`: instead of scraping a
//! pipeline log counter, this tails the journal file itself, so any
//! append-only JSONL journal with a top-level `"uuid"` string per row works
//! (for example `fulls.jsonl`, `fleet.jsonl`, `paipu.jsonl`).
//!
//! Each sampling interval appends one JSON row to `log` with the same sample
//! keys as the `.mjs` probe (`time`, `unique`, `elapsed`, `rate`,
//! `streak_secs`) plus `interval_secs` and `malformed`:
//! - `time`: UTC RFC 3339 timestamp of the sample.
//! - `interval_secs`: configured sampling cadence.
//! - `elapsed`: actual seconds covered by this sample.
//! - `unique`: new UUIDs first seen this interval (exact, deduped).
//! - `rate`: `unique / elapsed` unique rows per second.
//! - `streak_secs`: consecutive seconds with `rate >= target_per_sec`.
//! - `malformed`: rows that were not valid JSON or had no usable `"uuid"`
//!   (surfaced per interval, never silently dropped).
//!
//! Behavior notes:
//! - The pre-existing file content is streamed once as an exact dedup
//!   baseline (streaming, never read whole into memory). UUIDs are recorded
//!   in an ephemeral disk-backed SQLite index, so RAM stays bounded no matter
//!   how many distinct IDs the journal holds; full record payloads are never
//!   retained anywhere.
//! - The index is created fresh under the log file's parent directory with a
//!   random UUID in its name, so concurrent runs never share an index and no
//!   stale shared index can be picked up. Only this run's own temp file is
//!   removed on exit; nothing else is ever touched.
//! - Tailing is byte-offset based from a snapshotted initial length. A
//!   trailing partial line (torn write) is held back until its newline
//!   arrives instead of being counted.
//! - If the input shrinks below the tracked offset the run fails with a
//!   truncation error instead of silently rescanning from zero.
//! - An idle input (no appends) naturally logs `unique: 0`, `rate: 0.0` and
//!   resets the streak. No remote calls are made.

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};

/// Single journal row projected down to its identity.
///
/// Every other field is ignored on purpose; only the `uuid` ever reaches the
/// seen index.
#[derive(Deserialize)]
struct IdRow {
    uuid: Option<String>,
}

/// One appended log sample.
#[derive(Serialize)]
struct Sample {
    time: String,
    interval_secs: u64,
    elapsed: f64,
    unique: usize,
    rate: f64,
    streak_secs: f64,
    malformed: usize,
}

/// Exact on-disk set of UUIDs already counted.
///
/// Ephemeral per run: created fresh under the log parent, never shared, and
/// only this exact path is removed on exit.
struct SeenIndex {
    conn: rusqlite::Connection,
    path: PathBuf,
}

impl SeenIndex {
    /// Create a fresh empty index beside `log`.
    ///
    /// The filename carries a random UUID so concurrent runs never share an
    /// index and no stale shared index can be picked up or cleaned by mistake.
    fn create(log: &Path) -> Result<Self> {
        let dir: PathBuf = match log.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let tag = uuid::Uuid::new_v4();
        let path = dir.join(format!(".throughput-seen-{tag}.sqlite.tmp"));
        let path_display = path.display().to_string();
        let conn = rusqlite::Connection::open(&path)
            .with_context(|| format!("open seen index {path_display}"))?;
        // Disposable index rebuilt from the journal on every run: keep the
        // journal in memory and skip fsyncs for insert speed.
        conn.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF;")
            .with_context(|| format!("tune seen index {path_display}"))?;
        conn.execute_batch("CREATE TABLE seen(uuid TEXT PRIMARY KEY) WITHOUT ROWID;")
            .with_context(|| format!("init seen index {path_display}"))?;
        Ok(Self { conn, path })
    }

    /// Close and remove only this run's own temp file.
    ///
    /// Failures are warned (surfaced, not hidden) without masking the
    /// measurement outcome, and no other path is ever touched.
    fn remove(self) {
        let path_display = self.path.display().to_string();
        match self.conn.close() {
            Ok(()) => {}
            Err((_, err)) => {
                warn!("throughput: close seen index {path_display} failed: {err:#}");
                return;
            }
        }
        if let Err(err) = std::fs::remove_file(&self.path) {
            warn!("throughput: remove seen index {path_display} failed: {err:#}");
        }
    }
}

/// Measure unique-UUID throughput of an append-only JSONL journal.
///
/// # Arguments
/// * `input` - Append-only JSONL journal; each complete line should hold a
///   top-level `"uuid"` string (e.g. `fulls.jsonl`).
/// * `log` - JSONL file each interval sample is appended to (created if missing).
///   The ephemeral seen index lives beside it while the run is active.
/// * `interval_secs` - Sampling cadence in seconds (must be > 0).
/// * `duration_secs` - Total measurement window in seconds (must be > 0; the
///   run always returns after about this long).
/// * `target_per_sec` - Rate threshold for `streak_secs` (must be finite, >= 0).
///
/// # Errors
/// Propagates I/O errors (missing input, unreadable log parent, etc.),
/// rejects invalid config (`interval_secs == 0`, `duration_secs == 0`,
/// non-finite or negative `target_per_sec`), and fails clearly if the input
/// is truncated below the tracked tail offset.
pub async fn run(
    input: &Path,
    log: &Path,
    interval_secs: u64,
    duration_secs: u64,
    target_per_sec: f64,
) -> Result<()> {
    if interval_secs == 0 {
        anyhow::bail!("interval_secs must be > 0");
    }
    if duration_secs == 0 {
        anyhow::bail!("duration_secs must be > 0");
    }
    if !target_per_sec.is_finite() || target_per_sec < 0.0 {
        anyhow::bail!("target_per_sec must be finite and >= 0, got {target_per_sec}");
    }

    // Snapshot the starting length, then stream exactly those bytes as the
    // exact-uniqueness baseline; rows appended concurrently stay for interval 1.
    let initial_len = tokio::fs::metadata(input)
        .await
        .with_context(|| {
            let input_name = input.display().to_string();
            format!("stat input {input_name}")
        })?
        .len();
    // Fail fast on an unusable log location before doing baseline work, and
    // own the ephemeral index from here on.
    let mut index = SeenIndex::create(log)?;
    let outcome = measure(
        input,
        log,
        interval_secs,
        duration_secs,
        target_per_sec,
        initial_len,
        &mut index,
    )
    .await;
    // Always remove only this run's own temp file; never anything else.
    index.remove();
    outcome
}

/// Baseline plus interval loop; split out so `run` can guarantee cleanup of
/// its own index temp on every exit path.
async fn measure(
    input: &Path,
    log: &Path,
    interval_secs: u64,
    duration_secs: u64,
    target_per_sec: f64,
    initial_len: u64,
    index: &mut SeenIndex,
) -> Result<()> {
    let input_name = input.display().to_string();
    let log_name = log.display().to_string();
    let (mut offset, baseline_unique, baseline_malformed) =
        scan_range(input, &input_name, 0, initial_len, index).await?;
    if baseline_malformed > 0 {
        warn!(
            "throughput baseline: {baseline_malformed} malformed rows in existing {input_name} (surfaced, not counted as unique)"
        );
    }
    let mut total_unique = baseline_unique;
    info!("throughput baseline: {total_unique} unique ids in {input_name} (offset {offset})");

    let mut log_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .await
        .with_context(|| format!("open log {log_name}"))?;

    let interval = Duration::from_secs(interval_secs);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);
    let mut streak_secs = 0.0_f64;

    loop {
        let tick = tokio::time::Instant::now();
        if tick >= deadline {
            break;
        }
        // Cap the last sleep so the run stays bounded by `duration_secs`
        // instead of overshooting by a full interval.
        tokio::time::sleep(interval.min(deadline.saturating_duration_since(tick))).await;
        let elapsed = tick.elapsed().as_secs_f64();

        // Snapshot this interval's range; growth during the scan waits for the
        // next sample, keeping every sample's byte range exact.
        let current_len = tokio::fs::metadata(input)
            .await
            .with_context(|| format!("stat input {input_name}"))?
            .len();
        if current_len < offset {
            anyhow::bail!(
                "input truncated: {input_name} length {current_len} below offset {offset}; refusing to rescan"
            );
        }
        let (end, unique, malformed) =
            scan_range(input, &input_name, offset, current_len - offset, index).await?;
        offset = end;
        total_unique += unique;

        let rate = if elapsed > 0.0 {
            let unique_count =
                u32::try_from(unique).context("interval unique count exceeds u32")?;
            f64::from(unique_count) / elapsed
        } else {
            0.0
        };
        if rate >= target_per_sec {
            streak_secs += elapsed;
        } else {
            streak_secs = 0.0;
        }

        let sample = Sample {
            time: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            interval_secs,
            elapsed,
            unique,
            rate,
            streak_secs,
            malformed,
        };
        let mut line = serde_json::to_string(&sample).context("encode throughput sample")?;
        line.push('\n');
        log_file
            .write_all(line.as_bytes())
            .await
            .with_context(|| format!("append log {log_name}"))?;
        log_file
            .flush()
            .await
            .with_context(|| format!("flush log {log_name}"))?;
        info!(
            "throughput: unique={unique} total={total_unique} elapsed={elapsed:.3}s rate={rate:.1}/s streak={streak_secs:.1}s malformed={malformed}"
        );
    }

    Ok(())
}

/// Scan up to `limit` bytes from `start`, recording UUIDs in the seen index.
///
/// Returns `(new_offset, unique_new, malformed)`. One transaction covers the
/// whole scan, so bulk inserts stay fast; a failed scan rolls back while the
/// tail offset stays put, making a retry idempotent. Only complete
/// newline-terminated lines are processed; a trailing partial line (torn or
/// concurrently appended tail) is held back by leaving the offset before it.
async fn scan_range(
    input: &Path,
    input_name: &str,
    start: u64,
    limit: u64,
    index: &mut SeenIndex,
) -> Result<(u64, usize, usize)> {
    if limit == 0 {
        return Ok((start, 0, 0));
    }
    let mut file = tokio::fs::File::open(input)
        .await
        .with_context(|| format!("open input {input_name}"))?;
    file.seek(SeekFrom::Start(start))
        .await
        .with_context(|| format!("seek input {input_name} to {start}"))?;
    let mut limited = tokio::io::BufReader::new(file).take(limit);
    let tx = index
        .conn
        .transaction()
        .context("begin seen-index transaction")?;
    let mut inserts = tx
        .prepare("INSERT OR IGNORE INTO seen(uuid) VALUES (?1)")
        .context("prepare seen-index insert")?;
    let mut buf = Vec::new();
    let mut pos = start;
    let mut unique = 0_usize;
    let mut malformed = 0_usize;
    loop {
        buf.clear();
        let n = limited
            .read_until(b'\n', &mut buf)
            .await
            .with_context(|| format!("read input {input_name}"))?;
        if n == 0 {
            break;
        }
        if !buf.ends_with(b"\n") {
            // No terminating newline inside the snapshotted range: hold the
            // partial tail back for the next sample instead of counting it.
            break;
        }
        pos += n as u64;
        let Ok(text) = std::str::from_utf8(&buf) else {
            malformed += 1;
            continue;
        };
        let line = text.trim();
        if line.is_empty() {
            continue;
        }
        match extract_uuid(line) {
            Some(id) => {
                let changed = inserts
                    .execute(params![id])
                    .context("insert seen uuid")?;
                if changed == 1 {
                    unique += 1;
                }
            }
            None => malformed += 1,
        }
    }
    drop(inserts);
    tx.commit().context("commit seen-index transaction")?;
    Ok((pos, unique, malformed))
}

/// Extract just the UUID string from one journal line.
///
/// Returns `None` for invalid JSON or a missing/blank `"uuid"`; the caller
/// surfaces those as `malformed` instead of hiding them.
fn extract_uuid(line: &str) -> Option<String> {
    let row: IdRow = serde_json::from_str(line).ok()?;
    let id = row.uuid?;
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}
