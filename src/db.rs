use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

/// Amae-Koromo API returns max 200 records per player_records request
const AMAE_KOROMO_PAGE_LIMIT: i64 = 200;

pub struct Database {
    pub conn: Connection,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: String,
    pub date: String,
    pub num_players: i32,
    pub is_hanchan: bool,
    pub is_downloaded: bool,
    pub is_converted: bool,
    pub xml_data: Option<Vec<u8>>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn enable_wal_mode(&self) -> Result<()> {
        // journal_mode returns the mode name as string
        let _: String = self.conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        // busy_timeout returns the timeout value as integer
        let _: i64 = self.conn.query_row("PRAGMA busy_timeout = 5000", [], |row| row.get(0))?;
        Ok(())
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS logs (
                id TEXT PRIMARY KEY,
                date TEXT NOT NULL,
                num_players INTEGER NOT NULL,
                is_hanchan INTEGER NOT NULL,
                is_downloaded INTEGER NOT NULL DEFAULT 0,
                is_converted INTEGER NOT NULL DEFAULT 0,
                xml_data BLOB
            );

            CREATE TABLE IF NOT EXISTS fetch_state (
                date TEXT PRIMARY KEY,
                fetched_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_logs_downloaded ON logs(is_downloaded);
            CREATE INDEX IF NOT EXISTS idx_logs_converted ON logs(is_converted);

            -- Majsoul tables
            CREATE TABLE IF NOT EXISTS majsoul_players (
                id INTEGER PRIMARY KEY,
                nickname TEXT NOT NULL,
                level_id INTEGER,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS majsoul_logs (
                uuid TEXT PRIMARY KEY,
                player_id INTEGER NOT NULL,
                start_time INTEGER NOT NULL,
                mode_id INTEGER,
                num_players INTEGER,
                is_hanchan INTEGER,
                is_downloaded INTEGER DEFAULT 0,
                is_converted INTEGER DEFAULT 0,
                raw_data BLOB,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_majsoul_logs_downloaded ON majsoul_logs(is_downloaded);
            CREATE INDEX IF NOT EXISTS idx_majsoul_logs_converted ON majsoul_logs(is_converted);
            CREATE INDEX IF NOT EXISTS idx_majsoul_logs_mode_id ON majsoul_logs(mode_id);

            -- Majsoul room fetch state tracking
            CREATE TABLE IF NOT EXISTS majsoul_room_fetch_state (
                date TEXT NOT NULL,
                mode_id INTEGER NOT NULL,
                fetched_at TEXT DEFAULT CURRENT_TIMESTAMP,
                record_count INTEGER DEFAULT 0,
                PRIMARY KEY (date, mode_id)
            );

            -- Throne room players for full UUID fetching
            CREATE TABLE IF NOT EXISTS throne_players (
                account_id INTEGER PRIMARY KEY,
                nickname TEXT,
                fetched_at TEXT
            );
            ",
        )?;

        // Schema migration: add columns if they don't exist (for existing databases)
        let _ = self.conn.execute("ALTER TABLE majsoul_logs ADD COLUMN num_players INTEGER", []);
        let _ = self.conn.execute("ALTER TABLE majsoul_logs ADD COLUMN is_hanchan INTEGER", []);
        let _ = self.conn.execute("ALTER TABLE majsoul_logs ADD COLUMN full_uuid TEXT", []);

        // Create index on full_uuid after migration ensures column exists
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_majsoul_logs_full_uuid ON majsoul_logs(full_uuid)",
            [],
        )?;

        Ok(())
    }

    pub fn insert_log_id(&self, entry: &LogEntry) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO logs (id, date, num_players, is_hanchan, is_downloaded, is_converted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id,
                entry.date,
                entry.num_players,
                entry.is_hanchan as i32,
                entry.is_downloaded as i32,
                entry.is_converted as i32,
            ],
        )?;
        Ok(result > 0)
    }

    pub fn get_undownloaded_ids(&self, limit: Option<usize>) -> Result<Vec<String>> {
        let mut ids = Vec::new();

        if let Some(n) = limit {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM logs WHERE is_downloaded <= 0 ORDER BY id LIMIT ?1")?;
            let rows = stmt.query_map([n], |row| row.get(0))?;
            for id in rows {
                ids.push(id?);
            }
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM logs WHERE is_downloaded <= 0 ORDER BY id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            for id in rows {
                ids.push(id?);
            }
        }

        Ok(ids)
    }

    pub fn mark_downloaded(&self, id: &str, xml_data: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_downloaded = 1, xml_data = ?1 WHERE id = ?2",
            params![xml_data, id],
        )?;
        Ok(())
    }

    pub fn mark_download_error(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_downloaded = -1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn get_unconverted_logs(
        &self,
        limit: Option<usize>,
        num_players: Option<i32>,
        hanchan_only: bool,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut sql = String::from(
            "SELECT id, xml_data FROM logs
             WHERE is_downloaded = 1 AND is_converted = 0 AND xml_data IS NOT NULL",
        );

        if let Some(players) = num_players {
            sql.push_str(&format!(" AND num_players = {}", players));
        }

        if hanchan_only {
            sql.push_str(" AND is_hanchan = 1");
        }

        sql.push_str(" ORDER BY id");

        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    pub fn mark_converted(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_converted = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn mark_date_fetched(&self, date: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO fetch_state (date, fetched_at) VALUES (?1, datetime('now'))",
            params![date],
        )?;
        Ok(())
    }

    pub fn is_date_fetched(&self, date: &str) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM fetch_state WHERE date = ?1",
            params![date],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    // Majsoul methods
    pub fn insert_majsoul_player(&self, id: i64, nickname: &str, level_id: Option<i32>) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO majsoul_players (id, nickname, level_id) VALUES (?1, ?2, ?3)",
            params![id, nickname, level_id],
        )?;
        Ok(result > 0)
    }

    /// Extract short UUID from potentially full UUID (strips YYMMDD- prefix if present)
    pub fn normalize_uuid(uuid: &str) -> &str {
        // Full UUID format: "250101-a7d2bfbf-dac8-45b9-a667-861f82589725"
        // Short UUID format: "a7d2bfbf-dac8-45b9-a667-861f82589725"
        if uuid.len() > 7 && uuid.chars().nth(6) == Some('-') {
            // Check if first 6 chars are digits (YYMMDD)
            if uuid[..6].chars().all(|c| c.is_ascii_digit()) {
                return &uuid[7..];
            }
        }
        uuid
    }

    pub fn insert_majsoul_log(
        &self,
        uuid: &str,
        player_id: i64,
        start_time: i64,
        mode_id: Option<i32>,
    ) -> Result<bool> {
        let short_uuid = Self::normalize_uuid(uuid);
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO majsoul_logs (uuid, player_id, start_time, mode_id) VALUES (?1, ?2, ?3, ?4)",
            params![short_uuid, player_id, start_time, mode_id],
        )?;
        Ok(result > 0)
    }

    /// Insert majsoul log with full UUID (from player_records API)
    /// Normalizes to short UUID for primary key, stores full UUID separately
    pub fn insert_majsoul_log_with_full_uuid(
        &self,
        full_uuid: &str,
        player_id: i64,
        start_time: i64,
        mode_id: Option<i32>,
    ) -> Result<bool> {
        let short_uuid = Self::normalize_uuid(full_uuid);
        // Try insert first
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![short_uuid, player_id, start_time, mode_id, full_uuid],
        )?;
        // If already exists, update full_uuid if it was missing
        if result == 0 {
            self.conn.execute(
                "UPDATE majsoul_logs SET full_uuid = ?1 WHERE uuid = ?2 AND full_uuid IS NULL",
                params![full_uuid, short_uuid],
            )?;
        }
        Ok(result > 0)
    }

    pub fn count_majsoul_logs(&self) -> Result<(i64, i64, i64)> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM majsoul_logs", [], |row| row.get(0))?;
        let downloaded: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE is_downloaded = 1",
            [],
            |row| row.get(0),
        )?;
        let converted: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE is_converted = 1",
            [],
            |row| row.get(0),
        )?;
        Ok((total, downloaded, converted))
    }

    pub fn count_logs(&self) -> Result<(i64, i64, i64)> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))?;
        let downloaded: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM logs WHERE is_downloaded = 1",
            [],
            |row| row.get(0),
        )?;
        let converted: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM logs WHERE is_converted = 1",
            [],
            |row| row.get(0),
        )?;
        Ok((total, downloaded, converted))
    }

    pub fn get_majsoul_undownloaded(&self, limit: Option<usize>) -> Result<Vec<String>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT uuid FROM majsoul_logs WHERE is_downloaded = 0 ORDER BY start_time LIMIT {}",
                n
            ),
            None => "SELECT uuid FROM majsoul_logs WHERE is_downloaded = 0 ORDER BY start_time".to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut uuids = Vec::new();
        for uuid in rows {
            uuids.push(uuid?);
        }
        Ok(uuids)
    }

    pub fn mark_majsoul_downloaded(&self, uuid: &str, raw_data: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_logs SET is_downloaded = 1, raw_data = ?1 WHERE uuid = ?2",
            params![raw_data, uuid],
        )?;
        Ok(())
    }

    pub fn mark_majsoul_download_error(&self, uuid: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_logs SET is_downloaded = -1 WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(())
    }

    /// Get undownloaded Majsoul logs that have a full_uuid (required for download).
    ///
    /// Returns full_uuid values for records where:
    /// - is_downloaded = 0
    /// - full_uuid IS NOT NULL
    pub fn get_majsoul_undownloaded_with_full_uuid(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<String>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT full_uuid FROM majsoul_logs WHERE is_downloaded = 0 AND full_uuid IS NOT NULL ORDER BY start_time LIMIT {}",
                n
            ),
            None => "SELECT full_uuid FROM majsoul_logs WHERE is_downloaded = 0 AND full_uuid IS NOT NULL ORDER BY start_time".to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut uuids = Vec::new();
        for uuid in rows {
            uuids.push(uuid?);
        }
        Ok(uuids)
    }

    /// Count Majsoul logs that are downloadable (have full_uuid but not yet downloaded).
    pub fn count_majsoul_downloadable(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE is_downloaded = 0 AND full_uuid IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get unconverted Majsoul logs (downloaded but not yet converted)
    pub fn get_majsoul_unconverted(
        &self,
        limit: Option<usize>,
        num_players: Option<i32>,
        hanchan_only: bool,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut sql = String::from(
            "SELECT uuid, raw_data FROM majsoul_logs
             WHERE is_downloaded = 1 AND is_converted = 0 AND raw_data IS NOT NULL",
        );

        if let Some(players) = num_players {
            sql.push_str(&format!(" AND num_players = {}", players));
        }

        if hanchan_only {
            sql.push_str(" AND is_hanchan = 1");
        }

        sql.push_str(" ORDER BY start_time");

        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Mark a Majsoul log as converted
    pub fn mark_majsoul_converted(&self, uuid: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_logs SET is_converted = 1 WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(())
    }

    // Majsoul room fetch state methods

    /// Check if a specific date and mode has been fetched
    pub fn is_majsoul_room_fetched(&self, date: &str, mode_id: i32) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_room_fetch_state WHERE date = ?1 AND mode_id = ?2",
            params![date, mode_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Mark a date and mode as fetched
    pub fn mark_majsoul_room_fetched(&self, date: &str, mode_id: i32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO majsoul_room_fetch_state (date, mode_id, fetched_at)
             VALUES (?1, ?2, datetime('now'))",
            params![date, mode_id],
        )?;
        Ok(())
    }

    /// Mark a date and mode as fetched with record count
    pub fn mark_majsoul_room_fetched_with_count(&self, date: &str, mode_id: i32, count: i32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO majsoul_room_fetch_state (date, mode_id, fetched_at, record_count)
             VALUES (?1, ?2, datetime('now'), ?3)",
            params![date, mode_id, count],
        )?;
        Ok(())
    }

    /// Count Majsoul logs grouped by mode
    pub fn count_majsoul_logs_by_mode(&self) -> Result<Vec<(i32, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT mode_id, COUNT(*) FROM majsoul_logs WHERE mode_id IS NOT NULL GROUP BY mode_id ORDER BY mode_id"
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Count days fetched per mode
    pub fn count_majsoul_room_fetch_days(&self) -> Result<Vec<(i32, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT mode_id, COUNT(*) FROM majsoul_room_fetch_state GROUP BY mode_id"
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Get UUIDs without resolved paipu URLs
    pub fn get_majsoul_unresolved_paipu(&self, limit: Option<usize>) -> Result<Vec<(String, i64)>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT uuid, player_id FROM majsoul_logs WHERE paipu_url IS NULL ORDER BY start_time LIMIT {}",
                n
            ),
            None => "SELECT uuid, player_id FROM majsoul_logs WHERE paipu_url IS NULL ORDER BY start_time".to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Update paipu URL for a UUID
    pub fn set_majsoul_paipu_url(&self, uuid: &str, paipu_url: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_logs SET paipu_url = ?1 WHERE uuid = ?2",
            params![paipu_url, uuid],
        )?;
        Ok(())
    }

    /// Get all resolved paipu URLs
    pub fn get_majsoul_resolved_paipu(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT paipu_url FROM majsoul_logs WHERE paipu_url IS NOT NULL ORDER BY start_time"
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Count resolved vs unresolved paipu URLs
    pub fn count_majsoul_paipu_status(&self) -> Result<(i64, i64)> {
        let resolved: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE paipu_url IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let unresolved: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE paipu_url IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok((resolved, unresolved))
    }

    // Throne player methods for full UUID fetching

    /// Insert or update a throne player
    pub fn upsert_throne_player(&self, account_id: i64, nickname: &str) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO throne_players (account_id, nickname) VALUES (?1, ?2)",
            params![account_id, nickname],
        )?;
        Ok(result > 0)
    }

    /// Get unfetched throne players
    pub fn get_unfetched_throne_players(&self, limit: Option<usize>) -> Result<Vec<i64>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT account_id FROM throne_players WHERE fetched_at IS NULL LIMIT {}",
                n
            ),
            None => "SELECT account_id FROM throne_players WHERE fetched_at IS NULL".to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Mark a throne player as fetched
    pub fn mark_throne_player_fetched(&self, account_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE throne_players SET fetched_at = datetime('now') WHERE account_id = ?1",
            params![account_id],
        )?;
        Ok(())
    }

    /// Count how many games we have for a specific player
    pub fn count_player_games(&self, account_id: i64, mode_id: i32) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE player_id = ?1 AND mode_id = ?2",
            params![account_id, mode_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get throne players who were fetched but may have hit the page limit cap
    pub fn get_throne_players_needing_refetch(&self, limit: Option<usize>) -> Result<Vec<i64>> {
        let sql = match limit {
            Some(n) => format!(
                r#"
                SELECT tp.account_id
                FROM throne_players tp
                WHERE tp.fetched_at IS NOT NULL
                AND (
                    SELECT COUNT(*) FROM majsoul_logs ml
                    WHERE ml.player_id = tp.account_id AND ml.mode_id = 16
                ) = {}
                LIMIT {}
                "#,
                AMAE_KOROMO_PAGE_LIMIT,
                n
            ),
            None => format!(
                r#"
                SELECT tp.account_id
                FROM throne_players tp
                WHERE tp.fetched_at IS NOT NULL
                AND (
                    SELECT COUNT(*) FROM majsoul_logs ml
                    WHERE ml.player_id = tp.account_id AND ml.mode_id = 16
                ) = {}
                "#,
                AMAE_KOROMO_PAGE_LIMIT
            ),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let ids: Vec<i64> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    /// Reset fetched_at for a player so they can be re-fetched
    pub fn reset_throne_player_fetched(&self, account_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE throne_players SET fetched_at = NULL WHERE account_id = ?1",
            params![account_id],
        )?;
        Ok(())
    }

    /// Reset fetched_at for all players who hit the page limit cap
    pub fn reset_capped_throne_players(&self) -> Result<usize> {
        let sql = format!(
            r#"
            UPDATE throne_players
            SET fetched_at = NULL
            WHERE fetched_at IS NOT NULL
            AND account_id IN (
                SELECT player_id FROM majsoul_logs
                WHERE mode_id = 16
                GROUP BY player_id
                HAVING COUNT(*) = {}
            )
            "#,
            AMAE_KOROMO_PAGE_LIMIT
        );
        let result = self.conn.execute(&sql, [])?;
        Ok(result)
    }

    /// Update full_uuid for a majsoul log (by short uuid match)
    /// Alias for set_orphan_full_uuid - kept for API compatibility
    pub fn set_majsoul_full_uuid(&self, short_uuid: &str, full_uuid: &str) -> Result<bool> {
        self.set_orphan_full_uuid(short_uuid, full_uuid)
    }

    /// Count throne player stats
    pub fn count_throne_players(&self) -> Result<(i64, i64)> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM throne_players",
            [],
            |row| row.get(0),
        )?;
        let fetched: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM throne_players WHERE fetched_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok((total, fetched))
    }

    /// Count majsoul logs with full_uuid
    pub fn count_majsoul_full_uuids(&self) -> Result<(i64, i64)> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs",
            [],
            |row| row.get(0),
        )?;
        let with_full: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE full_uuid IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok((total, with_full))
    }

    /// Helper for simple count queries
    pub fn conn_query_row(&self, sql: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
        Ok(count)
    }

    /// Populate throne_players from existing majsoul_logs (extracts player IDs)
    pub fn populate_throne_players(&self) -> Result<usize> {
        // Get distinct player_ids from majsoul_logs where mode_id = 16 (throne)
        let count = self.conn.execute(
            "INSERT OR IGNORE INTO throne_players (account_id)
             SELECT DISTINCT player_id FROM majsoul_logs WHERE mode_id = 16",
            [],
        )?;
        Ok(count)
    }

    /// Get players who have orphaned games (games without full_uuid)
    /// Returns player_ids that have at least one orphaned game
    pub fn get_players_with_orphans(&self, limit: Option<usize>) -> Result<Vec<i64>> {
        let sql = match limit {
            Some(n) => format!(
                "SELECT DISTINCT player_id FROM majsoul_logs
                 WHERE full_uuid IS NULL AND mode_id = 16
                 ORDER BY player_id LIMIT {}",
                n
            ),
            None => "SELECT DISTINCT player_id FROM majsoul_logs
                     WHERE full_uuid IS NULL AND mode_id = 16
                     ORDER BY player_id".to_string(),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Count orphaned games (games without full_uuid)
    pub fn count_orphaned_games(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE full_uuid IS NULL AND mode_id = 16",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Update orphaned game's full_uuid by matching on start_time and player_id
    /// Returns true if an orphan was updated
    pub fn update_orphan_full_uuid(
        &self,
        player_id: i64,
        start_time: i64,
        full_uuid: &str,
    ) -> Result<bool> {
        let result = self.conn.execute(
            "UPDATE majsoul_logs SET full_uuid = ?1
             WHERE player_id = ?2 AND start_time = ?3 AND full_uuid IS NULL",
            params![full_uuid, player_id, start_time],
        )?;
        Ok(result > 0)
    }

    /// Get orphan short UUIDs that need resolution (no full_uuid)
    /// If mode_id is None, get orphans for all modes; if Some(id), filter by that mode
    pub fn get_orphan_short_uuids(&self, limit: Option<usize>, mode_id: Option<i32>) -> Result<Vec<String>> {
        let mut sql = String::from("SELECT uuid FROM majsoul_logs WHERE full_uuid IS NULL");

        if let Some(mode) = mode_id {
            sql.push_str(&format!(" AND mode_id = {}", mode));
        }

        sql.push_str(" ORDER BY start_time");

        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {}", n));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Set full_uuid for an orphan by its short uuid
    pub fn set_orphan_full_uuid(&self, short_uuid: &str, full_uuid: &str) -> Result<bool> {
        let result = self.conn.execute(
            "UPDATE majsoul_logs SET full_uuid = ?1 WHERE uuid = ?2 AND full_uuid IS NULL",
            params![full_uuid, short_uuid],
        )?;
        Ok(result > 0)
    }

    /// Cross-match orphan UUIDs by start_time against known full UUIDs (Throne mode only)
    /// Returns the number of orphans that were matched and updated
    pub fn cross_match_orphan_uuids(&self) -> Result<usize> {
        use std::collections::HashMap;

        // Build map of start_time -> full_uuid from all Throne records with full_uuid
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT start_time, full_uuid FROM majsoul_logs WHERE full_uuid IS NOT NULL AND mode_id = 16"
        )?;
        let uuid_map: HashMap<i64, String> = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // Find Throne orphans and try to match
        let mut orphan_stmt = self.conn.prepare(
            "SELECT uuid, start_time FROM majsoul_logs WHERE full_uuid IS NULL AND mode_id = 16"
        )?;
        let orphans: Vec<(String, i64)> = orphan_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let mut matched = 0usize;
        for (uuid, start_time) in &orphans {
            if let Some(full_uuid) = uuid_map.get(start_time) {
                self.conn.execute(
                    "UPDATE majsoul_logs SET full_uuid = ?1 WHERE uuid = ?2",
                    params![full_uuid, uuid],
                )?;
                matched += 1;
            }
        }

        Ok(matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_orphan_short_uuids() {
        let db = Database::open(":memory:").unwrap();
        // Insert test data
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES ('short1', 1, 100, 16, NULL)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES ('short2', 2, 200, 16, 'full-uuid')",
            [],
        ).unwrap();

        let orphans = db.get_orphan_short_uuids(Some(10), Some(16)).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0], "short1");
    }

    #[test]
    fn test_set_orphan_full_uuid() {
        let db = Database::open(":memory:").unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES ('short1', 1, 100, 16, NULL)",
            [],
        ).unwrap();

        let updated = db.set_orphan_full_uuid("short1", "250101-full-uuid-here").unwrap();
        assert!(updated);

        // Verify it was set
        let full: String = db.conn.query_row(
            "SELECT full_uuid FROM majsoul_logs WHERE uuid = 'short1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(full, "250101-full-uuid-here");
    }

    #[test]
    fn test_get_majsoul_undownloaded_with_full_uuid() {
        let db = Database::open(":memory:").unwrap();

        // Insert test data: mix of records with and without full_uuid
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short1', 1, 100, 16, '220101-full-uuid-1', 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short2', 1, 200, 16, NULL, 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short3', 1, 300, 16, '220101-full-uuid-3', 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short4', 1, 400, 16, '220101-full-uuid-4', 1)",
            [],
        ).unwrap();

        // Get all undownloaded with full_uuid
        let uuids = db.get_majsoul_undownloaded_with_full_uuid(None).unwrap();
        assert_eq!(uuids.len(), 2);
        assert_eq!(uuids[0], "220101-full-uuid-1");
        assert_eq!(uuids[1], "220101-full-uuid-3");

        // Test with limit
        let uuids_limited = db.get_majsoul_undownloaded_with_full_uuid(Some(1)).unwrap();
        assert_eq!(uuids_limited.len(), 1);
        assert_eq!(uuids_limited[0], "220101-full-uuid-1");
    }

    #[test]
    fn test_count_majsoul_downloadable() {
        let db = Database::open(":memory:").unwrap();

        // Insert test data
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short1', 1, 100, 16, '220101-full-uuid-1', 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short2', 1, 200, 16, NULL, 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short3', 1, 300, 16, '220101-full-uuid-3', 0)",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid, is_downloaded) VALUES ('short4', 1, 400, 16, '220101-full-uuid-4', 1)",
            [],
        ).unwrap();

        let count = db.count_majsoul_downloadable().unwrap();
        assert_eq!(count, 2); // short1 and short3 have full_uuid and is_downloaded = 0
    }
}
