//! Majsoul to MJAI conversion.

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{ParallelProgressIterator, ProgressBar, ProgressStyle};
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
use super::proto::decode_game_record;

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
    pub fn convert_logs(&self, db: &Database, limit: Option<usize>) -> Result<(usize, usize)> {
        let logs = db.get_majsoul_unconverted(limit)?;

        if logs.is_empty() {
            tracing::info!("No Majsoul logs to convert");
            return Ok((0, 0));
        }

        tracing::info!("Converting {} Majsoul logs in parallel", logs.len());

        let pb = ProgressBar::new(logs.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
                .progress_chars("#>-"),
        );

        let success = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);

        // Collect UUIDs that succeeded for later DB update
        let successful_uuids: Vec<String> = logs
            .into_par_iter()
            .progress_with(pb.clone())
            .filter_map(|(uuid, raw_data)| {
                match self.convert_single(&uuid, &raw_data) {
                    Ok(_) => {
                        success.fetch_add(1, Ordering::Relaxed);
                        Some(uuid)
                    }
                    Err(e) => {
                        warn!("Failed to convert {}: {}", uuid, e);
                        failed.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .collect();

        pb.finish_with_message("Done");

        // Mark converted in DB (sequential, but fast)
        for uuid in &successful_uuids {
            if let Err(e) = db.mark_majsoul_converted(uuid) {
                warn!("Failed to mark {} as converted: {}", uuid, e);
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
            .with_context(|| format!("Failed to decode game record: {}", uuid))?;

        debug!(
            "Decoding {}: {} players, {} records",
            uuid,
            record.player_names.len(),
            record.records.len()
        );

        // Parse all record actions into game events
        let mut events: Vec<GameEvent> = Vec::new();
        for action in &record.records {
            if let Some(event) = parse_record_action(&action.name, &action.data)? {
                events.push(event);
            }
        }

        // Convert game events to MJAI format
        let mjai_events = self.events_to_mjai(&record.player_names, &events)?;

        // Write gzipped MJAI output
        let output_path = self.output_dir.join(format!("{}.mjson.gz", uuid));
        let file = File::create(&output_path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());

        for event in mjai_events {
            let line = serde_json::to_string(&event)?;
            writeln!(encoder, "{}", line)?;
        }

        encoder.finish()?;
        Ok(())
    }

    /// Convert parsed game events to MJAI JSON events
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

        // Track state for reach_accepted
        let mut pending_reach: Option<u32> = None;

        for event in events {
            match event {
                GameEvent::NewRound(nr) => {
                    // Emit reach_accepted if pending from previous round
                    pending_reach = None;

                    // Calculate bakaze (round wind)
                    let bakaze = match nr.chang {
                        0 => "E",
                        1 => "S",
                        2 => "W",
                        _ => "N",
                    };

                    // Collect tehais (starting hands)
                    let tehais: Vec<Vec<&str>> = nr
                        .tiles
                        .iter()
                        .take(num_players)
                        .map(|t| t.iter().map(|s| s.as_str()).collect())
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
                    // If there was a pending reach, emit reach_accepted
                    if let Some(actor) = pending_reach.take() {
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": actor,
                        }));
                    }

                    mjai_events.push(json!({
                        "type": "tsumo",
                        "actor": dt.seat,
                        "pai": dt.tile,
                    }));
                }

                GameEvent::DiscardTile(dt) => {
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
                    // If there was a pending reach, emit reach_accepted
                    if let Some(actor) = pending_reach.take() {
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": actor,
                        }));
                    }

                    // Determine who the call was made from
                    let target = cpg.froms.first().copied().unwrap_or(0);

                    match cpg.call_type {
                        ChiPengGangType::Chi => {
                            mjai_events.push(json!({
                                "type": "chi",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": cpg.tiles.last().unwrap_or(&String::new()),
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                        ChiPengGangType::Pon => {
                            mjai_events.push(json!({
                                "type": "pon",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": cpg.tiles.last().unwrap_or(&String::new()),
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                        ChiPengGangType::Daiminkan => {
                            mjai_events.push(json!({
                                "type": "daiminkan",
                                "actor": cpg.seat,
                                "target": target,
                                "pai": cpg.tiles.last().unwrap_or(&String::new()),
                                "consumed": cpg.tiles.iter().take(cpg.tiles.len().saturating_sub(1)).collect::<Vec<_>>(),
                            }));
                        }
                    }
                }

                GameEvent::AnGangAddGang(ag) => {
                    match ag.gang_type {
                        AnGangAddGangType::Ankan => {
                            mjai_events.push(json!({
                                "type": "ankan",
                                "actor": ag.seat,
                                "consumed": [&ag.tiles, &ag.tiles, &ag.tiles, &ag.tiles],
                            }));
                        }
                        AnGangAddGangType::Kakan => {
                            mjai_events.push(json!({
                                "type": "kakan",
                                "actor": ag.seat,
                                "pai": ag.tiles,
                                "consumed": [&ag.tiles, &ag.tiles, &ag.tiles],
                            }));
                        }
                    }
                }

                GameEvent::Hule(h) => {
                    // If there was a pending reach, emit reach_accepted
                    if let Some(actor) = pending_reach.take() {
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": actor,
                        }));
                    }

                    for hule in &h.hules {
                        if hule.zimo {
                            mjai_events.push(json!({
                                "type": "hora",
                                "actor": hule.seat,
                                "target": hule.seat,
                                "pai": hule.hu_tile,
                            }));
                        } else {
                            // Ron - need to find who discarded
                            // The target is the previous player who discarded
                            // For simplicity, we calculate based on seat
                            let target = (hule.seat + num_players as u32 - 1) % num_players as u32;
                            mjai_events.push(json!({
                                "type": "hora",
                                "actor": hule.seat,
                                "target": target,
                                "pai": hule.hu_tile,
                            }));
                        }
                    }

                    mjai_events.push(json!({
                        "type": "end_kyoku",
                    }));
                }

                GameEvent::NoTile(_nt) => {
                    // If there was a pending reach, emit reach_accepted
                    if let Some(actor) = pending_reach.take() {
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": actor,
                        }));
                    }

                    mjai_events.push(json!({
                        "type": "ryukyoku",
                    }));

                    mjai_events.push(json!({
                        "type": "end_kyoku",
                    }));
                }

                GameEvent::LiuJu(lj) => {
                    // If there was a pending reach, emit reach_accepted
                    if let Some(actor) = pending_reach.take() {
                        mjai_events.push(json!({
                            "type": "reach_accepted",
                            "actor": actor,
                        }));
                    }

                    // Abortive draw with reason
                    let reason = match lj.liuju_type {
                        1 => "yao9", // 9 terminals
                        2 => "reach4", // 4 riichi
                        3 => "kan4", // 4 kan
                        4 => "kaze4", // 4 same wind
                        5 => "ron3", // Triple ron
                        _ => "unknown",
                    };

                    mjai_events.push(json!({
                        "type": "ryukyoku",
                        "reason": reason,
                    }));

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

        // End game event
        mjai_events.push(json!({
            "type": "end_game",
        }));

        Ok(mjai_events)
    }
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
}
