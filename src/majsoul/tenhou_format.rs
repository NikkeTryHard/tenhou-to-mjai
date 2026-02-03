//! Tenhou.net/6 JSON log format data structures.
//!
//! This module defines the data structures that serialize to the tenhou.net/6 format,
//! which is compatible with existing Tenhou log viewers and analysis tools.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tenhou.net/6 log format - the main structure for a complete game log.
///
/// Example output:
/// ```json
/// {
///   "ver": "2.3",
///   "ref": "231124-04996d02-e68f-422a-919f-b68487bb3ba1",
///   "ratingc": "PF4",
///   "rule": { "disp": "王座の間南喰", "aka53": 1, "aka52": 1, "aka51": 1 },
///   "lobby": 0,
///   "dan": ["雀聖★1", "魂天Lv3", "雀聖★1", "雀聖★1"],
///   "rate": [1271, 1030, 3300, 220],
///   "sx": ["C", "C", "C", "C"],
///   "name": ["朔月灰", "kikou", "NaRuuuu", "デア・リヒター"],
///   "sc": [29000, -1.0, 46400, 36.4, 31500, 11.5, -6900, -46.9],
///   "title": ["王座の間南喰", 1700816360],
///   "log": [...],
///   "playerMapping": [...]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenhouLog {
    /// Format version (always "2.3")
    pub ver: String,

    /// Game reference UUID (format: YYMMDD-uuid)
    #[serde(rename = "ref")]
    pub ref_: String,

    /// Rating category (e.g., "PF4" for 4-player, "PF3" for 3-player)
    pub ratingc: String,

    /// Game rule settings
    pub rule: TenhouRule,

    /// Lobby ID (always 0 for Majsoul)
    pub lobby: i32,

    /// Dan/rank names for each player
    pub dan: Vec<String>,

    /// Rating scores for each player
    pub rate: Vec<i32>,

    /// Sex/gender indicators (always "C" for Majsoul - character)
    pub sx: Vec<String>,

    /// Player nicknames
    pub name: Vec<String>,

    /// Final scores: [points1, delta1, points2, delta2, ...]
    /// Points are integers, deltas are floats
    pub sc: Vec<Value>,

    /// Title: [room_name, end_timestamp]
    pub title: Vec<Value>,

    /// Game log data - one entry per kyoku (round)
    /// Each kyoku is an array of arrays representing:
    /// [round_info, scores, dora, ura_dora, hand0, hand1, hand2, hand3,
    ///  draws0, discards0, draws1, discards1, draws2, discards2, draws3, discards3,
    ///  result]
    pub log: Vec<Vec<Value>>,

    /// Player mapping with account IDs (optional, Majsoul-specific extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "playerMapping")]
    pub player_mapping: Option<Vec<PlayerMapping>>,
}

impl TenhouLog {
    /// Create a new empty TenhouLog with default values
    pub fn new(ref_uuid: String, num_players: usize) -> Self {
        Self {
            ver: "2.3".to_string(),
            ref_: ref_uuid,
            ratingc: format!("PF{}", num_players),
            rule: TenhouRule::default_4p(),
            lobby: 0,
            dan: vec!["".to_string(); num_players],
            rate: vec![0; num_players],
            sx: vec!["C".to_string(); num_players],
            name: vec!["".to_string(); num_players],
            sc: vec![Value::from(0); num_players * 2],
            title: vec![Value::String("".to_string()), Value::from(0)],
            log: vec![],
            player_mapping: None,
        }
    }
}

/// Rule settings for the game
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenhouRule {
    /// Display name for the room/rule set
    pub disp: String,

    /// Number of red 5s (5-sou): 1 for 4-player, 1 for 3-player
    pub aka53: i32,

    /// Number of red 5p (5-pin): 1 for 4-player, 2 for 3-player
    pub aka52: i32,

    /// Number of red 5m (5-man): 1 for 4-player, 0 for 3-player (no manzu 2-8)
    pub aka51: i32,
}

impl TenhouRule {
    /// Default rule for 4-player games (1 red 5 of each suit)
    pub fn default_4p() -> Self {
        Self {
            disp: "".to_string(),
            aka53: 1,
            aka52: 1,
            aka51: 1,
        }
    }

    /// Default rule for 3-player games (no manzu 2-8, 2 red 5p)
    pub fn default_3p() -> Self {
        Self {
            disp: "".to_string(),
            aka53: 1,
            aka52: 2,
            aka51: 0,
        }
    }
}

/// Player mapping for Majsoul account information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerMapping {
    pub nickname: String,
    pub account_id: u32,
}

/// Wrapper for tensoul-py compatible output format.
/// This is the top-level structure that wraps the log with error handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensoulOutput {
    /// Whether an error occurred during conversion
    pub is_error: bool,

    /// Error message if is_error is true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,

    /// The converted log if is_error is false
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<TenhouLog>,
}

impl TensoulOutput {
    /// Create a successful output with the given log
    pub fn success(log: TenhouLog) -> Self {
        Self {
            is_error: false,
            error_msg: None,
            log: Some(log),
        }
    }

    /// Create an error output with the given message
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            is_error: true,
            error_msg: Some(msg.into()),
            log: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tenhou_log_serializes() {
        let log = TenhouLog {
            ver: "2.3".to_string(),
            ref_: "231124-04996d02-e68f-422a-919f-b68487bb3ba1".to_string(),
            ratingc: "PF4".to_string(),
            rule: TenhouRule {
                disp: "王座の間南喰".to_string(),
                aka53: 1,
                aka52: 1,
                aka51: 1,
            },
            lobby: 0,
            dan: vec![
                "雀聖★1".to_string(),
                "魂天Lv3".to_string(),
                "雀聖★1".to_string(),
                "雀聖★1".to_string(),
            ],
            rate: vec![1271, 1030, 3300, 220],
            sx: vec!["C".to_string(); 4],
            name: vec![
                "朔月灰".to_string(),
                "kikou".to_string(),
                "NaRuuuu".to_string(),
                "デア・リヒター".to_string(),
            ],
            sc: vec![
                Value::from(29000),
                Value::from(-1.0),
                Value::from(46400),
                Value::from(36.4),
                Value::from(31500),
                Value::from(11.5),
                Value::from(-6900),
                Value::from(-46.9),
            ],
            title: vec![
                Value::String("王座の間南喰".to_string()),
                Value::from(1700816360),
            ],
            log: vec![],
            player_mapping: Some(vec![
                PlayerMapping {
                    nickname: "朔月灰".to_string(),
                    account_id: 434208,
                },
                PlayerMapping {
                    nickname: "kikou".to_string(),
                    account_id: 72059462,
                },
            ]),
        };

        let json = serde_json::to_string(&log).unwrap();

        // Verify key fields are serialized correctly
        assert!(json.contains("\"ver\":\"2.3\""));
        assert!(json.contains("\"ref\":\"231124-04996d02-e68f-422a-919f-b68487bb3ba1\""));
        assert!(json.contains("\"ratingc\":\"PF4\""));
        assert!(json.contains("\"disp\":\"王座の間南喰\""));
        assert!(json.contains("\"aka53\":1"));
        assert!(json.contains("\"lobby\":0"));
        assert!(json.contains("\"playerMapping\""));
    }

    #[test]
    fn test_tenhou_log_deserializes() {
        let json = r#"{
            "ver": "2.3",
            "ref": "test-uuid",
            "ratingc": "PF4",
            "rule": { "disp": "test", "aka53": 1, "aka52": 1, "aka51": 1 },
            "lobby": 0,
            "dan": ["雀聖★1"],
            "rate": [1500],
            "sx": ["C"],
            "name": ["Player1"],
            "sc": [25000, 0.0],
            "title": ["test", 12345],
            "log": []
        }"#;

        let log: TenhouLog = serde_json::from_str(json).unwrap();
        assert_eq!(log.ver, "2.3");
        assert_eq!(log.ref_, "test-uuid");
        assert_eq!(log.ratingc, "PF4");
        assert_eq!(log.rule.disp, "test");
        assert_eq!(log.dan.len(), 1);
    }

    #[test]
    fn test_tensoul_output_success() {
        let log = TenhouLog::new("test-uuid".to_string(), 4);
        let output = TensoulOutput::success(log);

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"is_error\":false"));
        assert!(!json.contains("error_msg"));
        assert!(json.contains("\"log\":{"));
    }

    #[test]
    fn test_tensoul_output_error() {
        let output = TensoulOutput::error("Something went wrong");

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"is_error\":true"));
        assert!(json.contains("\"error_msg\":\"Something went wrong\""));
        assert!(!json.contains("\"log\":"));
    }

    #[test]
    fn test_rule_defaults() {
        let rule_4p = TenhouRule::default_4p();
        assert_eq!(rule_4p.aka53, 1);
        assert_eq!(rule_4p.aka52, 1);
        assert_eq!(rule_4p.aka51, 1);

        let rule_3p = TenhouRule::default_3p();
        assert_eq!(rule_3p.aka53, 1);
        assert_eq!(rule_3p.aka52, 2);
        assert_eq!(rule_3p.aka51, 0); // No manzu 2-8 in 3p
    }

    #[test]
    fn test_sc_mixed_types() {
        // The sc array contains mixed int/float values
        let log = TenhouLog {
            ver: "2.3".to_string(),
            ref_: "test".to_string(),
            ratingc: "PF4".to_string(),
            rule: TenhouRule::default_4p(),
            lobby: 0,
            dan: vec![],
            rate: vec![],
            sx: vec![],
            name: vec![],
            sc: vec![
                Value::from(29000),   // points (int)
                Value::from(-1.0),    // delta (float)
                Value::from(46400),   // points (int)
                Value::from(36.4),    // delta (float)
            ],
            title: vec![],
            log: vec![],
            player_mapping: None,
        };

        let json = serde_json::to_string(&log).unwrap();
        // Verify the mixed types are preserved
        assert!(json.contains("29000"));
        assert!(json.contains("-1.0") || json.contains("-1"));
        assert!(json.contains("46400"));
    }

    #[test]
    fn test_kyoku_log_structure() {
        // Test that kyoku log entries serialize as nested arrays
        let kyoku: Vec<Value> = vec![
            // Round info: [round*4+kyoku, honba, riichi_sticks]
            Value::Array(vec![Value::from(0), Value::from(0), Value::from(0)]),
            // Starting scores
            Value::Array(vec![
                Value::from(25000),
                Value::from(25000),
                Value::from(25000),
                Value::from(25000),
            ]),
            // Dora indicators
            Value::Array(vec![Value::from(52)]),
            // Ura dora indicators
            Value::Array(vec![]),
            // Player 0 hand
            Value::Array(vec![
                Value::from(22),
                Value::from(31),
                Value::from(13),
            ]),
        ];

        let json = serde_json::to_string(&kyoku).unwrap();
        assert!(json.starts_with("[[0,0,0]"));
    }
}
