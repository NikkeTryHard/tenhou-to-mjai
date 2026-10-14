//! Disk-backed integrity audit and canonical rank-aware resolver manifest.
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use anyhow::Result;
use super::types::GameRecord;

pub fn run(fulls: &Path, paipu: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let db_path = out.join("integrity.sqlite");
    anyhow::ensure!(!db_path.exists(), "audit output already exists: choose a fresh directory");
    let mut db = rusqlite::Connection::open(db_path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE games(uuid TEXT PRIMARY KEY, paipu TEXT NOT NULL, mode INTEGER NOT NULL, rank_key TEXT NOT NULL, filename TEXT NOT NULL, metadata TEXT NOT NULL); CREATE TABLE pointers(uuid TEXT PRIMARY KEY, paipu TEXT NOT NULL);")?;
    let mut report = serde_json::json!({"full_rows":0,"full_duplicates":0,"bad_full_rows":0,"paipu_rows":0,"paipu_duplicates":0,"bad_paipu_rows":0,"noncanonical_paipu":0,"orphan_paipu":0,"missing_paipu":0,"remote_verified_all":false});
    let mut manifest = BufWriter::new(std::fs::File::create(out.join("resolver-manifest.jsonl"))?);
    let mut tx = db.transaction()?;
    for (index, line) in BufReader::new(std::fs::File::open(fulls)?).lines().enumerate() {
        let line = line?;
        report["full_rows"] = (report["full_rows"].as_u64().unwrap() + 1).into();
        let game = serde_json::from_str::<GameRecord>(&line).ok();
        let row = game.as_ref().filter(|game| valid_uuid(&game.uuid)).and_then(super::paipu::row);
        let Some(row) = row else { increment(&mut report, "bad_full_rows"); continue; };
        let inserted = tx.execute("INSERT OR IGNORE INTO games VALUES(?1,?2,?3,?4,?5,?6)", rusqlite::params![row["uuid"].as_str(),row["paipu"].as_str(),row["mode"].as_i64(),row["rank_sort_key"].as_str(),row["filename"].as_str(),row.to_string()])?;
        if inserted == 0 { increment(&mut report,"full_duplicates"); } else { writeln!(manifest,"{row}")?; }
        if index.is_multiple_of(100_000) { tx.commit()?; tx = db.transaction()?; }
    }
    tx.commit()?;
    manifest.flush()?;
    let mut tx = db.transaction()?;
    for (index, line) in BufReader::new(std::fs::File::open(paipu)?).lines().enumerate() {
        increment(&mut report,"paipu_rows");
        let line = line?;
        let value = serde_json::from_str::<serde_json::Value>(&line).ok();
        let fields = value.as_ref().and_then(|value| Some((value["uuid"].as_str()?,value["paipu"].as_str()?)));
        let Some((uuid, pointer)) = fields.filter(|(uuid,pointer)| valid_uuid(uuid) && pointer.split_once("_a").is_some_and(|(base,suffix)| base == *uuid && suffix.parse::<i64>().is_ok())) else { increment(&mut report,"bad_paipu_rows"); continue; };
        if tx.execute("INSERT OR IGNORE INTO pointers VALUES(?1,?2)", rusqlite::params![uuid,pointer])? == 0 { increment(&mut report,"paipu_duplicates"); }
        if index.is_multiple_of(100_000) { tx.commit()?; tx = db.transaction()?; }
    }
    tx.commit()?;
    for (key, query) in [
        ("noncanonical_paipu","SELECT COUNT(*) FROM pointers p JOIN games g ON p.uuid=g.uuid WHERE p.paipu!=g.paipu"),
        ("orphan_paipu","SELECT COUNT(*) FROM pointers p LEFT JOIN games g ON p.uuid=g.uuid WHERE g.uuid IS NULL"),
        ("missing_paipu","SELECT COUNT(*) FROM games g LEFT JOIN pointers p ON p.uuid=g.uuid WHERE p.uuid IS NULL"),
    ] { report[key] = db.query_row(query,[],|row| row.get::<_,i64>(0))?.into(); }
    db.execute_batch("CREATE INDEX rank_order ON games(mode,rank_key DESC,uuid);")?;
    let mut sorted = BufWriter::new(std::fs::File::create(out.join("rank-sorted-manifest.jsonl"))?);
    let mut todo = BufWriter::new(std::fs::File::create(out.join("rank-sorted-uuids.txt"))?);
    let mut query = db.prepare("SELECT uuid,metadata FROM games WHERE mode IN (12,16) ORDER BY mode,rank_key DESC,uuid")?;
    for row in query.query_map([],|row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))? {
        let (uuid, metadata) = row?;
        writeln!(sorted,"{metadata}")?;
        writeln!(todo,"{uuid}")?;
    }
    todo.flush()?;
    sorted.flush()?;
    std::fs::write(out.join("report.json"),serde_json::to_vec_pretty(&report)?)?;
    println!("{}",serde_json::to_string_pretty(&report)?);
    Ok(())
}
fn increment(report: &mut serde_json::Value, key: &str) { report[key] = (report[key].as_u64().unwrap() + 1).into(); }
fn valid_uuid(value: &str) -> bool {
    let bare = if value.len() == 43 && value.as_bytes()[..6].iter().all(u8::is_ascii_digit) && value.as_bytes()[6] == b'-' { &value[7..] } else { value };
    bare.len() == 36 && uuid::Uuid::parse_str(bare).is_ok()
}
