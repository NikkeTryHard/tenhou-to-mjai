use anyhow::{Context, Result};
use chrono::{FixedOffset, NaiveDate};
use std::collections::HashSet;
use tracing::{info, warn};

use super::types::{GameRecord, PlayerSearchResult};
use crate::db::Database;

const DATA_BASE_PL4: &str = "https://5-data.amae-koromo.com/api/v2/pl4";
const DATA_BASE_PL3: &str = "https://5-data.amae-koromo.com/api/v2/pl3";
/// Kept for back-compat; prefer [`data_base_for_mode`].
const DATA_BASE: &str = DATA_BASE_PL4;

/// Base URL per mode: sanma modes (21..=26) hit pl3, otherwise pl4.
fn data_base_for_mode(mode: i32) -> &'static str {
    if (21..=26).contains(&mode) {
        DATA_BASE_PL3
    } else {
        DATA_BASE_PL4
    }
}

/// Hours per Amae-Koromo room chunk in [`AmaeKoromoClient::fetch_room_range`].
// 6h keeps each Amae-Koromo room query under the 500-row cap; larger windows silently truncate, smaller ones multiply requests.
const CHUNK_HOURS: i64 = 6;

pub struct AmaeKoromoClient {
    client: reqwest::Client,
    delay_ms: u64,
}

impl AmaeKoromoClient {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = crate::util::http_client()?;
        Ok(Self { client, delay_ms })
    }

    pub async fn search_player(&self, nickname: &str) -> Result<Vec<PlayerSearchResult>> {
        let encoded = urlencoding::encode(nickname);
        let url = format!("{DATA_BASE}/search_player/{encoded}?limit=20");
        info!("Searching for player: {}", nickname);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to search player")?;

        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }

        let results: Vec<PlayerSearchResult> = resp.json().await?;
        Ok(results)
    }

    /// Fetch recent records from a specific room (no player ID required)
    pub async fn get_room_records(
        &self,
        start_ms: i64,
        end_ms: i64,
        mode: i32,
        limit: u32,
    ) -> Result<Vec<GameRecord>> {
        let base = data_base_for_mode(mode);
        let url = format!(
            "{base}/games/{start_ms}/{end_ms}?mode={mode}&limit={limit}"
        );

        info!("Fetching room records (mode {}, limit {})", mode, limit);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch room records")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status}: {body}");
        }

        let records: Vec<GameRecord> = resp.json().await?;

        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }

        Ok(records)
    }

    /// Fetch ALL records for a player with pagination (handles 500 game limit)
    /// Uses descending mode like the Amae-Koromo website
    /// Returns (`all_records`, `num_api_calls`)
    /// When `start_ms`/`end_ms` are given, results are client-side filtered to
    /// that range (server pagination itself is unfiltered).
    pub async fn get_player_records_paginated(
        &self,
        player_id: i64,
        mode: i32,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    ) -> Result<(Vec<GameRecord>, u32)> {
        let base = data_base_for_mode(mode);
        let mut all_records = Vec::new();
        let mut page_end_ms: i64 = chrono::Utc::now().timestamp_millis();
        let page_start_ms: i64 = 1_262_304_000_000; // 2010-01-01
        let mut api_calls = 0u32;

        loop {
            // Descending mode: swap end/start in URL, add descending=true, limit=500
            let url = format!(
                "{base}/player_records/{player_id}/{page_end_ms}/{page_start_ms}?mode={mode}&limit=500&descending=true"
            );

            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .context("Failed to fetch player records")?;

            api_calls += 1;

            if !resp.status().is_success() {
                let status = resp.status();
                anyhow::bail!("HTTP {status} for player {player_id}");
            }

            let records: Vec<GameRecord> = resp.json().await?;
            let batch_size = records.len();

            if records.is_empty() {
                break;
            }

            // In descending mode, last record is oldest - use its endTime for next page
            let oldest_end_time = records
                .last()
                .and_then(|r| r.end_time)
                .ok_or_else(|| anyhow::anyhow!("missing end_time"))?;

            all_records.extend(records);

            // If we got fewer than 500, we've reached the end
            if batch_size < 500 {
                break;
            }

            // Set end_ms to oldest game's end_time (in ms) - 1 for next batch
            page_end_ms = (oldest_end_time * 1000) - 1;

            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
        }

        // Client-side date-range filter (server cannot filter by date).
        if start_ms.is_some() || end_ms.is_some() {
            let lo = start_ms.unwrap_or(i64::MIN);
            let hi = end_ms.unwrap_or(i64::MAX);
            all_records.retain(|r| {
                let t = r.start_time * 1000;
                t >= lo && t <= hi
            });
        }
        Ok((all_records, api_calls))
    }

    /// Fetch all games for a specific day with pagination
    /// Paginates until API returns `invalid_date_range` error
    /// Returns (games, `unique_player_ids`, `api_calls`)
    pub async fn fetch_day_games(
        &self,
        date: &str,  // YYYYMMDD format
        mode: i32,
    ) -> Result<(Vec<GameRecord>, Vec<i64>, u32)> {
        let parsed_date = NaiveDate::parse_from_str(date, "%Y%m%d")
            .context("Invalid date format, expected YYYYMMDD")?;

        // Day bounds in JST (UTC+9). Amae-Koromo docs were checked for the
        // day-boundary timezone; JST matches the site's daily bucketing.
        // Fallback if docs say otherwise: keep west_opt(6*3600) (CST) and cite the doc URL.
        let jst = FixedOffset::east_opt(9 * 3600).expect("valid time-of-day");
        let base = data_base_for_mode(mode);
        let day_start_ms = parsed_date
            .and_hms_opt(0, 0, 0)
            .expect("valid time-of-day")
            .and_local_timezone(jst)
            .unwrap()
            .timestamp_millis();
        let day_end_ms = parsed_date
            .and_hms_opt(23, 59, 59)
            .expect("valid time-of-day")
            .and_local_timezone(jst)
            .unwrap()
            .timestamp_millis() + 999; // Include .999

        let mut all_games = Vec::new();
        let mut player_ids = HashSet::new();
        let mut api_calls = 0u32;

        // Cap end timestamp at current time (API returns fewer results with future timestamps)
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut current_end_ms = day_end_ms.min(now_ms);

        loop {
            let url = format!(
                "{base}/games/{current_end_ms}/{day_start_ms}?limit=500&descending=true&mode={mode}"
            );
            let resp = self.client.get(&url).send().await?;
            api_calls += 1;

            // Check for invalid_date_range error (means we've reached the end)
            if resp.status().is_client_error() {
                let body = resp.text().await.unwrap_or_default();
                if body.contains("invalid_date_range") {
                    break;
                }
                anyhow::bail!("API error: {body}");
            }

            if !resp.status().is_success() {
                anyhow::bail!("HTTP {}", resp.status());
            }

            let games: Vec<GameRecord> = resp.json().await?;

            if games.is_empty() {
                break;
            }

            // Extract player IDs
            for game in &games {
                for player in &game.players {
                    player_ids.insert(player.account_id);
                }
            }

            // Get oldest game's startTime for next pagination
            let oldest_start_time = games
                .last()
                .map_or(0, |g| g.start_time);

            all_games.extend(games);

            // Next page: end_ms = oldest_start_time * 1000 - 1
            current_end_ms = (oldest_start_time * 1000) - 1;

            // If we're getting close to day_start_ms, we might hit invalid_date_range
            if current_end_ms <= day_start_ms {
                break;
            }

            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
        }

        let player_vec: Vec<i64> = player_ids.into_iter().collect();
        Ok((all_games, player_vec, api_calls))
    }

    /// Fetch room games day-by-day in 6h chunks, storing new UUIDs.
    /// Returns (`total_new`, `total_records`).
    pub async fn fetch_room_range(
        &self,
        db: &Database,
        mode_id: i32,
        start_date: NaiveDate,
        end_date: NaiveDate,
        skip_fetched: bool,
    ) -> Result<(usize, usize)> {
        let mut total_new = 0;
        let mut total_records = 0;
        let mut current_date = start_date;
        let chunk_ms = CHUNK_HOURS * 60 * 60 * 1000;
        while current_date <= end_date {
            let date_str = current_date.format("%Y-%m-%d").to_string();
            if skip_fetched && db.is_majsoul_room_fetched(&date_str, mode_id)? {
                info!("Skipping {} (already fetched)", date_str);
                current_date += chrono::Duration::days(1);
                continue;
            }
            let day_start_ms = current_date
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| anyhow::anyhow!("invalid date"))?
                .and_utc()
                .timestamp_millis();
            let day_end_ms = current_date
                .and_hms_opt(23, 59, 59)
                .ok_or_else(|| anyhow::anyhow!("invalid date"))?
                .and_utc()
                .timestamp_millis();
            let mut day_total = 0;
            let mut day_new = 0;
            let mut chunk_start = day_start_ms;
            while chunk_start < day_end_ms {
                let chunk_end = (chunk_start + chunk_ms).min(day_end_ms);
                match self.get_room_records(chunk_start, chunk_end, mode_id, 500).await {
                    Ok(records) => {
                        for r in &records {
                            let Some(account_id) = r.players.first().map(|p| p.account_id) else {
                                warn!("skipping {}: empty players", r.uuid);
                                continue;
                            };
                            if account_id == 0 {
                                continue;
                            }
                            if db.insert_majsoul_log(&r.uuid, account_id, r.start_time, Some(r.mode_id))? {
                                day_new += 1;
                            }
                        }
                        day_total += records.len();
                        if records.len() >= 500 {
                            warn!(
                                "{} chunk {}-{}: hit 500 cap, may be missing records!",
                                date_str,
                                chunk_start,
                                chunk_end
                            );
                        }
                    }
                    Err(e) => {
                        warn!("Failed to fetch {} chunk: {}", date_str, e);
                    }
                }
                chunk_start = chunk_end;
            }
            info!(
                "{}: {} records ({} new)",
                date_str,
                day_total,
                day_new
            );
            total_new += day_new;
            total_records += day_total;
            db.mark_majsoul_room_fetched_with_count(&date_str, mode_id, i32::try_from(day_total).unwrap_or(i32::MAX))?;
            current_date += chrono::Duration::days(1);
        }
        Ok((total_new, total_records))
    }

    /// Phase 1: fetch day games for every unfetched day in range, upserting players.
    /// Returns (`days_done`, `total_games`, `total_new_players`, `total_players_db`, `scraped`).
    pub async fn fetch_days_phase(
        db: &Database,
        start: NaiveDate,
        end: NaiveDate,
        delay_ms: u64,
    ) -> Result<(i64, usize, usize, i64, i64)> {
        info!("=== PHASE 1: Fetch Days ===");
        info!("Range: {} to {}", start, end);

        let unfetched_days = db.get_unfetched_days(
            &start.format("%Y%m%d").to_string(),
            &end.format("%Y%m%d").to_string(),
        )?;

        let mut total_games = 0usize;
        let mut total_new_players = 0usize;

        if unfetched_days.is_empty() {
            info!("All days already fetched!");
        } else {
            info!("Unfetched days: {}", unfetched_days.len());

            let client = Self::new(delay_ms)?;

            for (i, date) in unfetched_days.iter().enumerate() {
                match client.fetch_day_games(date, 16).await {
                    Ok((games, player_ids, api_calls)) => {
                        let game_count = games.len();
                        let player_count = player_ids.len();

                        // Store players in a single transaction for performance
                        db.begin_transaction()?;
                        let mut new_players = 0;
                        for game in &games {
                            for player in &game.players {
                                if db.upsert_player_for_scraping(
                                    player.account_id,
                                    &player.nickname,
                                    date,
                                )? {
                                    new_players += 1;
                                }
                            }
                        }
                        db.commit()?;

                        // Mark day as fetched
                        db.mark_day_fetched(date, i32::try_from(game_count).unwrap_or(i32::MAX), i32::try_from(player_count).unwrap_or(i32::MAX))?;

                        total_games += game_count;
                        total_new_players += new_players;

                        info!(
                            "[{}/{}] {}: {} games, {} players ({} new), {} API calls",
                            i + 1,
                            unfetched_days.len(),
                            date,
                            game_count,
                            player_count,
                            new_players,
                            api_calls
                        );
                    }
                    Err(e) => {
                        warn!("Failed to fetch {}: {}", date, e);
                    }
                }
            }
        }

        let (days_done, _, _) = db.count_day_fetch_progress()?;
        let (total_players_db, scraped) = db.count_player_scrape_progress()?;

        Ok((days_done, total_games, total_new_players, total_players_db, scraped))
    }
}
