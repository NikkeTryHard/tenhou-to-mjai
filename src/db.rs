use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

pub struct Database {
    conn: Connection,
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
            ",
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
                .prepare("SELECT id FROM logs WHERE is_downloaded = 0 ORDER BY id LIMIT ?1")?;
            let rows = stmt.query_map([n], |row| row.get(0))?;
            for id in rows {
                ids.push(id?);
            }
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM logs WHERE is_downloaded = 0 ORDER BY id")?;
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

    pub fn get_unconverted_logs(&self, limit: Option<usize>) -> Result<Vec<(String, Vec<u8>)>> {
        let mut results = Vec::new();

        if let Some(n) = limit {
            let mut stmt = self.conn.prepare(
                "SELECT id, xml_data FROM logs
                 WHERE is_downloaded = 1 AND is_converted = 0 AND xml_data IS NOT NULL
                 ORDER BY id LIMIT ?1",
            )?;
            let rows = stmt.query_map([n], |row| Ok((row.get(0)?, row.get(1)?)))?;
            for row in rows {
                results.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, xml_data FROM logs
                 WHERE is_downloaded = 1 AND is_converted = 0 AND xml_data IS NOT NULL
                 ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            for row in rows {
                results.push(row?);
            }
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
}
