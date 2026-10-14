//! Canonical share IDs and sortable game-time rank metadata.
use super::types::GameRecord;

/// Match Koromo's JavaScript bitwise encoding, including signed 32-bit XOR.
pub fn encoded_account_id(account: i64) -> i64 {
    let value = account.wrapping_mul(7).wrapping_add(1_117_113);
    let bytes = value.to_le_bytes();
    let bits = i32::from_le_bytes(bytes[..4].try_into().expect("four-byte slice"));
    1_358_437 + i64::from(bits ^ 0x0523_8e99)
}

pub fn row(game: &GameRecord) -> Option<serde_json::Value> {
    let player = game.players.first()?;
    let ranks: Vec<i32> = game.players.iter().map(|player| player.level).collect();
    let mut sorted = ranks.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let rank_key = sorted.iter().map(|level| format!("{level:05}")).collect::<Vec<_>>().join("-");
    let room = match game.mode_id { 12 => "jade", 16 => "throne", _ => "other" };
    let day = chrono::DateTime::from_timestamp(game.start_time, 0)
        .map(|date| (date + chrono::Duration::hours(9)).format("%Y-%m-%d").to_string());
    Some(serde_json::json!({
        "uuid": game.uuid,
        "paipu": format!("{}_a{}", game.uuid, encoded_account_id(player.account_id)),
        "day": day,
        "mode": game.mode_id,
        "startTime": game.start_time,
        "players": game.players,
        "rank_source": "game_record",
        "player_levels": ranks,
        "rank_sort_key": rank_key,
        "filename": format!("{room}__r{rank_key}__{}.mjai.json", game.uuid),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_encoding_matches_javascript_including_signed_overflow() {
        assert_eq!(encoded_account_id(1), 88_555_454);
        assert_eq!(encoded_account_id(71144487), 418_926_776);
        assert_eq!(encoded_account_id(400_000_000), -1_544_887_419);
    }
}
