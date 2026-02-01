use crate::score::*;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use std::fmt;

pub type GamePoint = i32;

pub struct InvalidTileNumberError;
pub struct InvalidYakuFormatError;
pub struct InvalidExtraRyuukyokuReasonError;

/// Represents a tile.
///
/// ```
/// 11...19 萬子
/// 21...29 筒子
/// 31...39 索子
/// 41...47 東南西北白発中
/// 51      赤5萬
/// 52      赤5筒
/// 53      赤5索
/// ```
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub struct Tile(u8);

/// Represents the relative direction of a player based on the current player’s perspective.
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub enum Direction {
    #[default]
    SelfSeat,
    Kamicha,
    Toimen,
    Shimocha,
}

/// Represents a tile obtained by Tsumo or a call (meld).
#[derive(Debug, PartialEq)]
pub enum IncomingTile {
    Tsumo(Tile),
    Chii { combination: (Tile, Tile, Tile) },
    Pon { combination: (Tile, Tile, Tile), dir: Direction },
    Daiminkan { combination: (Tile, Tile, Tile, Tile), dir: Direction },
}

/// Represents a tile discarded or used in an Ankan (closed Kan) or Kakan (added Kan).
#[derive(Debug, PartialEq)]
pub enum OutgoingTile {
    /// The discarded from the hand.
    Discard(Tile),

    /// Riichi declared with a tile discarded from the hand.
    Riichi(Tile),

    /// Represents an Ankan (closed Kan).
    ///
    /// It is likely that when an Ankan (closed Kan) is made with a 5,
    /// the red 5 is always specified (though this is not certain).
    Ankan(Tile),

    /// Represents and Kakan (added Kan).
    Kakan { combination: (Tile, Tile, Tile), dir: Direction, added: Tile },

    /// Discarding the drawn tile.
    Tsumogiri,

    /// Declaring Riichi while discarding the drawn tile.
    TsumogiriRiichi,

    /// Dummy tile.
    ///
    /// When daiminkan, add dummy(Tile(0)) to align the index.
    Dummy,
}

/// Represents the initial settings for each round.
#[derive(Debug, Default, PartialEq)]
pub struct RoundSettings {
    pub kyoku: u8,
    pub honba: u8,
    pub kyoutaku: u8,
    pub points: Vec<GamePoint>,
    pub dora: Vec<Tile>,
    pub ura_dora: Vec<Tile>,
}

/// Represents the number of Han for Yakus or the count of Yakuman.
#[derive(Debug, PartialEq)]
pub enum YakuLevel {
    Normal(u8),
    Yakuman(u8),
}

/// Pair of Yaku and its Han value.
#[derive(Debug, PartialEq)]
pub struct YakuPair {
    pub yaku: Yaku,
    pub level: YakuLevel,
}

/// Represents the winning information of a single player.
#[derive(Debug, Default, PartialEq)]
pub struct Agari {
    pub delta_points: Vec<GamePoint>,
    pub who: u8,
    pub from_who: u8,
    pub pao_who: u8,
    pub ranked_score: RankedScore,
    pub yaku: Vec<YakuPair>,
}

/// Represents the reason for a drawn game.
#[derive(Debug, Default, PartialEq)]
pub enum ExtraRyuukyokuReason {
    #[default]
    Ryuukyoku,
    KyuusyuKyuuhai,
    SuuchaRiichi,
    SanchaHoura,
    SuukanSanra,
    SuufuuRenda,
    NagashiMangan,
    TenpaiEverybody,
    TenpaiNobody,
}

const YAKU_NAME: [&str; 55] = [
    // 一飜
    "門前清自摸和",
    "立直",
    "一発",
    "槍槓",
    "嶺上開花",
    "海底摸月",
    "河底撈魚",
    "平和",
    "断幺九",
    "一盃口",
    "自風 東",
    "自風 南",
    "自風 西",
    "自風 北",
    "場風 東",
    "場風 南",
    "場風 西",
    "場風 北",
    "役牌 白",
    "役牌 發",
    "役牌 中",
    // 二飜
    "両立直",
    "七対子",
    "混全帯幺九",
    "一気通貫",
    "三色同順",
    "三色同刻",
    "三槓子",
    "対々和",
    "三暗刻",
    "小三元",
    "混老頭",
    // 三飜
    "二盃口",
    "純全帯幺九",
    "混一色",
    // 六飜
    "清一色",
    // 満貫
    "人和",
    // 役満
    "天和",
    "地和",
    "大三元",
    "四暗刻",
    "四暗刻単騎",
    "字一色",
    "緑一色",
    "清老頭",
    "九蓮宝燈",
    "純正九蓮宝燈",
    "国士無双",
    "国士無双１３面",
    "大四喜",
    "小四喜",
    "四槓子",
    // ドラ
    "ドラ",
    "裏ドラ",
    "赤ドラ",
];

/// Represents a Yaku (winning hand combination).
#[repr(u8)]
#[derive(Debug, Default, PartialEq, Clone, Copy, FromPrimitive)]
pub enum Yaku {
    #[default]
    MenzenTsumo,
    Riichi,
    Ippatsu,
    Chankan,
    Rinshankaihou,
    HaiteiTsumo,
    HouteiRon,
    Pinfu,
    Tanyao,
    Iipeikou,
    PlayerWindTon,
    PlayerWindNan,
    PlayerWindSha,
    PlayerWindPei,
    FieldWindTon,
    FieldWindNan,
    FieldWindSha,
    FieldWindPei,
    YakuhaiHaku,
    YakuhaiHatsu,
    YakuhaiChun,
    DoubleRiichi,
    Chiitoitsu,
    Chanta,
    Ikkitsuukan,
    SansyokuDoujun,
    SanshokuDoukou,
    Sankantsu,
    Toitoi,
    Sanannkou,
    Shousangen,
    Honroutou,
    Ryanpeikou,
    Junchan,
    Honiisou,
    Chiniisou,
    Renhou,
    Tenhou,
    Chiihou,
    Daisangen,
    Suuankou,
    SuuankouTanki,
    Tsuuiisou,
    Ryuuiisou,
    Chinroutou,
    Tyuurenpoutou,
    Tyuurenpoutou9,
    Kokushimusou,
    Kokushimusou13,
    Daisuushii,
    Syousuushii,
    Suukantsu,
    Dora,
    UraDora,
    AkaDora,
}

/// Represents information at the end of a round.
#[derive(Debug, PartialEq)]
pub enum RoundResult {
    Agari { agari_vec: Vec<Agari> },
    Ryuukyoku { reason: ExtraRyuukyokuReason, delta_points: Vec<GamePoint> },
}

/// Represents the rules for the entire match.
#[derive(Debug, Default, PartialEq)]
pub struct Rule {
    pub disp: String,
    pub aka53: bool,
    pub aka52: bool,
    pub aka51: bool,
}

/// Information for each player.
#[derive(Debug, Default, PartialEq)]
pub struct RoundPlayer {
    pub hand: Vec<Tile>,
    pub incoming: Vec<IncomingTile>,
    pub outgoing: Vec<OutgoingTile>,
}

/// Round information.
#[derive(Debug, Default, PartialEq)]
pub struct Round {
    pub settings: RoundSettings,
    pub players: Vec<RoundPlayer>,
    pub result: RoundResult,
}

/// Reconnection and disconnection information.
#[derive(Debug, Default, PartialEq)]
pub struct Connection {
    pub what: u8,

    /// round number.
    ///
    /// -1 if before first INIT
    pub log: i8,

    pub who: u8,
    pub step: u32,
}

/// Represents tenhou-json.
#[derive(Debug, Default, PartialEq)]
pub struct TenhouJson {
    pub ver: f64,
    pub reference: String,
    pub rounds: Vec<Round>,
    pub connections: Vec<Connection>,
    pub ratingc: String,
    pub rule: Rule,
    pub lobby: u32,
    pub dan: Vec<String>,
    pub rate: Vec<f64>,
    pub sx: Vec<String>,
    pub final_points: Vec<GamePoint>,
    pub final_results: Vec<f64>,
    pub names: Vec<String>,
}

impl Tile {
    pub fn from_u8(x: u8) -> Result<Self, InvalidTileNumberError> {
        if is_valid_tile(x) {
            Ok(Tile(x))
        } else {
            Err(InvalidTileNumberError)
        }
    }

    pub fn to_u8(&self) -> u8 {
        self.0
    }

    pub fn is_red(&self) -> bool {
        self.0 == 51 || self.0 == 52 || self.0 == 53
    }

    pub fn to_black(&self) -> Tile {
        match self.0 {
            51 => Tile(15),
            52 => Tile(25),
            53 => Tile(35),
            _ => *self,
        }
    }

    pub fn to_red(&self) -> Tile {
        match self.0 {
            15 => Tile(51),
            25 => Tile(52),
            35 => Tile(53),
            _ => *self,
        }
    }
}

impl YakuLevel {
    pub fn get_number(&self) -> u8 {
        match self {
            YakuLevel::Normal(x) => *x,
            YakuLevel::Yakuman(x) => *x,
        }
    }
}

impl Yaku {
    pub fn to_str(&self) -> &str {
        YAKU_NAME[*self as usize]
    }
}

impl ExtraRyuukyokuReason {
    pub fn to_str(&self) -> &str {
        match self {
            ExtraRyuukyokuReason::Ryuukyoku => "流局",
            ExtraRyuukyokuReason::KyuusyuKyuuhai => "九種九牌",
            ExtraRyuukyokuReason::SuuchaRiichi => "四家立直",
            ExtraRyuukyokuReason::SanchaHoura => "三家和了",
            ExtraRyuukyokuReason::SuukanSanra => "四槓散了",
            ExtraRyuukyokuReason::SuufuuRenda => "四風連打",
            ExtraRyuukyokuReason::NagashiMangan => "流し満貫",
            ExtraRyuukyokuReason::TenpaiEverybody => "全員聴牌",
            ExtraRyuukyokuReason::TenpaiNobody => "全員不聴",
        }
    }
}

impl fmt::Display for Yaku {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.to_str())
    }
}

impl fmt::Display for YakuLevel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            YakuLevel::Normal(x) => write!(f, "{}飜", x),
            YakuLevel::Yakuman(_) => write!(f, "役満"),
        }
    }
}

impl fmt::Display for YakuPair {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}({})", self.yaku, self.level)
    }
}

impl std::str::FromStr for Yaku {
    type Err = InvalidYakuFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(pos) = YAKU_NAME.iter().position(|name| *name == s) {
            Ok(Yaku::from_u8(pos as u8).unwrap())
        } else {
            Err(InvalidYakuFormatError)
        }
    }
}

impl std::str::FromStr for YakuLevel {
    type Err = InvalidYakuFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "役満" {
            Ok(YakuLevel::Yakuman(1))
        } else if let Ok(n) = s.trim_end_matches('飜').parse() {
            Ok(YakuLevel::Normal(n))
        } else {
            Err(InvalidYakuFormatError)
        }
    }
}

impl std::str::FromStr for YakuPair {
    type Err = InvalidYakuFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let start = s.find('(').ok_or(InvalidYakuFormatError)?;
        let end = s.find(')').ok_or(InvalidYakuFormatError)?;
        let yaku = Yaku::from_str(&s[..start])?;
        let level = YakuLevel::from_str(&s[start + 1..end])?;
        Ok(YakuPair { yaku, level })
    }
}

impl std::str::FromStr for ExtraRyuukyokuReason {
    type Err = InvalidExtraRyuukyokuReasonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "流局" => Ok(ExtraRyuukyokuReason::Ryuukyoku),
            "九種九牌" => Ok(ExtraRyuukyokuReason::KyuusyuKyuuhai),
            "四家立直" => Ok(ExtraRyuukyokuReason::SuuchaRiichi),
            "三家和了" => Ok(ExtraRyuukyokuReason::SanchaHoura),
            "四槓散了" => Ok(ExtraRyuukyokuReason::SuukanSanra),
            "四風連打" => Ok(ExtraRyuukyokuReason::SuufuuRenda),
            "流し満貫" => Ok(ExtraRyuukyokuReason::NagashiMangan),
            "全員聴牌" => Ok(ExtraRyuukyokuReason::TenpaiEverybody),
            "全員不聴" => Ok(ExtraRyuukyokuReason::TenpaiNobody),
            _ => Err(InvalidExtraRyuukyokuReasonError),
        }
    }
}

impl Default for RoundResult {
    fn default() -> Self {
        RoundResult::Ryuukyoku {
            reason: ExtraRyuukyokuReason::Ryuukyoku,
            delta_points: Vec::new(),
        }
    }
}

fn is_valid_tile(x: u8) -> bool {
    match x {
        x if (11..=19).contains(&x) => true, // m
        x if (21..=29).contains(&x) => true, // p
        x if (31..=39).contains(&x) => true, // s
        x if (41..=47).contains(&x) => true, // z
        x if (51..=53).contains(&x) => true, // red 5
        _ => false,
    }
}
