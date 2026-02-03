use anyhow::{Context, Result};
use chrono::NaiveDate;
use std::collections::HashSet;
use tracing::info;

use super::types::{GameRecord, PlayerSearchResult};

const DATA_BASE: &str = "https://5-data.amae-koromo.com/api/v2/pl4";

pub struct AmaeKoromoClient {
    client: reqwest::Client,
    delay_ms: u64,
}

impl AmaeKoromoClient {
    pub fn new(delay_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { client, delay_ms })
    }

    pub async fn search_player(&self, nickname: &str) -> Result<Vec<PlayerSearchResult>> {
        let url = format!("{}/search_player/{}?limit=20", DATA_BASE, nickname);
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
        let url = format!(
            "{}/games/{}/{}?mode={}&limit={}",
            DATA_BASE, start_ms, end_ms, mode, limit
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
            anyhow::bail!("HTTP {}: {}", status, body);
        }

        let records: Vec<GameRecord> = resp.json().await?;

        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        }

        Ok(records)
    }

    /// Fetch ALL records for a player with pagination (handles 500 game limit)
    /// Uses descending mode like the Amae-Koromo website
    /// Returns (all_records, num_api_calls)
    pub async fn get_player_records_paginated(
        &self,
        player_id: i64,
        mode: i32,
    ) -> Result<(Vec<GameRecord>, u32)> {
        let mut all_records = Vec::new();
        let mut end_ms: i64 = chrono::Utc::now().timestamp_millis();
        let start_ms: i64 = 1262304000000; // 2010-01-01
        let mut api_calls = 0u32;

        loop {
            // Descending mode: swap end/start in URL, add descending=true, limit=500
            let url = format!(
                "{}/player_records/{}/{}/{}?mode={}&limit=500&descending=true",
                DATA_BASE, player_id, end_ms, start_ms, mode
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
                anyhow::bail!("HTTP {} for player {}", status, player_id);
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
                .unwrap_or(0);

            all_records.extend(records);

            // If we got fewer than 500, we've reached the end
            if batch_size < 500 {
                break;
            }

            // Set end_ms to oldest game's end_time (in ms) - 1 for next batch
            end_ms = (oldest_end_time * 1000) - 1;

            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
        }

        Ok((all_records, api_calls))
    }

    /// Fetch all games for a specific day (Throne mode) with pagination
    /// Paginates until API returns `invalid_date_range` error
    /// Returns (games, unique_player_ids, api_calls)
    pub async fn fetch_day_games(
        &self,
        date: &str,  // YYYYMMDD format
    ) -> Result<(Vec<GameRecord>, Vec<i64>, u32)> {
        let parsed_date = NaiveDate::parse_from_str(date, "%Y%m%d")
            .context("Invalid date format, expected YYYYMMDD")?;

        // Calculate day boundaries in milliseconds (UTC)
        let day_start_ms = parsed_date
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day_end_ms = parsed_date
            .and_hms_opt(23, 59, 59)
            .unwrap()
            .and_utc()
            .timestamp_millis() + 999; // Include .999

        let mut all_games = Vec::new();
        let mut player_ids = HashSet::new();
        let mut api_calls = 0u32;
        let mut current_end_ms = day_end_ms;

        loop {
            let url = format!(
                "{}/games/{}/{}?limit=500&descending=true&mode=16",
                DATA_BASE, current_end_ms, day_start_ms
            );

            let resp = self.client.get(&url).send().await?;
            api_calls += 1;

            // Check for invalid_date_range error (means we've reached the end)
            if resp.status().is_client_error() {
                let body = resp.text().await.unwrap_or_default();
                if body.contains("invalid_date_range") {
                    break;
                }
                anyhow::bail!("API error: {}", body);
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
                .map(|g| g.start_time)
                .unwrap_or(0);

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
}
