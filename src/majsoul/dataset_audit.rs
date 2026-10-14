//! Cross-match the published on-disk dataset against live harvest journals.
//!
//! Filenames carry the game identity (`YYMMDD_uuid` for Majsoul, Tenhou log
//! ids otherwise), so folder-level matching needs no decompression. Harvest
//! uuids are normalized the same way (lowercase, alphanumeric only) because
//! journal dashes/underscores are cosmetic.
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
struct FolderStat {
    folder: String,
    files: usize,
    date_min: String,
    date_max: String,
    matched_fulls: usize,
    matched_paipu: usize,
}

/// Lowercase alphanumeric fingerprint of a game id from any source.
fn norm(id: &str) -> String {
    id.bytes().filter(u8::is_ascii_alphanumeric).map(|b| b.to_ascii_lowercase() as char).collect()
}

/// Split a dataset filename into (`date_prefix`, id). Majsoul names lead with
/// `YYMMDD_uuid` (the whole stem normalizes to the journal uuid incl. date);
/// anything else falls back to Tenhou style (`YYYYMMDDHH` stamp + log id).
fn split_name(file_name: &str, majsoul: bool) -> Option<(String, String)> {
    let stem = file_name.strip_suffix(".mjai.json.zst")?;
    if majsoul && stem.as_bytes().get(6) == Some(&b'_') {
        Some((stem[..6].to_owned(), norm(stem)))
    } else {
        Some((stem.get(..8).unwrap_or("").to_owned(), norm(stem)))
    }
}

/// uuid set from a JSONL journal's top-level `uuid` field, rayon-parsed.
/// Paipu rows carry `{full_uuid}_a{accountId}`; only the game part matches.
fn journal_uuids(path: &Path, paipu: bool) -> Result<HashSet<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let set: HashSet<String> = bytes
        .par_split(|b| *b == b'\n')
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }
            let v: serde_json::Value = serde_json::from_slice(line).ok()?;
            let id = v.get("uuid")?.as_str()?;
            let game = if paipu { id.split_once("_a").map_or(id, |(g, _)| g) } else { id };
            Some(norm(game))
        })
        .collect();
    Ok(set)
}

fn audit_folder(dir: &Path, fulls: &HashSet<String>, paipu: &HashSet<String>) -> Result<FolderStat> {
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_owned();
    let majsoul = folder.starts_with("majsoul-");
    let mut files = 0usize;
    let mut dates: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("list {}", dir.display()))? {
        let entry = entry?;
        files += 1;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((date, id)) = split_name(name, majsoul) else { continue };
        dates.push(date);
        ids.push(id);
    }
    dates.sort();
    let (matched_fulls, matched_paipu) = ids
        .par_iter()
        .map(|id| (usize::from(fulls.contains(id)), usize::from(paipu.contains(id))))
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    Ok(FolderStat {
        folder,
        files,
        date_min: dates.first().cloned().unwrap_or_default(),
        date_max: dates.last().cloned().unwrap_or_default(),
        matched_fulls,
        matched_paipu,
    })
}

/// Exact filename-set equality proves archive duplication (the throne-2025 case).
fn filename_set(dir: &Path) -> Result<HashSet<String>> {
    let mut set = HashSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        set.insert(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(set)
}

#[allow(clippy::too_many_lines)]
pub fn run(dataset: &Path, fulls_path: &Path, paipu_path: &Path, out: &Path) -> Result<()> {
    tracing::info!("audit: loading harvest uuid sets");
    let fulls = journal_uuids(fulls_path, false)?;
    let paipu = journal_uuids(paipu_path, true)?;
    tracing::info!("audit: fulls={} paipu={}", fulls.len(), paipu.len());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dataset)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let stats: Vec<FolderStat> = dirs
        .par_iter()
        .map(|d| audit_folder(d, &fulls, &paipu))
        .collect::<Result<Vec<_>>>()?;
    // Dupe proof for the reported throne-2025 / houou-2024 pair.
    let mut dupes: HashMap<String, bool> = HashMap::new();
    let by_name: HashMap<&str, &Path> = dirs
        .iter()
        .map(|d| (d.file_name().and_then(|n| n.to_str()).unwrap_or(""), d.as_path()))
        .collect();
    if let (Some(a), Some(b)) = (
        by_name.get("majsoul-throne-2025"),
        by_name.get("tenhou-houou-2024"),
    ) {
        let (sa, sb) = (filename_set(a)?, filename_set(b)?);
        dupes.insert("majsoul-throne-2025 == tenhou-houou-2024".to_owned(), sa == sb);
        tracing::info!("audit: throne25 files={} houou24 files={} equal={}", sa.len(), sb.len(), sa == sb);
    }
    std::fs::create_dir_all(out)?;
    let report = serde_json::json!({"folders": stats, "filename_dupe_checks": dupes});
    std::fs::write(out.join("report.json"), serde_json::to_string_pretty(&report)?)?;
    for s in &stats {
        tracing::info!(
            "audit: {:22} files={:7} {}..{} fulls={} paipu={}",
            s.folder,
            s.files,
            s.date_min,
            s.date_max,
            s.matched_fulls,
            s.matched_paipu
        );
    }
    Ok(())
}
