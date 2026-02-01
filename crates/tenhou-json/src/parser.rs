use crate::model::*;
use crate::score::*;
use serde_json::value::Index;
use serde_json::Value;
use std::str::FromStr;
use thiserror::Error;

pub type TenhouJsonResult<T> = Result<T, TenhouJsonError>;

#[derive(Debug, Error)]
#[error("{kind} at {path}")]
pub struct TenhouJsonError {
    pub kind: TenhouJsonErrorKind,
    pub path: String,
}

impl TenhouJsonError {
    pub fn new(kind: TenhouJsonErrorKind) -> Self {
        TenhouJsonError { path: String::new(), kind }
    }
}

#[derive(Debug, Error)]
pub enum TenhouJsonErrorKind {
    #[error("Cannot parse json")]
    JsonParseError,
    #[error("Missing field")]
    MissingField,
    #[error("Missmatch type")]
    TypeMismatch,
    #[error("Invalid array length")]
    InvalidArrayLength,
    #[error("Invalid meld format")]
    InvalidMeld,
    #[error("Invalid riichi format")]
    InvalidRiichi,
    #[error("Invalid ankan format")]
    InvalidAnkan,
    #[error("Invalid kakan format")]
    InvalidKakan,
    #[error("Invalid decoration")]
    InvalidDecoration,
    #[error("Invalid tile number")]
    InvalidTileNumber,
    #[error("Invalid extra ryuukyoku reason")]
    InvalidExtraRyuukyokuReason,
    #[error("Invalid yaku name")]
    InvalidYakuName,
    #[error("Invalid yaku level")]
    InvalidYakuLevel,
    #[error("Invalid yaku format")]
    InvalidYakuFormat,
    #[error("Invalid ranked score")]
    InvalidRankedScore,
    #[error("Invalid agari format")]
    InvalidAgariFormat,
    #[error("Invalid letter position")]
    InvalidLetterPosition,
}

trait WithContext {
    fn context(self, key: &str) -> Self;
    fn index_context(self, index: usize) -> Self;
}

impl<T> WithContext for TenhouJsonResult<T> {
    fn context(self, key: &str) -> Self {
        self.map_err(|e| {
            TenhouJsonError { path: format!("{}.{}", key, e.path), ..e } // TODO array index
        })
    }

    fn index_context(self, index: usize) -> Self {
        self.context(&format!("[{}]", index))
    }
}

fn conv_i64(v: &Value) -> TenhouJsonResult<i64> {
    v.as_i64().ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::TypeMismatch))
}

fn conv_i32(v: &Value) -> TenhouJsonResult<i32> {
    Ok(conv_i64(v)? as i32)
}

fn conv_i8(v: &Value) -> TenhouJsonResult<i8> {
    Ok(conv_i64(v)? as i8)
}

fn conv_u32(v: &Value) -> TenhouJsonResult<u32> {
    Ok(conv_i64(v)? as u32)
}

fn conv_u8(v: &Value) -> TenhouJsonResult<u8> {
    Ok(conv_i64(v)? as u8)
}

fn conv_f64(v: &Value) -> TenhouJsonResult<f64> {
    v.as_f64().ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::TypeMismatch))
}

fn conv_array(v: &Value) -> TenhouJsonResult<&Vec<Value>> {
    v.as_array().ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::TypeMismatch))
}

fn conv_str(v: &Value) -> TenhouJsonResult<&str> {
    v.as_str().ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::TypeMismatch))
}

fn conv_string(v: &Value) -> TenhouJsonResult<String> {
    Ok(conv_str(v)?.to_string())
}

fn conv_rule(v: &Value) -> TenhouJsonResult<Rule> {
    Ok(Rule {
        disp: get_field_string(v, "disp")?,
        aka51: get_field_u32(v, "aka51")? != 0,
        aka52: get_field_u32(v, "aka52")? != 0,
        aka53: get_field_u32(v, "aka53")? != 0,
    })
}

fn conv_tile_from_u8(x: u8) -> TenhouJsonResult<Tile> {
    Tile::from_u8(x).map_err(|_| TenhouJsonError::new(TenhouJsonErrorKind::InvalidTileNumber))
}

fn conv_tile_from_ascii(x0: u8, x1: u8) -> TenhouJsonResult<Tile> {
    let y0 = x0 - b'0';
    let y1 = x1 - b'0';
    conv_tile_from_u8(y0 * 10 + y1)
}

fn conv_tile(v: &Value) -> TenhouJsonResult<Tile> {
    conv_tile_from_u8(conv_u8(v)?)
}

fn parse_decorated_tile(s: &str) -> TenhouJsonResult<(Vec<Tile>, u8, usize)> {
    if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidMeld));
    }

    let xs: Vec<u8> = s.bytes().collect();

    let (letter_pos, letter) = xs.iter().enumerate().find(|(_, c)| c.is_ascii_alphabetic()).ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::InvalidMeld))?;
    if letter_pos % 2 != 0 {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidMeld));
    }

    let numbers: Vec<u8> = xs.iter().enumerate().filter(|(i, _)| *i != letter_pos).map(|(_, c)| *c).collect();
    if !numbers.iter().all(|c| c.is_ascii_digit()) {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidMeld));
    }

    let tiles = numbers.chunks(2).map(|c| conv_tile_from_ascii(c[0], c[1])).collect::<TenhouJsonResult<Vec<_>>>()?;

    Ok((tiles, *letter, letter_pos))
}

fn conv_incoming_tile(v: &Value) -> TenhouJsonResult<IncomingTile> {
    if v.is_i64() {
        // normal tsumo
        Ok(IncomingTile::Tsumo(conv_tile(v)?))
    } else {
        // chii/pon
        let s = conv_str(v)?;
        let (tiles, letter, letter_pos) = parse_decorated_tile(s)?;
        match letter {
            b'c' => {
                if letter_pos != 0 {
                    return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidLetterPosition));
                }
                Ok(IncomingTile::Chii { combination: (tiles[0], tiles[1], tiles[2]) })
            }
            b'p' => {
                let dir = match letter_pos {
                    0 => Direction::Kamicha,
                    2 => Direction::Toimen,
                    4 => Direction::Shimocha,
                    _ => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidLetterPosition)),
                };
                Ok(IncomingTile::Pon {
                    combination: (tiles[0], tiles[1], tiles[2]),
                    dir,
                })
            }
            b'm' => {
                let dir = match letter_pos {
                    0 => Direction::Kamicha,
                    2 => Direction::Toimen,
                    6 => Direction::Shimocha,
                    _ => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidLetterPosition)),
                };
                Ok(IncomingTile::Daiminkan {
                    combination: (tiles[0], tiles[1], tiles[2], tiles[3]),
                    dir,
                })
            }
            _ => Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidMeld))?,
        }
    }
}

fn conv_outgoing_tile(v: &Value) -> TenhouJsonResult<OutgoingTile> {
    if v.is_i64() {
        // normal discard
        let x = conv_u8(v)?;
        match x {
            60 => Ok(OutgoingTile::Tsumogiri),
            0 => Ok(OutgoingTile::Dummy),
            x => Ok(OutgoingTile::Discard(conv_tile_from_u8(x)?)),
        }
    } else {
        // riichi/ankan/kakan
        let s = conv_str(v)?;

        // this is special case
        if s == "r60" {
            return Ok(OutgoingTile::TsumogiriRiichi);
        }

        let (tiles, letter, letter_pos) = parse_decorated_tile(s)?;
        if letter == b'r' {
            if tiles.len() != 1 {
                return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidRiichi));
            }
            Ok(OutgoingTile::Riichi(tiles[0]))
        } else if letter == b'a' {
            if tiles.len() != 4 {
                return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidAnkan));
            }
            Ok(OutgoingTile::Ankan(tiles[3]))
        } else if letter == b'k' {
            if tiles.len() != 4 {
                return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidKakan));
            }

            let dir = match letter_pos {
                0 => Direction::Kamicha,
                2 => Direction::Toimen,
                4 => Direction::Shimocha,
                _ => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidLetterPosition)),
            };

            // reference:
            // 2019010215gm-00a9-0000-93e74c9f.json
            // 35p3553 -> 35k353553
            // added tile is after 'k'?
            let added_index = letter_pos / 2;
            let added = tiles[added_index];
            let mut comb = tiles.clone();
            comb.remove(added_index);

            Ok(OutgoingTile::Kakan {
                combination: (comb[0], comb[1], comb[2]),
                dir,
                added,
            })
        } else {
            Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidDecoration))
        }
    }
}

fn conv_tiles(v: &Value) -> TenhouJsonResult<Vec<Tile>> {
    conv_array(v)?.iter().map(conv_tile).collect()
}

fn conv_incoming_tiles(v: &Value) -> TenhouJsonResult<Vec<IncomingTile>> {
    conv_array(v)?.iter().map(conv_incoming_tile).collect()
}

fn conv_outgoing_tiles(v: &Value) -> TenhouJsonResult<Vec<OutgoingTile>> {
    conv_array(v)?.iter().map(conv_outgoing_tile).collect()
}

fn conv_round_setting(vs: &[Value]) -> TenhouJsonResult<RoundSettings> {
    let h1 = conv_i32_array(&vs[0])?;
    if h1.len() != 3 {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidArrayLength));
    }

    Ok(RoundSettings {
        kyoku: h1[0] as u8,
        honba: h1[1] as u8,
        kyoutaku: h1[2] as u8,
        points: conv_i32_array(&vs[1])?,
        dora: conv_tiles(&vs[2])?,
        ura_dora: conv_tiles(&vs[3])?,
    })
}

fn conv_round_player(vs: &[Value]) -> TenhouJsonResult<RoundPlayer> {
    Ok(RoundPlayer {
        hand: conv_tiles(&vs[0])?,
        incoming: conv_incoming_tiles(&vs[1])?,
        outgoing: conv_outgoing_tiles(&vs[2])?,
    })
}

fn conv_round_players(vs: &[Value]) -> TenhouJsonResult<Vec<RoundPlayer>> {
    vs.chunks(3).map(conv_round_player).collect()
}

fn conv_extra_ryuukyoku_reason(s: &str) -> TenhouJsonResult<ExtraRyuukyokuReason> {
    ExtraRyuukyokuReason::from_str(s).map_err(|_| TenhouJsonError::new(TenhouJsonErrorKind::InvalidExtraRyuukyokuReason))
}

fn conv_ranked_score(v: &Value) -> TenhouJsonResult<RankedScore> {
    let s = conv_str(v)?;
    RankedScore::from_str(s).map_err(|_| TenhouJsonError::new(TenhouJsonErrorKind::InvalidRankedScore))
}

fn conv_yaku_pair(v: &Value) -> TenhouJsonResult<YakuPair> {
    let s = conv_str(v)?;
    YakuPair::from_str(s).map_err(|_| TenhouJsonError::new(TenhouJsonErrorKind::InvalidYakuFormat))
}

fn conv_yaku_pair_array(vs: &[Value]) -> TenhouJsonResult<Vec<YakuPair>> {
    vs.iter().map(conv_yaku_pair).collect()
}

fn conv_agari(chunk0: &Value, chunk1: &Value) -> TenhouJsonResult<Agari> {
    let xs = conv_array(chunk1)?;
    if xs.len() <= 4 {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidAgariFormat));
    }

    Ok(Agari {
        delta_points: conv_i32_array(chunk0)?,
        who: conv_u8(&xs[0])?,
        from_who: conv_u8(&xs[1])?,
        pao_who: conv_u8(&xs[2])?,
        ranked_score: conv_ranked_score(&xs[3])?,
        yaku: conv_yaku_pair_array(&xs[4..])?,
    })
}

fn conv_agari_array(vs: &[Value]) -> TenhouJsonResult<Vec<Agari>> {
    vs.chunks(2).map(|chunk| conv_agari(&chunk[0], &chunk[1])).collect()
}

fn conv_round_result(v: &Value) -> TenhouJsonResult<RoundResult> {
    let xs = conv_array(v)?;
    if xs.is_empty() {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidArrayLength));
    }

    match conv_str(&xs[0])? {
        "和了" => Ok(RoundResult::Agari { agari_vec: conv_agari_array(&xs[1..])? }),
        x => {
            // NOT CLEAR:
            // If the score changes due to double riichi, will the nine tiles affect delta_points?
            Ok(RoundResult::Ryuukyoku {
                reason: conv_extra_ryuukyoku_reason(x)?,
                delta_points: if xs.len() >= 2 { conv_i32_array(&xs[1])? } else { vec![] },
            })
        }
    }
}

fn conv_round(v: &Value) -> TenhouJsonResult<Round> {
    let xs = conv_array(v)?;

    // header(4) + players(4*3) + result(1) == 17
    if xs.len() != 17 {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidArrayLength));
    }

    Ok(Round {
        settings: conv_round_setting(&xs[0..4])?,
        players: conv_round_players(&xs[4..16])?,
        result: conv_round_result(&xs[16])?,
    })
}

fn conv_connection(v: &Value) -> TenhouJsonResult<Connection> {
    Ok(Connection {
        what: get_field_u8(v, "what")?,
        log: get_field_i8(v, "log")?,
        who: get_field_u8(v, "who")?,
        step: get_field_u32(v, "step")?,
    })
}

fn conv_tenhou_json(v: &Value) -> TenhouJsonResult<TenhouJson> {
    let sc = get_field(v, "sc")?;
    let sc_array = conv_array(sc)?;
    let (even_sc, odd_sc) = get_partition_even_odd(sc_array);
    let final_points = even_sc.iter().map(conv_i32).collect::<TenhouJsonResult<Vec<i32>>>()?;
    let final_results = odd_sc.iter().map(conv_f64).collect::<TenhouJsonResult<Vec<f64>>>()?;

    Ok(TenhouJson {
        ver: get_field_f64(v, "ver")?,
        reference: get_field_string(v, "ref")?,
        rounds: get_field_round_array(v, "log")?,
        connections: get_field_connection_array(v, "connection")?,
        ratingc: get_field_string(v, "ratingc")?,
        rule: get_field_rule(v, "rule")?,
        lobby: get_field_u32(v, "lobby")?,
        dan: get_field_string_array(v, "dan")?,
        rate: get_field_f64_array(v, "rate")?,
        sx: get_field_string_array(v, "sx")?,
        final_points,
        final_results,
        names: get_field_string_array(v, "name")?,
    })
}

fn conv_string_array(v: &Value) -> TenhouJsonResult<Vec<String>> {
    conv_array(v)?.iter().enumerate().map(|(i, x)| conv_string(x).index_context(i)).collect()
}

fn conv_f64_array(v: &Value) -> TenhouJsonResult<Vec<f64>> {
    conv_array(v)?.iter().enumerate().map(|(i, x)| conv_f64(x).index_context(i)).collect()
}

fn conv_i32_array(v: &Value) -> TenhouJsonResult<Vec<i32>> {
    conv_array(v)?.iter().enumerate().map(|(i, x)| conv_i32(x).index_context(i)).collect()
}

fn conv_round_array(v: &Value) -> TenhouJsonResult<Vec<Round>> {
    conv_array(v)?.iter().enumerate().map(|(i, x)| conv_round(x).index_context(i)).collect()
}

fn conv_connection_array(v: &Value) -> TenhouJsonResult<Vec<Connection>> {
    conv_array(v)?.iter().enumerate().map(|(i, x)| conv_connection(x).index_context(i)).collect()
}

fn get_field<I: Index + ToString>(json: &Value, index: I) -> TenhouJsonResult<&Value> {
    json.get(&index).ok_or_else(|| TenhouJsonError::new(TenhouJsonErrorKind::MissingField))
}

fn get_field_u8(json: &Value, key: &str) -> TenhouJsonResult<u8> {
    let v = get_field(json, key)?;
    conv_u8(v).context(key)
}

fn get_field_i8(json: &Value, key: &str) -> TenhouJsonResult<i8> {
    let v = get_field(json, key)?;
    conv_i8(v).context(key)
}

fn get_field_u32(json: &Value, key: &str) -> TenhouJsonResult<u32> {
    let v = get_field(json, key)?;
    conv_u32(v).context(key)
}

fn get_field_f64(json: &Value, key: &str) -> TenhouJsonResult<f64> {
    let v = get_field(json, key)?;
    conv_f64(v).context(key)
}

fn get_field_string(json: &Value, key: &str) -> TenhouJsonResult<String> {
    let v = get_field(json, key)?;
    conv_string(v).context(key)
}

fn get_field_string_array(json: &Value, key: &str) -> TenhouJsonResult<Vec<String>> {
    let v = get_field(json, key)?;
    conv_string_array(v).context(key)
}

fn get_field_f64_array(json: &Value, key: &str) -> TenhouJsonResult<Vec<f64>> {
    let v = get_field(json, key)?;
    conv_f64_array(v).context(key)
}

fn get_field_rule(json: &Value, key: &str) -> TenhouJsonResult<Rule> {
    let v = get_field(json, key)?;
    conv_rule(v).context(key)
}

fn get_field_round_array(json: &Value, key: &str) -> TenhouJsonResult<Vec<Round>> {
    let v = get_field(json, key)?;
    conv_round_array(v).context(key)
}

fn get_field_connection_array(json: &Value, key: &str) -> TenhouJsonResult<Vec<Connection>> {
    if let Some(v) = json.get(key) {
        conv_connection_array(v).context(key)
    } else {
        Ok(vec![])
    }
}

fn get_partition_even_odd<T: Clone>(v: &[T]) -> (Vec<T>, Vec<T>) {
    (v.iter().step_by(2).cloned().collect(), v.iter().skip(1).step_by(2).cloned().collect())
}

pub fn parse_tenhou_json(text: &str) -> TenhouJsonResult<TenhouJson> {
    let json: Value = serde_json::from_str(text).map_err(|_| TenhouJsonError::new(TenhouJsonErrorKind::JsonParseError))?;
    conv_tenhou_json(&json)
}
