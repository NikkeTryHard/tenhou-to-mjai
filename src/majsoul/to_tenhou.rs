//! Convert Majsoul protobuf game records to Tenhou JSON format.
//!
//! This module provides the conversion logic to transform Majsoul's protobuf-encoded
//! game records into the tenhou.net/6 JSON format, which is compatible with existing
//! Tenhou log viewers and analysis tools like Mortal.

use super::tenhou_format::{PlayerMapping, TenhouLog, TenhouRule, TensoulOutput};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

// =============================================================================
// Room/Mode Mappings
// =============================================================================

/// Room name mappings from mode_id to Japanese display name
/// These match the tensoul cfg.json format
pub fn get_room_name(mode_id: u32) -> String {
    match mode_id {
        // 4-player modes
        2 => "銅の間東喰".to_string(),
        3 => "銅の間南喰".to_string(),
        5 => "銀の間東喰".to_string(),
        6 => "銀の間南喰".to_string(),
        8 => "金の間東喰".to_string(),
        9 => "金の間南喰".to_string(),
        11 => "玉の間東喰".to_string(),
        12 => "玉の間南喰".to_string(),
        15 => "王座の間東喰".to_string(),
        16 => "王座の間南喰".to_string(),
        // 3-player modes
        21 => "銅の間東喰(三人)".to_string(),
        22 => "銅の間南喰(三人)".to_string(),
        23 => "銀の間東喰(三人)".to_string(),
        24 => "銀の間南喰(三人)".to_string(),
        25 => "金の間東喰(三人)".to_string(),
        26 => "金の間南喰(三人)".to_string(),
        // Default
        _ => format!("Mode {}", mode_id),
    }
}

/// Check if mode is 3-player
pub fn is_sanma(mode_id: u32) -> bool {
    mode_id >= 21 && mode_id <= 26
}

/// Check if mode is hanchan (south round)
pub fn is_hanchan(mode_id: u32) -> bool {
    matches!(mode_id, 3 | 6 | 9 | 12 | 16 | 22 | 24 | 26)
}

// =============================================================================
// Dan/Rank Mappings
// =============================================================================

/// Dan/rank name mappings from level_id to Japanese display name
pub fn get_dan_name(level_id: u32) -> String {
    // Level ID format: 1XXYY where XX is major rank, YY is stars
    // 101xx = 初心 (Novice)
    // 102xx = 雀士 (Adept)
    // 103xx = 雀傑 (Expert)
    // 104xx = 雀豪 (Master)
    // 105xx = 雀聖 (Saint)
    // 106xx = 魂天 (Celestial)
    match level_id {
        // 4-player ranks
        10101 => "初心★1".to_string(),
        10102 => "初心★2".to_string(),
        10103 => "初心★3".to_string(),
        10201 => "雀士★1".to_string(),
        10202 => "雀士★2".to_string(),
        10203 => "雀士★3".to_string(),
        10301 => "雀傑★1".to_string(),
        10302 => "雀傑★2".to_string(),
        10303 => "雀傑★3".to_string(),
        10401 => "雀豪★1".to_string(),
        10402 => "雀豪★2".to_string(),
        10403 => "雀豪★3".to_string(),
        10501 => "雀聖★1".to_string(),
        10502 => "雀聖★2".to_string(),
        10503 => "雀聖★3".to_string(),
        10601 => "魂天".to_string(),
        // Celestial with levels (魂天Lv1, Lv2, etc.)
        l if l >= 10602 && l <= 10620 => format!("魂天Lv{}", l - 10600),
        // 3-player ranks (20xxx series)
        20101 => "初心★1".to_string(),
        20102 => "初心★2".to_string(),
        20103 => "初心★3".to_string(),
        20201 => "雀士★1".to_string(),
        20202 => "雀士★2".to_string(),
        20203 => "雀士★3".to_string(),
        20301 => "雀傑★1".to_string(),
        20302 => "雀傑★2".to_string(),
        20303 => "雀傑★3".to_string(),
        20401 => "雀豪★1".to_string(),
        20402 => "雀豪★2".to_string(),
        20403 => "雀豪★3".to_string(),
        20501 => "雀聖★1".to_string(),
        20502 => "雀聖★2".to_string(),
        20503 => "雀聖★3".to_string(),
        20601 => "魂天".to_string(),
        l if l >= 20602 && l <= 20620 => format!("魂天Lv{}", l - 20600),
        // Unknown
        _ => format!("Rank {}", level_id),
    }
}

// =============================================================================
// Tile ID Conversion
// =============================================================================

/// Convert a Majsoul tile ID to Tenhou format.
///
/// Majsoul tile encoding:
/// - 0-8: 1m-9m (manzu/characters)
/// - 9-17: 1p-9p (pinzu/circles)
/// - 18-26: 1s-9s (souzu/bamboo)
/// - 27-33: E/S/W/N/P/F/C (honors)
///
/// Tenhou tile encoding:
/// - 11-19: 1m-9m
/// - 21-29: 1p-9p
/// - 31-39: 1s-9s
/// - 41-47: E/S/W/N/P/F/C
/// - 51: red 5m
/// - 52: red 5p
/// - 53: red 5s
///
/// The `tile_instance` is used to detect red fives:
/// - For 5m/5p/5s, if tile_instance == 0, it's the red five
pub fn majsoul_tile_to_tenhou(tile_id: u32, tile_instance: u32) -> u32 {
    let suit = tile_id / 9;
    let num = tile_id % 9;

    match suit {
        0 => {
            // Manzu (1m-9m)
            if num == 4 && tile_instance == 0 {
                51 // Red 5m
            } else {
                11 + num
            }
        }
        1 => {
            // Pinzu (1p-9p)
            if num == 4 && tile_instance == 0 {
                52 // Red 5p
            } else {
                21 + num
            }
        }
        2 => {
            // Souzu (1s-9s)
            if num == 4 && tile_instance == 0 {
                53 // Red 5s
            } else {
                31 + num
            }
        }
        3 => {
            // Honors: E=0, S=1, W=2, N=3, P=4, F=5, C=6
            41 + num
        }
        _ => 0, // Invalid
    }
}

/// Convert a Tenhou tile ID to the corresponding tile number.
/// This is the inverse direction, useful for validation.
pub fn tenhou_tile_to_string(tile: u32) -> String {
    match tile {
        11..=19 => format!("{}m", tile - 10),
        21..=29 => format!("{}p", tile - 20),
        31..=39 => format!("{}s", tile - 30),
        41 => "E".to_string(),
        42 => "S".to_string(),
        43 => "W".to_string(),
        44 => "N".to_string(),
        45 => "P".to_string(), // Haku (White)
        46 => "F".to_string(), // Hatsu (Green)
        47 => "C".to_string(), // Chun (Red)
        51 => "0m".to_string(), // Red 5m
        52 => "0p".to_string(), // Red 5p
        53 => "0s".to_string(), // Red 5s
        60 => "?".to_string(),  // Tsumogiri marker
        _ => format!("?{}", tile),
    }
}

// =============================================================================
// Action Encoding
// =============================================================================

/// Encode a riichi declaration in Tenhou format.
/// Format: "r{tile}" where tile is the discarded tile
pub fn encode_riichi(tile: u32) -> String {
    format!("r{}", tile)
}

/// Encode a chi (chii) call in Tenhou format.
/// Format: "c{tile1}{tile2}{tile3}" where the called tile comes first
pub fn encode_chi(called_tile: u32, hand_tiles: &[u32]) -> String {
    let mut tiles = vec![called_tile];
    tiles.extend_from_slice(hand_tiles);
    format!("c{}{}{}", tiles[0], tiles[1], tiles[2])
}

/// Encode a pon call in Tenhou format.
/// Format: "p{tile}{tile}{tile}" or "{tile}p{tile}{tile}" depending on caller
pub fn encode_pon(called_tile: u32, hand_tiles: &[u32], from_player: u32) -> String {
    // The position of 'p' indicates which player the tile was called from
    // For simplicity, we use the standard format
    format!("p{}{}{}", hand_tiles[0], hand_tiles[1], called_tile)
}

/// Encode a kan (minkan/daiminkan) call in Tenhou format.
/// Format: "m{tile}{tile}{tile}{tile}"
pub fn encode_daiminkan(tiles: &[u32]) -> String {
    format!("m{}{}{}{}", tiles[0], tiles[1], tiles[2], tiles[3])
}

/// Encode an ankan (concealed kan) in Tenhou format.
/// Format: "{tile}{tile}k{tile}{tile}" - the k is in the middle
pub fn encode_ankan(tile: u32) -> String {
    format!("{}{}k{}{}", tile, tile, tile, tile)
}

/// Encode a kakan (added kan) in Tenhou format.
/// Format: varies based on the original pon
pub fn encode_kakan(tile: u32, pon_tiles: &[u32]) -> String {
    // The added tile goes after the 'k'
    format!("{}{}{}k{}", pon_tiles[0], pon_tiles[1], pon_tiles[2], tile)
}

// =============================================================================
// Result Encoding
// =============================================================================

/// Result type for a kyoku (round)
#[derive(Debug, Clone)]
pub enum KyokuResult {
    /// Agari (win) - can be multiple winners in case of double/triple ron
    Agari(Vec<AgariInfo>),
    /// Ryuukyoku (draw) with score changes
    Ryuukyoku {
        reason: String,
        score_changes: Vec<i32>,
    },
}

/// Information about an agari (win)
#[derive(Debug, Clone)]
pub struct AgariInfo {
    /// Winner seat (0-3)
    pub winner: u32,
    /// Loser seat (0-3), same as winner for tsumo
    pub from: u32,
    /// Score changes for each player
    pub score_changes: Vec<i32>,
    /// Han count
    pub han: u32,
    /// Fu count
    pub fu: u32,
    /// Point value string (e.g., "満貫8000点")
    pub point_string: String,
    /// Yaku list
    pub yaku: Vec<String>,
}

/// Encode an agari result in Tenhou format
pub fn encode_agari(agari: &AgariInfo) -> Vec<Value> {
    let mut result = vec![
        Value::String("和了".to_string()),
        Value::Array(agari.score_changes.iter().map(|&s| Value::from(s)).collect()),
    ];

    // Agari details: [winner, from, winner, point_string, yaku1, yaku2, ...]
    let mut details: Vec<Value> = vec![
        Value::from(agari.winner),
        Value::from(agari.from),
        Value::from(agari.winner),
        Value::String(agari.point_string.clone()),
    ];
    for yaku in &agari.yaku {
        details.push(Value::String(yaku.clone()));
    }
    result.push(Value::Array(details));

    result
}

/// Encode a ryuukyoku result in Tenhou format
pub fn encode_ryuukyoku(reason: &str, score_changes: &[i32]) -> Vec<Value> {
    vec![
        Value::String(reason.to_string()),
        Value::Array(score_changes.iter().map(|&s| Value::from(s)).collect()),
    ]
}

// =============================================================================
// Converter Skeleton
// =============================================================================

/// Convert a Majsoul game record to Tenhou JSON format.
///
/// This is a skeleton implementation that will be filled in as we implement
/// the protobuf parsing in later batches.
pub fn convert_to_tenhou(
    _record_data: &[u8],
    uuid: &str,
    mode_id: u32,
) -> Result<TensoulOutput> {
    tracing::warn!(
        "Protobuf parsing not yet implemented - returning skeleton data for {}",
        uuid
    );
    let num_players = if is_sanma(mode_id) { 3 } else { 4 };

    let mut log = TenhouLog::new(uuid.to_string(), num_players);

    // Set rule based on mode
    log.rule = if num_players == 3 {
        TenhouRule::default_3p()
    } else {
        TenhouRule::default_4p()
    };
    log.rule.disp = get_room_name(mode_id);

    // Title contains room name and timestamp
    log.title = vec![
        Value::String(get_room_name(mode_id)),
        Value::from(0), // Timestamp will be filled from record
    ];

    // TODO: Parse protobuf record data and fill in:
    // - Player names, dan, rate from header
    // - Kyoku data from game events
    // - Final scores

    Ok(TensoulOutput::success(log))
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Room Name Tests
    // =========================================================================

    #[test]
    fn test_get_room_name() {
        assert_eq!(get_room_name(16), "王座の間南喰");
        assert_eq!(get_room_name(15), "王座の間東喰");
        assert_eq!(get_room_name(12), "玉の間南喰");
        assert_eq!(get_room_name(9), "金の間南喰");
        assert_eq!(get_room_name(999), "Mode 999");
    }

    #[test]
    fn test_is_sanma() {
        assert!(!is_sanma(16)); // 4-player throne south
        assert!(is_sanma(26));  // 3-player gold south
        assert!(is_sanma(21));  // 3-player bronze east
    }

    #[test]
    fn test_is_hanchan() {
        assert!(is_hanchan(16));  // Throne south = hanchan
        assert!(!is_hanchan(15)); // Throne east = tonpuu
        assert!(is_hanchan(26));  // 3p gold south = hanchan
    }

    // =========================================================================
    // Dan Name Tests
    // =========================================================================

    #[test]
    fn test_get_dan_name() {
        assert_eq!(get_dan_name(10101), "初心★1");
        assert_eq!(get_dan_name(10501), "雀聖★1");
        assert_eq!(get_dan_name(10503), "雀聖★3");
        assert_eq!(get_dan_name(10601), "魂天");
        assert_eq!(get_dan_name(10603), "魂天Lv3");
        assert_eq!(get_dan_name(10610), "魂天Lv10");
    }

    #[test]
    fn test_get_dan_name_3p() {
        assert_eq!(get_dan_name(20501), "雀聖★1");
        assert_eq!(get_dan_name(20601), "魂天");
        assert_eq!(get_dan_name(20605), "魂天Lv5");
    }

    // =========================================================================
    // Tile Conversion Tests
    // =========================================================================

    #[test]
    fn test_majsoul_tile_to_tenhou_manzu() {
        // Regular manzu tiles
        assert_eq!(majsoul_tile_to_tenhou(0, 1), 11); // 1m
        assert_eq!(majsoul_tile_to_tenhou(4, 1), 15); // 5m (not red)
        assert_eq!(majsoul_tile_to_tenhou(8, 1), 19); // 9m

        // Red 5m
        assert_eq!(majsoul_tile_to_tenhou(4, 0), 51); // Red 5m
    }

    #[test]
    fn test_majsoul_tile_to_tenhou_pinzu() {
        // Regular pinzu tiles
        assert_eq!(majsoul_tile_to_tenhou(9, 1), 21);  // 1p
        assert_eq!(majsoul_tile_to_tenhou(13, 1), 25); // 5p (not red)
        assert_eq!(majsoul_tile_to_tenhou(17, 1), 29); // 9p

        // Red 5p
        assert_eq!(majsoul_tile_to_tenhou(13, 0), 52); // Red 5p
    }

    #[test]
    fn test_majsoul_tile_to_tenhou_souzu() {
        // Regular souzu tiles
        assert_eq!(majsoul_tile_to_tenhou(18, 1), 31); // 1s
        assert_eq!(majsoul_tile_to_tenhou(22, 1), 35); // 5s (not red)
        assert_eq!(majsoul_tile_to_tenhou(26, 1), 39); // 9s

        // Red 5s
        assert_eq!(majsoul_tile_to_tenhou(22, 0), 53); // Red 5s
    }

    #[test]
    fn test_majsoul_tile_to_tenhou_honors() {
        assert_eq!(majsoul_tile_to_tenhou(27, 0), 41); // East
        assert_eq!(majsoul_tile_to_tenhou(28, 0), 42); // South
        assert_eq!(majsoul_tile_to_tenhou(29, 0), 43); // West
        assert_eq!(majsoul_tile_to_tenhou(30, 0), 44); // North
        assert_eq!(majsoul_tile_to_tenhou(31, 0), 45); // Haku (White)
        assert_eq!(majsoul_tile_to_tenhou(32, 0), 46); // Hatsu (Green)
        assert_eq!(majsoul_tile_to_tenhou(33, 0), 47); // Chun (Red)
    }

    #[test]
    fn test_tenhou_tile_to_string() {
        assert_eq!(tenhou_tile_to_string(11), "1m");
        assert_eq!(tenhou_tile_to_string(15), "5m");
        assert_eq!(tenhou_tile_to_string(19), "9m");
        assert_eq!(tenhou_tile_to_string(21), "1p");
        assert_eq!(tenhou_tile_to_string(31), "1s");
        assert_eq!(tenhou_tile_to_string(41), "E");
        assert_eq!(tenhou_tile_to_string(45), "P"); // Haku
        assert_eq!(tenhou_tile_to_string(46), "F"); // Hatsu
        assert_eq!(tenhou_tile_to_string(47), "C"); // Chun
        assert_eq!(tenhou_tile_to_string(51), "0m"); // Red 5m
        assert_eq!(tenhou_tile_to_string(52), "0p"); // Red 5p
        assert_eq!(tenhou_tile_to_string(53), "0s"); // Red 5s
        assert_eq!(tenhou_tile_to_string(60), "?");  // Tsumogiri
    }

    // =========================================================================
    // Action Encoding Tests
    // =========================================================================

    #[test]
    fn test_encode_riichi() {
        assert_eq!(encode_riichi(14), "r14");
        assert_eq!(encode_riichi(47), "r47");
    }

    #[test]
    fn test_encode_chi() {
        // Chi 2-3-4 manzu, calling 2m
        assert_eq!(encode_chi(12, &[13, 14]), "c121314");
    }

    #[test]
    fn test_encode_ankan() {
        // Ankan of North
        assert_eq!(encode_ankan(44), "4444k4444");
    }

    // =========================================================================
    // Result Encoding Tests
    // =========================================================================

    #[test]
    fn test_encode_agari() {
        let agari = AgariInfo {
            winner: 1,
            from: 3,
            score_changes: vec![0, 17000, 0, -16000],
            han: 9,
            fu: 30,
            point_string: "倍満16000点".to_string(),
            yaku: vec!["清一色(5飜)".to_string(), "ドラ(4飜)".to_string()],
        };

        let result = encode_agari(&agari);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], Value::String("和了".to_string()));

        // Check score changes
        if let Value::Array(scores) = &result[1] {
            assert_eq!(scores.len(), 4);
            assert_eq!(scores[1], Value::from(17000));
            assert_eq!(scores[3], Value::from(-16000));
        } else {
            panic!("Expected array for score changes");
        }

        // Check details
        if let Value::Array(details) = &result[2] {
            assert_eq!(details[0], Value::from(1)); // winner
            assert_eq!(details[1], Value::from(3)); // from
            assert_eq!(details[3], Value::String("倍満16000点".to_string()));
            assert_eq!(details[4], Value::String("清一色(5飜)".to_string()));
        } else {
            panic!("Expected array for details");
        }
    }

    #[test]
    fn test_encode_ryuukyoku() {
        let result = encode_ryuukyoku("流局", &[-1000, -1000, -1000, 3000]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Value::String("流局".to_string()));

        if let Value::Array(scores) = &result[1] {
            assert_eq!(scores[3], Value::from(3000));
        }
    }

    // =========================================================================
    // Converter Tests
    // =========================================================================

    #[test]
    fn test_convert_to_tenhou_skeleton() {
        let result = convert_to_tenhou(&[], "231124-test-uuid", 16).unwrap();
        assert!(!result.is_error);

        let log = result.log.unwrap();
        assert_eq!(log.ver, "2.3");
        assert_eq!(log.ref_, "231124-test-uuid");
        assert_eq!(log.ratingc, "PF4");
        assert_eq!(log.rule.disp, "王座の間南喰");
        assert_eq!(log.dan.len(), 4);
    }

    #[test]
    fn test_convert_to_tenhou_3p() {
        let result = convert_to_tenhou(&[], "test-uuid", 26).unwrap();
        let log = result.log.unwrap();

        assert_eq!(log.ratingc, "PF3");
        assert_eq!(log.dan.len(), 3);
        assert_eq!(log.rule.aka51, 0); // No red 5m in 3p
        assert_eq!(log.rule.aka52, 2); // Two red 5p in 3p
    }
}
