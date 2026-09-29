mod validate;

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct ArchiveResult {
    name: String,
    total_files: u64,
    valid_files: u64,
    invalid_files: u64,
    total_lines: u64,
    bad_files: Vec<(String, Vec<String>)>,
    error: Option<String>,
    elapsed_secs: f64,
}

fn make_err(name: String, start: Instant, msg: String) -> ArchiveResult {
    ArchiveResult {
        name,
        total_files: 0,
        valid_files: 0,
        invalid_files: 0,
        total_lines: 0,
        bad_files: vec![],
        error: Some(msg),
        elapsed_secs: start.elapsed().as_secs_f64(),
    }
}

fn process_archive(path: &Path) -> ArchiveResult {
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let size_mb = path
        .metadata()
        .map(|m| m.len() as f64 / 1048576.0)
        .unwrap_or(0.0);
    eprintln!("\n{}", "=".repeat(70));
    eprintln!("Processing: {name}  ({size_mb:.1} MB)");

    let start = Instant::now();

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return make_err(name, start, format!("open failed: {e}")),
    };

    let decoder = match zstd::stream::Decoder::new(std::io::BufReader::with_capacity(1 << 20, file))
    {
        Ok(d) => d,
        Err(e) => return make_err(name, start, format!("zstd decode failed: {e}")),
    };

    let mut archive = tar::Archive::new(decoder);

    let entries = match archive.entries() {
        Ok(e) => e,
        Err(e) => return make_err(name, start, format!("tar read failed: {e}")),
    };

    let mut total_files = 0u64;
    let mut valid_files = 0u64;
    let mut invalid_files = 0u64;
    let mut total_lines = 0u64;
    let mut bad_files: Vec<(String, Vec<String>)> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  warning: bad tar entry: {e}");
                continue;
            }
        };

        let entry_path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        let fname = entry_path.to_string_lossy().to_string();
        if !fname.ends_with(".mjai.json") {
            continue;
        }

        buf.clear();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }

        let r = validate::validate_file(fname, &buf);
        total_files += 1;
        total_lines += r.line_count;
        if r.valid {
            valid_files += 1;
        } else {
            invalid_files += 1;
            bad_files.push((r.name, r.errors));
        }

        if total_files.is_multiple_of(50_000) {
            eprintln!("  ... {total_files} files processed");
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let status = if invalid_files == 0 {
        "✅ ALL VALID"
    } else {
        "❌ HAS INVALID"
    };
    eprintln!(
        "  {total_files} files, {valid_files} valid, {invalid_files} invalid  {status}  ({elapsed:.1}s)"
    );

    for (bf, errs) in bad_files.iter().take(5) {
        eprintln!("    BAD: {bf}");
        for e in errs.iter().take(3) {
            eprintln!("      - {e}");
        }
    }
    if bad_files.len() > 5 {
        eprintln!("    ... and {} more bad files", bad_files.len() - 5);
    }

    ArchiveResult {
        name,
        total_files,
        valid_files,
        invalid_files,
        total_lines,
        bad_files,
        error: None,
        elapsed_secs: elapsed,
    }
}

fn clean_archive(src: &Path, dst: &Path) -> Result<(u64, u64, u64), String> {
    let file = fs::File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
    let decoder = zstd::stream::Decoder::new(BufReader::with_capacity(1 << 20, file))
        .map_err(|e| format!("zstd decode: {e}"))?;
    let mut in_archive = tar::Archive::new(decoder);
    let in_entries = in_archive
        .entries()
        .map_err(|e| format!("tar entries: {e}"))?;

    let out_file = fs::File::create(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    let encoder =
        zstd::stream::Encoder::new(out_file, 19).map_err(|e| format!("zstd encoder: {e}"))?;
    let mut out_tar = tar::Builder::new(encoder);

    let mut kept = 0u64;
    let mut dropped = 0u64;
    let mut total = 0u64;
    let mut buf: Vec<u8> = Vec::new();

    for entry in in_entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let entry_path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };

        let fname = entry_path.to_string_lossy().to_string();

        if !fname.ends_with(".mjai.json") {
            buf.clear();
            let _ = entry.read_to_end(&mut buf);
            let mut header = entry.header().clone();
            out_tar
                .append_data(&mut header, &entry_path, &buf[..])
                .map_err(|e| format!("write passthrough: {e}"))?;
            continue;
        }

        total += 1;
        buf.clear();
        if entry.read_to_end(&mut buf).is_err() {
            dropped += 1;
            continue;
        }

        let r = validate::validate_file(fname.clone(), &buf);
        if r.valid {
            let mut header = tar::Header::new_gnu();
            header.set_size(buf.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            out_tar
                .append_data(&mut header, &entry_path, &buf[..])
                .map_err(|e| format!("write {fname}: {e}"))?;
            kept += 1;
        } else {
            dropped += 1;
            eprintln!(
                "  DROPPED: {fname} ({})",
                r.errors.first().unwrap_or(&"?".into())
            );
        }

        if total.is_multiple_of(50_000) {
            eprintln!("  ... {total} files scanned, {kept} kept, {dropped} dropped");
        }
    }

    let encoder = out_tar
        .into_inner()
        .map_err(|e| format!("finalize tar: {e}"))?;
    encoder
        .finish()
        .map_err(|e| format!("finalize zstd: {e}"))?;

    Ok((kept, dropped, total))
}

fn run_clean(dataset_dir: &str, staging_dir: &str) {
    let bad_archives = [
        "majsoul-jade-mjai-2019.tar.zst",
        "majsoul-jade-mjai-2023.tar.zst",
        "majsoul-jade-mjai-2024.tar.zst",
        "majsoul-jade-mjai-2026.tar.zst",
    ];

    fs::create_dir_all(staging_dir).expect("cannot create staging dir");

    for archive_name in &bad_archives {
        let src = Path::new(dataset_dir).join(archive_name);
        let dst = Path::new(staging_dir).join(archive_name);

        if !src.exists() {
            eprintln!("SKIP: {archive_name} not found");
            continue;
        }

        let size_mb = src
            .metadata()
            .map(|m| m.len() as f64 / 1048576.0)
            .unwrap_or(0.0);
        eprintln!("\n{}", "=".repeat(70));
        eprintln!("Cleaning: {archive_name}  ({size_mb:.1} MB)");

        let start = Instant::now();
        match clean_archive(&src, &dst) {
            Ok((kept, dropped, total)) => {
                let elapsed = start.elapsed().as_secs_f64();
                let dst_size = dst
                    .metadata()
                    .map(|m| m.len() as f64 / 1048576.0)
                    .unwrap_or(0.0);
                eprintln!("  Done: {total} total, {kept} kept, {dropped} dropped  ({elapsed:.1}s)");
                eprintln!("  {size_mb:.1} MB -> {dst_size:.1} MB");
                eprintln!("  Output: {}", dst.display());

                // replace original
                fs::rename(&dst, &src).unwrap_or_else(|e| {
                    // cross-device: /mnt/dev → /home requires copy+delete
                    eprintln!("  rename failed ({e}), copying instead...");
                    fs::copy(&dst, &src).expect("copy failed");
                    fs::remove_file(&dst).expect("remove staging file failed");
                });
                eprintln!("  Replaced original ✅");
            }
            Err(e) => {
                eprintln!("  FAILED: {e}");
                let _ = fs::remove_file(&dst);
            }
        }
    }

    let _ = fs::remove_dir(staging_dir);
    eprintln!("\nAll done. Run validator again to confirm.");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let clean_mode = args.iter().any(|a| a == "--clean");
    let dataset_dir = args
        .iter()
        .find(|a| !a.starts_with('-') && *a != &args[0])
        .cloned()
        .unwrap_or_else(|| "/home/nikketryhard/dev/tenhou-to-mjai/dataset/mjai".into());

    if clean_mode {
        run_clean(&dataset_dir, "/mnt/dev/mjai-clean");
        return;
    }

    let dir = Path::new(&dataset_dir);

    let mut archives: Vec<PathBuf> = fs::read_dir(dir)
        .expect("cannot read dataset dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "zst"))
        .collect();
    archives.sort();

    eprintln!("Found {} archives in {}", archives.len(), dataset_dir);

    let total_start = Instant::now();

    let results: Vec<ArchiveResult> = archives.iter().map(|a| process_archive(a)).collect();

    let total_time = total_start.elapsed().as_secs_f64();

    println!("\n{}", "=".repeat(70));
    println!("FINAL SUMMARY");
    println!("{}", "=".repeat(70));

    let total_archives = results.len();
    let total_files: u64 = results.iter().map(|r| r.total_files).sum();
    let total_valid: u64 = results.iter().map(|r| r.valid_files).sum();
    let total_invalid: u64 = results.iter().map(|r| r.invalid_files).sum();
    let total_lines: u64 = results.iter().map(|r| r.total_lines).sum();

    println!("\nArchives:       {total_archives}");
    println!("Total files:    {total_files}");
    println!("Valid:          {total_valid}");
    println!("Invalid:        {total_invalid}");
    println!("Total lines:    {total_lines}");
    println!("Total time:     {total_time:.1}s");

    let extract_errors: Vec<&ArchiveResult> =
        results.iter().filter(|r| r.error.is_some()).collect();
    if !extract_errors.is_empty() {
        println!("\n❌ EXTRACTION ERRORS ({}):", extract_errors.len());
        for r in &extract_errors {
            println!("  - {}: {}", r.name, r.error.as_deref().unwrap_or("?"));
        }
    }

    let bad_archives: Vec<&ArchiveResult> =
        results.iter().filter(|r| r.invalid_files > 0).collect();
    if !bad_archives.is_empty() {
        println!("\n❌ ARCHIVES WITH INVALID FILES ({}):", bad_archives.len());
        for r in &bad_archives {
            println!(
                "  - {}: {} invalid / {} total",
                r.name, r.invalid_files, r.total_files
            );
            for (bf, errs) in r.bad_files.iter().take(3) {
                println!("      {bf}: {}", errs.first().unwrap_or(&"?".into()));
            }
        }
    } else if extract_errors.is_empty() {
        println!("\n✅ ALL ARCHIVES VALID");
    }

    println!("\nPER-SOURCE BREAKDOWN:");
    let mut sources: HashMap<String, (u64, u64, u64, u64)> = HashMap::new();
    for r in &results {
        let source = r.name.split("-mjai-").next().unwrap_or(&r.name).to_string();
        let e = sources.entry(source).or_insert((0, 0, 0, 0));
        e.0 += r.total_files;
        e.1 += r.valid_files;
        e.2 += r.invalid_files;
        e.3 += r.total_lines;
    }

    println!(
        "{:<25} {:>10} {:>10} {:>10} {:>15}",
        "Source", "Files", "Valid", "Invalid", "Lines"
    );
    println!("{}", "-".repeat(75));
    let mut source_list: Vec<_> = sources.iter().collect();
    source_list.sort_by_key(|(k, _)| (*k).clone());
    for (source, (files, valid, invalid, lines)) in &source_list {
        let status = if *invalid == 0 { "✅" } else { "❌" };
        println!(
            "{:<25} {:>10} {:>10} {:>10} {:>15} {status}",
            source, files, valid, invalid, lines
        );
    }

    println!("\nPER-ARCHIVE DETAIL:");
    println!(
        "{:<45} {:>8} {:>8} {:>8} {:>12} {:>6}",
        "Archive", "Files", "Valid", "Invalid", "Lines", "Time"
    );
    println!("{}", "-".repeat(92));
    for r in &results {
        let status = if r.error.is_some() {
            "💥"
        } else if r.invalid_files > 0 {
            "❌"
        } else {
            "✅"
        };
        println!(
            "{:<45} {:>8} {:>8} {:>8} {:>12} {:>5.1}s {status}",
            r.name, r.total_files, r.valid_files, r.invalid_files, r.total_lines, r.elapsed_secs
        );
    }

    if total_invalid > 0 || !extract_errors.is_empty() {
        std::process::exit(1);
    }
}
