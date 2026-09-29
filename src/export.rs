use anyhow::Result;
use flate2::read::GzDecoder;
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{info, warn};

use crate::util::write_atomic;
use crate::db::Database;

/// Export page size: bounds the parallel working set per batch.
const PAGE_SIZE: usize = 500;

pub fn export_logs(db: &Database, output_dir: &Path, limit: Option<usize>) -> Result<(usize, usize)> {
    // Export covers every downloaded log, whether or not it was converted.
    let logs = db.get_downloaded_logs(limit)?;

    if logs.is_empty() {
        info!("No logs to export");
        return Ok((0, 0));
    }

    fs::create_dir_all(output_dir)?;

    info!("Exporting {} logs (parallel)", logs.len());
    let pb = crate::util::progress_bar(logs.len() as u64)?;

    let success = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    // Process in 500-row pages to bound the parallel working set.
    for page in logs.chunks(PAGE_SIZE) {
        page.par_iter().for_each(|(id, compressed_xml)| {
            match export_single(id, compressed_xml, output_dir) {
                Ok(()) => {
                    success.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!("Failed to export {}: {}", id, e);
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            pb.inc(1);
        });
    }

    pb.finish_with_message("Done");
    Ok((success.load(Ordering::Relaxed), failed.load(Ordering::Relaxed)))
}

fn export_single(id: &str, compressed_xml: &[u8], output_dir: &Path) -> Result<()> {
    // Decompress XML
    let mut decoder = GzDecoder::new(compressed_xml);
    let mut xml_str = String::new();
    decoder.read_to_string(&mut xml_str)?;

    // Write to file atomically (tmp file + rename), so a failure never
    // leaves a partial `{id}.xml` behind.
    let output_path = output_dir.join(format!("{id}.xml"));
    write_atomic(&output_path, |tmp_path| {
        let mut file = File::create(tmp_path)?;
        file.write_all(xml_str.as_bytes())?;
        Ok(())
    })
}
