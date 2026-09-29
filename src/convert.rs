use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::ParallelProgressIterator;
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{info, warn};

use crate::db::Database;
use crate::util::write_atomic;

/// Blob page size: bounds peak memory when no `--limit` is given.
const PAGE_SIZE: usize = 500;

pub struct Converter {
    output_dir: std::path::PathBuf,
}

impl Converter {
    pub fn new(output_dir: impl AsRef<Path>) -> Result<Self> {
        let output_dir = output_dir.as_ref().to_path_buf();
        fs::create_dir_all(&output_dir)?;
        Ok(Self { output_dir })
    }

    pub fn convert_logs(
        &self,
        db: &Database,
        limit: Option<usize>,
        num_players: Option<i32>,
        hanchan_only: bool,
    ) -> Result<(usize, usize)> {
        if let Some(n) = limit {
            let logs = db.get_unconverted_logs(Some(n), num_players, hanchan_only, None)?;
            if logs.is_empty() {
                info!("No logs to convert");
                return Ok((0, 0));
            }
            return self.convert_batch(db, logs);
        }

        // Page through the work queue 500 rows at a time with an id cursor.
        // Ids are unique and `ORDER BY id` is total, so `id > cursor` visits
        // every queued row exactly once per run with bounded memory. Still-
        // queued failures are passed by the cursor within a run and retried
        // on the next run via the attempts counter — never re-fetched forever.
        let mut total_ok = 0;
        let mut total_failed = 0;
        let mut after_id: Option<String> = None;
        loop {
            let batch =
                db.get_unconverted_logs(Some(PAGE_SIZE), num_players, hanchan_only, after_id.as_deref())?;
            if batch.is_empty() {
                break;
            }
            let raw_len = batch.len();
            after_id = batch.last().map(|(id, _)| id.clone());
            let (ok, failed) = self.convert_batch(db, batch)?;
            total_ok += ok;
            total_failed += failed;
            if raw_len < PAGE_SIZE {
                break;
            }
        }

        if total_ok == 0 && total_failed == 0 {
            info!("No logs to convert");
        }
        Ok((total_ok, total_failed))
    }

    /// Convert one page of blobs in parallel; mark results sequentially.
    /// Never returns `Err` mid-batch: per-item failures are counted and the
    /// row is marked via `mark_convert_error` for retry-then-quarantine.
    fn convert_batch(
        &self,
        db: &Database,
        logs: Vec<(String, Vec<u8>)>,
    ) -> Result<(usize, usize)> {
        info!("Converting {} logs in parallel", logs.len());

        let pb = crate::util::progress_bar(logs.len() as u64)?;

        let success = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);

        // Collect per-item outcomes; DB updates happen sequentially below
        // (rusqlite Connection is not shareable across the rayon pool).
        let outcomes: Vec<(String, bool)> = logs
            .into_par_iter()
            .progress_with(pb.clone())
            .map(|(id, compressed_xml)| {
                match self.convert_single(&id, &compressed_xml) {
                    Ok(()) => {
                        success.fetch_add(1, Ordering::Relaxed);
                        (id, true)
                    }
                    Err(e) => {
                        warn!("Failed to convert {}: {}", id, e);
                        failed.fetch_add(1, Ordering::Relaxed);
                        (id, false)
                    }
                }
            })
            .collect();

        pb.finish_with_message("Done");

        // Mark converted / convert-error in DB (sequential, but fast).
        // Marking errors are warned, never propagated: prior per-item
        // progress must survive a late DB failure.
        for (id, ok) in &outcomes {
            if *ok {
                if let Err(e) = db.mark_converted(id) {
                    warn!("Failed to mark {} as converted: {}", id, e);
                }
            } else if let Err(e) = db.mark_convert_error(id) {
                warn!("Failed to mark {} convert error: {}", id, e);
            }
        }

        Ok((success.load(Ordering::Relaxed), failed.load(Ordering::Relaxed)))
    }

    /// Convert every game in one mjlog payload (usually exactly one).
    /// All-or-nothing per log id: a late-game failure removes any files
    /// already written for this id so no partial outputs survive.
    fn convert_single(&self, id: &str, compressed_xml: &[u8]) -> Result<()> {
        // Decompress XML
        let mut decoder = GzDecoder::new(compressed_xml);
        let mut xml_str = String::new();
        decoder
            .read_to_string(&mut xml_str)
            .context("Failed to decompress XML")?;

        // Parse XML with mjlog
        let mjlogs = mjlog::parser::parse_mjlogs(&xml_str).context("Failed to parse mjlog XML")?;

        if mjlogs.is_empty() {
            anyhow::bail!("No games found in mjlog");
        }

        if mjlogs.len() != 1 {
            warn!(
                "Log {} contains {} games, converting all",
                id,
                mjlogs.len()
            );
        }

        let total = mjlogs.len();
        let mut written: Vec<PathBuf> = Vec::new();
        for (idx, mjlog) in mjlogs.iter().enumerate() {
            if let Err(e) = self.convert_one_game(id, idx, total, mjlog) {
                for path in &written {
                    if let Err(rm_err) = fs::remove_file(path) {
                        warn!("Failed to clean up {}: {}", path.display(), rm_err);
                    }
                }
                return Err(e);
            }
            written.push(output_path_for(&self.output_dir, id, idx, total));
        }
        Ok(())
    }

    fn convert_one_game(
        &self,
        id: &str,
        idx: usize,
        total: usize,
        mjlog: &mjlog::model::Mjlog,
    ) -> Result<()> {
        // Convert to tenhou JSON
        let tenhou_json = mjlog2json_core::conv::conv_to_tenhou_json(mjlog)
            .context("Failed to convert to tenhou JSON")?;

        // Export to JSON string using tenhou-json's exporter
        let json_str = tenhou_json::exporter::export_tenhou_json(&tenhou_json)
            .context("Failed to export tenhou JSON")?;

        // Parse with convlog
        let log =
            convlog::tenhou::Log::from_json_str(&json_str).context("Failed to parse with convlog")?;

        // Convert to MJAI events
        let events = convlog::tenhou_to_mjai(&log).context("Failed to convert to MJAI")?;

        // Write gzipped MJAI output atomically (tmp file + rename), so a
        // failure never leaves a partial `{id}.mjson.gz` behind.
        let output_path = output_path_for(&self.output_dir, id, idx, total);
        write_atomic(&output_path, |tmp_path| {
            let file = File::create(tmp_path)?;
            let mut encoder = GzEncoder::new(file, Compression::default());

            for event in &events {
                let line = serde_json::to_string(event)?;
                writeln!(encoder, "{line}")?;
            }

            encoder.finish()?;
            Ok(())
        })
    }
}

/// Output filename for one game of a payload: `{id}.mjson.gz` for the common
/// single-game case, `{id}-{idx}.mjson.gz` for multi-game payloads.
fn output_path_for(output_dir: &Path, id: &str, idx: usize, total: usize) -> PathBuf {
    if total == 1 {
        output_dir.join(format!("{id}.mjson.gz"))
    } else {
        output_dir.join(format!("{id}-{idx}.mjson.gz"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_game_keeps_plain_filename() {
        let dir = Path::new("/tmp/out");
        assert_eq!(
            output_path_for(dir, "abc", 0, 1),
            dir.join("abc.mjson.gz")
        );
    }

    #[test]
    fn multi_game_indexes_filenames() {
        let dir = Path::new("/tmp/out");
        assert_eq!(
            output_path_for(dir, "abc", 0, 2),
            dir.join("abc-0.mjson.gz")
        );
        assert_eq!(
            output_path_for(dir, "abc", 1, 2),
            dir.join("abc-1.mjson.gz")
        );
    }
}

