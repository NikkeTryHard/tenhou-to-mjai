//! Multi-account bulk raw protobuf downloader.
//!
//! Downloads raw game record bytes from Majsoul and saves to disk as .pb files.
//! Conversion to Tenhou JSON is a separate step for robustness.
//!
//! Architecture:
//! - Reads UUIDs from todo.txt (flat file, one per line)
//! - Tracks completions in completed.log (append-only journal)
//! - Spawns N tokio tasks, each with its own authenticated RPC connection
//! - Shared work queue via async-channel
//! - indicatif progress bar

use anyhow::{Context, Result};
use indicatif::ProgressBar;
use std::collections::HashSet;
use std::io::{BufRead, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;

/// Load UUIDs from todo.txt, subtract completed.log, return remaining.
///
/// Both files are read via `BufRead::lines()` with trim + nonempty filter,
/// which fixes CRLF, missing-trailing-newline, and the old O(n^2) `contains`
/// resume check.
fn load_remaining_uuids(
    todo_file: &Path,
    completed_file: &Path,
    output_dir: &Path,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    // Load todo list (trimmed, nonempty).
    let todo_file_handle =
        std::fs::File::open(todo_file).with_context(|| format!("Failed to read {}", todo_file.display()))?;
    let mut all_uuids: Vec<String> = Vec::new();
    for line in std::io::BufReader::new(todo_file_handle).lines() {
        match line {
            Ok(l) => {
                let l = l.trim().to_string();
                if !l.is_empty() {
                    all_uuids.push(l);
                }
            }
            // Skip (not abort on) unreadable lines; same as the old
            // `filter_map(Result::ok)`, but surfaced instead of silent.
            Err(e) => warn!("skipping unreadable todo line: {e}"),
        }
    }
    info!("Total UUIDs in todo: {}", all_uuids.len());

    // Load completed set (trimmed, nonempty).
    let mut done: HashSet<String> = HashSet::new();
    if completed_file.exists() {
        let handle = std::fs::File::open(completed_file)?;
        for line in std::io::BufReader::new(handle).lines() {
            match line {
                Ok(l) => {
                    let l = l.trim().to_string();
                    if !l.is_empty() {
                        done.insert(l);
                    }
                }
                Err(e) => warn!("skipping unreadable completed line: {e}"),
            }
        }
    }

    // Also scan existing .pb files on disk (uuid stems stored verbatim).
    if output_dir.exists() {
        for entry in std::fs::read_dir(output_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".pb") {
                done.insert(stem.to_string());
            }
        }
    }

    info!("Already completed: {}", done.len());

    let remaining: Vec<String> = all_uuids.into_iter().filter(|u| !done.contains(u)).collect();
    let remaining = match limit {
        Some(n) => remaining.into_iter().take(n).collect(),
        None => remaining,
    };

    info!("Remaining to download: {}", remaining.len());
    Ok(remaining)
}

/// One-time migration: rename legacy `a_b.pb` stems back to `a-b.pb` when the
/// underscore stem parses as a UUID once underscores become dashes.
/// Non-UUID-shaped stems are skipped.
pub fn migrate_underscore_pb_files(output_dir: &Path) -> Result<usize> {
    let mut renamed = 0usize;
    if !output_dir.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(output_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "pb" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.contains('_') {
            continue;
        }
        let candidate = stem.replace('_', "-");
        // Only rename stems matching UUID shape: try parse as UUID-with-dashes.
        if candidate.parse::<uuid::Uuid>().is_ok() {
            let dest = path.with_file_name(format!("{candidate}.pb"));
            if !dest.exists() {
                std::fs::rename(&path, &dest)?;
                renamed += 1;
            }
        }
    }
    if renamed > 0 {
        info!("Migrated {} underscore .pb files back to dashed UUIDs", renamed);
    }
    Ok(renamed)
}

/// Stats shared between workers
struct SharedStats {
    success: AtomicU64,
    failed: AtomicU64,
    logged_in: AtomicU64,
    login_failed: AtomicU64,
}

/// Worker: owns one RPC connection, pulls UUIDs from receiver, writes .pb files
// 14 args are the spawned worker's config bundle (D7 fan-out); bundling them
// in a struct would churn the spawn site for no behavior gain.
#[allow(clippy::too_many_arguments)]
async fn worker(
    worker_id: usize,
    username: String,
    password: String,
    endpoint: String,
    version: String,
    route_id: String,
    origin: String,
    rx: Arc<Mutex<mpsc::Receiver<String>>>,
    output_dir: PathBuf,
    journal_tx: mpsc::Sender<String>,
    stats: Arc<SharedStats>,
    pb: ProgressBar,
    error_tx: mpsc::Sender<(String, String)>,
    delay_ms: u64,
) {
    // Connect and login (per-server Origin).
    let rpc = match MajsoulRpc::connect(&endpoint, &origin).await {
        Ok(r) => r,
        Err(e) => {
            debug!("[{}] Connection failed: {}", worker_id, e);
            stats.login_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    if let Err(e) = rpc.login_native(&username, &password, &version, &route_id).await {
        debug!("[{}] Login failed for {}: {}", worker_id, username, e);
        stats.login_failed.fetch_add(1, Ordering::Relaxed);
        let _ = rpc.close().await;
        return;
    }

    stats.logged_in.fetch_add(1, Ordering::Relaxed);

    // version.json "X.Y.w" -> login/fetch "web-X.Y" (see gateway.rs).
    let client_version = format!("web-{}", version.replace(".w", ""));

    // Download loop
    loop {
        let uuid = {
            let mut guard = rx.lock().await;
            guard.recv().await
        };

        // Channel closed, no more work.
        let Some(uuid) = uuid else { break };

        match rpc.fetch_game_record(&uuid, &client_version).await {
            Ok(data) => {
                // Write {uuid}.pb directly (no dash/underscore mapping).
                let filename = format!("{uuid}.pb");
                let filepath = output_dir.join(&filename);

                // Atomic write via temp file
                let tmp_path = output_dir.join(format!(".tmp_{filename}"));
                match std::fs::write(&tmp_path, &data) {
                    Ok(()) => {
                        if let Err(e) = std::fs::rename(&tmp_path, &filepath) {
                            warn!("[{}] rename failed: {}", worker_id, e);
                            let _ = std::fs::remove_file(&tmp_path);
                            stats.failed.fetch_add(1, Ordering::Relaxed);
                            let _ = error_tx.send((uuid, e.to_string())).await;
                        } else {
                            stats.success.fetch_add(1, Ordering::Relaxed);
                            let _ = journal_tx.send(uuid).await;
                        }
                    }
                    Err(e) => {
                        warn!("[{}] write failed: {}", worker_id, e);
                        stats.failed.fetch_add(1, Ordering::Relaxed);
                        let _ = error_tx.send((uuid, e.to_string())).await;
                    }
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                stats.failed.fetch_add(1, Ordering::Relaxed);
                let _ = error_tx.send((uuid, err_str)).await;
            }
        }

        pb.inc(1);

        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    }

    // Graceful close
    let _ = rpc.close().await;
}

/// Journal writer: receives completed UUIDs and appends to completed.log
async fn journal_writer(
    mut rx: mpsc::Receiver<String>,
    completed_file: PathBuf,
) -> Result<()> {
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&completed_file)
    {
        Ok(f) => f,
        Err(e) => return Err(e).with_context(|| format!("Failed to open {}", completed_file.display())),
    };

    while let Some(uuid) = rx.recv().await {
        if let Err(e) = writeln!(file, "{uuid}") {
            warn!("journal write failed: {}", e);
            return Err(e).context("journal writeln failed");
        }
        // Line-buffered: flush after each line for crash safety
        if let Err(e) = file.flush() {
            warn!("journal flush failed: {}", e);
            return Err(e).context("journal flush failed");
        }
    }
    Ok(())
}

/// Error logger: receives (uuid, error) pairs and writes to failed.log
async fn error_logger(
    mut rx: mpsc::Receiver<(String, String)>,
    output_dir: PathBuf,
) -> Result<()> {
    let failed_path = output_dir.join("failed.log");
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&failed_path)
    {
        Ok(f) => f,
        Err(e) => return Err(e).with_context(|| format!("Failed to open {}", failed_path.display())),
    };

    while let Some((uuid, error)) = rx.recv().await {
        if let Err(e) = writeln!(file, "{uuid}\t{error}") {
            warn!("error-log write failed: {}", e);
            return Err(e).context("error-log writeln failed");
        }
        if let Err(e) = file.flush() {
            warn!("error-log flush failed: {}", e);
            return Err(e).context("error-log flush failed");
        }
    }
    Ok(())
}

/// Main entry point for multi-account raw download
// Length is worker/journal fan-out plus resume-set handling (D7); splitting
// would churn the verified resume/error-log wiring. 8 args are the CLI-passed
// download config; bundling them would churn every caller for no gain.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn raw_download(
    accounts_file: &Path,
    password: &str,
    todo_file: &Path,
    completed_file: &Path,
    output_dir: &Path,
    server: &str,
    limit: Option<usize>,
    delay_ms: u64,
) -> Result<(u64, u64)> {
    // Create output directory
    std::fs::create_dir_all(output_dir)?;
    // One-time migration of legacy underscore stems.
    let _ = migrate_underscore_pb_files(output_dir)?;

    // Load accounts
    let accounts: Vec<String> = {
        let content = std::fs::read_to_string(accounts_file)
            .with_context(|| format!("Failed to read {}", accounts_file.display()))?;
        content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    };
    info!("Loaded {} accounts", accounts.len());

    if accounts.is_empty() {
        anyhow::bail!("No accounts found in {}", accounts_file.display());
    }

    // Load remaining UUIDs
    let remaining = load_remaining_uuids(todo_file, completed_file, output_dir, limit)?;
    if remaining.is_empty() {
        info!("Nothing to download!");
        return Ok((0, 0));
    }

    // Discover gateway (once, shared by all workers)
    let client = crate::util::http_client()?;
    let (endpoint, version, route_id) = discover_gateway(&client, server).await?;
    let origin = super::rpc::origin_for_server(server).to_string();
    info!("Gateway: {} (route: {})", endpoint, route_id);

    // Setup progress bar
    let pb = crate::util::progress_bar_with(remaining.len() as u64, "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) ETA {eta}", "=> ")?;

    // Setup channels
    let (uuid_tx, uuid_rx) = mpsc::channel::<String>(1024);
    let uuid_rx = Arc::new(Mutex::new(uuid_rx));

    let (journal_tx, journal_rx) = mpsc::channel::<String>(4096);
    let (error_tx, error_rx) = mpsc::channel::<(String, String)>(4096);

    let stats = Arc::new(SharedStats {
        success: AtomicU64::new(0),
        failed: AtomicU64::new(0),
        logged_in: AtomicU64::new(0),
        login_failed: AtomicU64::new(0),
    });

    // Spawn journal writer
    let journal_handle = tokio::spawn(journal_writer(journal_rx, completed_file.to_path_buf()));

    // Spawn error logger
    let error_handle = tokio::spawn(error_logger(error_rx, output_dir.to_path_buf()));

    // Spawn workers (one per account)
    let mut worker_handles = Vec::new();
    for (i, account) in accounts.iter().enumerate() {
        let handle = tokio::spawn(worker(
            i,
            account.clone(),
            password.to_string(),
            endpoint.clone(),
            version.clone(),
            route_id.clone(),
            origin.clone(),
            Arc::clone(&uuid_rx),
            output_dir.to_path_buf(),
            journal_tx.clone(),
            Arc::clone(&stats),
            pb.clone(),
            error_tx.clone(),
            delay_ms,
        ));
        worker_handles.push(handle);
    }

    // Drop our copies of the senders so channels close when workers finish
    drop(journal_tx);
    drop(error_tx);

    // Wait for all workers to finish logging in
    let total_accounts = accounts.len() as u64;
    loop {
        let ok = stats.logged_in.load(Ordering::Relaxed);
        let fail = stats.login_failed.load(Ordering::Relaxed);
        if ok + fail >= total_accounts {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let logged_in = stats.logged_in.load(Ordering::Relaxed);
    let login_failed = stats.login_failed.load(Ordering::Relaxed);
    info!("Logged in: {}/{} accounts ({} failed)", logged_in, total_accounts, login_failed);

    if logged_in == 0 {
        info!("No accounts connected, aborting");
        pb.finish_with_message("No workers");
        drop(uuid_tx);
        for handle in worker_handles {
            let _ = handle.await;
        }
        let _ = journal_handle.await;
        let _ = error_handle.await;
        return Ok((0, 0));
    }

    // Feed UUIDs to the work queue
    for uuid in remaining {
        if uuid_tx.send(uuid).await.is_err() {
            break; // All receivers dropped
        }
    }
    drop(uuid_tx); // Signal no more work

    // Wait for all workers to finish
    for handle in worker_handles {
        let _ = handle.await;
    }

    // Wait for journal and error logger to flush
    let _ = journal_handle.await;
    let _ = error_handle.await;

    pb.finish_with_message("Done");

    let success = stats.success.load(Ordering::Relaxed);
    let failed = stats.failed.load(Ordering::Relaxed);

    Ok((success, failed))
}
