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
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::io::{BufRead, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;

/// Load UUIDs from todo.txt, subtract completed.log, return remaining
fn load_remaining_uuids(
    todo_file: &Path,
    completed_file: &Path,
    output_dir: &Path,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    // Load todo list
    let todo = std::fs::read_to_string(todo_file)
        .with_context(|| format!("Failed to read {}", todo_file.display()))?;
    let all_uuids: Vec<String> = todo.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect();
    info!("Total UUIDs in todo: {}", all_uuids.len());

    // Load completed set
    let mut done = HashSet::new();
    if completed_file.exists() {
        let content = std::fs::read_to_string(completed_file)?;
        for line in content.split('\n') {
            let trimmed = line.trim();
            // Only accept lines that had a proper newline (crash-safe)
            if !trimmed.is_empty() && content.contains(&format!("{}\n", line)) {
                done.insert(trimmed.to_string());
            }
        }
    }

    // Also scan existing .pb files on disk
    if output_dir.exists() {
        for entry in std::fs::read_dir(output_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".pb") {
                let uuid = name[..name.len() - 3].replace('_', "-");
                done.insert(uuid);
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

/// Stats shared between workers
struct SharedStats {
    success: AtomicU64,
    failed: AtomicU64,
    logged_in: AtomicU64,
    login_failed: AtomicU64,
}

/// Worker: owns one RPC connection, pulls UUIDs from receiver, writes .pb files
async fn worker(
    worker_id: usize,
    username: String,
    password: String,
    endpoint: String,
    version: String,
    route_id: String,
    rx: Arc<Mutex<mpsc::Receiver<String>>>,
    output_dir: PathBuf,
    journal_tx: mpsc::Sender<String>,
    stats: Arc<SharedStats>,
    pb: ProgressBar,
    error_tx: mpsc::Sender<(String, String)>,
    delay_ms: u64,
) {
    // Connect and login
    let rpc = match MajsoulRpc::connect(&endpoint).await {
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

    let client_version = format!("web-{}", version);

    // Download loop
    loop {
        let uuid = {
            let mut guard = rx.lock().await;
            guard.recv().await
        };

        let uuid = match uuid {
            Some(u) => u,
            None => break, // Channel closed, no more work
        };

        match rpc.fetch_game_record(&uuid, &client_version).await {
            Ok(data) => {
                // Write raw bytes to disk
                let filename = format!("{}.pb", uuid.replace('-', "_"));
                let filepath = output_dir.join(&filename);

                // Atomic write via temp file
                let tmp_path = output_dir.join(format!(".tmp_{}", filename));
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
) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&completed_file)
        .expect("Failed to open completed.log");

    while let Some(uuid) = rx.recv().await {
        let _ = writeln!(file, "{}", uuid);
        // Line-buffered: flush after each line for crash safety
        let _ = file.flush();
    }
}

/// Error logger: receives (uuid, error) pairs and writes to failed.log
async fn error_logger(
    mut rx: mpsc::Receiver<(String, String)>,
    output_dir: PathBuf,
) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_dir.join("failed.log"))
        .expect("Failed to open failed.log");

    while let Some((uuid, error)) = rx.recv().await {
        let _ = writeln!(file, "{}\t{}", uuid, error);
    }
}

/// Main entry point for multi-account raw download
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
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .build()?;
    let (endpoint, version, route_id) = discover_gateway(&client, server).await?;
    info!("Gateway: {} (route: {})", endpoint, route_id);

    // Setup progress bar
    let pb = ProgressBar::new(remaining.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) ETA {eta}")?
            .progress_chars("=> "),
    );

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
