//! Majsoul Record* event decoders.

use anyhow::Result;

use super::proto::{extract_string, extract_varint, FieldIterator};
use super::tiles::tile_str_to_mjai;

/// Decoded RecordNewRound event
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

/// Decoded RecordDealTile event
#[derive(Debug, Clone)]
pub struct DealTile {
    pub seat: u32,
    pub tile: String,
    pub moqie: bool, // True if tsumogiri
}

/// Decoded RecordDiscardTile event
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

/// Decoded RecordChiPengGang event
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

/// Decoded RecordAnGangAddGang event
#[derive(Debug, Clone)]
pub struct AnGangAddGang {
    pub seat: u32,
    pub gang_type: AnGangAddGangType,
    pub tiles: String, // Single tile for kakan, representative for ankan
}

/// A single winning hand in RecordHule
#[derive(Debug, Clone)]
pub struct HuleInfo {
    pub seat: u32,
    pub zimo: bool,
    pub hand: Vec<String>,
    pub hu_tile: String,
    pub fu: u32,
    pub point_rong: i32, // Points from ron
    pub point_zimo_qin: i32, // Points from dealer tsumo
    pub point_zimo_xian: i32, // Points from non-dealer tsumo
}

/// Decoded RecordHule event
#[derive(Debug, Clone)]
pub struct Hule {
    pub hules: Vec<HuleInfo>,
    pub delta_scores: Vec<i32>,
    pub scores: Vec<i32>, // Final scores after this round
}

/// Decoded RecordNoTile event (exhaustive draw)
#[derive(Debug, Clone)]
pub struct NoTile {
    pub scores: Vec<i32>,
    pub delta_scores: Vec<i32>,
}

/// Decoded RecordLiuJu event (abortive draw)
#[derive(Debug, Clone)]
pub struct LiuJu {
    pub liuju_type: u32, // 1=9 terminals, 2=4 riichi, 3=4 kan, 4=4 wind, etc.
}

/// Decoded RecordBaBei (north tile declaration in 3-player)
#[derive(Debug, Clone)]
pub struct BaBei {
    pub seat: u32,
    pub moqie: bool,
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

/// Parse RecordNewRound from protobuf data
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
            (1, 0) => chang = extract_varint(field.data)? as u32,
            (2, 0) => ju = extract_varint(field.data)? as u32,
            (3, 0) => ben = extract_varint(field.data)? as u32,
            // Field 4: tiles0 (repeated string)
            (4, 2) => tiles[0].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 5: tiles1
            (5, 2) => tiles[1].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 6: tiles2
            (6, 2) => tiles[2].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 7: tiles3
            (7, 2) => tiles[3].push(tile_str_to_mjai(&extract_string(field.data))?),
            // Field 8: dora
            (8, 2) => dora_marker = tile_str_to_mjai(&extract_string(field.data))?,
            // Field 10: scores (repeated int32)
            (10, 0) => scores.push(extract_varint(field.data)? as i32),
            // Field 11: liqibang
            (11, 0) => liqibang = extract_varint(field.data)? as u32,
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

/// Parse RecordDealTile from protobuf data
pub fn parse_deal_tile(data: &[u8]) -> Result<DealTile> {
    let mut seat = 0u32;
    let mut tile = String::new();
    let mut moqie = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = extract_varint(field.data)? as u32,
            (2, 2) => tile = tile_str_to_mjai(&extract_string(field.data))?,
            (4, 0) => moqie = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(DealTile { seat, tile, moqie })
}

/// Parse RecordDiscardTile from protobuf data
pub fn parse_discard_tile(data: &[u8]) -> Result<DiscardTile> {
    let mut seat = 0u32;
    let mut tile = String::new();
    let mut is_liqi = false;
    let mut moqie = false;
    let mut is_wliqi = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = extract_varint(field.data)? as u32,
            (2, 2) => tile = tile_str_to_mjai(&extract_string(field.data))?,
            (3, 0) => is_liqi = extract_varint(field.data)? != 0,
            (4, 0) => moqie = extract_varint(field.data)? != 0,
            (6, 0) => is_wliqi = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(DiscardTile {
        seat,
        tile,
        is_liqi,
        moqie,
        is_wliqi,
    })
}

/// Parse RecordChiPengGang from protobuf data
pub fn parse_chi_peng_gang(data: &[u8]) -> Result<ChiPengGang> {
    let mut seat = 0u32;
    let mut call_type = ChiPengGangType::Chi;
    let mut tiles = Vec::new();
    let mut froms = Vec::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = extract_varint(field.data)? as u32,
            (2, 0) => {
                let t = extract_varint(field.data)? as u32;
                call_type = match t {
                    0 => ChiPengGangType::Chi,
                    1 => ChiPengGangType::Pon,
                    2 => ChiPengGangType::Daiminkan,
                    n => {
                        tracing::warn!("Unknown ChiPengGang type: {}, defaulting to Chi", n);
                        ChiPengGangType::Chi
                    }
                };
            }
            (3, 2) => tiles.push(tile_str_to_mjai(&extract_string(field.data))?),
            (4, 0) => froms.push(extract_varint(field.data)? as u32),
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

/// Parse RecordAnGangAddGang from protobuf data
pub fn parse_an_gang_add_gang(data: &[u8]) -> Result<AnGangAddGang> {
    let mut seat = 0u32;
    let mut gang_type = AnGangAddGangType::Ankan;
    let mut tiles = String::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = extract_varint(field.data)? as u32,
            (2, 0) => {
                let t = extract_varint(field.data)? as u32;
                gang_type = match t {
                    3 => AnGangAddGangType::Kakan,
                    2 => AnGangAddGangType::Ankan,
                    n => {
                        tracing::warn!("Unknown AnGangAddGang type: {}, defaulting to Ankan", n);
                        AnGangAddGangType::Ankan
                    }
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

/// Parse a single HuleInfo from protobuf data
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
            (2, 0) => seat = extract_varint(field.data)? as u32,
            (3, 0) => zimo = extract_varint(field.data)? != 0,
            (5, 2) => hand.push(tile_str_to_mjai(&extract_string(field.data))?),
            (6, 2) => hu_tile = tile_str_to_mjai(&extract_string(field.data))?,
            (8, 0) => fu = extract_varint(field.data)? as u32,
            (10, 0) => point_rong = extract_varint(field.data)? as i32,
            (11, 0) => point_zimo_qin = extract_varint(field.data)? as i32,
            (12, 0) => point_zimo_xian = extract_varint(field.data)? as i32,
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

/// Parse RecordHule from protobuf data
pub fn parse_hule(data: &[u8]) -> Result<Hule> {
    let mut hules = Vec::new();
    let mut delta_scores = Vec::new();
    let mut scores = Vec::new();

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 2) => hules.push(parse_hule_info(field.data)?),
            (3, 0) => delta_scores.push(extract_varint(field.data)? as i32),
            (4, 0) => scores.push(extract_varint(field.data)? as i32),
            _ => {}
        }
    }

    Ok(Hule {
        hules,
        delta_scores,
        scores,
    })
}

/// Parse RecordNoTile from protobuf data
pub fn parse_no_tile(data: &[u8]) -> Result<NoTile> {
    let mut scores = Vec::new();
    let mut delta_scores = Vec::new();

    // NoTile has a nested ScoreInfo message, we need to handle it
    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            // Field 3: scores (repeated ScoreInfo or simple int)
            (3, 0) => scores.push(extract_varint(field.data)? as i32),
            // Field 4: delta_scores
            (4, 0) => delta_scores.push(extract_varint(field.data)? as i32),
            _ => {}
        }
    }

    Ok(NoTile {
        scores,
        delta_scores,
    })
}

/// Parse RecordLiuJu from protobuf data
pub fn parse_liu_ju(data: &[u8]) -> Result<LiuJu> {
    let mut liuju_type = 0u32;

    for field in FieldIterator::new(data) {
        let field = field?;
        if field.number == 1 && field.wire_type == 0 {
            liuju_type = extract_varint(field.data)? as u32;
        }
    }

    Ok(LiuJu { liuju_type })
}

/// Parse RecordBaBei from protobuf data
pub fn parse_babei(data: &[u8]) -> Result<BaBei> {
    let mut seat = 0u32;
    let mut moqie = false;

    for field in FieldIterator::new(data) {
        let field = field?;
        match (field.number, field.wire_type) {
            (1, 0) => seat = extract_varint(field.data)? as u32,
            (2, 0) => moqie = extract_varint(field.data)? != 0,
            _ => {}
        }
    }

    Ok(BaBei { seat, moqie })
}

/// Parse a RecordAction into a GameEvent
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
        _ => None, // Skip unknown record types
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
}
