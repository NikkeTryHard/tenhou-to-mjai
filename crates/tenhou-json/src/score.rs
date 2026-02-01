use std::fmt;

pub struct InvalidRankedScoreError;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScoreRank {
    Normal { fu: u8, han: u8 },
    Mangan,
    Haneman,
    Baiman,
    Sanbaiman,
    Yakuman, // includes kazoe yakuman
}

impl Default for ScoreRank {
    fn default() -> Self {
        ScoreRank::Normal { fu: 0, han: 0 }
    }
}

const RANKS: [(&str, ScoreRank); 5] = [("満貫", ScoreRank::Mangan), ("跳満", ScoreRank::Haneman), ("倍満", ScoreRank::Baiman), ("三倍満", ScoreRank::Sanbaiman), ("役満", ScoreRank::Yakuman)];

#[derive(Debug, PartialEq)]
pub enum Score {
    OyaTsumo(i32),
    KoTsumo(i32, i32), // (non-dealer, dealer)
    Ron(i32),
}

impl Default for Score {
    fn default() -> Self {
        Score::Ron(0)
    }
}

#[derive(Debug, PartialEq, Default)]
pub struct RankedScore {
    pub rank: ScoreRank,
    pub score: Score,
}

impl fmt::Display for ScoreRank {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ScoreRank::Normal { fu, han } => write!(f, "{}符{}飜", fu, han),
            ScoreRank::Mangan => write!(f, "満貫"),
            ScoreRank::Haneman => write!(f, "跳満"),
            ScoreRank::Baiman => write!(f, "倍満"),
            ScoreRank::Sanbaiman => write!(f, "三倍満"),
            ScoreRank::Yakuman => write!(f, "役満"),
        }
    }
}

impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Score::OyaTsumo(x) => write!(f, "{}点∀", x),
            Score::KoTsumo(ko, oya) => write!(f, "{}-{}点", ko, oya),
            Score::Ron(x) => write!(f, "{}点", x),
        }
    }
}

impl fmt::Display for RankedScore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", self.rank, self.score)
    }
}

impl std::str::FromStr for RankedScore {
    type Err = InvalidRankedScoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_exact_ranked_score(s).ok_or(InvalidRankedScoreError)
    }
}

fn parse_number<T: std::str::FromStr>(it: &mut std::str::Chars) -> Option<T> {
    let mut num = String::new();
    while let Some(ch) = it.clone().next() {
        if ch.is_ascii_digit() {
            num.push(ch);
            it.next(); // consume
        } else {
            break;
        }
    }
    num.parse().ok()
}

fn parse_symbol(it: &mut std::str::Chars, symbol: &str) -> bool {
    let mut tmp = it.clone();
    for expected in symbol.chars() {
        if tmp.next() != Some(expected) {
            return false;
        }
    }
    *it = tmp; // consume
    true
}

fn parse_rank_normal(it: &mut std::str::Chars) -> Option<ScoreRank> {
    let mut tmp = it.clone();

    let fu = parse_number(&mut tmp)?;
    if !parse_symbol(&mut tmp, "符") {
        return None;
    }

    let han = parse_number(&mut tmp)?;
    if !parse_symbol(&mut tmp, "飜") {
        return None;
    }

    *it = tmp; // consume
    Some(ScoreRank::Normal { fu, han })
}

fn parse_rank_mangan(it: &mut std::str::Chars) -> Option<ScoreRank> {
    let mut tmp = it.clone();

    for (rank_str, rank) in &RANKS {
        if parse_symbol(&mut tmp, rank_str) {
            *it = tmp; // consume
            return Some(*rank);
        }
    }
    None
}

fn parse_rank(it: &mut std::str::Chars) -> Option<ScoreRank> {
    let mut tmp = it.clone();

    if let Some(x) = parse_rank_normal(&mut tmp) {
        *it = tmp; // consume
        return Some(x);
    }

    if let Some(x) = parse_rank_mangan(&mut tmp) {
        *it = tmp; // consume
        return Some(x);
    }

    None
}

fn parse_score(it: &mut std::str::Chars) -> Option<Score> {
    let mut tmp = it.clone();

    let num = parse_number(&mut tmp)?;
    if parse_symbol(&mut tmp, "-") {
        // non-dealer tsumo
        let num2 = parse_number(&mut tmp)?;
        if !parse_symbol(&mut tmp, "点") {
            return None;
        }

        *it = tmp; // consume
        Some(Score::KoTsumo(num, num2))
    } else if parse_symbol(&mut tmp, "点") {
        if parse_symbol(&mut tmp, "∀") {
            // dealer tsumo
            *it = tmp; // consume
            Some(Score::OyaTsumo(num))
        } else {
            // ron
            *it = tmp; // consume
            Some(Score::Ron(num))
        }
    } else {
        None
    }
}

fn parse_ranked_score(it: &mut std::str::Chars) -> Option<RankedScore> {
    Some(RankedScore {
        rank: parse_rank(it)?,
        score: parse_score(it)?,
    })
}

fn parse_exact_ranked_score(s: &str) -> Option<RankedScore> {
    let mut it = s.chars();
    let ret = parse_ranked_score(&mut it)?;
    if it.next().is_none() {
        Some(ret)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::score::*;

    #[test]
    fn test_parse_ron() {
        assert_eq!(
            parse_exact_ranked_score("40符3飜7700点"),
            Some(RankedScore {
                rank: ScoreRank::Normal { fu: 40, han: 3 },
                score: Score::Ron(7700)
            })
        );
        assert_eq!(
            parse_exact_ranked_score("満貫8000点"),
            Some(RankedScore {
                rank: ScoreRank::Mangan,
                score: Score::Ron(8000)
            })
        );
        assert_eq!(parse_exact_ranked_score("40符3飜7700点 "), None);
    }

    #[test]
    fn test_parse_ko_tsumo() {
        assert_eq!(
            parse_exact_ranked_score("30符3飜1000-2000点"),
            Some(RankedScore {
                rank: ScoreRank::Normal { fu: 30, han: 3 },
                score: Score::KoTsumo(1000, 2000)
            })
        );
        assert_eq!(
            parse_exact_ranked_score("跳満3000-6000点"),
            Some(RankedScore {
                rank: ScoreRank::Haneman,
                score: Score::KoTsumo(3000, 6000)
            })
        );
        assert_eq!(parse_exact_ranked_score("30符3飜1000-2000点 "), None);
    }

    #[test]
    fn test_parse_oya_tsumo() {
        assert_eq!(
            parse_exact_ranked_score("30符3飜2000点∀"),
            Some(RankedScore {
                rank: ScoreRank::Normal { fu: 30, han: 3 },
                score: Score::OyaTsumo(2000)
            })
        );
        assert_eq!(
            parse_exact_ranked_score("満貫4000点∀"),
            Some(RankedScore {
                rank: ScoreRank::Mangan,
                score: Score::OyaTsumo(4000)
            })
        );
        assert_eq!(parse_exact_ranked_score("30符3飜2000点∀ "), None);
    }
}
