use anyhow::{Context, Result};
use chrono::NaiveDate;
use flate2::read::GzDecoder;
use futures::stream::{self, StreamExt};
use regex::Regex;
use std::io::Read;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::db::{Database, LogEntry};

const TENHOU_BASE_URL: &str = "https://tenhou.net/sc/raw/dat";

pub struct Fetcher {
    client: reqwest::Client,
    delay_ms: u64,
}

impl Fetcher {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = crate::util::http_client()?;
        Ok(Self { client, delay_ms })
    }

    // Allow: date-range fetch is sequential retry boilerplate; splitting would churn the batch loop.
    #[allow(clippy::too_many_lines)]
    pub async fn fetch_date_range(
        &self,
        db: &Database,
        start: NaiveDate,
        end: NaiveDate,
        log_types: &[&str],
        skip_fetched: bool,
        concurrent: usize,
    ) -> Result<usize> {
        // Sanitize log types: trim, drop empties, require at least one.
        let requested: Vec<String> = log_types
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if requested.is_empty() {
            anyhow::bail!("--log-types must name at least one type");
        }

        // Collect all dates to process

        let mut current = start;
        let mut dates_to_fetch = Vec::new();
        while current <= end {
            let date_str = current.format("%Y%m%d").to_string();

            // Tenhou politeness (rule 1): re-check a date file at most every 20 minutes,
            // even after failures or interrupted runs. Applies to forced re-fetches too.
            if db.was_fetch_checked_within(&date_str, 20)? {
                info!("Skipping {}: checked within 20 minutes (Tenhou politeness)", date_str);
            } else if skip_fetched && db.is_date_fetched(&date_str)? {
                info!("Skipping already fetched date: {}", date_str);
            } else {
                dates_to_fetch.push(current);
            }
            current = match current.succ_opt() {
                Some(d) => d,
                None => break,
            };
        }

        if dates_to_fetch.is_empty() {
            info!("No dates to fetch");
            return Ok(0);
        }

        info!(
            "Fetching {} dates (concurrent: {})",
            dates_to_fetch.len(),
            concurrent
        );

        let pb = crate::util::progress_bar(dates_to_fetch.len() as u64)?;

        let total_new = Arc::new(AtomicUsize::new(0));
        let log_types = requested;

        let results: Vec<_> = stream::iter(dates_to_fetch)
            .map(|date| {
                let client = self.client.clone();
                let delay_ms = self.delay_ms;
                let log_types = log_types.clone();
                let _total_new = Arc::clone(&total_new);
                let pb = pb.clone();

                async move {
                    let date_str = date.format("%Y%m%d").to_string();
                    let year = date.format("%Y").to_string();
                    let mut entries_for_date = Vec::new();
                    let mut any_ok = false;
                    let mut skipped_3p = 0usize;

                    for log_type in &log_types {
                        let url = format!(
                            "{TENHOU_BASE_URL}/{year}/{log_type}{date_str}.html.gz"
                        );

                        match Self::fetch_log_ids_from_url_static(&client, &url).await {
                            Ok((entries, skipped)) => {
                                any_ok = true;
                                skipped_3p += skipped;
                                let count = entries.len();
                                entries_for_date.extend(entries);
                                info!(
                                    "Fetched {} {} - {} entries",
                                    log_type, date_str, count
                                );
                            }
                            Err(e) => {
                                warn!("Failed to fetch {} {}: {}", log_type, date_str, e);
                            }
                        }

                        if delay_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        }
                    }

                    if skipped_3p > 0 {
                        info!(
                            "Skipped {} 3-player games (4p-only mode) on {}",
                            skipped_3p, date_str
                        );
                    }

                    pb.inc(1);
                    (date_str, entries_for_date, any_ok)
                }
            })
            .buffer_unordered(concurrent)
            .collect()
            .await;

        pb.finish_with_message("Done");

        // Insert results into database (must be sequential for SQLite).
        // A date is marked fetched only when at least one fetch succeeded
        // and yielded entries; otherwise it stays unmarked for retry.
        let mut total = 0;
        for (date_str, entries, any_ok) in results {
            // Every attempt counts for the 20-minute re-check throttle, success or not.
            db.record_fetch_attempt(&date_str)?;
            let mut new_count = 0;
            for entry in &entries {
                if db.insert_log_id(entry)? {
                    new_count += 1;
                }
            }
            if !entries.is_empty() {
                info!(
                    "Inserted {} - {} total, {} new",
                    date_str,
                    entries.len(),
                    new_count
                );
            }
            total += new_count;
            if any_ok && !entries.is_empty() {
                db.mark_date_fetched(&date_str)?;
            } else {
                warn!(
                    "Date {} not marked as fetched (any_ok={}, entries={}), will retry",
                    date_str,
                    any_ok,
                    entries.len()
                );
            }
        }

        Ok(total)
    }

    async fn fetch_log_ids_from_url_static(
        client: &reqwest::Client,
        url: &str,
    ) -> Result<(Vec<LogEntry>, usize)> {
        let response = client.get(url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }

        let bytes = response.bytes().await?;

        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut html = String::new();
        decoder
            .read_to_string(&mut html)
            .context("Failed to decompress gzip")?;

        Self::parse_log_ids_static(&html)
    }

    pub(crate) fn parse_log_ids_static(html: &str) -> Result<(Vec<LogEntry>, usize)> {
        // Pattern: log=2025010100gm-00a9-0000-d7141b66
        let log_id_re = Regex::new(r"log=(\d{10}gm-([0-9a-f]{4})-[0-9a-f]{4}-[0-9a-f]{8})")?;

        let mut entries = Vec::new();
        let mut skipped_3p = 0usize;

        for cap in log_id_re.captures_iter(html) {
            let full_id = cap.get(1).unwrap().as_str();
            let game_type_str = cap.get(2).unwrap().as_str();

            // Parse game type using bitmask (same as houou-logs)
            // Bit 4 (0x010) = 3-player if set, 4-player if unset
            // Bit 3 (0x008) = Hanchan if set, Tonpu if unset
            let Ok(game_type) = u16::from_str_radix(game_type_str, 16) else {
                continue;
            };

            let is_3p = (game_type & 0x010) != 0;
            let is_hanchan = (game_type & 0x008) != 0;

            // Only capture 4-player games
            if is_3p {
                skipped_3p += 1;
                continue;
            }

            let num_players = 4;

            // Extract date from log ID (first 8 chars)
            let date = &full_id[0..8];

            entries.push(LogEntry {
                id: full_id.to_string(),
                date: date.to_string(),
                num_players,
                is_hanchan,
                is_downloaded: false,
                is_converted: false,
                _xml_data: None,
            });
        }

        Ok((entries, skipped_3p))
    }

    /// Import log IDs from a directory of `*.html.gz` files (no network).
    /// Returns (`new_ids`, `files_ok`, `files_failed`).
    pub fn import_html_gz_dir(db: &Database, dir: &std::path::Path) -> Result<(usize, usize, usize)> {
        let mut total_new = 0usize;
        let mut files_ok = 0usize;
        let mut files_failed = 0usize;
        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("gz") {
                continue;
            }
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !fname.ends_with(".html.gz") {
                continue;
            }
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    warn!("import: failed to read {}: {}", path.display(), e);
                    files_failed += 1;
                    continue;
                }
            };
            let mut decoder = GzDecoder::new(&bytes[..]);
            let mut html = String::new();
            if let Err(e) = decoder.read_to_string(&mut html) {
                warn!("import: failed to gunzip {}: {}", path.display(), e);
                files_failed += 1;
                continue;
            }
            let (entries, _) = Self::parse_log_ids_static(&html)?;
            if entries.is_empty() {
                warn!("import: {} contained no log IDs, not marked", path.display());
                continue;
            }
            let mut new_here = 0usize;
            let mut dates = std::collections::HashSet::new();
            for entry in &entries {
                if db.insert_log_id(entry)? {
                    new_here += 1;
                }
                dates.insert(entry.date.clone());
            }
            for date in dates {
                db.mark_date_fetched(&date)?;
            }
            total_new += new_here;
            files_ok += 1;
        }
        Ok((total_new, files_ok, files_failed))
    }
}
