use mjlog::model::*;
use mjlog::parser::MjlogError;
use std::iter::once;
use tenhou_json::calc::*;
use tenhou_json::model::*;
use tenhou_json::parser::*;
use tenhou_json::score::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConvError {
    #[error(transparent)]
    MjlogError(#[from] MjlogError),
    #[error(transparent)]
    TenhouJsonError(#[from] TenhouJsonError),
    #[error("Action GO is not found")]
    NotFoundActionGO,
    #[error("Action UN1 is not found")]
    NotFoundActionUN1,
    #[error("Terminal action is not found")]
    NotFoundTerminalAction,
    #[error("Round is not found")]
    NotFoundRound,
    #[error("FinalResult is not found")]
    NotFoundFinalResult,
    #[error("Invalid round format")]
    InvalidRoundFormat,
    #[error("Invalid tile format")]
    InvalidTileFormat,
}

type ConvResult<T> = Result<T, ConvError>;

fn extract_round_indices(actions: &[Action]) -> Vec<(usize, usize)> {
    let mut indices: Vec<(usize, usize)> = Vec::new();
    let mut start = None;

    for (i, a) in actions.iter().enumerate() {
        if a.is_init() {
            if let Some(start_index) = start {
                indices.push((start_index, i));
            }
            start = Some(i);
        }
    }

    if let Some(start_index) = start {
        indices.push((start_index, actions.len()));
    }

    indices
}

// NOT CLEAR: When double ron
fn find_final_result(actions: &[Action]) -> ConvResult<(Vec<i32>, Vec<f64>)> {
    // find from last
    for a in actions.iter().rev() {
        match a {
            Action::AGARI(ActionAGARI { owari, .. }) => {
                if let Some(x) = owari {
                    return Ok(x.clone());
                } else {
                    return Err(ConvError::InvalidRoundFormat);
                }
            }
            Action::RYUUKYOKU(ActionRYUUKYOKU { owari, .. }) => {
                if let Some(x) = owari {
                    return Ok(x.clone());
                } else {
                    return Err(ConvError::InvalidRoundFormat);
                }
            }
            _ => {}
        }
    }
    Err(ConvError::NotFoundFinalResult)
}

const DAN_NAME: [&str; 21] = [
    "新人", "９級", "８級", "７級", "６級", "５級", "４級", "３級", "２級", "１級", "初段", "二段", "三段", "四段", "五段", "六段", "七段", "八段", "九段", "十段", "天鳳",
];

fn conv_dan(dan: &TenhouRank) -> String {
    DAN_NAME[*dan as usize].to_string()
}

fn conv_tile_from_u8(x: u8) -> ConvResult<Tile> {
    Tile::from_u8(x).map_err(|_| ConvError::InvalidTileFormat)
}

fn conv_hai_to_tile(hai: Hai, red_enable: bool) -> ConvResult<Tile> {
    let hai_number = hai.to_u8();

    if red_enable {
        match hai_number {
            16 => return conv_tile_from_u8(51),
            52 => return conv_tile_from_u8(52),
            88 => return conv_tile_from_u8(53),
            _ => {}
        }
    }

    // pict_order
    // 123456789m123456789p123456789s1234567z
    let pict_order = hai_number / 4;

    // 1 == m
    // 2 == p
    // 3 == s
    // 4 == z
    let pict_type = (pict_order / 9) + 1;

    // 1..9mps or 1..7z
    let pict_num = (pict_order % 9) + 1;

    conv_tile_from_u8(pict_type * 10 + pict_num)
}

fn get_dora_vec(dora_hyouji: Hai, mid_actions: &[Action]) -> ConvResult<Vec<Tile>> {
    let dora_hais: Vec<Hai> = once(dora_hyouji).chain(mid_actions.iter().filter_map(|x| x.as_dora()).map(|x| x.hai)).collect();
    dora_hais.iter().map(|x| conv_hai_to_tile(*x, true)).collect()
}

fn get_ura_dora(end_action: &Action) -> ConvResult<Vec<Tile>> {
    match end_action {
        Action::AGARI(ActionAGARI { dora_hai_ura, .. }) => dora_hai_ura.iter().map(|x| conv_hai_to_tile(*x, true)).collect(),
        Action::RYUUKYOKU(_) => Ok(vec![]),
        _ => panic!("unexpected end action"),
    }
}

/// Note:
/// The ura-dora is only recorded in the winning information of the riichi declarer.
/// Therefore, in the case of multiple ron, the ura-dora must be retrieved from each winner.
/// However, if it is found for one player, it will be the same for all winners.
fn get_ura_dora_vec(end_actions: &[&Action]) -> ConvResult<Vec<Tile>> {
    for a in end_actions {
        let ura_dora = get_ura_dora(a)?;
        if !ura_dora.is_empty() {
            return Ok(ura_dora);
        }
    }
    Ok(Vec::new())
}

fn conv_rule(settings: &GameSettings) -> ConvResult<Rule> {
    let room_str = match settings.room {
        TenhouRoom::Ippan => "般",
        TenhouRoom::Joukyu => "上",
        TenhouRoom::Tokujou => "特",
        TenhouRoom::Houou => "鳳",
    };

    let hanchan_str = if settings.hanchan { "南" } else { "東" };
    let aka_str = if settings.no_red { "" } else { "赤" };
    let kuitan_str = if settings.no_kuitan { "" } else { "喰" };
    let soku_str = if settings.soku { "速" } else { "" };

    Ok(Rule {
        disp: format!("{}{}{}{}{}", room_str, hanchan_str, kuitan_str, aka_str, soku_str),
        aka53: !settings.no_red,
        aka52: !settings.no_red,
        aka51: !settings.no_red,
    })
}

fn conv_round_setting(actions: &[Action]) -> ConvResult<RoundSettings> {
    let start_action = &actions[0];
    assert!(start_action.is_init());

    let init = start_action.as_init().unwrap();
    let end_actions: Vec<&Action> = actions.iter().filter(|x| x.is_agari() || x.is_ryuukyoku()).collect();

    if end_actions.is_empty() {
        return Err(ConvError::NotFoundTerminalAction);
    }

    Ok(RoundSettings {
        kyoku: init.seed.kyoku,
        honba: init.seed.honba,
        kyoutaku: init.seed.kyoutaku,
        points: init.ten.iter().map(|x| x * 100).collect(),
        dora: get_dora_vec(init.seed.dora_hyouji, actions)?,
        ura_dora: get_ura_dora_vec(&end_actions)?,
    })
}

fn conv_yaku(x: mjlog::model::Yaku) -> tenhou_json::model::Yaku {
    match x {
        mjlog::model::Yaku::MenzenTsumo => tenhou_json::model::Yaku::MenzenTsumo,
        mjlog::model::Yaku::Riichi => tenhou_json::model::Yaku::Riichi,
        mjlog::model::Yaku::Ippatsu => tenhou_json::model::Yaku::Ippatsu,
        mjlog::model::Yaku::Chankan => tenhou_json::model::Yaku::Chankan,
        mjlog::model::Yaku::Rinshankaihou => tenhou_json::model::Yaku::Rinshankaihou,
        mjlog::model::Yaku::HaiteiTsumo => tenhou_json::model::Yaku::HaiteiTsumo,
        mjlog::model::Yaku::HouteiRon => tenhou_json::model::Yaku::HouteiRon,
        mjlog::model::Yaku::Pinfu => tenhou_json::model::Yaku::Pinfu,
        mjlog::model::Yaku::Tanyao => tenhou_json::model::Yaku::Tanyao,
        mjlog::model::Yaku::Iipeikou => tenhou_json::model::Yaku::Iipeikou,
        mjlog::model::Yaku::PlayerWindTon => tenhou_json::model::Yaku::PlayerWindTon,
        mjlog::model::Yaku::PlayerWindNan => tenhou_json::model::Yaku::PlayerWindNan,
        mjlog::model::Yaku::PlayerWindSha => tenhou_json::model::Yaku::PlayerWindSha,
        mjlog::model::Yaku::PlayerWindPei => tenhou_json::model::Yaku::PlayerWindPei,
        mjlog::model::Yaku::FieldWindTon => tenhou_json::model::Yaku::FieldWindTon,
        mjlog::model::Yaku::FieldWindNan => tenhou_json::model::Yaku::FieldWindNan,
        mjlog::model::Yaku::FieldWindSha => tenhou_json::model::Yaku::FieldWindSha,
        mjlog::model::Yaku::FieldWindPei => tenhou_json::model::Yaku::FieldWindPei,
        mjlog::model::Yaku::YakuhaiHaku => tenhou_json::model::Yaku::YakuhaiHaku,
        mjlog::model::Yaku::YakuhaiHatsu => tenhou_json::model::Yaku::YakuhaiHatsu,
        mjlog::model::Yaku::YakuhaiChun => tenhou_json::model::Yaku::YakuhaiChun,
        mjlog::model::Yaku::DoubleRiichi => tenhou_json::model::Yaku::DoubleRiichi,
        mjlog::model::Yaku::Chiitoitsu => tenhou_json::model::Yaku::Chiitoitsu,
        mjlog::model::Yaku::Chanta => tenhou_json::model::Yaku::Chanta,
        mjlog::model::Yaku::Ikkitsuukan => tenhou_json::model::Yaku::Ikkitsuukan,
        mjlog::model::Yaku::SansyokuDoujun => tenhou_json::model::Yaku::SansyokuDoujun,
        mjlog::model::Yaku::SanshokuDoukou => tenhou_json::model::Yaku::SanshokuDoukou,
        mjlog::model::Yaku::Sankantsu => tenhou_json::model::Yaku::Sankantsu,
        mjlog::model::Yaku::Toitoi => tenhou_json::model::Yaku::Toitoi,
        mjlog::model::Yaku::Sanannkou => tenhou_json::model::Yaku::Sanannkou,
        mjlog::model::Yaku::Shousangen => tenhou_json::model::Yaku::Shousangen,
        mjlog::model::Yaku::Honroutou => tenhou_json::model::Yaku::Honroutou,
        mjlog::model::Yaku::Ryanpeikou => tenhou_json::model::Yaku::Ryanpeikou,
        mjlog::model::Yaku::Junchan => tenhou_json::model::Yaku::Junchan,
        mjlog::model::Yaku::Honiisou => tenhou_json::model::Yaku::Honiisou,
        mjlog::model::Yaku::Chiniisou => tenhou_json::model::Yaku::Chiniisou,
        mjlog::model::Yaku::Renhou => tenhou_json::model::Yaku::Renhou,
        mjlog::model::Yaku::Tenhou => tenhou_json::model::Yaku::Tenhou,
        mjlog::model::Yaku::Chiihou => tenhou_json::model::Yaku::Chiihou,
        mjlog::model::Yaku::Daisangen => tenhou_json::model::Yaku::Daisangen,
        mjlog::model::Yaku::Suuankou => tenhou_json::model::Yaku::Suuankou,
        mjlog::model::Yaku::SuuankouTanki => tenhou_json::model::Yaku::SuuankouTanki,
        mjlog::model::Yaku::Tsuuiisou => tenhou_json::model::Yaku::Tsuuiisou,
        mjlog::model::Yaku::Ryuuiisou => tenhou_json::model::Yaku::Ryuuiisou,
        mjlog::model::Yaku::Chinroutou => tenhou_json::model::Yaku::Chinroutou,
        mjlog::model::Yaku::Tyuurenpoutou => tenhou_json::model::Yaku::Tyuurenpoutou,
        mjlog::model::Yaku::Tyuurenpoutou9 => tenhou_json::model::Yaku::Tyuurenpoutou9,
        mjlog::model::Yaku::Kokushimusou => tenhou_json::model::Yaku::Kokushimusou,
        mjlog::model::Yaku::Kokushimusou13 => tenhou_json::model::Yaku::Kokushimusou13,
        mjlog::model::Yaku::Daisuushii => tenhou_json::model::Yaku::Daisuushii,
        mjlog::model::Yaku::Syousuushii => tenhou_json::model::Yaku::Syousuushii,
        mjlog::model::Yaku::Suukantsu => tenhou_json::model::Yaku::Suukantsu,
        mjlog::model::Yaku::Dora => tenhou_json::model::Yaku::Dora,
        mjlog::model::Yaku::UraDora => tenhou_json::model::Yaku::UraDora,
        mjlog::model::Yaku::AkaDora => tenhou_json::model::Yaku::AkaDora,
    }
}

fn conv_extra_ryuukyoku_reason(x: &Option<mjlog::model::ExtraRyuukyokuReason>) -> tenhou_json::model::ExtraRyuukyokuReason {
    match x {
        Some(mjlog::model::ExtraRyuukyokuReason::KyuusyuKyuuhai) => tenhou_json::model::ExtraRyuukyokuReason::KyuusyuKyuuhai,
        Some(mjlog::model::ExtraRyuukyokuReason::SuuchaRiichi) => tenhou_json::model::ExtraRyuukyokuReason::SuuchaRiichi,
        Some(mjlog::model::ExtraRyuukyokuReason::SanchaHoura) => tenhou_json::model::ExtraRyuukyokuReason::SanchaHoura,
        Some(mjlog::model::ExtraRyuukyokuReason::SuukanSanra) => tenhou_json::model::ExtraRyuukyokuReason::SuukanSanra,
        Some(mjlog::model::ExtraRyuukyokuReason::SuufuuRenda) => tenhou_json::model::ExtraRyuukyokuReason::SuufuuRenda,
        Some(mjlog::model::ExtraRyuukyokuReason::NagashiMangan) => tenhou_json::model::ExtraRyuukyokuReason::NagashiMangan,
        None => tenhou_json::model::ExtraRyuukyokuReason::Ryuukyoku,
    }
}

fn is_not_ura_zero(x: &YakuPair) -> bool {
    !matches!(
        x,
        YakuPair {
            yaku: tenhou_json::model::Yaku::UraDora,
            level: YakuLevel::Normal(0)
        }
    )
}

fn conv_ranked_score_normal(v: &ActionAGARI, han: u8, oya: Player) -> RankedScore {
    match (v.is_tsumo(), v.who == oya) {
        (true, true) => get_oya_tsumo(v.fu, han),
        (true, false) => get_ko_tsumo(v.fu, han),
        (false, true) => get_oya_ron(v.fu, han),
        (false, false) => get_ko_ron(v.fu, han),
    }
}

fn conv_ranked_score_yakuman(v: &ActionAGARI, num: u8, oya: Player) -> RankedScore {
    match (v.is_tsumo(), v.who == oya) {
        (true, true) => get_oya_tsumo_yakuman(num),
        (true, false) => get_ko_tsumo_yakuman(num),
        (false, true) => get_oya_ron_yakuman(num),
        (false, false) => get_ko_ron_yakuman(num),
    }
}

fn conv_yaku_vec(vs: &[(mjlog::model::Yaku, u8)]) -> Vec<YakuPair> {
    vs.iter()
        .map(|&(yaku, han)| YakuPair {
            yaku: conv_yaku(yaku),
            level: YakuLevel::Normal(han),
        })
        .filter(is_not_ura_zero)
        .collect()
}

fn conv_yakuman_vec(vs: &[mjlog::model::Yaku]) -> Vec<YakuPair> {
    vs.iter()
        .map(|&yaku| YakuPair {
            yaku: conv_yaku(yaku),
            level: YakuLevel::Yakuman(1),
        })
        .collect()
}

fn conv_agari(v: &ActionAGARI, oya: Player) -> ConvResult<Agari> {
    let delta_points = v.delta_points.iter().map(|&x| x * 100).collect();
    let who = v.who.to_u8();
    let from_who = v.from_who.to_u8();
    let pao_who = if let Some(w) = v.pao_who { w.to_u8() } else { v.who.to_u8() };

    let (yaku, ranked_score) = if !v.yaku.is_empty() {
        let yaku = conv_yaku_vec(&v.yaku);
        let han = yaku.iter().fold(0, |sum, YakuPair { level, .. }| sum + level.get_number());
        (yaku, conv_ranked_score_normal(v, han, oya))
    } else if !v.yakuman.is_empty() {
        let yaku = conv_yakuman_vec(&v.yakuman);
        let num = yaku.iter().fold(0, |sum, YakuPair { level, .. }| sum + level.get_number());
        (yaku, conv_ranked_score_yakuman(v, num, oya))
    } else {
        panic!("unexpected");
    };

    Ok(Agari {
        delta_points,
        who,
        from_who,
        pao_who,
        ranked_score,
        yaku,
    })
}

fn conv_agari_vec(vs: &[&ActionAGARI], oya: Player) -> ConvResult<Vec<Agari>> {
    vs.iter().map(|x| conv_agari(x, oya)).collect()
}

fn conv_round_result_from_agari(vs: &[&ActionAGARI], oya: Player) -> ConvResult<RoundResult> {
    Ok(RoundResult::Agari { agari_vec: conv_agari_vec(vs, oya)? })
}

fn conv_delta_points_ryuukyoku(v: &ActionRYUUKYOKU) -> Vec<i32> {
    if v.delta_points.iter().any(|&x| x != 0) {
        v.delta_points.iter().map(|&x| x * 100).collect()
    } else {
        Vec::new()
    }
}

fn conv_round_result_from_ryuukyoku(v: &ActionRYUUKYOKU) -> ConvResult<RoundResult> {
    let reason = match conv_extra_ryuukyoku_reason(&v.reason) {
        tenhou_json::model::ExtraRyuukyokuReason::Ryuukyoku => match (v.hai0.is_some(), v.hai1.is_some(), v.hai2.is_some(), v.hai3.is_some()) {
            (true, true, true, true) => tenhou_json::model::ExtraRyuukyokuReason::TenpaiEverybody,
            (false, false, false, false) => tenhou_json::model::ExtraRyuukyokuReason::TenpaiNobody,
            _ => tenhou_json::model::ExtraRyuukyokuReason::Ryuukyoku,
        },
        x => x,
    };

    Ok(RoundResult::Ryuukyoku {
        reason,
        delta_points: conv_delta_points_ryuukyoku(v),
    })
}

fn conv_round_result(actions: &[Action]) -> ConvResult<RoundResult> {
    let init_action = actions[0].as_init().unwrap();

    let ryuukyoku_actions: Vec<&ActionRYUUKYOKU> = actions.iter().filter_map(|x| x.as_ryuukyoku()).collect();
    if ryuukyoku_actions.len() == 1 {
        return conv_round_result_from_ryuukyoku(ryuukyoku_actions[0]);
    }

    // Note: Consider double ron
    let agari_actions: Vec<&ActionAGARI> = actions.iter().filter_map(|x| x.as_agari()).collect();
    if !agari_actions.is_empty() {
        return conv_round_result_from_agari(&agari_actions, init_action.oya);
    }

    // not found terminal action, or there are multi ryuukyoku tags
    Err(ConvError::InvalidRoundFormat)
}

fn conv_tiles(xs: &[Hai]) -> ConvResult<Vec<Tile>> {
    xs.iter().map(|&x| conv_hai_to_tile(x, true)).collect()
}

// tenhou json's initial hand order:
// normal 5 -> red 5 -> normal 6
fn get_initial_hand_order(t: &Tile) -> u32 {
    match t.to_u8() {
        51 => 151,
        52 => 251,
        53 => 351,
        x => x as u32 * 10,
    }
}

fn is_valid_player_action(action: &Action, target_player: Player) -> bool {
    match action {
        Action::DRAW(ActionDRAW { who, .. }) => *who == target_player,
        Action::DISCARD(ActionDISCARD { who, .. }) => *who == target_player,
        Action::REACH1(ActionREACH1 { who, .. }) => *who == target_player,
        Action::N(ActionN { who, .. }) => *who == target_player,
        _ => false,
    }
}

fn conv_dir(d: mjlog::model::Direction) -> tenhou_json::model::Direction {
    match d {
        mjlog::model::Direction::SelfSeat => tenhou_json::model::Direction::SelfSeat,
        mjlog::model::Direction::Shimocha => tenhou_json::model::Direction::Shimocha,
        mjlog::model::Direction::Kamicha => tenhou_json::model::Direction::Kamicha,
        mjlog::model::Direction::Toimen => tenhou_json::model::Direction::Toimen,
    }
}

fn replay_actions(actions: &[&Action]) -> ConvResult<(Vec<IncomingTile>, Vec<OutgoingTile>)> {
    let mut incoming = vec![];
    let mut outgoing = vec![];
    let mut reach_declared = false;
    let mut last_draw = None;

    for a in actions {
        match a {
            Action::DRAW(x) => {
                let tile = conv_hai_to_tile(x.hai, true)?;
                incoming.push(IncomingTile::Tsumo(tile));
                last_draw = Some(x.hai);
            }
            Action::DISCARD(x) => {
                match last_draw {
                    Some(h) if h == x.hai => {
                        if reach_declared {
                            outgoing.push(OutgoingTile::TsumogiriRiichi)
                        } else {
                            outgoing.push(OutgoingTile::Tsumogiri)
                        }
                    }
                    _ => {
                        let tile = conv_hai_to_tile(x.hai, true)?;
                        if reach_declared {
                            outgoing.push(OutgoingTile::Riichi(tile))
                        } else {
                            outgoing.push(OutgoingTile::Discard(tile))
                        }
                    }
                }
                reach_declared = false;
                last_draw = None;
            }
            Action::REACH1(_) => {
                reach_declared = true;
            }
            Action::N(x) => {
                match x.m {
                    Meld::Chii { combination, called_position } => {
                        // mjlog: sorted in ascending order.
                        // tenhou json: the placement order on the board.
                        let orders = match called_position {
                            0 => combination,
                            1 => (combination.1, combination.0, combination.2),
                            2 => (combination.2, combination.0, combination.1),
                            _ => panic!("unexpected called position"),
                        };

                        let incoming_tile = IncomingTile::Chii {
                            combination: (conv_hai_to_tile(orders.0, true)?, conv_hai_to_tile(orders.1, true)?, conv_hai_to_tile(orders.2, true)?),
                        };
                        incoming.push(incoming_tile);
                    }
                    Meld::Pon { dir: src_dir, called, unused, .. } => {
                        let dir = conv_dir(src_dir);
                        // mjlog: sorted in ascending order.
                        // tenhou json: the placement order on the board.
                        if called.is_number5() {
                            let called_tile = conv_hai_to_tile(called, true)?;
                            let unused_tile = conv_hai_to_tile(unused, true)?;
                            let tile = called_tile.to_black();

                            if unused_tile.is_red() {
                                incoming.push(IncomingTile::Pon { dir, combination: (tile, tile, tile) })
                            } else if called_tile.is_red() {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Kamicha => (called_tile, tile, tile),
                                    tenhou_json::model::Direction::Toimen => (tile, called_tile, tile),
                                    tenhou_json::model::Direction::Shimocha => (tile, tile, called_tile),
                                    _ => panic!("unexpected"),
                                };
                                incoming.push(IncomingTile::Pon { dir, combination });
                            } else {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Shimocha => (tile, tile.to_red(), tile),
                                    _ => (tile, tile, tile.to_red()),
                                };
                                incoming.push(IncomingTile::Pon { dir, combination });
                            }
                        } else {
                            // combination, called, unused, all the same
                            let tile = conv_hai_to_tile(called, true)?;
                            incoming.push(IncomingTile::Pon { dir, combination: (tile, tile, tile) })
                        }
                    }
                    Meld::Kakan { dir: src_dir, called, added, .. } => {
                        let dir = conv_dir(src_dir);

                        // mjlog: sorted in ascending order.
                        // tenhou json: the placement order on the board.
                        if called.is_number5() {
                            let called_tile = conv_hai_to_tile(called, true)?;
                            let added_tile = conv_hai_to_tile(added, true)?;
                            let tile = called_tile.to_black();

                            if added_tile.is_red() {
                                outgoing.push(OutgoingTile::Kakan {
                                    dir,
                                    combination: (tile, tile, tile),
                                    added: added_tile,
                                })
                            } else if called_tile.is_red() {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Kamicha => (called_tile, tile, tile),
                                    tenhou_json::model::Direction::Toimen => (tile, called_tile, tile),
                                    tenhou_json::model::Direction::Shimocha => (tile, tile, called_tile),
                                    _ => panic!("unexpected"),
                                };
                                outgoing.push(OutgoingTile::Kakan { dir, combination, added: added_tile });
                            } else {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Shimocha => (tile, tile.to_red(), tile),
                                    _ => (tile, tile, tile.to_red()),
                                };
                                outgoing.push(OutgoingTile::Kakan { dir, combination, added: added_tile });
                            }
                        } else {
                            // combination, called, added, all the same
                            let tile = conv_hai_to_tile(called, true)?;
                            outgoing.push(OutgoingTile::Kakan {
                                dir,
                                combination: (tile, tile, tile),
                                added: tile,
                            })
                        }
                    }
                    Meld::Daiminkan { dir: src_dir, hai } => {
                        let dir = conv_dir(src_dir);
                        if hai.is_number5() {
                            let called_tile = conv_hai_to_tile(hai, true)?;
                            let tile = called_tile.to_black();

                            if called_tile.is_red() {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Kamicha => (called_tile, tile, tile, tile),
                                    tenhou_json::model::Direction::Toimen => (tile, called_tile, tile, tile),
                                    tenhou_json::model::Direction::Shimocha => (tile, tile, tile, called_tile),
                                    _ => panic!("unexpected"),
                                };
                                incoming.push(IncomingTile::Daiminkan { combination, dir });
                            } else {
                                let combination = match dir {
                                    tenhou_json::model::Direction::Shimocha => (tile, tile, tile.to_red(), tile),
                                    _ => (tile, tile, tile, tile.to_red()),
                                };
                                incoming.push(IncomingTile::Daiminkan { combination, dir });
                            }
                        } else {
                            let tile = conv_hai_to_tile(hai, true)?;
                            incoming.push(IncomingTile::Daiminkan { combination: (tile, tile, tile, tile), dir });
                        }
                        outgoing.push(OutgoingTile::Dummy)
                    }
                    Meld::Ankan { hai } => {
                        // NOT CLEAR
                        // I think the red 5 is always recorded when ankan of 5.
                        outgoing.push(OutgoingTile::Ankan(conv_hai_to_tile(hai, true)?.to_red()))
                    }
                }
            }
            _ => panic!("unexpected"),
        }
    }

    // The last dummy is invalid and should be removed.
    while outgoing.last() == Some(&OutgoingTile::Dummy) {
        outgoing.pop();
    }

    Ok((incoming, outgoing))
}

fn conv_round_players(actions: &[Action]) -> ConvResult<Vec<RoundPlayer>> {
    let init_action = actions[0].as_init().unwrap();

    let mut players = vec![];
    for (i, h) in init_action.hai.iter().enumerate() {
        let mut hand = conv_tiles(h)?;
        hand.sort_by_key(get_initial_hand_order);

        let player_actions: Vec<&Action> = actions.iter().filter(|x| is_valid_player_action(x, Player::new(i as u8))).collect();
        let (incoming, outgoing) = replay_actions(&player_actions)?;

        players.push(RoundPlayer { hand, incoming, outgoing });
    }
    Ok(players)
}

fn conv_round(actions: &[Action]) -> ConvResult<Round> {
    Ok(Round {
        settings: conv_round_setting(actions)?,
        players: conv_round_players(actions)?,
        result: conv_round_result(actions)?,
    })
}

fn conv_rounds(actions: &[Action], indices: &[(usize, usize)]) -> ConvResult<Vec<Round>> {
    let mut rounds = vec![];

    for &(start, end) in indices {
        rounds.push(conv_round(&actions[start..end])?);
    }

    Ok(rounds)
}

fn conv_connections(actions: &[Action], indices: &[(usize, usize)]) -> ConvResult<Vec<Connection>> {
    let mut connections = vec![];

    // before first INIT
    for a in &actions[0..indices[0].0] {
        match a {
            Action::BYE(bye) => connections.push(Connection {
                what: 0,
                log: -1,
                who: bye.who.to_u8(),
                step: 0,
            }),
            Action::UN2(un2) => connections.push(Connection {
                what: 1,
                log: -1,
                who: un2.who.to_u8(),
                step: 0,
            }),
            _ => {}
        }
    }

    // rounds
    for (log_index, &(start, end)) in indices.iter().enumerate() {
        let mut step = 0;

        for a in &actions[start..end] {
            match a {
                Action::BYE(bye) => connections.push(Connection {
                    what: 0,
                    log: log_index as i8,
                    who: bye.who.to_u8(),
                    step: step as u32,
                }),
                Action::UN2(un2) => connections.push(Connection {
                    what: 1,
                    log: log_index as i8,
                    who: un2.who.to_u8(),
                    step: step as u32,
                }),
                Action::INIT(_) => {}
                Action::TAIKYOKU(_) => {}
                Action::SHUFFLE(_) => {}
                Action::GO(_) => {}
                Action::UN1(_) => {}
                Action::AGARI(_) => {}
                Action::RYUUKYOKU(_) => {}
                Action::DORA(_) => {}
                Action::REACH1(_) => {}
                Action::REACH2(_) => {}
                Action::N(_) => step += 1,
                Action::DRAW(_) => step += 1,
                Action::DISCARD(_) => step += 1,
            }
        }
    }

    Ok(connections)
}

pub fn conv_to_tenhou_json(mjlog: &Mjlog) -> ConvResult<TenhouJson> {
    let action_go = if let Some(Action::GO(x)) = mjlog.actions.iter().find(|x| x.is_go()) { Ok(x) } else { Err(ConvError::NotFoundActionGO) }?;
    let action_un1 = if let Some(Action::UN1(x)) = mjlog.actions.iter().find(|x| x.is_un1()) { Ok(x) } else { Err(ConvError::NotFoundActionUN1) }?;
    let round_indices = extract_round_indices(&mjlog.actions);
    if round_indices.is_empty() {
        return Err(ConvError::NotFoundRound);
    }

    let (final_points_raw, final_results_raw): (Vec<i32>, Vec<f64>) = find_final_result(&mjlog.actions)?;
    let final_points = final_points_raw.iter().map(|x| x * 100).collect();
    let final_results = final_results_raw.clone();

    Ok(TenhouJson {
        ver: 2.3, // Using this conversion system
        reference: String::new(),
        rounds: conv_rounds(&mjlog.actions, &round_indices)?,
        connections: conv_connections(&mjlog.actions, &round_indices)?,
        ratingc: "PF4".to_string(), // What does this mean?
        rule: conv_rule(&action_go.settings)?,
        lobby: action_go.lobby,
        dan: action_un1.dan.iter().map(conv_dan).collect(),
        rate: action_un1.rate.clone(),
        sx: action_un1.sx.clone(),
        final_points,
        final_results,
        names: action_un1.names.clone(),
    })
}
