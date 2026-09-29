use phf::phf_set;
use serde_json::Value;

static VALID_TILES: phf::Set<&'static str> = phf_set! {
    "1m","2m","3m","4m","5m","6m","7m","8m","9m","5mr",
    "1p","2p","3p","4p","5p","6p","7p","8p","9p","5pr",
    "1s","2s","3s","4s","5s","6s","7s","8s","9s","5sr",
    "E","S","W","N","P","F","C","?",
};

static VALID_EVENTS: phf::Set<&'static str> = phf_set! {
    "start_game","end_game","start_kyoku","end_kyoku",
    "tsumo","dahai","chi","pon","daiminkan","kakan","ankan",
    "hora","ryukyoku","dora","reach","reach_accepted",
    "nukidora","none",
};

pub struct FileResult {
    pub name: String,
    pub valid: bool,
    pub errors: Vec<String>,
    pub line_count: u64,
}

fn validate_tile(tile: &str) -> bool {
    VALID_TILES.contains(tile)
}

pub fn validate_file(name: String, data: &[u8]) -> FileResult {
    let mut result = FileResult {
        name,
        valid: true,
        errors: Vec::new(),
        line_count: 0,
    };

    if data.is_empty() {
        result.valid = false;
        result.errors.push("empty file".into());
        return result;
    }

    let text = match std::str::from_utf8(data) {
        Ok(t) => t,
        Err(e) => {
            result.valid = false;
            result.errors.push(format!("invalid UTF-8: {e}"));
            return result;
        }
    };

    let mut first_type: Option<String> = None;
    let mut last_type: Option<String> = None;
    let mut kyoku_starts: u64 = 0;
    let mut kyoku_ends: u64 = 0;
    let mut has_start_kyoku = false;
    let mut prev_line: Option<&str> = None;

    for (i, line) in text.lines().enumerate() {
        let lineno = i + 1;
        result.line_count += 1;

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if Some(line) == prev_line
            && result.errors.len() < 50 {
                result
                    .errors
                    .push(format!("L{lineno}: duplicate consecutive line"));
            }
        prev_line = Some(line);

        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                result.valid = false;
                if result.errors.len() < 50 {
                    result.errors.push(format!("L{lineno}: bad JSON: {e}"));
                }
                continue;
            }
        };

        let obj = match event.as_object() {
            Some(o) => o,
            None => {
                result.valid = false;
                if result.errors.len() < 50 {
                    result.errors.push(format!("L{lineno}: not a JSON object"));
                }
                continue;
            }
        };

        let etype = match obj.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => {
                result.valid = false;
                if result.errors.len() < 50 {
                    result.errors.push(format!("L{lineno}: missing 'type'"));
                }
                continue;
            }
        };

        if first_type.is_none() {
            first_type = Some(etype.to_string());
        }
        last_type = Some(etype.to_string());

        if !VALID_EVENTS.contains(etype) && result.errors.len() < 50 {
            result
                .errors
                .push(format!("L{lineno}: unknown event '{etype}'"));
        }

        match etype {
            "start_kyoku" => {
                kyoku_starts += 1;
                has_start_kyoku = true;
            }
            "end_kyoku" => {
                kyoku_ends += 1;
            }
            _ => {}
        }

        if let Some(actor) = obj.get("actor")
            && let Some(a) = actor.as_i64()
                && !(0..=3).contains(&a) {
                    result.valid = false;
                    if result.errors.len() < 50 {
                        result
                            .errors
                            .push(format!("L{lineno}: actor={a} out of range"));
                    }
                }

        for field in &["pai", "dora_marker"] {
            if let Some(v) = obj.get(*field).and_then(|v| v.as_str())
                && !validate_tile(v) {
                    result.valid = false;
                    if result.errors.len() < 50 {
                        result
                            .errors
                            .push(format!("L{lineno}: bad tile '{v}' in {field}"));
                    }
                }
        }

        for field in &["consumed", "ura_markers"] {
            if let Some(arr) = obj.get(*field).and_then(|v| v.as_array()) {
                for v in arr {
                    if let Some(t) = v.as_str()
                        && !validate_tile(t) {
                            result.valid = false;
                            if result.errors.len() < 50 {
                                result
                                    .errors
                                    .push(format!("L{lineno}: bad tile '{t}' in {field}"));
                            }
                        }
                }
            }
        }

        if let Some(tehais) = obj.get("tehais").and_then(|v| v.as_array()) {
            if tehais.len() != 4 {
                result.valid = false;
                if result.errors.len() < 50 {
                    result
                        .errors
                        .push(format!("L{lineno}: tehais len={}", tehais.len()));
                }
            } else {
                for (pi, hand) in tehais.iter().enumerate() {
                    if let Some(tiles) = hand.as_array() {
                        for t in tiles {
                            if let Some(ts) = t.as_str()
                                && !validate_tile(ts) {
                                    result.valid = false;
                                    if result.errors.len() < 50 {
                                        result.errors.push(format!(
                                            "L{lineno}: bad tile '{ts}' in P{pi} hand"
                                        ));
                                    }
                                }
                        }
                    }
                }
            }
        }

        if result.errors.len() >= 50 {
            break;
        }
    }

    if result.line_count == 0 {
        result.valid = false;
        result.errors.push("no events".into());
        return result;
    }

    if first_type.as_deref() != Some("start_game") {
        result.valid = false;
        result.errors.push(format!(
            "first event '{}', expected 'start_game'",
            first_type.as_deref().unwrap_or("?")
        ));
    }

    if last_type.as_deref() != Some("end_game") {
        result.valid = false;
        result.errors.push(format!(
            "last event '{}', expected 'end_game'",
            last_type.as_deref().unwrap_or("?")
        ));
    }

    if !has_start_kyoku {
        result.valid = false;
        result.errors.push("no start_kyoku events".into());
    }

    if kyoku_starts != kyoku_ends {
        result.valid = false;
        result.errors.push(format!(
            "kyoku mismatch: {kyoku_starts} starts vs {kyoku_ends} ends"
        ));
    }

    result
}
