use super::json_scheme::{ActionItem, KyokuMeta, RawLog, ResultItem};
use crate::{KyokuFilter, Tile};

use serde::Serialize;
use serde_json::{self as json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid json: {source}")]
    InvalidJSON {
        #[from]
        source: json::Error,
    },
    #[error("not four-player game: {0}")]
    NotFourPlayer(String),
    #[error("unknown game length: {0}")]
    UnknownGameLength(String),
    #[error("invalid hora detail")]
    InvalidHoraDetail,
}

/// The overview structure of log in tenhou.net/6 format.
#[derive(Debug, Clone)]
pub struct Log {
    pub names: [String; 4],
    pub game_length: GameLength,
    pub has_aka: bool,
    pub kyokus: Vec<Kyoku>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GameLength {
    Hanchan = 0,
    Tonpuu = 4,
}

/// Contains information about a kyoku.
#[derive(Debug, Clone)]
pub struct Kyoku {
    pub meta: KyokuMeta,
    pub scoreboard: [i32; 4],
    pub dora_indicators: Vec<Tile>,
    pub ura_indicators: Vec<Tile>,
    pub action_tables: [ActionTable; 4],
    pub end_status: EndStatus,
}

#[derive(Debug, Clone)]
pub enum EndStatus {
    Hora { details: Vec<HoraDetail> },
    Ryukyoku { score_deltas: [i32; 4] },
}

#[derive(Debug, Clone, Default)]
pub struct HoraDetail {
    pub who: u8,
    pub target: u8,
    pub score_deltas: [i32; 4],
}

/// A group of "配牌", "取" and "出", describing a player's
/// gaming status and actions throughout a kyoku.
#[derive(Debug, Clone)]
pub struct ActionTable {
    pub haipai: [Tile; 13],
    pub takes: Vec<ActionItem>,
    pub discards: Vec<ActionItem>,
}

impl Log {
    /// Parse a tenhou.net/6 log from JSON string.
    #[inline]
    pub fn from_json_str(json_string: &str) -> Result<Self, ParseError> {
        let raw_log: RawLog = json::from_str(json_string)?;
        Self::try_from(raw_log)
    }

    #[inline]
    pub fn filter_kyokus(&mut self, kyoku_filter: &KyokuFilter) {
        self.kyokus
            .retain(|l| kyoku_filter.test(l.meta.kyoku_num, l.meta.honba));
    }
}

impl TryFrom<RawLog> for Log {
    type Error = ParseError;

    fn try_from(raw_log: RawLog) -> Result<Self, Self::Error> {
        let RawLog {
            logs, names, rule, ..
        } = raw_log;

        if rule.disp.contains('三') || rule.disp.contains("3-Player") {
            return Err(ParseError::NotFourPlayer(rule.disp.clone()));
        }
        // disp is a free-form room string (e.g. Te南喰赤); reject unknown winds instead of defaulting so sanma/typo logs cannot silently parse as hanchan.
        let game_length = if rule.disp.contains('東') || rule.disp.contains("East") {
            GameLength::Tonpuu
        } else if rule.disp.contains('南') || rule.disp.contains("South") {
            GameLength::Hanchan
        } else {
            return Err(ParseError::UnknownGameLength(rule.disp.clone()));
        };
        let has_aka = rule.aka + rule.aka51 + rule.aka52 + rule.aka53 > 0;

        let mut kyokus = Vec::with_capacity(logs.len());
        for log in logs {
            let mut kyoku = Kyoku {
                meta: log.meta,
                scoreboard: log.scoreboard,
                dora_indicators: log.dora_indicators,
                ura_indicators: log.ura_indicators,
                action_tables: [
                    ActionTable {
                        haipai: log.haipai_0,
                        takes: log.takes_0,
                        discards: log.discards_0,
                    },
                    ActionTable {
                        haipai: log.haipai_1,
                        takes: log.takes_1,
                        discards: log.discards_1,
                    },
                    ActionTable {
                        haipai: log.haipai_2,
                        takes: log.takes_2,
                        discards: log.discards_2,
                    },
                    ActionTable {
                        haipai: log.haipai_3,
                        takes: log.takes_3,
                        discards: log.discards_3,
                    },
                ],
                end_status: EndStatus::Ryukyoku {
                    score_deltas: [0; 4], // default
                },
            };

            if let Some(ResultItem::Status(status_text)) = log.results.first() {
                if status_text == "和了" {
                    // Allow: tenhou.net pair layout is format-pinned to 2-element chunks.
                    #[allow(clippy::chunks_exact_to_as_chunks)]
                    let chunks = log.results[1..].chunks_exact(2);
                    // A non-empty remainder means a truncated/corrupt array, not a short-but-valid one — fail the whole log.
                    if !chunks.remainder().is_empty() {
                        return Err(ParseError::InvalidHoraDetail);
                    }
                    let mut details = vec![];
                    for detail_tuple in chunks {
                        let [ResultItem::ScoreDeltas(score_deltas), ResultItem::HoraDetail(who_target_tuple)] = detail_tuple else {
                            return Err(ParseError::InvalidHoraDetail);
                        };
                        let who_value = who_target_tuple.first().ok_or(ParseError::InvalidHoraDetail)?;
                        let target_value = who_target_tuple.get(1).ok_or(ParseError::InvalidHoraDetail)?;
                        let Value::Number(who_number) = who_value else {
                            return Err(ParseError::InvalidHoraDetail);
                        };
                        let Value::Number(target_number) = target_value else {
                            return Err(ParseError::InvalidHoraDetail);
                        };
                        let who_u64 = who_number.as_u64().ok_or(ParseError::InvalidHoraDetail)?;
                        let target_u64 = target_number.as_u64().ok_or(ParseError::InvalidHoraDetail)?;
                        let who = u8::try_from(who_u64).map_err(|_| ParseError::InvalidHoraDetail)?;
                        let target = u8::try_from(target_u64).map_err(|_| ParseError::InvalidHoraDetail)?;
                        let hora_detail = HoraDetail {
                            score_deltas: *score_deltas,
                            who,
                            target,
                        };
                        details.push(hora_detail);
                    }
                    if details.is_empty() {
                        return Err(ParseError::InvalidHoraDetail);
                    }
                    kyoku.end_status = EndStatus::Hora { details };
                } else {
                    let score_deltas = match log.results.get(1) {
                        None => [0; 4],
                        Some(ResultItem::ScoreDeltas(dts)) => *dts,
                        Some(_) => return Err(ParseError::InvalidHoraDetail),
                    };
                    kyoku.end_status = EndStatus::Ryukyoku { score_deltas };
                }
            }

            kyokus.push(kyoku);
        }

        Ok(Self {
            names,
            game_length,
            has_aka,
            kyokus,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{"name":["A","B","C","D"],"rule":{"disp":"特南喰赤","aka":1},"log":[[[0,0,0],[25000,25000,25000,25000],[37],[37],[11,12,13,14,15,16,17,18,19,21,22,23,24],[12],[11],[11,12,13,14,15,16,17,18,19,21,22,23,24],[13],[14],[11,12,13,14,15,16,17,18,19,21,22,23,24],[15],[16],[11,12,13,14,15,16,17,18,19,21,22,23,24],[17],[18],["流局"]]]}"#;

    #[test]
    fn garbage_disp_is_rejected() {
        let bad = BASE.replace("特南喰赤", "???");
        let err = Log::from_json_str(&bad).expect_err("garbage disp must fail");
        assert!(matches!(err, ParseError::UnknownGameLength(_)));
    }

    #[test]
    fn sanma_disp_is_rejected_with_payload() {
        let bad = BASE.replace("特南喰赤", "特三喰赤");
        let err = Log::from_json_str(&bad).expect_err("sanma must fail");
        assert!(matches!(err, ParseError::NotFourPlayer(disp) if disp == "特三喰赤"));
    }

    #[test]
    fn float_hora_who_is_rejected() {
        // A float seat number is not an integer seat and must not silently truncate.
        let bad = BASE.replace(r#"["流局"]"#, r#"["和了",[8000,-8000,0,0],[0.5,1,0]]"#);
        let err = Log::from_json_str(&bad).expect_err("float hora who must fail");
        assert!(matches!(err, ParseError::InvalidHoraDetail));
    }

    #[test]
    fn swapped_hora_pair_is_rejected() {
        // Slot 1 must carry ScoreDeltas, never the HoraDetail tuple.
        let bad = BASE.replace(r#"["流局"]"#, r#"["和了",[0,1,0],[8000,-8000,0,0]]"#);
        let err = Log::from_json_str(&bad).expect_err("swapped pair must fail");
        assert!(matches!(err, ParseError::InvalidHoraDetail));
    }

    #[test]
    fn valid_hora_still_parses() {
        let ok = BASE.replace(
            r#"["流局"]"#,
            r#"["和了",[8000,-8000,0,0],[0,1,0,"30符3飜8000点"]]"#,
        );
        let log = Log::from_json_str(&ok).expect("valid hora must parse");
        match &log.kyokus[0].end_status {
            EndStatus::Hora { details } => {
                assert_eq!(details.len(), 1);
                assert_eq!(details[0].who, 0);
                assert_eq!(details[0].target, 1);
            }
            EndStatus::Ryukyoku { .. } => panic!("expected hora"),
        }
    }
}
