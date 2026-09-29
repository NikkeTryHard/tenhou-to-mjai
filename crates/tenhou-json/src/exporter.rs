use crate::model::*;
use crate::parser::*;
use serde_json::{json, Value};
use std::iter::once;

fn export_rule(rule: &Rule) -> Value {
    json!({
        "disp": rule.disp,
        "aka53": if rule.aka53 {1} else {0},
        "aka52": if rule.aka52 {1} else {0},
        "aka51": if rule.aka51 {1} else {0},
    })
}

fn export_tile(tile: &Tile) -> Value {
    json!(tile.to_u8())
}

fn export_incoming_tile(incoming: &IncomingTile) -> TenhouJsonResult<Value> {
    match incoming {
        IncomingTile::Tsumo(t) => Ok(export_tile(t)),
        IncomingTile::Chii { combination: (t1, t2, t3) } => Ok(json!(format!("c{}{}{}", t1.to_u8(), t2.to_u8(), t3.to_u8()))),
        IncomingTile::Pon { combination: (t1, t2, t3), dir } => Ok(json!(match dir {
            Direction::Kamicha => format!("p{}{}{}", t1.to_u8(), t2.to_u8(), t3.to_u8()),
            Direction::Toimen => format!("{}p{}{}", t1.to_u8(), t2.to_u8(), t3.to_u8()),
            Direction::Shimocha => format!("{}{}p{}", t1.to_u8(), t2.to_u8(), t3.to_u8()),
            Direction::SelfSeat => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidDirection)),
        })),
        IncomingTile::Daiminkan { combination: (t1, t2, t3, t4), dir } => Ok(json!(match dir {
            Direction::Kamicha => format!("m{}{}{}{}", t1.to_u8(), t2.to_u8(), t3.to_u8(), t4.to_u8()),
            Direction::Toimen => format!("{}m{}{}{}", t1.to_u8(), t2.to_u8(), t3.to_u8(), t4.to_u8()),
            Direction::Shimocha => format!("{}{}{}m{}", t1.to_u8(), t2.to_u8(), t3.to_u8(), t4.to_u8()),
            Direction::SelfSeat => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidDirection)),
        })),
    }
}

fn export_outgoing_tile(outgoing: &OutgoingTile) -> TenhouJsonResult<Value> {
    match outgoing {
        OutgoingTile::Discard(t) => Ok(export_tile(t)),
        OutgoingTile::Riichi(t) => Ok(json!(format!("r{}", t.to_u8()))),
        // 60/0/r60 wire codes (see tenhou-json parser).
        OutgoingTile::Tsumogiri => Ok(json!(60)),
        OutgoingTile::TsumogiriRiichi => Ok(json!("r60")),
        OutgoingTile::Ankan(t) => {
            let b = t.to_black().to_u8();
            Ok(json!(format!("{}{}{}a{}", b, b, b, t.to_u8()))) // I think red is last always
        }
        OutgoingTile::Kakan { combination: (t1, t2, t3), dir, added } => Ok(json!(match dir {
            Direction::Kamicha => format!("k{}{}{}{}", added.to_u8(), t1.to_u8(), t2.to_u8(), t3.to_u8()),
            Direction::Toimen => format!("{}k{}{}{}", t1.to_u8(), added.to_u8(), t2.to_u8(), t3.to_u8()),
            Direction::Shimocha => format!("{}{}k{}{}", t1.to_u8(), t2.to_u8(), added.to_u8(), t3.to_u8()),
            Direction::SelfSeat => return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidDirection)),
        })),
        OutgoingTile::Dummy => Ok(json!(0)),
    }
}

fn export_tiles(tiles: &[Tile]) -> Vec<Value> {
    tiles.iter().map(export_tile).collect::<Vec<_>>()
}

fn export_incoming_tiles(incoming: &[IncomingTile]) -> TenhouJsonResult<Vec<Value>> {
    incoming.iter().map(export_incoming_tile).collect()
}

fn export_outgoing_tiles(outgoing: &[OutgoingTile]) -> TenhouJsonResult<Vec<Value>> {
    outgoing.iter().map(export_outgoing_tile).collect()
}

fn export_agari(agari: &Agari) -> [Value; 2] {
    let mut vec = vec![json!(agari.who), json!(agari.from_who), json!(agari.pao_who), json!(agari.ranked_score.to_string())];
    vec.extend(agari.yaku.iter().map(|x| json!(x.to_string())));

    [json!(agari.delta_points), json!(vec)]
}

fn export_round_result(result: &RoundResult) -> Value {
    match result {
        RoundResult::Agari { agari_vec } => json!(once(json!("和了")).chain(agari_vec.iter().flat_map(export_agari)).collect::<Vec<_>>()),
        RoundResult::Ryuukyoku { reason, delta_points } if delta_points.is_empty() => json!([json!(reason.to_str())]),
        RoundResult::Ryuukyoku { reason, delta_points } => json!([json!(reason.to_str()), json!(delta_points)]),
    }
}

fn export_round(round: &Round) -> TenhouJsonResult<Value> {
    if round.players.len() != 4 {
        return Err(TenhouJsonError::new(TenhouJsonErrorKind::InvalidPlayerCount(round.players.len())));
    }
    Ok(json!([
        [round.settings.kyoku, round.settings.honba, round.settings.kyoutaku],
        round.settings.points,
        export_tiles(&round.settings.dora),
        export_tiles(&round.settings.ura_dora),
        export_tiles(&round.players[0].hand),
        export_incoming_tiles(&round.players[0].incoming)?,
        export_outgoing_tiles(&round.players[0].outgoing)?,
        export_tiles(&round.players[1].hand),
        export_incoming_tiles(&round.players[1].incoming)?,
        export_outgoing_tiles(&round.players[1].outgoing)?,
        export_tiles(&round.players[2].hand),
        export_incoming_tiles(&round.players[2].incoming)?,
        export_outgoing_tiles(&round.players[2].outgoing)?,
        export_tiles(&round.players[3].hand),
        export_incoming_tiles(&round.players[3].incoming)?,
        export_outgoing_tiles(&round.players[3].outgoing)?,
        export_round_result(&round.result),
    ]))
}

fn export_rounds(rounds: &[Round]) -> TenhouJsonResult<Value> {
    let mut ret = vec![];
    for round in rounds {
        ret.push(export_round(round)?);
    }
    Ok(json!(ret))
}

fn export_rate(rate: &[f64]) -> Value {
    let mut ret = vec![];
    for &x in rate {
        if x.fract() == 0.0 {
            ret.push(json!(x as i64));
        } else {
            ret.push(json!(x));
        }
    }
    json!(ret)
}

fn export_sc(final_points: &[i32], final_results: &[f64]) -> Value {
    let mut ret = vec![];
    for (&a, &b) in final_points.iter().zip(final_results.iter()) {
        ret.push(json!(a));
        if b.fract() == 0.0 {
            ret.push(json!(b as i64));
        } else {
            ret.push(json!(b));
        }
    }
    json!(ret)
}

fn export_connection(connection: &Connection) -> Value {
    json!({
        "what": connection.what,
        "log": connection.log,
        "who": connection.who,
        "step": connection.step,
    })
}

fn export_connections(connections: &[Connection]) -> Value {
    json!(connections.iter().map(export_connection).collect::<Vec<_>>())
}

pub fn export_tenhou_json(src: &TenhouJson) -> TenhouJsonResult<String> {
    // use IndexMap to ignore "connection"
    let mut root = serde_json::Map::new();

    root.insert("ver".to_string(), json!(src.ver));
    root.insert("ref".to_string(), json!(src.reference));
    root.insert("log".to_string(), export_rounds(&src.rounds)?);
    if !src.connections.is_empty() {
        root.insert("connection".to_string(), export_connections(&src.connections));
    }

    root.insert("ratingc".to_string(), json!(src.ratingc));
    root.insert("rule".to_string(), export_rule(&src.rule));
    root.insert("lobby".to_string(), json!(src.lobby));
    root.insert("dan".to_string(), json!(src.dan));
    root.insert("rate".to_string(), export_rate(&src.rate));
    root.insert("sx".to_string(), json!(src.sx));
    root.insert("sc".to_string(), export_sc(&src.final_points, &src.final_results));
    root.insert("name".to_string(), json!(src.names));

    Ok(Value::Object(root).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile11() -> Tile {
        match Tile::from_u8(11) {
            Ok(t) => t,
            Err(_) => panic!("11 is a valid tile"),
        }
    }

    #[test]
    fn three_player_round_is_rejected() {
        let round = Round {
            players: vec![RoundPlayer::default(), RoundPlayer::default(), RoundPlayer::default()],
            ..Default::default()
        };
        let err = export_round(&round).expect_err("3-player round must fail");
        assert!(matches!(err.kind, TenhouJsonErrorKind::InvalidPlayerCount(3)));
    }

    #[test]
    fn self_seat_melds_are_rejected() {
        let t = tile11();
        let pon = IncomingTile::Pon {
            combination: (t, t, t),
            dir: Direction::SelfSeat,
        };
        let err = export_incoming_tile(&pon).expect_err("SelfSeat pon must fail");
        assert!(matches!(err.kind, TenhouJsonErrorKind::InvalidDirection));

        let kan = IncomingTile::Daiminkan {
            combination: (t, t, t, t),
            dir: Direction::SelfSeat,
        };
        let err = export_incoming_tile(&kan).expect_err("SelfSeat daiminkan must fail");
        assert!(matches!(err.kind, TenhouJsonErrorKind::InvalidDirection));

        let kakan = OutgoingTile::Kakan {
            combination: (t, t, t),
            dir: Direction::SelfSeat,
            added: t,
        };
        let err = export_outgoing_tile(&kakan).expect_err("SelfSeat kakan must fail");
        assert!(matches!(err.kind, TenhouJsonErrorKind::InvalidDirection));
    }
}
