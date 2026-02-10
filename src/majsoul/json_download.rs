//! Download Majsoul games and convert to Tenhou JSON format.
//!
//! This module provides functionality to download game records from Majsoul
//! and convert them directly to Tenhou JSON format, suitable for analysis
//! tools like Mortal.

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use tracing::{info, warn};

use super::gateway::discover_gateway;
use super::rpc::MajsoulRpc;
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
/// Tuple of (success_count, failed_count)
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
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .build()?;

    let (endpoint, version, route_id) = {
        let mut attempts = 0;
        loop {
            match discover_gateway(&client, server).await {
                Ok(result) => break result,
                Err(e) if attempts < 3 => {
                    attempts += 1;
                    warn!("Gateway discovery failed (attempt {}): {}", attempts, e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => return Err(e),
            }
        }
    };

    // Connect with retry
    let rpc = {
        let mut attempts = 0;
        loop {
            match MajsoulRpc::connect(&endpoint).await {
                Ok(rpc) => break rpc,
                Err(e) if attempts < 3 => {
                    attempts += 1;
                    warn!("Connection failed (attempt {}): {}", attempts, e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => return Err(e),
            }
        }
    };

    // Login with native credentials
    rpc.login_native(username, password, &version, &route_id).await?;

    info!("Downloading {} game records to {:?}", uuids.len(), output_dir);

    let pb = ProgressBar::new(uuids.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"),
    );

    let mut success = 0;
    let mut failed = 0;

    for uuid in &uuids {
        match rpc.fetch_game_record(uuid, "").await {
            Ok(data) => {
                // Get mode_id from database for this UUID
                let mode_id = db.get_majsoul_mode_id(uuid).unwrap_or(16);

                // Convert to Tenhou JSON format
                match convert_to_tenhou(&data, uuid, mode_id as u32) {
                    Ok(tensoul_output) => {
                        if tensoul_output.is_error {
                            warn!("Conversion error for {}: {:?}", uuid, tensoul_output.error_msg);
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
                let err_str = e.to_string();
                warn!("Failed to fetch {}: {}", uuid, err_str);

                // Check for rate limiting
                if err_str.contains("151") {
                    pb.finish_with_message("Rate limited");
                    rpc.close().await?;
                    return Err(anyhow::anyhow!(
                        "Rate limited (error 151). Try again later."
                    ));
                }

                db.mark_majsoul_download_error(uuid)?;
                failed += 1;
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
