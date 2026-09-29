use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

/// Amae-Koromo API returns max 200 records per `player_records` request
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
    // Field is write-only by design: blobs are read back via SQL tuples, never via the struct.
    pub _xml_data: Option<Vec<u8>>,
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

    // 3 strikes then quarantine: transient net/parse flakes clear in 1-2, persistents need manual reset via reset_* .
    pub const MAX_DOWNLOAD_ATTEMPTS: i64 = 3;
    pub const MAX_CONVERT_ATTEMPTS: i64 = 3;

    fn push_limit(sql: &mut String, values: &mut Vec<rusqlite::types::Value>, limit: Option<usize>) {
        if let Some(n) = limit {
            sql.push_str(" LIMIT ?");
            values.push(rusqlite::types::Value::Integer(i64::try_from(n).unwrap_or(i64::MAX)));
        }
    }

    // Allow: schema DDL plus tolerated-duplicate migrations are long by nature.
    #[allow(clippy::too_many_lines)]
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
                download_attempts INTEGER NOT NULL DEFAULT 0,
                convert_attempts INTEGER NOT NULL DEFAULT 0,
                xml_data BLOB
            );

            CREATE TABLE IF NOT EXISTS fetch_state (
                date TEXT PRIMARY KEY,
                fetched_at TEXT NOT NULL,
                checked_at TEXT
            );

            -- Per-date attempt timestamps for the 20-minute re-check throttle.
            -- Separate from fetch_state so attempts never mark a date fetched.
            CREATE TABLE IF NOT EXISTS fetch_attempts (
                date TEXT PRIMARY KEY,
                checked_at TEXT NOT NULL
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
                player_id INTEGER NOT NULL CHECK (player_id > 0),
                start_time INTEGER NOT NULL,
                mode_id INTEGER,
                num_players INTEGER,
                is_hanchan INTEGER,
                is_downloaded INTEGER DEFAULT 0,
                is_converted INTEGER DEFAULT 0,
                download_attempts INTEGER NOT NULL DEFAULT 0,
                convert_attempts INTEGER NOT NULL DEFAULT 0,
                full_uuid TEXT,
                paipu_url TEXT DEFAULT NULL,
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
                account_id INTEGER PRIMARY KEY CHECK (account_id > 0),
                nickname TEXT,
                fetched_at TEXT
            );

            -- Two-phase pipeline tables
            CREATE TABLE IF NOT EXISTS majsoul_pipeline_players (
                player_id INTEGER PRIMARY KEY,
                nickname TEXT,
                first_seen_date TEXT,
                scraped_at TEXT,
                game_count INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS majsoul_day_fetch_state (
                date TEXT PRIMARY KEY,
                fetched_at TEXT,
                game_count INTEGER DEFAULT 0,
                player_count INTEGER DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_majsoul_pipeline_players_scraped ON majsoul_pipeline_players(scraped_at);
            ",
        )?;

        // Schema migration: add columns if they don't exist (for existing databases)
        // Each tolerates duplicate-column errors so old DBs migrate forward.
        // Unexpected errors are logged, not swallowed silently.
        for ddl in [
            "ALTER TABLE majsoul_logs ADD COLUMN num_players INTEGER",
            "ALTER TABLE majsoul_logs ADD COLUMN is_hanchan INTEGER",
            "ALTER TABLE majsoul_logs ADD COLUMN full_uuid TEXT",
            "ALTER TABLE majsoul_logs ADD COLUMN paipu_url TEXT",
            "ALTER TABLE majsoul_logs ADD COLUMN download_attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE majsoul_logs ADD COLUMN convert_attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE logs ADD COLUMN download_attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE logs ADD COLUMN convert_attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE fetch_state ADD COLUMN checked_at TEXT",
        ] {
            if let Err(e) = self.conn.execute(ddl, []) {
                // Tolerate only SQLite duplicate-column failures (old DBs migrating
                // forward, or fresh DBs whose CREATE TABLE already has the column).
                // Match the SqliteFailure variant + message, not bare Display: ErrorCode
                // is too coarse here (duplicate column surfaces as generic SQLITE_ERROR).
                let is_duplicate = matches!(
                    &e,
                    rusqlite::Error::SqliteFailure(_, Some(msg))
                        if msg.contains("duplicate column name") || msg.contains("already exists")
                );
                if !is_duplicate {
                    tracing::warn!("migration '{ddl}' failed: {e}");
                }
            }
        }

        // Create index on full_uuid after migration ensures column exists
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_majsoul_logs_full_uuid ON majsoul_logs(full_uuid)",
            [],
        )?;

        Ok(())
    }

    pub fn insert_log_id(&self, entry: &LogEntry) -> Result<bool> {
        // IGNORE: re-fetch must never overwrite stored blobs/progress; fetch_state uses REPLACE to refresh timestamps.
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO logs (id, date, num_players, is_hanchan, is_downloaded, is_converted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id,
                entry.date,
                entry.num_players,
                i32::from(entry.is_hanchan),
                i32::from(entry.is_downloaded),
                i32::from(entry.is_converted),
            ],
        )?;
        Ok(result > 0)
    }

    pub fn get_undownloaded_ids(&self, limit: Option<usize>) -> Result<Vec<String>> {
        use rusqlite::types::Value;
        // Retry queue: fresh rows plus error rows with attempts remaining.
        let mut sql = String::from(
            "SELECT id FROM logs WHERE (is_downloaded = 0 OR (is_downloaded = -1 AND download_attempts < ?)) ORDER BY id",
        );
        let mut values: Vec<Value> = vec![Value::Integer(Self::MAX_DOWNLOAD_ATTEMPTS)];
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
        let mut ids = Vec::new();
        for id in rows {
            ids.push(id?);
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
            "UPDATE logs SET is_downloaded = -1, download_attempts = download_attempts + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn mark_convert_error(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE logs SET is_converted = -1, convert_attempts = convert_attempts + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn reset_download_errors(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE logs SET is_downloaded = 0, download_attempts = 0 WHERE is_downloaded = -1",
            [],
        )?;
        Ok(n)
    }

    pub fn reset_convert_errors(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE logs SET is_converted = 0, convert_attempts = 0 WHERE is_converted = -1",
            [],
        )?;
        Ok(n)
    }

    pub fn get_unconverted_logs(
        &self,
        limit: Option<usize>,
        num_players: Option<i32>,
        hanchan_only: bool,
        after_id: Option<&str>,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        use rusqlite::types::Value;
        // Retry queue: fresh rows plus error rows with attempts remaining.
        let mut sql = String::from(
            "SELECT id, xml_data FROM logs
             WHERE is_downloaded = 1 AND xml_data IS NOT NULL
             AND (is_converted = 0 OR (is_converted = -1 AND convert_attempts < ?))",
        );
        let mut values: Vec<Value> = vec![Value::Integer(Self::MAX_CONVERT_ATTEMPTS)];

        if let Some(players) = num_players {
            sql.push_str(" AND num_players = ?");
            values.push(Value::Integer(i64::from(players)));
        }

        if hanchan_only {
            sql.push_str(" AND is_hanchan = 1");
        }

        // Cursor page: ids are unique, so `id > cursor` visits every queued
        // row exactly once per run. Re-querying `LIMIT n` from the head would
        // return still-queued failures forever and starve the table tail.
        if let Some(cursor) = after_id {
            sql.push_str(" AND id > ?");
            values.push(Value::Text(cursor.to_owned()));
        }

        sql.push_str(" ORDER BY id");

        Self::push_limit(&mut sql, &mut values, limit);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }

        Ok(results)
    }

    /// Get downloaded Tenhou logs regardless of conversion state (for export).
    pub fn get_downloaded_logs(&self, limit: Option<usize>) -> Result<Vec<(String, Vec<u8>)>> {
        use rusqlite::types::Value;
        let mut sql = String::from(
            "SELECT id, xml_data FROM logs WHERE is_downloaded = 1 AND xml_data IS NOT NULL ORDER BY id",
        );
        let mut values: Vec<Value> = Vec::new();

        Self::push_limit(&mut sql, &mut values, limit);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

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

    /// Record that a date file was attempted (success or failure) for Tenhou politeness.
    /// Tenhou asks clients to re-check a file at most every 20 minutes; callers gate
    /// on `was_fetch_checked_within`. Kept in a separate table so attempts never
    /// mark a date fetched (a failed date must stay retryable).
    pub fn record_fetch_attempt(&self, date: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO fetch_attempts (date, checked_at) VALUES (?1, datetime('now'))
             ON CONFLICT(date) DO UPDATE SET checked_at = datetime('now')",
            params![date],
        )?;
        Ok(())
    }

    /// True when the date was attempted within the last `minutes` minutes.
    /// NOTE: full `FileIndex` size-gating (Tenhou rule 2) is deliberately NOT implemented:
    /// list.cgi lists scc files HOURLY (sccYYYYMMDDHH.html.gz) while fetch downloads
    /// DAILY sccYYYYMMDD.html.gz URLs, so no size entry corresponds to a fetched file.
    /// This 20-minute attempt throttle (rule 1) is the enforceable subset.
    pub fn was_fetch_checked_within(&self, date: &str, minutes: i64) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM fetch_attempts
             WHERE date = ?1 AND checked_at > datetime('now', ?2)",
            params![date, format!("-{minutes} minutes")],
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
        // Pure byte logic: b[6] == b'-' implies the ..6 / 7.. boundaries are ASCII-safe.
        let b = uuid.as_bytes();
        if b.len() > 7 && b[6] == b'-'
            && uuid[..6].bytes().all(|c| c.is_ascii_digit()) {
                return &uuid[7..];
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

    /// Insert majsoul log with full UUID (from `player_records` API)
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
        use rusqlite::types::Value;
        // Retry queue: fresh rows plus error rows with attempts remaining.
        let mut sql = String::from(
            "SELECT uuid FROM majsoul_logs WHERE (is_downloaded = 0 OR (is_downloaded = -1 AND download_attempts < ?)) ORDER BY start_time",
        );
        let mut values: Vec<Value> = vec![Value::Integer(Self::MAX_DOWNLOAD_ATTEMPTS)];
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
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
            "UPDATE majsoul_logs SET is_downloaded = -1, download_attempts = download_attempts + 1 WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(())
    }

    /// Persist a Majsoul convert failure for retry-then-quarantine (see `convert_logs`).
    pub fn mark_majsoul_convert_error(&self, uuid: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_logs SET is_converted = -1, convert_attempts = convert_attempts + 1 WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(())
    }

    pub fn reset_majsoul_download_errors(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE majsoul_logs SET is_downloaded = 0, download_attempts = 0 WHERE is_downloaded = -1",
            [],
        )?;
        Ok(n)
    }

    pub fn reset_majsoul_convert_errors(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE majsoul_logs SET is_converted = 0, convert_attempts = 0 WHERE is_converted = -1",
            [],
        )?;
        Ok(n)
    }

    /// Get `mode_id` for a Majsoul log by UUID.
    pub fn get_majsoul_mode_id(&self, uuid: &str) -> Result<i32> {
        // NULL mode defaults to 16 (Throne), the dominant scrape target; keeps legacy rows downloadable.
        let mode_id: i32 = self.conn.query_row(
            "SELECT COALESCE(mode_id, 16) FROM majsoul_logs WHERE uuid = ?1 OR full_uuid = ?1",
            params![uuid],
            |row| row.get(0),
        )?;
        Ok(mode_id)
    }

    /// Get undownloaded Majsoul logs that have a `full_uuid` (required for download).
    ///
    /// Returns `full_uuid` values for records where:
    /// - (`is_downloaded` = 0, or error with attempts remaining)
    /// - `full_uuid` IS NOT NULL
    pub fn get_majsoul_undownloaded_with_full_uuid(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<String>> {
        use rusqlite::types::Value;
        let mut sql = String::from(
            "SELECT full_uuid FROM majsoul_logs WHERE full_uuid IS NOT NULL AND (is_downloaded = 0 OR (is_downloaded = -1 AND download_attempts < ?)) ORDER BY start_time",
        );
        let mut values: Vec<Value> = vec![Value::Integer(Self::MAX_DOWNLOAD_ATTEMPTS)];
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
        let mut uuids = Vec::new();
        for uuid in rows {
            uuids.push(uuid?);
        }
        Ok(uuids)
    }

    /// Count Majsoul logs that are downloadable (have `full_uuid` but not yet downloaded).
    pub fn count_majsoul_downloadable(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE full_uuid IS NOT NULL AND (is_downloaded = 0 OR (is_downloaded = -1 AND download_attempts < ?1))",
            params![Self::MAX_DOWNLOAD_ATTEMPTS],
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
        use rusqlite::types::Value;
        // Retry queue: fresh rows plus error rows with attempts remaining.
        let mut sql = String::from(
            "SELECT uuid, raw_data FROM majsoul_logs
             WHERE is_downloaded = 1 AND raw_data IS NOT NULL
             AND (is_converted = 0 OR (is_converted = -1 AND convert_attempts < ?))",
        );
        let mut values: Vec<Value> = vec![Value::Integer(Self::MAX_CONVERT_ATTEMPTS)];

        if let Some(players) = num_players {
            sql.push_str(" AND num_players = ?");
            values.push(Value::Integer(i64::from(players)));
        }

        if hanchan_only {
            sql.push_str(" AND is_hanchan = 1");
        }

        sql.push_str(" ORDER BY start_time");

        Self::push_limit(&mut sql, &mut values, limit);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

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


    /// Get UUIDs without resolved paipu URLs, with the `mode_id` needed to build the view URL.
    pub fn get_majsoul_unresolved_paipu(&self, limit: Option<usize>) -> Result<Vec<(String, i64, i32)>> {
        use rusqlite::types::Value;
        // NULL mode defaults to 16 (Throne), the dominant scrape target; keeps legacy rows downloadable.
        let mut sql = String::from(
            "SELECT uuid, player_id, COALESCE(mode_id, 16) FROM majsoul_logs WHERE paipu_url IS NULL ORDER BY start_time",
        );
        let mut values: Vec<Value> = Vec::new();
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
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
        use rusqlite::types::Value;
        let mut sql = String::from("SELECT account_id FROM throne_players WHERE fetched_at IS NULL");
        let mut values: Vec<Value> = Vec::new();
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
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

    /// Reset `fetched_at` for all players who hit the page limit cap
    pub fn reset_capped_throne_players(&self) -> Result<usize> {
        // == page cap means the listing was truncated, so the player must be re-scraped.
        let result = self.conn.execute(
            "
            UPDATE throne_players
            SET fetched_at = NULL
            WHERE fetched_at IS NOT NULL
            AND account_id IN (
                SELECT player_id FROM majsoul_logs
                WHERE mode_id = 16
                GROUP BY player_id
                HAVING COUNT(*) = ?1
            )
            ",
            params![AMAE_KOROMO_PAGE_LIMIT],
        )?;
        Ok(result)
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

    /// Count majsoul logs with `full_uuid`
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

    /// Populate `throne_players` from existing `majsoul_logs` (extracts player IDs)
    pub fn populate_throne_players(&self) -> Result<usize> {
        // Get distinct player_ids from majsoul_logs where mode_id = 16 (throne)
        let count = self.conn.execute(
            "INSERT OR IGNORE INTO throne_players (account_id)
             SELECT DISTINCT player_id FROM majsoul_logs WHERE mode_id = 16",
            [],
        )?;
        Ok(count)
    }

    /// Count orphaned games (games without `full_uuid`)
    pub fn count_orphaned_games(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_logs WHERE full_uuid IS NULL AND mode_id = 16",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get orphan short UUIDs that need resolution (no `full_uuid`)
    /// If `mode_id` is None, get orphans for all modes; if Some(id), filter by that mode
    pub fn get_orphan_short_uuids(&self, limit: Option<usize>, mode_id: Option<i32>) -> Result<Vec<String>> {
        use rusqlite::types::Value;
        let mut sql = String::from("SELECT uuid FROM majsoul_logs WHERE full_uuid IS NULL");
        let mut values: Vec<Value> = Vec::new();

        if let Some(mode) = mode_id {
            sql.push_str(" AND mode_id = ?");
            values.push(Value::Integer(i64::from(mode)));
        }

        sql.push_str(" ORDER BY start_time");

        Self::push_limit(&mut sql, &mut values, limit);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Set `full_uuid` for an orphan by its short uuid
    pub fn set_orphan_full_uuid(&self, short_uuid: &str, full_uuid: &str) -> Result<bool> {
        let result = self.conn.execute(
            "UPDATE majsoul_logs SET full_uuid = ?1 WHERE uuid = ?2 AND full_uuid IS NULL",
            params![full_uuid, short_uuid],
        )?;
        Ok(result > 0)
    }

    /// Cross-match orphan UUIDs by (`player_id`, `start_time`) against known full UUIDs (Throne mode only).
    ///
    /// Returns `(matched, ambiguous)`: `matched` orphans were assigned a `full_uuid`;
    /// `ambiguous` counts distinct (`player_id`, `start_time`) keys where matching was
    /// skipped because the key was not unique on either side (no last-writer-win).
    pub fn cross_match_orphan_uuids(&self) -> Result<(usize, usize)> {
        use std::collections::{HashMap, HashSet};

        // Build map of (player_id, start_time) -> full_uuid from Throne records with full_uuid.
        // A key claimed by more than one full record is ambiguous: drop it entirely.
        let mut stmt = self.conn.prepare(
            "SELECT player_id, start_time, full_uuid FROM majsoul_logs WHERE full_uuid IS NOT NULL AND mode_id = 16"
        )?;
        let fulls: Vec<(i64, i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .filter_map(std::result::Result::ok)
            .collect();
        let mut uuid_map: HashMap<(i64, i64), String> = HashMap::new();
        let mut ambiguous_keys: HashSet<(i64, i64)> = HashSet::new();
        for (player_id, start_time, full_uuid) in fulls {
            let key = (player_id, start_time);
            if ambiguous_keys.contains(&key) {
                continue;
            }
            if uuid_map.remove(&key).is_some() {
                // Second full contender for this key: skip both, count as ambiguous.
                ambiguous_keys.insert(key);
            } else {
                uuid_map.insert(key, full_uuid);
            }
        }

        // Find Throne orphans and group them by key.
        let mut orphan_stmt = self.conn.prepare(
            "SELECT uuid, player_id, start_time FROM majsoul_logs WHERE full_uuid IS NULL AND mode_id = 16"
        )?;
        let orphans: Vec<(String, i64, i64)> = orphan_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .filter_map(std::result::Result::ok)
            .collect();
        let mut groups: HashMap<(i64, i64), Vec<String>> = HashMap::new();
        for (uuid, player_id, start_time) in orphans {
            groups.entry((player_id, start_time)).or_default().push(uuid);
        }

        let mut matched = 0usize;
        let mut ambiguous = 0usize;
        for (key, uuids) in &groups {
            if uuids.len() > 1 {
                // Several orphans share one key: cannot attribute a single full_uuid.
                ambiguous += 1;
                continue;
            }
            if ambiguous_keys.contains(key) {
                ambiguous += 1;
                continue;
            }
            if let Some(full_uuid) = uuid_map.get(key) {
                self.conn.execute(
                    "UPDATE majsoul_logs SET full_uuid = ?1 WHERE uuid = ?2",
                    params![full_uuid, uuids[0]],
                )?;
                matched += 1;
            }
        }

        Ok((matched, ambiguous))
    }

    // ==================== Two-Phase Pipeline Methods ====================

    /// Check if a day has been fetched (Phase 1)
    // Test-only helper: exercised by day-fetch tests, never by shipped code paths.
    #[allow(dead_code)]
    pub fn is_day_fetched(&self, date: &str) -> Result<bool> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_day_fetch_state WHERE date = ?1",
            params![date],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Begin a transaction (for batching writes)
    pub fn begin_transaction(&self) -> Result<()> {
        self.conn.execute("BEGIN TRANSACTION", [])?;
        Ok(())
    }

    /// Commit the current transaction
    pub fn commit(&self) -> Result<()> {
        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    /// Mark a day as fetched with stats
    pub fn mark_day_fetched(&self, date: &str, game_count: i32, player_count: i32) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO majsoul_day_fetch_state (date, fetched_at, game_count, player_count)
             VALUES (?1, datetime('now'), ?2, ?3)",
            params![date, game_count, player_count],
        )?;
        Ok(())
    }

    /// Get unfetched days in a date range (returns YYYYMMDD strings)
    pub fn get_unfetched_days(&self, start: &str, end: &str) -> Result<Vec<String>> {
        use chrono::NaiveDate;
        use std::collections::HashSet;

        let start_date = NaiveDate::parse_from_str(start, "%Y%m%d")
            .map_err(|e| anyhow::anyhow!("Invalid start date: {e}"))?;
        let end_date = NaiveDate::parse_from_str(end, "%Y%m%d")
            .map_err(|e| anyhow::anyhow!("Invalid end date: {e}"))?;

        // Query all fetched dates in range at once (single query instead of N+1)
        let mut stmt = self.conn.prepare(
            "SELECT date FROM majsoul_day_fetch_state WHERE date >= ?1 AND date <= ?2"
        )?;
        let fetched_dates: HashSet<String> = stmt
            .query_map(params![start, end], |row| row.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();

        // Generate all dates and filter out fetched ones
        let mut unfetched = Vec::new();
        let mut current = start_date;
        while current <= end_date {
            let date_str = current.format("%Y%m%d").to_string();
            if !fetched_dates.contains(&date_str) {
                unfetched.push(date_str);
            }
            current += chrono::Duration::days(1);
        }

        Ok(unfetched)
    }

    /// Count day fetch progress
    pub fn count_day_fetch_progress(&self) -> Result<(i64, i64, i64)> {
        let days: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_day_fetch_state",
            [],
            |row| row.get(0),
        )?;
        let games: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(game_count), 0) FROM majsoul_day_fetch_state",
            [],
            |row| row.get(0),
        )?;
        let players: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(player_count), 0) FROM majsoul_day_fetch_state",
            [],
            |row| row.get(0),
        )?;
        Ok((days, games, players))
    }

    /// Insert a new player discovered during Phase 1 (day fetching)
    /// Returns true only if player was newly inserted, false if already existed
    pub fn upsert_player_for_scraping(
        &self,
        player_id: i64,
        nickname: &str,
        first_seen_date: &str,
    ) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO majsoul_pipeline_players (player_id, nickname, first_seen_date)
             VALUES (?1, ?2, ?3)",
            params![player_id, nickname, first_seen_date],
        )?;
        Ok(result > 0)
    }

    /// Get unscraped player IDs (Phase 2)
    pub fn get_unscraped_players(&self, limit: Option<usize>) -> Result<Vec<i64>> {
        use rusqlite::types::Value;
        let mut sql = String::from(
            "SELECT player_id FROM majsoul_pipeline_players WHERE scraped_at IS NULL ORDER BY player_id",
        );
        let mut values: Vec<Value> = Vec::new();
        Self::push_limit(&mut sql, &mut values, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&values), |row| row.get(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Mark a player as scraped with game count
    pub fn mark_player_scraped(&self, player_id: i64, game_count: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE majsoul_pipeline_players SET scraped_at = datetime('now'), game_count = ?1 WHERE player_id = ?2",
            params![game_count, player_id],
        )?;
        Ok(())
    }

    /// Count player scraping progress
    pub fn count_player_scrape_progress(&self) -> Result<(i64, i64)> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_pipeline_players",
            [],
            |row| row.get(0),
        )?;
        let scraped: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM majsoul_pipeline_players WHERE scraped_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok((total, scraped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paipu_round_trip() {
        let db = Database::open(":memory:").unwrap();
        db.insert_majsoul_log("short-uuid-1", 42, 1_700_000_000, Some(12))
            .unwrap();
        // Unresolved row carries (uuid, player_id, mode_id).
        let unresolved = db.get_majsoul_unresolved_paipu(None).unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].0, "short-uuid-1");
        assert_eq!(unresolved[0].1, 42);
        assert_eq!(unresolved[0].2, 12);
        // NULL mode falls back to 16.
        db.insert_majsoul_log("short-uuid-2", 43, 1_700_000_001, None)
            .unwrap();
        let unresolved = db.get_majsoul_unresolved_paipu(None).unwrap();
        let fallback = unresolved.iter().find(|r| r.0 == "short-uuid-2").unwrap();
        assert_eq!(fallback.2, 16);
        // Resolve one; it leaves the queue and appears in the resolved list.
        db.set_majsoul_paipu_url("short-uuid-1", "https://example/paipu/1")
            .unwrap();
        let unresolved = db.get_majsoul_unresolved_paipu(None).unwrap();
        assert!(unresolved.iter().all(|r| r.0 != "short-uuid-1"));
        let resolved = db.get_majsoul_resolved_paipu().unwrap();
        assert_eq!(resolved, vec!["https://example/paipu/1".to_string()]);
        // LIMIT binds instead of interpolating.
        let limited = db.get_majsoul_unresolved_paipu(Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_download_attempt_transitions() {
        let db = Database::open(":memory:").unwrap();
        db.conn
            .execute(
                "INSERT INTO logs (id, date, num_players, is_hanchan) VALUES ('log1', '20240101', 4, 1)",
                [],
            )
            .unwrap();
        // Fresh row is queued.
        assert_eq!(db.get_undownloaded_ids(None).unwrap(), vec!["log1".to_string()]);
        // One error: still queued.
        db.mark_download_error("log1").unwrap();
        assert_eq!(db.get_undownloaded_ids(None).unwrap(), vec!["log1".to_string()]);
        // Two more errors (3 total): quarantined.
        db.mark_download_error("log1").unwrap();
        db.mark_download_error("log1").unwrap();
        assert!(db.get_undownloaded_ids(None).unwrap().is_empty());
        assert_eq!(db.count_majsoul_downloadable().unwrap(), 0);
        // Reset re-queues.
        assert_eq!(db.reset_download_errors().unwrap(), 1);
        assert_eq!(db.get_undownloaded_ids(None).unwrap(), vec!["log1".to_string()]);
    }

    #[test]
    fn test_convert_attempt_transitions() {
        let db = Database::open(":memory:").unwrap();
        db.conn
            .execute(
                "INSERT INTO logs (id, date, num_players, is_hanchan, is_downloaded, xml_data) VALUES ('log1', '20240101', 4, 1, 1, X'0102')",
                [],
            )
            .unwrap();
        assert_eq!(db.get_unconverted_logs(None, None, false, None).unwrap().len(), 1);
        db.mark_convert_error("log1").unwrap();
        assert_eq!(db.get_unconverted_logs(None, None, false, None).unwrap().len(), 1);
        db.mark_convert_error("log1").unwrap();
        db.mark_convert_error("log1").unwrap();
        assert!(db.get_unconverted_logs(None, None, false, None).unwrap().is_empty());
        assert_eq!(db.reset_convert_errors().unwrap(), 1);
        assert_eq!(db.get_unconverted_logs(None, None, false, None).unwrap().len(), 1);
    }

    #[test]
    fn test_unconverted_logs_cursor_paging() {
        let db = Database::open(":memory:").unwrap();
        for id in ["a", "b", "c"] {
            db.conn
                .execute(
                    "INSERT INTO logs (id, date, num_players, is_hanchan, is_downloaded, xml_data) VALUES (?1, '20240101', 4, 1, 1, X'0102')",
                    params![id],
                )
                .unwrap();
        }
        // Head row stays queued as a retryable failure; the cursor still
        // advances past it so the tail is visited exactly once per run.
        db.mark_convert_error("a").unwrap();
        let page1 = db.get_unconverted_logs(Some(1), None, false, None).unwrap();
        assert_eq!(page1.len(), 1);
        let cursor = page1[0].0.clone();
        let page2 = db
            .get_unconverted_logs(Some(10), None, false, Some(&cursor))
            .unwrap();
        let ids: Vec<&str> = page2.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
        assert!(db
            .get_unconverted_logs(Some(10), None, false, Some("c"))
            .unwrap()
            .is_empty());
    }


    #[test]
    fn test_majsoul_attempt_transitions() {
        let db = Database::open(":memory:").unwrap();
        db.insert_majsoul_log_with_full_uuid("250101-full-uuid-1", 7, 100, Some(16))
            .unwrap();
        assert_eq!(db.get_majsoul_undownloaded(None).unwrap().len(), 1);
        assert_eq!(
            db.get_majsoul_undownloaded_with_full_uuid(None).unwrap().len(),
            1
        );
        assert_eq!(db.count_majsoul_downloadable().unwrap(), 1);
        db.mark_majsoul_download_error("full-uuid-1").unwrap();
        assert_eq!(db.count_majsoul_downloadable().unwrap(), 1);
        db.mark_majsoul_download_error("full-uuid-1").unwrap();
        db.mark_majsoul_download_error("full-uuid-1").unwrap();
        assert!(db.get_majsoul_undownloaded(None).unwrap().is_empty());
        assert!(db
            .get_majsoul_undownloaded_with_full_uuid(None)
            .unwrap()
            .is_empty());
        assert_eq!(db.count_majsoul_downloadable().unwrap(), 0);
        assert_eq!(db.reset_majsoul_download_errors().unwrap(), 1);
        assert_eq!(db.count_majsoul_downloadable().unwrap(), 1);
        // Convert queue honors attempts too.
        db.mark_majsoul_downloaded("full-uuid-1", b"raw").unwrap();
        assert_eq!(db.get_majsoul_unconverted(None, None, false).unwrap().len(), 1);
        db.mark_majsoul_convert_error("full-uuid-1").unwrap();
        db.mark_majsoul_convert_error("full-uuid-1").unwrap();
        db.mark_majsoul_convert_error("full-uuid-1").unwrap();
        assert!(db
            .get_majsoul_unconverted(None, None, false)
            .unwrap()
            .is_empty());
        assert_eq!(db.reset_majsoul_convert_errors().unwrap(), 1);
        assert_eq!(db.get_majsoul_unconverted(None, None, false).unwrap().len(), 1);
    }

    #[test]
    fn test_get_downloaded_logs() {
        let db = Database::open(":memory:").unwrap();
        db.conn
            .execute(
                "INSERT INTO logs (id, date, num_players, is_hanchan, is_downloaded, is_converted, xml_data) VALUES ('a', '20240101', 4, 1, 1, 0, X'0102')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO logs (id, date, num_players, is_hanchan, is_downloaded, is_converted, xml_data) VALUES ('b', '20240101', 4, 1, 1, 1, X'0304')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO logs (id, date, num_players, is_hanchan, is_downloaded, xml_data) VALUES ('c', '20240101', 4, 1, 0, X'0506')",
                [],
            )
            .unwrap();
        // Both downloaded rows surface regardless of conversion state; pending row excluded.
        let rows = db.get_downloaded_logs(None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "a");
        assert_eq!(rows[1].0, "b");
        let limited = db.get_downloaded_logs(Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].0, "a");
    }

    #[test]
    fn test_cross_match_unique_pair() {
        let db = Database::open(":memory:").unwrap();
        db.insert_majsoul_log_with_full_uuid("250101-full-uuid-9", 9, 500, Some(16))
            .unwrap();
        db.insert_majsoul_log("orphan-uuid-9", 9, 500, Some(16)).unwrap();
        let (matched, ambiguous) = db.cross_match_orphan_uuids().unwrap();
        assert_eq!((matched, ambiguous), (1, 0));
        let full: Option<String> = db
            .conn
            .query_row(
                "SELECT full_uuid FROM majsoul_logs WHERE uuid = 'orphan-uuid-9'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(full.as_deref(), Some("250101-full-uuid-9"));
    }

    #[test]
    fn test_cross_match_ambiguous_orphans() {
        // Two orphans share one (player_id, start_time) key with a single full
        // record: neither may be assigned; the key counts as ambiguous once.
        let db = Database::open(":memory:").unwrap();
        db.insert_majsoul_log_with_full_uuid("250101-full-uuid-1", 1, 100, Some(16))
            .unwrap();
        db.insert_majsoul_log("orphan-a", 1, 100, Some(16)).unwrap();
        db.insert_majsoul_log("orphan-b", 1, 100, Some(16)).unwrap();
        let (matched, ambiguous) = db.cross_match_orphan_uuids().unwrap();
        assert_eq!((matched, ambiguous), (0, 1));
        for uuid in ["orphan-a", "orphan-b"] {
            let full: Option<String> = db
                .conn
                .query_row(
                    "SELECT full_uuid FROM majsoul_logs WHERE uuid = ?1",
                    rusqlite::params![uuid],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(full, None);
        }
    }

    #[test]
    fn test_cross_match_ambiguous_full_records() {
        // Two full records in the same second for one player: an orphan with that
        // key must not be cross-assigned to either contender.
        let db = Database::open(":memory:").unwrap();
        db.conn
            .execute(
                "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES ('short-a', 2, 200, 16, '250101-full-a')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO majsoul_logs (uuid, player_id, start_time, mode_id, full_uuid) VALUES ('short-b', 2, 200, 16, '250101-full-b')",
                [],
            )
            .unwrap();
        db.insert_majsoul_log("orphan-c", 2, 200, Some(16)).unwrap();
        let (matched, ambiguous) = db.cross_match_orphan_uuids().unwrap();
        assert_eq!((matched, ambiguous), (0, 1));
        let full: Option<String> = db
            .conn
            .query_row(
                "SELECT full_uuid FROM majsoul_logs WHERE uuid = 'orphan-c'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(full, None);
    }

    #[test]
    fn test_normalize_uuid_multibyte_no_panic() {
        // Byte 6 lands inside a multibyte char; the old char-index guard panicked here.
        assert_eq!(Database::normalize_uuid("abcdeé-xyz"), "abcdeé-xyz");
        assert_eq!(
            Database::normalize_uuid("250101-a7d2bfbf-dac8-45b9-a667-861f82589725"),
            "a7d2bfbf-dac8-45b9-a667-861f82589725"
        );
        assert_eq!(
            Database::normalize_uuid("a7d2bfbf-dac8-45b9-a667-861f82589725"),
            "a7d2bfbf-dac8-45b9-a667-861f82589725"
        );
        assert_eq!(Database::normalize_uuid("short"), "short");
    }

    #[test]
    fn test_fetch_attempt_throttle_does_not_mark_fetched() {
        let db = Database::open(":memory:").unwrap();
        assert!(!db.was_fetch_checked_within("20240101", 20).unwrap());
        db.record_fetch_attempt("20240101").unwrap();
        assert!(db.was_fetch_checked_within("20240101", 20).unwrap());
        assert!(!db.was_fetch_checked_within("20240102", 20).unwrap());
        // An attempt must NOT count as fetched: failed dates stay retryable.
        assert!(!db.is_date_fetched("20240101").unwrap());
        // Marking fetched preserves the attempt record.
        db.mark_date_fetched("20240101").unwrap();
        assert!(db.is_date_fetched("20240101").unwrap());
        assert!(db.was_fetch_checked_within("20240101", 20).unwrap());
    }

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

    #[test]
    fn test_day_fetch_state() {
        let db = Database::open(":memory:").unwrap();

        // Not fetched initially
        assert!(!db.is_day_fetched("20250101").unwrap());

        // Mark as fetched
        db.mark_day_fetched("20250101", 150, 42).unwrap();
        assert!(db.is_day_fetched("20250101").unwrap());

        // Get unfetched days
        let unfetched = db.get_unfetched_days("20250101", "20250103").unwrap();
        assert_eq!(unfetched.len(), 2); // 20250102, 20250103

        // Count progress
        let (days, games, players) = db.count_day_fetch_progress().unwrap();
        assert_eq!(days, 1);
        assert_eq!(games, 150);
        assert_eq!(players, 42);
    }

    #[test]
    fn test_player_scraping_methods() {
        let db = Database::open(":memory:").unwrap();

        // Upsert player
        db.upsert_player_for_scraping(12345, "TestPlayer", "20250101").unwrap();

        // Get unscraped players
        let unscraped = db.get_unscraped_players(Some(10)).unwrap();
        assert_eq!(unscraped.len(), 1);
        assert_eq!(unscraped[0], 12345);

        // Mark as scraped
        db.mark_player_scraped(12345, 50).unwrap();

        // Should be empty now
        let unscraped = db.get_unscraped_players(Some(10)).unwrap();
        assert!(unscraped.is_empty());

        // Count stats
        let (total, scraped) = db.count_player_scrape_progress().unwrap();
        assert_eq!(total, 1);
        assert_eq!(scraped, 1);
    }
}
