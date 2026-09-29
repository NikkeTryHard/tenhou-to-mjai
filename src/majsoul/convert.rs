//! Majsoul to MJAI conversion.

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::ParallelProgressIterator;
use rayon::prelude::*;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{debug, warn};

use crate::db::Database;

use super::events::{
    parse_record_action, AnGangAddGangType, ChiPengGangType, GameEvent,
};
use super::proto::{RecordAction, decode_game_record};

pub struct MajsoulConverter {
    output_dir: PathBuf,
}

impl MajsoulConverter {
    pub fn new(output_dir: impl AsRef<Path>) -> Result<Self> {
        let output_dir = output_dir.as_ref().to_path_buf();
        fs::create_dir_all(&output_dir)?;
        Ok(Self { output_dir })
    }

    /// Convert all unconverted Majsoul logs from database
    pub fn convert_logs(
        &self,
        db: &Database,
        limit: Option<usize>,
        num_players: Option<i32>,
        hanchan_only: bool,
    ) -> Result<(usize, usize)> {
        let logs = db.get_majsoul_unconverted(limit, num_players, hanchan_only)?;

        if logs.is_empty() {
            tracing::info!("No Majsoul logs to convert");
            return Ok((0, 0));
        }

        tracing::info!("Converting {} Majsoul logs in parallel", logs.len());

        let pb = crate::util::progress_bar(logs.len() as u64)?;

        let success = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);

        // Collect per-item outcomes; failures are persisted below so corrupt rows
        // quarantine instead of slowing every batch (same policy as Tenhou convert).
        let outcomes: Vec<(String, bool)> = logs
            .into_par_iter()
            .progress_with(pb.clone())
            .map(|(uuid, raw_data)| {
                match self.convert_single(&uuid, &raw_data) {
                    Ok(()) => {
                        success.fetch_add(1, Ordering::Relaxed);
                        (uuid, true)
                    }
                    Err(e) => {
                        warn!("Failed to convert {}: {}", uuid, e);
                        failed.fetch_add(1, Ordering::Relaxed);
                        (uuid, false)
                    }
                }
            })
            .collect();

        pb.finish_with_message("Done");

        // Mark outcomes in DB (sequential, but fast)
        for (uuid, ok) in &outcomes {
            if *ok {
                if let Err(e) = db.mark_majsoul_converted(uuid) {
                    warn!("Failed to mark {} as converted: {}", uuid, e);
                }
            } else if let Err(e) = db.mark_majsoul_convert_error(uuid) {
                warn!("Failed to mark {} convert error: {}", uuid, e);
            }
        }

        Ok((
            success.load(Ordering::Relaxed),
            failed.load(Ordering::Relaxed),
        ))
    }

    /// Convert a single game record to MJAI format
    fn convert_single(&self, uuid: &str, raw_data: &[u8]) -> Result<()> {
        // Decode the protobuf game record
        let record = decode_game_record(raw_data)
            .with_context(|| format!("Failed to decode game record: {uuid}"))?;

        if record.records.is_empty() {
            anyhow::bail!("No game records found in {uuid}");
        }

        debug!(
            "Decoding {}: {} players, {} records",
            uuid,
            record.player_names.len(),
            record.records.len()
        );

        let (events, unknown) = collect_events(&record.records)?;
        if unknown > 0 {
            warn!("{}: skipped {} unknown Record actions", uuid, unknown);
        }

        // Convert game events to MJAI format
        let mjai_events = self.events_to_mjai(&record.player_names, &events)?;

        // Write gzipped MJAI output
        let output_path = self.output_dir.join(format!("{uuid}.mjson.gz"));
        let file = File::create(&output_path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());

        for event in mjai_events {
            let line = serde_json::to_string(&event)?;
            writeln!(encoder, "{line}")?;
        }

        encoder.finish()?;
        Ok(())
    }

    /// Convert parsed game events to MJAI JSON events
    // Length is one match arm per event type (E3-verified emission); splitting
    // the arms across helpers would churn the exact-MJAI golden fixtures.
    #[allow(clippy::too_many_lines)]
    // Method for API symmetry with `convert_single`/`convert_logs` (future
    // instance config would use `self`); the unused receiver is intentional.
    #[allow(clippy::unused_self)]
    fn events_to_mjai(
        &self,
        player_names: &[String],
        events: &[GameEvent],
    ) -> Result<Vec<Value>> {
        let mut mjai_events = Vec::new();
        let num_players = player_names.len();

        // Start game event
        mjai_events.push(json!({
            "type": "start_game",
            "names": player_names,
        }));

        // Track state for reach_accepted (only on reacher's next DealTile).
        let mut pending_reach: Option<u32> = None;
        let mut dropped_reach = 0usize;
        // Last discarder for ron; reset each kyoku. Kakan sets it (chankan).
        let mut last_discarder: Option<u32> = None;
        let mut oya: u32 = 0;

        for event in events {
            match event {
                GameEvent::NewRound(nr) => {
                    if pending_reach.take().is_some() {
                        dropped_reach += 1;
                    }
                    last_discarder = None;
                    oya = nr.ju;

                    // Bakaze: 0->E,1->S,2->W,3->N, else corrupt.
                    let bakaze = match nr.chang {
                        0 => "E",
                        1 => "S",
                        2 => "W",
                        3 => "N",
                        n => anyhow::bail!("Invalid bakaze: {n}"),
                    };

                    // Collect tehais (starting hands)
                    let tehais: Vec<Vec<&str>> = nr
                        .tiles
                        .iter()
                        .take(num_players)
                        .map(|t| t.iter().map(std::string::String::as_str).collect())
                        .collect();

                    mjai_events.push(json!({
                        "type": "start_kyoku",
                        "bakaze": bakaze,
                        "dora_marker": nr.dora_marker,
                        "kyoku": nr.ju + 1,
                        "honba": nr.ben,
                        "kyotaku": nr.liqibang,
                        "oya": nr.ju,
                        "scores": nr.scores,
                        "tehais": tehais,
                    }));
                }

                GameEvent::DealTile(dt) => {
                    // reach_accepted only when the reacher draws next.
                    if pending_reach == Some(dt.seat) {
                        pending_reach = None;
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": dt.seat,
                        }));
                    }

                    mjai_events.push(json!({
                        "type": "tsumo",
                        "actor": dt.seat,
                        "pai": dt.tile,
                    }));
                }

                GameEvent::DiscardTile(dt) => {
                    // Track last discarder for ron target calculation
                    last_discarder = Some(dt.seat);

                    // Check for riichi declaration
                    if dt.is_liqi || dt.is_wliqi {
                        mjai_events.push(json!({
                            "type": "reach",
                            "actor": dt.seat,
                        }));
                        pending_reach = Some(dt.seat);
                    }

                    mjai_events.push(json!({
                        "type": "dahai",
                        "actor": dt.seat,
                        "pai": dt.tile,
                        "tsumogiri": dt.moqie,
                    }));
                }

                GameEvent::ChiPengGang(cpg) => {
                    let target = cpg
                        .froms
                        .first()
                        .copied()
                        .ok_or_else(|| anyhow::anyhow!("empty chi/pon tiles"))?;
                    let pai = cpg
                        .tiles
                        .last()
                        .ok_or_else(|| anyhow::anyhow!("empty chi/pon tiles"))?;

                    // saturating: empty tiles already bailed via first/last above, so 0-len here is unreachable; take(0) stays a safe no-op.
                    match cpg.call_type {
                        ChiPengGangType::Chi => {
                            mjai_events.push(json!({
                                "type": "chi",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": pai,
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                        ChiPengGangType::Pon => {
                            mjai_events.push(json!({
                                "type": "pon",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": pai,
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                        ChiPengGangType::Daiminkan => {
                            mjai_events.push(json!({
                                "type": "daiminkan",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": pai,
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                    }
                }

                GameEvent::AnGangAddGang(ag) => {
                    match ag.gang_type {
                        AnGangAddGangType::Ankan => {
                            // Passthrough: record's real tile repeated; 0m/0p/0s
                            // already map to 5Xr via tile_str_to_mjai.
                            let consumed = vec![ag.tiles.clone(); 4];
                            mjai_events.push(json!({
                                "type": "ankan",
                                "actor": ag.seat,
                                "consumed": consumed,
                            }));
                        }
                        AnGangAddGangType::Kakan => {
                            // Chankan needs the kakan actor as discarder.
                            last_discarder = Some(ag.seat);
                            mjai_events.push(json!({
                                "type": "kakan",
                                "actor": ag.seat,
                                "pai": &ag.tiles,
                            }));
                        }
                    }
                }

                GameEvent::Hule(h) => {
                    if pending_reach.take().is_some() {
                        dropped_reach += 1;
                    }

                    // One hora per winner (double-ron shares the discarder); bail instead of defaulting target to seat 0.
                    for hule in &h.hules {
                        let target = if hule.zimo {
                            hule.seat
                        } else {
                            match last_discarder {
                                Some(d) => d,
                                None => anyhow::bail!("ron without preceding discard"),
                            }
                        };
                        // Majsoul splits tsumo income: dealer collects qin x 3, non-dealer collects qin + 2 x xian (oya is the dealer seat).
                        let points = if hule.zimo {
                            if hule.seat == oya {
                                hule.point_zimo_qin * 3
                            } else {
                                hule.point_zimo_qin + hule.point_zimo_xian * 2
                            }
                        } else {
                            hule.point_rong
                        };
                        mjai_events.push(json!({
                            "type": "hora",
                            "actor": hule.seat,
                            "target": target,
                            "pai": hule.hu_tile,
                            "fu": hule.fu,
                            "points": points,
                            "deltas": h.delta_scores,
                            "scores": h.scores,
                        }));
                    }

                    mjai_events.push(json!({
                        "type": "end_kyoku",
                    }));
                }

                // NoTile = exhaustive draw carrying tenpai settlement scores; LiuJu = abortive draw carrying reason only, no scores.
                GameEvent::NoTile(nt) => {
                    if pending_reach.take().is_some() {
                        dropped_reach += 1;
                    }
                    if nt.scores.is_empty() && nt.delta_scores.is_empty() {
                        anyhow::bail!("NoTile with no scores");
                    }
                    mjai_events.push(json!({
                        "type": "ryukyoku",
                        "scores": nt.scores,
                        "deltas": nt.delta_scores,
                    }));

                    mjai_events.push(json!({
                        "type": "end_kyoku",
                    }));
                }


                GameEvent::LiuJu(lj) => {
                    if pending_reach.take().is_some() {
                        dropped_reach += 1;
                    }
                    match lj.liuju_type {
                        1 => mjai_events.push(json!({"type": "ryukyoku", "reason": "yao9"})),
                        2 => mjai_events.push(json!({"type": "ryukyoku", "reason": "reach4"})),
                        3 => mjai_events.push(json!({"type": "ryukyoku", "reason": "kan4"})),
                        4 => mjai_events.push(json!({"type": "ryukyoku", "reason": "kaze4"})),
                        5 => mjai_events.push(json!({"type": "ryukyoku", "reason": "ron3"})),
                        _ => mjai_events.push(json!({"type": "ryukyoku"})),
                    }

                    mjai_events.push(json!({
                        "type": "end_kyoku",
                    }));
                }

                GameEvent::BaBei(bb) => {
                    // North tile (kita) in 3-player mahjong
                    mjai_events.push(json!({
                        "type": "nukidora",
                        "actor": bb.seat,
                        "pai": "N",
                    }));
                }
            }
        }

        if pending_reach.take().is_some() {
            dropped_reach += 1;
        }
        if dropped_reach > 0 {
            warn!("dropped {} pending reach declarations without acceptance", dropped_reach);
        }
        // End game event
        mjai_events.push(json!({
            "type": "end_game",
        }));

        Ok(mjai_events)
    }
}

/// Convert raw .pb files from a directory to MJAI format (no database needed)
///
/// Reads .pb files from `input_dir`, converts to .mjai.json in `output_dir`,
/// and optionally deletes the .pb files after successful conversion.
pub fn convert_raw_files(
    input_dir: &Path,
    output_dir: &Path,
    delete_after: bool,
) -> Result<(usize, usize)> {
    fs::create_dir_all(output_dir)?;

    // Collect all .pb files
    let pb_files: Vec<PathBuf> = fs::read_dir(input_dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "pb"))
        .collect();

    if pb_files.is_empty() {
        tracing::info!("No .pb files found in {}", input_dir.display());
        return Ok((0, 0));
    }

    // Filter out already-converted files
    let pending: Vec<PathBuf> = pb_files
        .into_iter()
        .filter(|p| {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            let mjai_path = output_dir.join(format!("{stem}.mjai.json"));
            !mjai_path.exists()
        })
        .collect();

    if pending.is_empty() {
        tracing::info!("All files already converted");
        return Ok((0, 0));
    }

    tracing::info!("Converting {} .pb files to MJAI", pending.len());

    let pb = crate::util::progress_bar_with(pending.len() as u64, "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) ({eta})", "#>-")?;

    let success = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    let converter = MajsoulConverter::new(output_dir)?;

    pending
        .par_iter()
        .progress_with(pb.clone())
        .for_each(|pb_path| {
            let stem = pb_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            match convert_single_file(&converter, pb_path, output_dir) {
                Ok(()) => {
                    success.fetch_add(1, Ordering::Relaxed);
                    if delete_after {
                        if let Err(e) = fs::remove_file(pb_path) {
                            warn!("Failed to remove {}: {}", pb_path.display(), e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to convert {}: {:#}", stem, e);
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

    pb.finish_with_message("Done");

    Ok((
        success.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
    ))
}

/// Parse all record actions into game events, counting unknown skips.
fn collect_events(records: &[RecordAction]) -> Result<(Vec<GameEvent>, usize)> {
    let mut events = Vec::new();
    let mut unknown = 0usize;
    for action in records {
        match parse_record_action(&action.name, &action.data)? {
            Some(event) => events.push(event),
            None => unknown += 1,
        }
    }
    Ok((events, unknown))
}

/// Convert a single .pb file to MJAI .mjai.json
fn convert_single_file(
    converter: &MajsoulConverter,
    pb_path: &Path,
    output_dir: &Path,
) -> Result<()> {
    let raw_data = fs::read(pb_path).context("Failed to read .pb file")?;

    if raw_data.len() < 20 {
        anyhow::bail!("File too small ({} bytes)", raw_data.len());
    }

    let stem = pb_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Decode the protobuf game record
    let record = decode_game_record(&raw_data)
        .with_context(|| format!("Failed to decode: {stem}"))?;

    if record.records.is_empty() {
        anyhow::bail!("No game records found in {stem}");
    }

    let (events, unknown) = collect_events(&record.records)?;
    if unknown > 0 {
        warn!("{}: skipped {} unknown Record actions", stem, unknown);
    }

    let mjai_events = converter.events_to_mjai(&record.player_names, &events)?;

    // Write plain MJAI output (not gzipped - easier to work with)
    let output_path = output_dir.join(format!("{stem}.mjai.json"));
    let mut file = File::create(&output_path)?;

    for event in mjai_events {
        let line = serde_json::to_string(&event)?;
        writeln!(file, "{line}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_converter_creation() {
        let dir = std::env::temp_dir().join("majsoul_convert_test");
        let converter = MajsoulConverter::new(&dir).unwrap();
        assert!(converter.output_dir.exists());
        // Cleanup
        let _ = std::fs::remove_dir(&dir);
    }

    fn test_names() -> Vec<String> {
        vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()]
    }

    fn test_round() -> crate::majsoul::events::NewRound {
        crate::majsoul::events::NewRound {
            chang: 0,
            ju: 0,
            ben: 0,
            liqibang: 0,
            dora_marker: "5m".to_string(),
            scores: vec![25000, 25000, 25000, 25000],
            tiles: vec![vec!["1m".to_string(); 13]; 4],
        }
    }

    fn converter_for_test() -> MajsoulConverter {
        let dir = std::env::temp_dir().join("majsoul_golden_test");
        MajsoulConverter::new(&dir).unwrap()
    }

    fn assert_no_seat0_fallback(events: &[serde_json::Value]) {
        // No hora may carry a defaulted target: every hora must have explicit
        // actor/target/pai, and no ryukyoku may carry reason "unknown".
        for e in events {
            if e.get("type").and_then(|v| v.as_str()) == Some("hora") {
                assert!(e.get("actor").is_some());
                assert!(e.get("target").is_some());
                assert!(e.get("pai").is_some());
            }
            if let Some(reason) = e.get("reason").and_then(|v| v.as_str()) {
                assert_ne!(reason, "unknown");
            }
        }
    }

    #[test]
    fn test_golden_ron() {
        use crate::majsoul::events::{DiscardTile, GameEvent, Hule, HuleInfo};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::DiscardTile(DiscardTile { seat: 0, tile: "5m".to_string(), is_liqi: false, moqie: false, is_wliqi: false }),
            GameEvent::Hule(Hule {
                hules: vec![HuleInfo { seat: 1, zimo: false, hand: vec![], hu_tile: "5m".to_string(), fu: 30, point_rong: 8000, point_zimo_qin: 0, point_zimo_xian: 0 }],
                delta_scores: vec![-8000, 8000, 0, 0],
                scores: vec![17000, 33000, 25000, 25000],
            }),
        ];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let hora = out.iter().find(|e| e.get("type").and_then(|v| v.as_str()) == Some("hora")).unwrap();
        assert_eq!(hora.get("actor").and_then(serde_json::Value::as_u64).unwrap(), 1);
        assert_eq!(hora.get("target").and_then(serde_json::Value::as_u64).unwrap(), 0);
        assert_eq!(hora.get("fu").and_then(serde_json::Value::as_u64).unwrap(), 30);
        assert_eq!(hora.get("points").and_then(serde_json::Value::as_i64).unwrap(), 8000);
        assert_no_seat0_fallback(&out);
    }

    #[test]
    fn test_golden_ankan5_passthrough() {
        use crate::majsoul::events::{AnGangAddGang, AnGangAddGangType, GameEvent};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::AnGangAddGang(AnGangAddGang { seat: 0, gang_type: AnGangAddGangType::Ankan, tiles: "5pr".to_string() }),
        ];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let ankan = out.iter().find(|e| e.get("type").and_then(|v| v.as_str()) == Some("ankan")).unwrap();
        let consumed = ankan.get("consumed").and_then(|v| v.as_array()).unwrap();
        assert_eq!(consumed.len(), 4);
        // Passthrough only: all four equal the record tile, no red injection.
        for t in consumed {
            assert_eq!(t.as_str().unwrap(), "5pr");
        }
    }

    #[test]
    fn test_golden_double_ron_split_targets() {
        use crate::majsoul::events::{DiscardTile, GameEvent, Hule, HuleInfo};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::DiscardTile(DiscardTile { seat: 2, tile: "3s".to_string(), is_liqi: false, moqie: false, is_wliqi: false }),
            GameEvent::Hule(Hule {
                hules: vec![
                    HuleInfo { seat: 0, zimo: false, hand: vec![], hu_tile: "3s".to_string(), fu: 30, point_rong: 4000, point_zimo_qin: 0, point_zimo_xian: 0 },
                    HuleInfo { seat: 1, zimo: false, hand: vec![], hu_tile: "3s".to_string(), fu: 40, point_rong: 4000, point_zimo_qin: 0, point_zimo_xian: 0 },
                ],
                delta_scores: vec![4000, 4000, -8000, 0],
                scores: vec![29000, 29000, 17000, 25000],
            }),
        ];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let horas: Vec<_> = out.iter().filter(|e| e.get("type").and_then(|v| v.as_str()) == Some("hora")).collect();
        assert_eq!(horas.len(), 2);
        // Per-winner targets (both the discarder here, computed per winner).
        assert_eq!(horas[0].get("target").and_then(serde_json::Value::as_u64).unwrap(), 2);
        assert_eq!(horas[1].get("target").and_then(serde_json::Value::as_u64).unwrap(), 2);
        assert_eq!(horas[0].get("fu").and_then(serde_json::Value::as_u64).unwrap(), 30);
        assert_eq!(horas[1].get("fu").and_then(serde_json::Value::as_u64).unwrap(), 40);
        assert_no_seat0_fallback(&out);
    }

    #[test]
    fn test_golden_scored_ryukyoku() {
        use crate::majsoul::events::{GameEvent, NoTile};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::NoTile(NoTile { scores: vec![26000, 24000, 25000, 25000], delta_scores: vec![1000, -1000, 0, 0] }),
        ];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let ryu = out.iter().find(|e| e.get("type").and_then(|v| v.as_str()) == Some("ryukyoku")).unwrap();
        assert_eq!(ryu.get("scores").and_then(|v| v.as_array()).unwrap().len(), 4);
        assert!(ryu.get("deltas").and_then(|v| v.as_array()).unwrap().iter().any(|v| v.as_i64().unwrap() != 0));
    }

    #[test]
    fn test_golden_chankan_uses_kakan_actor() {
        use crate::majsoul::events::{AnGangAddGang, AnGangAddGangType, DiscardTile, GameEvent, Hule, HuleInfo};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::DiscardTile(DiscardTile { seat: 1, tile: "2m".to_string(), is_liqi: false, moqie: false, is_wliqi: false }),
            GameEvent::AnGangAddGang(AnGangAddGang { seat: 2, gang_type: AnGangAddGangType::Kakan, tiles: "3m".to_string() }),
            GameEvent::Hule(Hule {
                hules: vec![HuleInfo { seat: 0, zimo: false, hand: vec![], hu_tile: "3m".to_string(), fu: 30, point_rong: 8000, point_zimo_qin: 0, point_zimo_xian: 0 }],
                delta_scores: vec![8000, 0, -8000, 0],
                scores: vec![33000, 25000, 17000, 25000],
            }),
        ];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let hora = out.iter().find(|e| e.get("type").and_then(|v| v.as_str()) == Some("hora")).unwrap();
        // Chankan target is the kakan actor (2), not the earlier discarder (1).
        assert_eq!(hora.get("target").and_then(serde_json::Value::as_u64).unwrap(), 2);
    }

    #[test]
    fn test_ron_without_discard_bails() {
        use crate::majsoul::events::{GameEvent, Hule, HuleInfo};
        let c = converter_for_test();
        let events = vec![
            GameEvent::NewRound(test_round()),
            GameEvent::Hule(Hule {
                hules: vec![HuleInfo { seat: 1, zimo: false, hand: vec![], hu_tile: "5m".to_string(), fu: 30, point_rong: 8000, point_zimo_qin: 0, point_zimo_xian: 0 }],
                delta_scores: vec![],
                scores: vec![],
            }),
        ];
        assert!(c.events_to_mjai(&test_names(), &events).is_err());
    }

    #[test]
    fn test_liuju_unknown_has_no_reason_key() {
        use crate::majsoul::events::{GameEvent, LiuJu};
        let c = converter_for_test();
        let events = vec![GameEvent::NewRound(test_round()), GameEvent::LiuJu(LiuJu { liuju_type: 99 })];
        let out = c.events_to_mjai(&test_names(), &events).unwrap();
        let ryu = out.iter().find(|e| e.get("type").and_then(|v| v.as_str()) == Some("ryukyoku")).unwrap();
        assert!(ryu.get("reason").is_none());
    }
}
