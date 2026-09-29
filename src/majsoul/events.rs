//! Majsoul Record* event decoders.

use anyhow::Result;

use super::proto::{decode_packed_varints, extract_string, extract_varint, i32_from_varint, u32_checked, FieldIterator};
use super::tiles::tile_str_to_mjai;

/// Decoded `RecordNewRound` event
#[derive(Debug, Clone)]
pub struct NewRound {
    pub chang: u32,        // Round wind (0=E, 1=S, 2=W)
    pub ju: u32,           // Dealer position (0-3)
    pub ben: u32,          // Honba count
    pub liqibang: u32,     // Riichi sticks on table
    pub dora_marker: String, // Dora indicator tile
    pub scores: Vec<i32>,  // Starting scores
    pub tiles: Vec<Vec<String>>, // Starting hands (tiles0-tiles3)
}

/// Decoded `RecordDealTile` event
#[derive(Debug, Clone)]
pub struct DealTile {
    pub seat: u32,
    pub tile: String,
    pub _moqie: bool, // True if tsumogiri (parsed, not emitted: tsumo carries none)
}

/// Decoded `RecordDiscardTile` event
#[derive(Debug, Clone)]
pub struct DiscardTile {
    pub seat: u32,
    pub tile: String,
    pub is_liqi: bool,   // Riichi declaration
    pub moqie: bool,     // Tsumogiri
    pub is_wliqi: bool,  // Double riichi
}

/// Chi/Pon/Daiminkan type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChiPengGangType {
    Chi = 0,
    Pon = 1,
    Daiminkan = 2,
}

/// Decoded `RecordChiPengGang` event
#[derive(Debug, Clone)]
pub struct ChiPengGang {
    pub seat: u32,
    pub call_type: ChiPengGangType,
    pub tiles: Vec<String>,
    pub froms: Vec<u32>,
}

/// Ankan/Kakan type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnGangAddGangType {
    Ankan = 2,
    Kakan = 3,
}

/// Decoded `RecordAnGangAddGang` event
#[derive(Debug, Clone)]
pub struct AnGangAddGang {
    pub seat: u32,
    pub gang_type: AnGangAddGangType,
    pub tiles: String, // Single tile for kakan, representative for ankan
}

/// A single winning hand in `RecordHule`
#[derive(Debug, Clone)]
pub struct HuleInfo {
    pub seat: u32,
    pub zimo: bool,
    // Decoded for completeness but never emitted (MJAI hora carries no hand
    // tiles), so the dead-code lint is suppressed here.
    #[allow(dead_code)]
    pub hand: Vec<String>,
    pub hu_tile: String,
    pub fu: u32,
    pub point_rong: i32, // Points from ron
    pub point_zimo_qin: i32, // Points from dealer tsumo
    pub point_zimo_xian: i32, // Points from non-dealer tsumo
}

/// Decoded `RecordHule` event
#[derive(Debug, Clone)]
pub struct Hule {
    pub hules: Vec<HuleInfo>,
    pub delta_scores: Vec<i32>,
    pub scores: Vec<i32>, // Final scores after this round
}

/// Decoded `RecordNoTile` event (exhaustive draw)
#[derive(Debug, Clone)]
pub struct NoTile {
    pub scores: Vec<i32>,
    pub delta_scores: Vec<i32>,
}

/// Decoded `RecordLiuJu` event (abortive draw)
#[derive(Debug, Clone)]
pub struct LiuJu {
    pub liuju_type: u32, // 1=9 terminals, 2=4 riichi, 3=4 kan, 4=4 wind, etc.
}

/// Decoded `RecordBaBei` (north tile declaration in 3-player)
#[derive(Debug, Clone)]
pub struct BaBei {
    pub seat: u32,
    pub _moqie: bool, // Parsed, not emitted (nukidora carries no tsumogiri).
}

/// All possible game events
#[derive(Debug, Clone)]
pub enum GameEvent {
    NewRound(NewRound),
    DealTile(DealTile),
    DiscardTile(DiscardTile),
    ChiPengGang(ChiPengGang),
    AnGangAddGang(AnGangAddGang),
    Hule(Hule),
    NoTile(NoTile),
    LiuJu(LiuJu),
    BaBei(BaBei),
}

/// Parse `RecordNewRound` from protobuf data
pub fn parse_new_round(data: &[u8]) -> Result<NewRound> {
    let mut chang = 0u32;
    let mut ju = 0u32;
    let mut ben = 0u32;
    let mut liqibang = 0u32;
    let mut dora_marker = String::new();
    let mut scores = Vec::new();
    let mut tiles: Vec<Vec<String>> = vec![Vec::new(); 4];

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => chang = u32_checked(extract_varint(field.data)?, "chang")?,
            (2, 0) => ju = u32_checked(extract_varint(field.data)?, "ju")?,
            (3, 0) => ben = u32_checked(extract_varint(field.data)?, "ben")?,
            // Field 5: scores (packed repeated int32)
            (5, 2) => scores.extend(decode_packed_varints(field.data)?),
            // Field 5: scores (unpacked individual varint)
            (5, 0) => scores.push(i32_from_varint(extract_varint(field.data)?)),
            // Field 6: liqibang
            (6, 0) => liqibang = u32_checked(extract_varint(field.data)?, "liqibang")?,
            // Field 7: tiles0 (repeated string)
            (7, 2) => tiles[0].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 8: tiles1
            (8, 2) => tiles[1].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 9: tiles2
            (9, 2) => tiles[2].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 10: tiles3
            (10, 2) => tiles[3].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 16: doras (repeated string) - use first as dora marker
            (16, 2)
                if dora_marker.is_empty() => {
                    dora_marker = tile_str_to_mjai(&extract_string(field.data))?;
                }
            _ => {}
        }
    }

    Ok(NewRound {
        chang,
        ju,
        ben,
        liqibang,
        dora_marker,
        scores,
        tiles,
    })
}

/// Parse `RecordDealTile` from protobuf data
pub fn parse_deal_tile(data: &[u8]) -> Result<DealTile> {
    let mut seat = 0u32;
    let mut tile = String::new();
    let mut moqie = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (2, 2) => tile = tile_str_to_mjai(&extract_string(field.data))?,
            (4, 0) => moqie = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(DealTile { seat, tile, _moqie: moqie })
}

/// Parse `RecordDiscardTile` from protobuf data
pub fn parse_discard_tile(data: &[u8]) -> Result<DiscardTile> {
    let mut seat = 0u32;
    let mut tile = String::new();
    let mut is_liqi = false;
    let mut moqie = false;
    let mut is_double_riichi = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (2, 2) => tile = tile_str_to_mjai(&extract_string(field.data))?,
            (3, 0) => is_liqi = extract_varint(field.data)? != 0,
            (5, 0) => moqie = extract_varint(field.data)? != 0,
            (9, 0) => is_double_riichi = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(DiscardTile {
        seat,
        tile,
        is_liqi,
        moqie,
        is_wliqi: is_double_riichi,
    })
}

/// Parse `RecordChiPengGang` from protobuf data
pub fn parse_chi_peng_gang(data: &[u8]) -> Result<ChiPengGang> {
    let mut seat = 0u32;
    let mut call_type = ChiPengGangType::Chi;
    let mut tiles = Vec::new();
    let mut froms = Vec::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (2, 0) => {
                let t = u32_checked(extract_varint(field.data)?, "chi_peng_gang type")?;
                call_type = match t {
                    0 => ChiPengGangType::Chi,
                    1 => ChiPengGangType::Pon,
                    2 => ChiPengGangType::Daiminkan,
                    n => anyhow::bail!("unknown ChiPengGang type: {n}"),
                };
            }
            (3, 2) => tiles.push(tile_str_to_mjai(&extract_string(field.data))?),
            // froms arrive packed or unpacked by emitter version; negatives bail (seat ids, unlike scores, are never negative).
            (4, 2) => {
                for v in decode_packed_varints(field.data)? {
                    froms.push(u32::try_from(v).map_err(|_| anyhow::anyhow!("from out of range: {v}"))?);
                }
            }
            (4, 0) => froms.push(u32_checked(extract_varint(field.data)?, "from")?),
            _ => {}
        }
    }

    Ok(ChiPengGang {
        seat,
        call_type,
        tiles,
        froms,
    })
}

/// Parse `RecordAnGangAddGang` from protobuf data
pub fn parse_an_gang_add_gang(data: &[u8]) -> Result<AnGangAddGang> {
    let mut seat = 0u32;
    let mut gang_type = AnGangAddGangType::Ankan;
    let mut tiles = String::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (2, 0) => {
                let t = u32_checked(extract_varint(field.data)?, "an_gang_add_gang type")?;
                gang_type = match t {
                    3 => AnGangAddGangType::Kakan,
                    2 => AnGangAddGangType::Ankan,
                    n => anyhow::bail!("unknown AnGangAddGang type: {n}"),
                };
            }
            (3, 2) => tiles = tile_str_to_mjai(&extract_string(field.data))?,
            _ => {}
        }
    }

    Ok(AnGangAddGang {
        seat,
        gang_type,
        tiles,
    })
}

/// Parse a single `HuleInfo` from protobuf data
fn parse_hule_info(data: &[u8]) -> Result<HuleInfo> {
    let mut seat = 0u32;
    let mut zimo = false;
    let mut hand = Vec::new();
    let mut hu_tile = String::new();
    let mut fu = 0u32;
    let mut point_rong = 0i32;
    let mut point_zimo_qin = 0i32;
    let mut point_zimo_xian = 0i32;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 2) => hand.push(tile_str_to_mjai(&extract_string(field.data))?),
            (3, 2) => hu_tile = tile_str_to_mjai(&extract_string(field.data))?,
            (4, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (5, 0) => zimo = extract_varint(field.data)? != 0,
            (13, 0) => fu = u32_checked(extract_varint(field.data)?, "fu")?,
            (15, 0) => point_rong = i32_from_varint(extract_varint(field.data)?),
            (16, 0) => point_zimo_qin = i32_from_varint(extract_varint(field.data)?),
            (17, 0) => point_zimo_xian = i32_from_varint(extract_varint(field.data)?),
            _ => {}
        }
    }

    Ok(HuleInfo {
        seat,
        zimo,
        hand,
        hu_tile,
        fu,
        point_rong,
        point_zimo_qin,
        point_zimo_xian,
    })
}

/// Parse `RecordHule` from protobuf data
pub fn parse_hule(data: &[u8]) -> Result<Hule> {
    let mut hules = Vec::new();
    let mut delta_scores = Vec::new();
    let mut scores = Vec::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 2) => hules.push(parse_hule_info(field.data)?),
            (3, 2) => delta_scores.extend(decode_packed_varints(field.data)?),
            (3, 0) => delta_scores.push(i32_from_varint(extract_varint(field.data)?)),
            (5, 2) => scores.extend(decode_packed_varints(field.data)?),
            (5, 0) => scores.push(i32_from_varint(extract_varint(field.data)?)),
            _ => {}
        }
    }

    Ok(Hule {
        hules,
        delta_scores,
        scores,
    })
}

/// Parse `RecordNoTile` from protobuf data
pub fn parse_no_tile(data: &[u8]) -> Result<NoTile> {
    let mut scores = Vec::new();
    let mut delta_scores = Vec::new();

    // NoTile field 3 is repeated ScoresInfo messages (not plain ints)
    // ScoresInfo: field 2=old_scores, field 3=delta_scores, field 7=score.
    // Each of 2/3/7 may appear unpacked (wire 0) or packed (wire 2);
    // take-by-position with Option (no == 0 sentinel confusion).
    for field in FieldIterator::new(data) {
        let field = field?;
        // Field 3: ScoresInfo (repeated message)
        if field.number == 3 && field.wire_type == 2 {
            let mut old_score: Option<i32> = None;
            let mut delta: Option<i32> = None;
            let mut score: Option<i32> = None;
            for inner in FieldIterator::new(field.data) {
                let inner = inner?;
                match (inner.number, inner.wire_type) {
                    // old_scores (repeated, take first)
                    (2, 0) => {
                        if old_score.is_none() {
                            old_score = Some(i32_from_varint(extract_varint(inner.data)?));
                        }
                    }
                    (2, 2) => {
                        if old_score.is_none() {
                            let v = decode_packed_varints(inner.data)?;
                            if let Some(first) = v.first() {
                                old_score = Some(*first);
                            }
                        }
                    }
                    // delta_scores (repeated, take first)
                    (3, 0) => {
                        if delta.is_none() {
                            delta = Some(i32_from_varint(extract_varint(inner.data)?));
                        }
                    }
                    (3, 2) => {
                        if delta.is_none() {
                            let v = decode_packed_varints(inner.data)?;
                            if let Some(first) = v.first() {
                                delta = Some(*first);
                            }
                        }
                    }
                    // score (final score)
                    (7, 0) => score = Some(i32_from_varint(extract_varint(inner.data)?)),
                    (7, 2) => {
                        let v = decode_packed_varints(inner.data)?;
                        if let Some(first) = v.first() {
                            score = Some(*first);
                        }
                    }
                    _ => {}
                }
            }
            // Prefer explicit final score; fall back to old+delta.
            let final_score = score.or_else(|| match (old_score, delta) {
                (Some(o), Some(d)) => Some(o + d),
                (Some(o), None) => Some(o),
                _ => None,
            });
            if let (Some(s), Some(d)) = (final_score, delta.or(Some(0))) {
                scores.push(s);
                delta_scores.push(d);
            }
        }
    }

    Ok(NoTile {
        scores,
        delta_scores,
    })
}

/// Parse `RecordLiuJu` from protobuf data
pub fn parse_liu_ju(data: &[u8]) -> Result<LiuJu> {
    let mut liuju_type = 0u32;

    for field in FieldIterator::new(data) {
        let field = field?;
        if field.number == 1 && field.wire_type == 0 {
            liuju_type = u32_checked(extract_varint(field.data)?, "liuju_type")?;
        }
    }

    Ok(LiuJu { liuju_type })
}

/// Parse `RecordBaBei` from protobuf data
pub fn parse_babei(data: &[u8]) -> Result<BaBei> {
    let mut seat = 0u32;
    let mut moqie = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = u32_checked(extract_varint(field.data)?, "seat")?,
            (8, 0) => moqie = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(BaBei { seat, _moqie: moqie })
}

/// Parse a `RecordAction` into a `GameEvent`.
/// Unknown `.lq.Record*` names warn and return `None` (forward-compatible);
/// callers count them and surface a per-game summary.
pub fn parse_record_action(name: &str, data: &[u8]) -> Result<Option<GameEvent>> {
    let event = match name {
        ".lq.RecordNewRound" => Some(GameEvent::NewRound(parse_new_round(data)?)),
        ".lq.RecordDealTile" => Some(GameEvent::DealTile(parse_deal_tile(data)?)),
        ".lq.RecordDiscardTile" => Some(GameEvent::DiscardTile(parse_discard_tile(data)?)),
        ".lq.RecordChiPengGang" => Some(GameEvent::ChiPengGang(parse_chi_peng_gang(data)?)),
        ".lq.RecordAnGangAddGang" => Some(GameEvent::AnGangAddGang(parse_an_gang_add_gang(data)?)),
        ".lq.RecordHule" => Some(GameEvent::Hule(parse_hule(data)?)),
        ".lq.RecordNoTile" => Some(GameEvent::NoTile(parse_no_tile(data)?)),
        ".lq.RecordLiuJu" => Some(GameEvent::LiuJu(parse_liu_ju(data)?)),
        ".lq.RecordBaBei" => Some(GameEvent::BaBei(parse_babei(data)?)),
        _ => {
            tracing::warn!("unknown Record action: {}", name);
            None
        }
    };
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chi_peng_gang_types() {
        assert_eq!(ChiPengGangType::Chi as u32, 0);
        assert_eq!(ChiPengGangType::Pon as u32, 1);
        assert_eq!(ChiPengGangType::Daiminkan as u32, 2);
    }

    #[test]
    fn test_no_tile_packed_deltas_nonzero() {
        // Build NoTile with one ScoresInfo where delta is packed (wire 2).
        // ScoresInfo: field 2 old_scores packed [25000], field 3 delta packed [1000],
        // field 7 score varint 26000.
        fn enc_varint(mut v: u64) -> Vec<u8> {
            let mut b = Vec::new();
            loop {
                let mut byte = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                b.push(byte);
                if v == 0 {
                    break;
                }
            }
            b
        }
        // old_scores packed: tag (2,2)=0x12, len, then varint 25000.
        let old_packed = enc_varint(25000);
        let mut old_field = enc_varint((2u64 << 3) | 2);
        old_field.extend(enc_varint(old_packed.len() as u64));
        old_field.extend(old_packed);
        // delta packed: tag (3,2)=0x1a, len, then varint 1000.
        let delta_packed = enc_varint(1000);
        let mut delta_field = enc_varint((3u64 << 3) | 2);
        delta_field.extend(enc_varint(delta_packed.len() as u64));
        delta_field.extend(delta_packed);
        // score: tag (7,0)=0x38, varint 26000.
        let mut score_field = enc_varint(7u64 << 3);
        score_field.extend(enc_varint(26000));
        let mut scores_info = Vec::new();
        scores_info.extend(old_field);
        scores_info.extend(delta_field);
        scores_info.extend(score_field);
        // NoTile field 3 len-delim ScoresInfo.
        let mut data = enc_varint((3u64 << 3) | 2);
        data.extend(enc_varint(scores_info.len() as u64));
        data.extend(scores_info);
        let nt = parse_no_tile(&data).unwrap();
        assert_eq!(nt.scores, vec![26000]);
        assert_eq!(nt.delta_scores, vec![1000]);
        assert!(nt.delta_scores.iter().any(|&x| x != 0));
    }
}
