use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
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
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    // Count files first for progress bar
    let files: Vec<_> = WalkDir::new(input)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "gz"))
        .collect();

    if files.is_empty() {
        anyhow::bail!("No .mjson.gz files found in {:?}", input);
    }

    info!("Packaging {} files into {:?}", files.len(), output);

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"),
    );

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
