use anyhow::{Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use tracing::info;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

pub fn package_directory(input: &Path, output: &Path) -> Result<usize> {
    let file = File::create(output).context("Failed to create zip file")?;
    let mut zip = ZipWriter::new(file);
    // Stored, not Deflated: inputs are already .mjson.gz; recompressing burns CPU for ~zero bytes.
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    // Count files first for progress bar
    let files: Vec<_> = WalkDir::new(input)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "gz"))
        .collect();

    if files.is_empty() {
        anyhow::bail!("No .mjson.gz files found in {}", input.display());
    }

    info!("Packaging {} files into {:?}", files.len(), output);

    let pb = crate::util::progress_bar(files.len() as u64)?;

    let mut count = 0;
    for entry in files {
        let path = entry.path();
        let name = path
            .strip_prefix(input)
            .unwrap_or(path)
            .to_string_lossy();

        zip.start_file(name.as_ref(), options)?;

        let mut f = File::open(path)?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        zip.write_all(&buffer)?;

        count += 1;
        pb.inc(1);
    }

    zip.finish()?;
    pb.finish_with_message("Done");

    Ok(count)
}
