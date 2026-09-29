//! Download Majsoul games and convert to Tenhou JSON format.
//!
//! This module provides functionality to download game records from Majsoul
//! and convert them directly to Tenhou JSON format, suitable for analysis
//! tools like Mortal.

use anyhow::Result;
use std::path::Path;
use tracing::{info, warn};

use super::gateway::{FetchOutcome, classify_outcome, discover_and_connect};
use super::to_tenhou::convert_to_tenhou;
use crate::db::Database;

/// Download games from Majsoul and save as Tenhou JSON files.
///
/// # Arguments
/// * `db` - Database containing game UUIDs to download
/// * `output_dir` - Directory to save JSON files
/// * `limit` - Maximum number of games to download
/// * `username` - Username for native login (required)
/// * `password` - Password for native login (required)
/// * `delay_ms` - Delay between requests in milliseconds
/// * `server` - Server region (en, jp)
///
/// # Returns
/// Tuple of (`success_count`, `failed_count`)
// Length is gateway/login/retry boilerplate around one per-uuid loop (D2
// error policy); splitting would churn the verified retry behavior.
#[allow(clippy::too_many_lines)]
pub async fn download_as_json(
    db: &Database,
    output_dir: &Path,
    limit: Option<usize>,
    username: &str,
    password: &str,
    delay_ms: u64,
    server: &str,
) -> Result<(usize, usize)> {
    // Create output directory if it doesn't exist
    tokio::fs::create_dir_all(output_dir).await?;

    // Get UUIDs to download (games with full_uuid that haven't been downloaded)
    let uuids = db.get_majsoul_undownloaded_with_full_uuid(limit)?;
    if uuids.is_empty() {
        info!("No pending downloads");
        return Ok((0, 0));
    }

    info!("Found {} games to download", uuids.len());
    info!("Using native login for {}", username);

    // Connect to Majsoul gateway
    let client = crate::util::http_client()?;

    // Discover gateway, connect, and login with retry (per-server Origin
    // handled inside the helper).
    let (rpc, version) = {
        let mut attempts = 0;
        loop {
            match discover_and_connect(&client, server, username, password).await {
                Ok(result) => break result,
                Err(e) if attempts < 3 => {
                    attempts += 1;
                    warn!("Gateway connect failed (attempt {}): {}", attempts, e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => return Err(e),
            }
        }
    };

    // version.json "X.Y.w" -> login/fetch "web-X.Y" (see gateway.rs).
    let client_version = format!("web-{}", version.replace(".w", ""));

    info!("Downloading {} game records to {:?}", uuids.len(), output_dir);

    let pb = crate::util::progress_bar(uuids.len() as u64)?;

    let mut success = 0;
    let mut failed = 0;
    let mut version_retries = 0u32;

    for uuid in &uuids {
        match rpc.fetch_game_record(uuid, &client_version).await {
            Ok(data) => {
                // Get mode_id from database for this UUID (propagate DB errors).
                let mode_id = db.get_majsoul_mode_id(uuid)?;
                let mode_u32 = u32::try_from(mode_id)
                    .map_err(|_| anyhow::anyhow!("mode_id out of range: {mode_id}"))?;

                // Convert to Tenhou JSON format
                match convert_to_tenhou(&data, uuid, mode_u32) {
                    Ok(tensoul_output) => {
                        if tensoul_output.is_error {
                            // Gate: error output is a download failure, never written.
                            warn!("Conversion error for {}: {:?}", uuid, tensoul_output.error_msg);
                            if let Err(db_err) = db.mark_majsoul_download_error(uuid) {
                                warn!("Failed to mark error for {}: {}", uuid, db_err);
                            }
                            failed += 1;
                        } else if let Some(log) = tensoul_output.log {
                            // Save as JSON file
                            let filename = format!("{}.json", uuid.replace(['/', '\\', ':'], "_"));
                            let filepath = output_dir.join(&filename);

                            match serde_json::to_string_pretty(&log) {
                                Ok(json) => {
                                    if let Err(e) = tokio::fs::write(&filepath, json).await {
                                        warn!("Failed to write {}: {}", filename, e);
                                        failed += 1;
                                    } else {
                                        // Mark as downloaded in DB
                                        if let Err(e) = db.mark_majsoul_downloaded(uuid, &data) {
                                            warn!("Failed to mark {} as downloaded: {}", uuid, e);
                                        }
                                        success += 1;
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to serialize {}: {}", uuid, e);
                                    failed += 1;
                                }
                            }
                        } else {
                            warn!("No log data for {}", uuid);
                            failed += 1;
                        }
                    }
                    Err(e) => {
                        warn!("Conversion failed for {}: {}", uuid, e);
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch {}: {}", uuid, e);
                match classify_outcome(&e) {
                    FetchOutcome::Done => {
                        if version_retries >= 3 {
                            pb.finish_with_message("Version mismatch");
                            rpc.close().await?;
                            anyhow::bail!("Version mismatch persists after 3 gateway rediscoveries");
                        }
                        version_retries += 1;
                        warn!("Version mismatch, sleeping 60s (retry {})", version_retries);
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        if let Err(db_err) = db.mark_majsoul_download_error(uuid) {
                            warn!("Failed to mark error for {}: {}", uuid, db_err);
                        }
                        failed += 1;
                    }
                    FetchOutcome::Abort(err) => {
                        pb.finish_with_message("Rate limited");
                        rpc.close().await?;
                        return Err(err);
                    }
                    FetchOutcome::MarkAndContinue => {
                        db.mark_majsoul_download_error(uuid)?;
                        failed += 1;
                    }
                }
            }
        }

        pb.inc(1);
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    }

    pb.finish_with_message("Done");
    rpc.close().await?;

    Ok((success, failed))
}
