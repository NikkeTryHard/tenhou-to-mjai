use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{ParallelProgressIterator, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{info, warn};

use crate::db::Database;

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
        let logs = db.get_unconverted_logs(limit, num_players, hanchan_only)?;

        if logs.is_empty() {
            info!("No logs to convert");
            return Ok((0, 0));
        }

        info!("Converting {} logs in parallel", logs.len());

        let pb = ProgressBar::new(logs.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let success = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);

        // Collect IDs that succeeded for later DB update
        let successful_ids: Vec<String> = logs
            .into_par_iter()
            .progress_with(pb.clone())
            .filter_map(|(id, compressed_xml)| {
                match self.convert_single(&id, &compressed_xml) {
                    Ok(_) => {
                        success.fetch_add(1, Ordering::Relaxed);
                        Some(id)
                    }
                    Err(e) => {
                        warn!("Failed to convert {}: {}", id, e);
                        failed.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .collect();

        pb.finish_with_message("Done");

        // Mark converted in DB (sequential, but fast)
        for id in &successful_ids {
            if let Err(e) = db.mark_converted(id) {
                warn!("Failed to mark {} as converted: {}", id, e);
            }
        }

        Ok((success.load(Ordering::Relaxed), failed.load(Ordering::Relaxed)))
    }

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

        // Convert to tenhou JSON (take first game)
        let tenhou_json = mjlog2json_core::conv::conv_to_tenhou_json(&mjlogs[0])
            .context("Failed to convert to tenhou JSON")?;

        // Export to JSON string using tenhou-json's exporter
        let json_str = tenhou_json::exporter::export_tenhou_json(&tenhou_json)
            .context("Failed to export tenhou JSON")?;

        // Parse with convlog
        let log =
            convlog::tenhou::Log::from_json_str(&json_str).context("Failed to parse with convlog")?;

        // Convert to MJAI events
        let events = convlog::tenhou_to_mjai(&log).context("Failed to convert to MJAI")?;

        // Write gzipped MJAI output
        let output_path = self.output_dir.join(format!("{}.mjson.gz", id));
        let file = File::create(&output_path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());

        for event in events {
            let line = serde_json::to_string(&event)?;
            writeln!(encoder, "{}", line)?;
        }

        encoder.finish()?;
        Ok(())
    }
}
