use anyhow::Result;
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{info, warn};

use crate::db::Database;

pub fn export_logs(db: &Database, output_dir: &Path, limit: Option<usize>) -> Result<(usize, usize)> {
    let logs = db.get_unconverted_logs(limit, None, false)?;

    if logs.is_empty() {
        info!("No logs to export");
        return Ok((0, 0));
    }

    fs::create_dir_all(output_dir)?;

    info!("Exporting {} logs (parallel)", logs.len());

    let pb = ProgressBar::new(logs.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"),
    );

    let success = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    logs.par_iter().for_each(|(id, compressed_xml)| {
        match export_single(id, compressed_xml, output_dir) {
            Ok(_) => {
                success.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                warn!("Failed to export {}: {}", id, e);
                failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        pb.inc(1);
    });

    pb.finish_with_message("Done");
    Ok((success.load(Ordering::Relaxed), failed.load(Ordering::Relaxed)))
}

fn export_single(id: &str, compressed_xml: &[u8], output_dir: &Path) -> Result<()> {
    // Decompress XML
    let mut decoder = GzDecoder::new(compressed_xml);
    let mut xml_str = String::new();
    decoder.read_to_string(&mut xml_str)?;

    // Write to file
    let output_path = output_dir.join(format!("{}.xml", id));
    let mut file = File::create(&output_path)?;
    file.write_all(xml_str.as_bytes())?;

    Ok(())
}
