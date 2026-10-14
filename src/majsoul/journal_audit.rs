//! Exact journal counts and optional pairwise overlap, with a disk-backed UUID index.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{fs::File, io::{BufRead, BufReader}, path::{Path, PathBuf}};

pub fn run(inputs: &[PathBuf], output: &Path, overlaps: bool) -> Result<()> {
    anyhow::ensure!(!inputs.is_empty(), "provide at least one journal");
    std::fs::create_dir_all(output)?;
    let index = output.join("journal-audit.sqlite");
    anyhow::ensure!(!index.exists(), "audit index already exists; choose a fresh output directory");
    let mut db = rusqlite::Connection::open(index)?;
    db.execute_batch("CREATE TABLE ids(source INTEGER NOT NULL,uuid TEXT NOT NULL,PRIMARY KEY(source,uuid)) WITHOUT ROWID;")?;
    let mut reports = Vec::new();
    for (source, path) in inputs.iter().enumerate() {
        let source = i64::try_from(source)?;
        let snapshot = std::fs::metadata(path)?.len();
        let mut consumed = 0u64;
        let mut rows = 0u64;
        let mut malformed = 0u64;
        let mut missing_uuid = 0u64;
        let mut unique = 0u64;
        let mut modes = std::collections::BTreeSet::new();
        let mut min_day: Option<String> = None;
        let mut max_day: Option<String> = None;
        let mut reader = BufReader::new(File::open(path)?);
        let mut line = String::new();
        let mut tx = db.transaction()?;
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 { break; }
            consumed += u64::try_from(n)?;
            if consumed > snapshot || !line.ends_with('\n') { break; }
            if line.trim().is_empty() { continue; }
            rows += 1;
            let Ok(value) = serde_json::from_str::<Value>(&line) else { malformed += 1; continue; };
            let uuid = value["uuid"].as_str().or_else(|| value["game"]["uuid"].as_str());
            if let Some(uuid) = uuid.filter(|u| !u.is_empty()) {
                unique += u64::try_from(tx.execute("INSERT OR IGNORE INTO ids VALUES(?1,?2)",rusqlite::params![source,uuid])?)?;
            } else { missing_uuid += 1; }
            if let Some(mode) = value["mode"].as_i64().or_else(|| value["modeId"].as_i64()).or_else(|| value["game"]["modeId"].as_i64()) { modes.insert(mode); }
            if let Some(day) = value["day"].as_str().or_else(|| value["_day"].as_str()) {
                if min_day.as_deref().is_none_or(|old| day < old) { min_day = Some(day.to_owned()); }
                if max_day.as_deref().is_none_or(|old| day > old) { max_day = Some(day.to_owned()); }
            }
            if rows.is_multiple_of(100_000) { tx.commit()?; tx = db.transaction()?; }
        }
        tx.commit()?;
        reports.push(json!({"path":path,"snapshot_bytes":snapshot,"rows":rows,"unique_uuids":unique,"malformed":malformed,"missing_uuid":missing_uuid,"duplicates":rows-malformed-missing_uuid-unique,"modes":modes,"day_min":min_day,"day_max":max_day}));
    }
    let mut intersections = Vec::new();
    if overlaps {
        for left in 0..inputs.len() {
            for right in left+1..inputs.len() {
                let count: i64 = db.query_row("SELECT COUNT(*) FROM ids a JOIN ids b ON a.uuid=b.uuid WHERE a.source=?1 AND b.source=?2",rusqlite::params![i64::try_from(left)?,i64::try_from(right)?],|row|row.get(0))?;
                intersections.push(json!({"left":inputs[left],"right":inputs[right],"unique_overlap":count}));
            }
        }
    }
    let report = json!({"journals":reports,"overlaps":intersections});
    std::fs::write(output.join("report.json"),serde_json::to_vec_pretty(&report)?).context("write journal audit report")?;
    println!("{}",serde_json::to_string_pretty(&report)?);
    Ok(())
}
