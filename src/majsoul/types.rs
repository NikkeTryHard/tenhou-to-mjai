use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerLevel {
    pub id: i32,
    pub score: i32,
    pub delta: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerSearchResult {
    pub id: i64,
    pub nickname: String,
    pub level: Option<PlayerLevel>,
    pub latest_timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRecord {
    #[serde(rename = "accountId")]
    pub account_id: i64,
    pub nickname: String,
    pub level: i32,
    pub score: i32,
    #[serde(rename = "gradingScore")]
    pub grading_score: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameRecord {
    pub uuid: String,
    #[serde(rename = "modeId")]
    pub mode_id: i32,
    #[serde(rename = "startTime")]
    pub start_time: i64,
    #[serde(rename = "endTime")]
    pub end_time: Option<i64>,
    pub players: Vec<PlayerRecord>,
}
