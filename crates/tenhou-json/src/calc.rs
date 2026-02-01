use crate::score::*;

fn calc_base_points(fu: u8, han: u8) -> i32 {
    (fu as i32) << (han+2)
}

fn ceil_to_100(x: i32) -> i32 {
    (x+99)/100*100
}

pub fn get_oya_ron(fu: u8, han: u8) -> RankedScore {
    let base_points = calc_base_points(fu, han);
    if base_points >= 2000 {
        match han {
            han if 13 <= han => RankedScore { rank: ScoreRank::Yakuman, score: Score::Ron(48000) },
            han if 11 <= han => RankedScore { rank: ScoreRank::Sanbaiman, score: Score::Ron(36000) },
            han if 8 <= han => RankedScore { rank: ScoreRank::Baiman, score: Score::Ron(24000) },
            han if 6 <= han => RankedScore { rank: ScoreRank::Haneman, score: Score::Ron(18000) },
            _ => RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(12000) },
        }
    }
    else {
        RankedScore { rank: ScoreRank::Normal {fu,han}, score: Score::Ron(ceil_to_100(base_points*6)) }
    }
}

pub fn get_ko_ron(fu: u8, han: u8) -> RankedScore {
    let base_points = calc_base_points(fu, han);
    if base_points >= 2000 {
        match han {
            han if 13 <= han => RankedScore { rank: ScoreRank::Yakuman, score: Score::Ron(32000) },
            han if 11 <= han => RankedScore { rank: ScoreRank::Sanbaiman, score: Score::Ron(24000) },
            han if 8 <= han => RankedScore { rank: ScoreRank::Baiman, score: Score::Ron(16000) },
            han if 6 <= han => RankedScore { rank: ScoreRank::Haneman, score: Score::Ron(12000) },
            _ => RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(8000) },
        }
    }
    else {
        RankedScore { rank: ScoreRank::Normal {fu,han}, score: Score::Ron(ceil_to_100(base_points*4)) }
    }
}

pub fn get_oya_tsumo(fu: u8, han: u8) -> RankedScore {
    let base_points = calc_base_points(fu, han);
    if base_points >= 2000 {
        match han {
            han if 13 <= han => RankedScore { rank: ScoreRank::Yakuman, score: Score::OyaTsumo(16000)},
            han if 11 <= han => RankedScore { rank: ScoreRank::Sanbaiman, score: Score::OyaTsumo(12000)},
            han if 8 <= han => RankedScore { rank: ScoreRank::Baiman, score: Score::OyaTsumo(8000)},
            han if 6 <= han => RankedScore { rank: ScoreRank::Haneman, score: Score::OyaTsumo(6000)},
            _ => RankedScore { rank: ScoreRank::Mangan, score: Score::OyaTsumo(4000)},
        }
    }
    else {
        RankedScore { rank: ScoreRank::Normal {fu,han}, score: Score::OyaTsumo(ceil_to_100(base_points*2))}
    }
}

pub fn get_ko_tsumo(fu:u8, han:u8) -> RankedScore {
    let base_points = calc_base_points(fu, han);
    if base_points >= 2000 {
        match han {
            han if 13 <= han => RankedScore { rank: ScoreRank::Yakuman, score: Score::KoTsumo(8000,16000)},
            han if 11 <= han => RankedScore { rank: ScoreRank::Sanbaiman, score: Score::KoTsumo(6000,12000)},
            han if 8 <= han => RankedScore { rank: ScoreRank::Baiman, score: Score::KoTsumo(4000,8000)},
            han if 6 <= han => RankedScore { rank: ScoreRank::Haneman, score: Score::KoTsumo(3000,6000)},
            _ => RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)},
        }
    }
    else {
        RankedScore { rank: ScoreRank::Normal {fu, han}, score: Score::KoTsumo(ceil_to_100(base_points), ceil_to_100(base_points*2)) }
    }
}

pub fn get_oya_tsumo_yakuman(num: u8) -> RankedScore {
    RankedScore { rank: ScoreRank::Yakuman, score: Score::OyaTsumo(16000 * num as i32)}
}

pub fn get_ko_tsumo_yakuman(num: u8) -> RankedScore {
    RankedScore { rank: ScoreRank::Yakuman, score: Score::KoTsumo(8000 * num as i32, 16000 * num as i32)}
}

pub fn get_oya_ron_yakuman(num: u8) -> RankedScore {
    RankedScore { rank: ScoreRank::Yakuman, score: Score::Ron(48000 * num as i32)}
}

pub fn get_ko_ron_yakuman(num: u8) -> RankedScore {
    RankedScore { rank: ScoreRank::Yakuman, score: Score::Ron(32000 * num as i32)}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ko_ron_scores() {
        // Since all hands except pinfu tsumo are rounded up to 30 fu, there is no case where a ron results in 20 fu.
        assert_eq!(get_ko_ron(30,1), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 1 }, score: Score::Ron(1000)}); // Note: naki tanyao only / pinfu only
        assert_eq!(get_ko_ron(30,2), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 2 }, score: Score::Ron(2000)});
        assert_eq!(get_ko_ron(30,3), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 3 }, score: Score::Ron(3900)});
        assert_eq!(get_ko_ron(30,4), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 4 }, score: Score::Ron(7700)});
        assert_eq!(get_ko_ron(30,5), RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(8000)});
        assert_eq!(get_ko_ron(30,6), RankedScore { rank: ScoreRank::Haneman, score: Score::Ron(12000)});
        assert_eq!(get_ko_ron(40,1), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 1 }, score: Score::Ron(1300)});
        assert_eq!(get_ko_ron(40,2), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 2 }, score: Score::Ron(2600)});
        assert_eq!(get_ko_ron(40,3), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 3 }, score: Score::Ron(5200)});
        assert_eq!(get_ko_ron(40,4), RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(8000)});
        assert_eq!(get_ko_ron(40,5), RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(8000)});
        assert_eq!(get_ko_ron(40,6), RankedScore { rank: ScoreRank::Haneman, score: Score::Ron(12000)});
        assert_eq!(get_ko_ron(25,1), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 1 }, score: Score::Ron(800)}); // Note: doesn't actually exist
        assert_eq!(get_ko_ron(25,2), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 2 }, score: Score::Ron(1600)});
        assert_eq!(get_ko_ron(25,3), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 3 }, score: Score::Ron(3200)});
        assert_eq!(get_ko_ron(25,4), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 4 }, score: Score::Ron(6400)});
        assert_eq!(get_ko_ron(25,5), RankedScore { rank: ScoreRank::Mangan, score: Score::Ron(8000)});
        assert_eq!(get_ko_ron(25,6), RankedScore { rank: ScoreRank::Haneman, score: Score::Ron(12000)});
    }

    #[test]
    fn test_ko_tsumo_scores() {
        assert_eq!(get_ko_tsumo(20,1), RankedScore { rank: ScoreRank::Normal { fu: 20, han: 1 }, score: Score::KoTsumo(200,400)}); // Note: doesn't actually exist
        assert_eq!(get_ko_tsumo(20,2), RankedScore { rank: ScoreRank::Normal { fu: 20, han: 2 }, score: Score::KoTsumo(400,700)}); // Note: pinfu tsumo
        assert_eq!(get_ko_tsumo(20,3), RankedScore { rank: ScoreRank::Normal { fu: 20, han: 3 }, score: Score::KoTsumo(700,1300)});
        assert_eq!(get_ko_tsumo(20,4), RankedScore { rank: ScoreRank::Normal { fu: 20, han: 4 }, score: Score::KoTsumo(1300,2600)});
        assert_eq!(get_ko_tsumo(20,5), RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)});
        assert_eq!(get_ko_tsumo(20,6), RankedScore { rank: ScoreRank::Haneman, score: Score::KoTsumo(3000,6000)});
        assert_eq!(get_ko_tsumo(30,1), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 1 }, score: Score::KoTsumo(300,500)}); // Note: naki tanyao
        assert_eq!(get_ko_tsumo(30,2), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 2 }, score: Score::KoTsumo(500,1000)});
        assert_eq!(get_ko_tsumo(30,3), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 3 }, score: Score::KoTsumo(1000,2000)});
        assert_eq!(get_ko_tsumo(30,4), RankedScore { rank: ScoreRank::Normal { fu: 30, han: 4 }, score: Score::KoTsumo(2000,3900)});
        assert_eq!(get_ko_tsumo(30,5), RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)});
        assert_eq!(get_ko_tsumo(30,6), RankedScore { rank: ScoreRank::Haneman, score: Score::KoTsumo(3000,6000)});
        assert_eq!(get_ko_tsumo(40,1), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 1 }, score: Score::KoTsumo(400,700)});
        assert_eq!(get_ko_tsumo(40,2), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 2 }, score: Score::KoTsumo(700,1300)});
        assert_eq!(get_ko_tsumo(40,3), RankedScore { rank: ScoreRank::Normal { fu: 40, han: 3 }, score: Score::KoTsumo(1300,2600)});
        assert_eq!(get_ko_tsumo(40,4), RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)});
        assert_eq!(get_ko_tsumo(40,5), RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)});
        assert_eq!(get_ko_tsumo(40,6), RankedScore { rank: ScoreRank::Haneman, score: Score::KoTsumo(3000,6000)});
        assert_eq!(get_ko_tsumo(25,1), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 1 }, score: Score::KoTsumo(200,400)}); // Note: doesn't actually exist
        assert_eq!(get_ko_tsumo(25,2), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 2 }, score: Score::KoTsumo(400,800)});
        assert_eq!(get_ko_tsumo(25,3), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 3 }, score: Score::KoTsumo(800,1600)});
        assert_eq!(get_ko_tsumo(25,4), RankedScore { rank: ScoreRank::Normal { fu: 25, han: 4 }, score: Score::KoTsumo(1600,3200)});
        assert_eq!(get_ko_tsumo(25,5), RankedScore { rank: ScoreRank::Mangan, score: Score::KoTsumo(2000,4000)});
        assert_eq!(get_ko_tsumo(25,6), RankedScore { rank: ScoreRank::Haneman, score: Score::KoTsumo(3000,6000)});
    }
}
