//! # model
//!
//! Represents mjlog XML
//!
//! # Reference
//!
//! <https://m77.hatenablog.com/entry/2017/05/21/214529>

use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use serde_derive::{Serialize, Deserialize};
use thiserror::Error;

/// Occurs when there is no corresponding identifier.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Invalid hai number")]
    InvalidHaiNumber,
    #[error("Invalid player number")]
    InvalidPlayerNumber,
    #[error("Invalid tenhou rank")]
    InvalidTenhouRank,
    #[error("Invalid extra ryuukyoku reason")]
    InvalidExtraRyuukyokuReason,
}

/// Represents tiles numbered from 0 to 135.
///
/// When red 5 is enabled, it is assigned to the tile where mod 4 == 0. (16,52,88)
///
/// ```
/// order:
/// 1111..0555..9999m 1111..0555..9999p 1111..0555..9999s 1111..7777z
/// (0m == red 5m)
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Hai(u8);

/// Player index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Player(u8);

/// GamePoint represents each player's score, which usually starts at 25,000 or 30,000.
pub type GamePoint = i32;

/// Represents the relative direction of a player based on the current player’s perspective.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, FromPrimitive)]
pub enum Direction {
    /// 自分(Self)
    #[default]
    SelfSeat,
    /// 下家(Right)
    Shimocha,
    /// 対面(Across)
    Toimen,
    /// 上家(Left)
    Kamicha,
}

/// Represents the room type in Tenhou.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, FromPrimitive)]
pub enum TenhouRoom {
    /// 一般卓
    #[default]
    Ippan,
    /// 上級卓
    Joukyu,
    /// 特上卓
    Tokujou,
    /// 鳳凰卓
    Houou,
}

/// Represents the rank type in Tenhou.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, FromPrimitive)]
pub enum TenhouRank {
    #[default]
    Newcomer,
    Kyu9,
    Kyu8,
    Kyu7,
    Kyu6,
    Kyu5,
    Kyu4,
    Kyu3,
    Kyu2,
    Kyu1,
    Dan1,
    Dan2,
    Dan3,
    Dan4,
    Dan5,
    Dan6,
    Dan7,
    Dan8,
    Dan9,
    Dan10,
    Tenhou,
}

/// Game settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GameSettings {
    pub vs_human: bool,
    pub no_red: bool,
    pub no_kuitan: bool,
    pub hanchan: bool,
    pub sanma: bool,
    pub soku: bool,
    pub room: TenhouRoom,
}

/// Represents the initial settings for each round.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InitSeed {
    pub kyoku: u8,
    pub honba: u8,
    pub kyoutaku: u8,
    pub dice: (u8, u8),
    pub dora_hyouji: Hai,
}

/// Represents the details of a call (meld).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Meld {
    Chii {
        combination: (Hai, Hai, Hai),
        // 0 == min, 1 == mid, 2 == max
        called_position: u8,
    },
    Pon {
        dir: Direction,
        combination: (Hai, Hai, Hai),
        called: Hai,
        unused: Hai,
    },
    // Almost same as pon
    Kakan {
        dir: Direction,
        combination: (Hai, Hai, Hai),
        called: Hai,
        added: Hai,
    },
    Daiminkan {
        dir: Direction,
        hai: Hai,
    },
    Ankan {
        hai: Hai,
    },
}

/// Represents special draw conditions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum ExtraRyuukyokuReason {
    /// 九種九牌
    #[default]
    KyuusyuKyuuhai,
    /// 四家立直
    SuuchaRiichi,
    /// 三家和了
    SanchaHoura,
    /// 四槓散了
    SuukanSanra,
    /// 四風連打
    SuufuuRenda,
    /// 流し満貫
    NagashiMangan,
}

/// Represents the winning hand rank, such as Mangan.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, FromPrimitive)]
pub enum ScoreRank {
    #[default]
    Normal,
    /// 満貫
    Mangan,
    /// 跳満
    Haneman,
    /// 倍満
    Baiman,
    /// 三倍満
    Sanbaiman,
    /// 役満
    Yakuman,
}

/// Represents the name of a Yaku (winning hand combination).
#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, FromPrimitive)]
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

/// Corresponds to the AGARI tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionAGARI {
    /// Bonus points for consecutive draws or dealer wins.
    pub honba: u8,

    /// Deposit points for unresolved Riichi bets.
    pub kyoutaku: u8,

    /// The hand at the time of winning. Melds are not included, but the winning tile is included.
    pub hai: Vec<Hai>,

    /// Vector representing call (meld) information.
    pub m: Vec<Meld>,

    /// Winning tiles at the time of completion.
    pub machi: Hai,

    /// Hand value points in mahjong scoring, e.g. 20, 30.
    pub fu: u8,

    /// Total winning points.
    pub net_score: u32,

    /// Detailed information on winning points.
    pub score_rank: ScoreRank,

    /// List of winning Yakus (hand combinations).
    ///
    /// ```yaku``` is valid for normal winning hands, in which case ```yakuman``` will be empty.
    pub yaku: Vec<(Yaku, u8)>,

    /// List of Yakuman.
    ///
    /// ```yakuman``` is valid when achieving a Yakuman hand, in which case ```yaku``` will be empty.
    pub yakuman: Vec<Yaku>,

    /// Dora indicator.
    pub dora_hai: Vec<Hai>,

    /// Ura-dora indicator.
    ///
    /// The ura-dora is only specified when a riichi declaration has been made.
    /// In the XML, if its size is 0 the field itself is not output;
    /// however, since this essentially conveys the same meaning, it is not treated as an Option.
    pub dora_hai_ura: Vec<Hai>,

    /// Winning player number.
    pub who: Player,

    /// Player number from whom the win was claimed.
    ///
    /// If won by Tsumo, it will be the player's own number.
    pub from_who: Player,

    /// Player number responsible for the payment (if applicable).
    ///
    /// If there is no player responsible for the payment, the winning player's number will be used.
    pub pao_who: Option<Player>,

    /// Each player's score before the win occurred.
    pub before_points: Vec<GamePoint>,

    /// Point changes due to the win, including the effects of Kyotaku and Honba.
    pub delta_points: Vec<GamePoint>,

    /// Final results at the end of the game.
    ///
    /// If there are remaining rounds, this will be ```None```.
    pub owari: Option<(Vec<GamePoint>, Vec<f64>)>,
}

/// Corresponds to the RYUUKYOKU tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRYUUKYOKU {
    /// Bonus points for consecutive draws or dealer wins.
    pub honba: u8,

    /// Deposit points for unresolved Riichi bets.
    pub kyoutaku: u8,

    /// Each player's score before the win occurred.
    pub before_points: Vec<GamePoint>,

    /// Point changes due to the win, including the effects of Kyotaku and Honba.
    pub delta_points: Vec<GamePoint>,

    /// Hands at the time of a drawn game.
    pub hai0: Option<Vec<Hai>>,

    /// Hands at the time of a drawn game.
    pub hai1: Option<Vec<Hai>>,

    /// Hands at the time of a drawn game.
    pub hai2: Option<Vec<Hai>>,

    /// Hands at the time of a drawn game.
    pub hai3: Option<Vec<Hai>>,

    /// Represents special draw conditions.
    ///
    /// In the case of a normal draw, this will be ```None```.
    pub reason: Option<ExtraRyuukyokuReason>,

    /// Final results at the end of the game.
    ///
    /// If there are remaining rounds, this will be ```None```.
    pub owari: Option<(Vec<GamePoint>, Vec<f64>)>,
}

/// Corresponds to the SHUFFLE tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSHUFFLE {
    pub seed: String,
}

/// Corresponds to the GO tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionGO {
    /// In the original XML, this is named "type", but it has been chaned to avoid conflicts with Rust reserved keywords.
    pub settings: GameSettings,
    pub lobby: u32,
}

/// Corresponds to initial state of the UN tag.
///
/// In the original XML, the initial state and reconnection share the UN tag.
/// However, since user utilize them differently, they are intentionally separated into two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionUN1 {
    pub names: Vec<String>,
    pub dan: Vec<TenhouRank>,
    pub rate: Vec<f64>,
    pub sx: Vec<String>,
}

/// Corresponds to the UN tag in the case of reconnection.
///
/// In the original XML, it is expressed as options from n0 to n3, but since that is confusing, it has been reorganized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionUN2 {
    pub who: Player,
    pub name: String,
}

/// Corresponds to the BYE tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionBYE {
    pub who: Player,
}

/// Corresponds to the TAIKYOKU tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTAIKYOKU {
    pub oya: Player,
}

/// Corresponds to the INIT tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionINIT {
    pub seed: InitSeed,
    pub ten: Vec<GamePoint>,
    pub oya: Player,
    pub hai: Vec<Vec<Hai>>,
}

/// Corresponds to the REACH tag in case of declaration (step 1).
///
/// For REACH, although there is a single tag covering both step 1 and step 2,
/// we split the enum into two since they are usually handled separately.
/// At step 1, a riichi declaration is made.
/// Afterwards, a tile is discarded, and if no ron occurs, step is set to 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionREACH1 {
    pub who: Player,
}

/// Corresponds to the REACH tag after a tile is discarded (step 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionREACH2 {
    pub who: Player,
    pub ten: Vec<GamePoint>,
}

/// Corresponds to the N tag, represents a call (meld).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionN {
    pub who: Player,
    pub m: Meld,
}

/// Corresponds to the DORA tag, represents a new Dora indicator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDORA {
    pub hai: Hai,
}

/// Corresponds to the T, U, V, and W tag.
///
/// Tsumo actions are represented by the T, U, V, and W tags,
/// but since they share common properties, they are unified into a single structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDRAW {
    pub who: Player,
    pub hai: Hai,
}

/// Corresponds to the D, E, F, and G tag.
///
/// Discard actions are represented by the D, E, F, and G tags,
/// but since they share common properties, they are unified into a single structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDISCARD {
    pub who: Player,
    pub hai: Hai,
}

/// Corresponds to each tag within ```mgloggm```.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    SHUFFLE(ActionSHUFFLE),
    GO(ActionGO),
    UN1(ActionUN1),
    UN2(ActionUN2),
    BYE(ActionBYE),
    TAIKYOKU(ActionTAIKYOKU),
    INIT(ActionINIT),
    REACH1(ActionREACH1),
    REACH2(ActionREACH2),
    N(ActionN),
    DORA(ActionDORA),
    AGARI(ActionAGARI),
    RYUUKYOKU(ActionRYUUKYOKU),
    DRAW(ActionDRAW),
    DISCARD(ActionDISCARD),
}

/// Corresponds to the entire mjloggm tag.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Mjlog {
    pub ver: f64,
    pub actions: Vec<Action>,
}

impl Hai {
    pub fn new(x: u8) -> Hai {
        Hai(x)
    }

    pub fn to_u8(&self) -> u8 {
        self.0
    }

    pub fn is_number5(&self) -> bool {
        // 123456789m123456789p123456789s1234567z
        let pict_index = self.0 / 4;
        // mpsz
        let pict_type = pict_index / 9;
        let number = (pict_index % 9) + 1;
        pict_type <= 2 && number == 5
    }
}

impl Player {
    pub fn new(x: u8) -> Self {
        Player(x)
    }

    pub fn to_u8(&self) -> u8 {
        self.0
    }
}

impl ActionAGARI {
    pub fn is_tsumo(&self) -> bool {
        self.who == self.from_who
    }
}

impl Action {
    pub fn as_shuffle(&self) -> Option<&ActionSHUFFLE> {
        match self {
            Action::SHUFFLE(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_go(&self) -> Option<&ActionGO> {
        match self {
            Action::GO(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_un1(&self) -> Option<&ActionUN1> {
        match self {
            Action::UN1(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_un2(&self) -> Option<&ActionUN2> {
        match self {
            Action::UN2(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_bye(&self) -> Option<&ActionBYE> {
        match self {
            Action::BYE(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_taikyoku(&self) -> Option<&ActionTAIKYOKU> {
        match self {
            Action::TAIKYOKU(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_init(&self) -> Option<&ActionINIT> {
        match self {
            Action::INIT(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_reach1(&self) -> Option<&ActionREACH1> {
        match self {
            Action::REACH1(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_reach2(&self) -> Option<&ActionREACH2> {
        match self {
            Action::REACH2(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_n(&self) -> Option<&ActionN> {
        match self {
            Action::N(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_dora(&self) -> Option<&ActionDORA> {
        match self {
            Action::DORA(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_agari(&self) -> Option<&ActionAGARI> {
        match self {
            Action::AGARI(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_ryuukyoku(&self) -> Option<&ActionRYUUKYOKU> {
        match self {
            Action::RYUUKYOKU(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_draw(&self) -> Option<&ActionDRAW> {
        match self {
            Action::DRAW(x) => Some(x),
            _ => None,
        }
    }

    pub fn as_discard(&self) -> Option<&ActionDISCARD> {
        match self {
            Action::DISCARD(x) => Some(x),
            _ => None,
        }
    }

    pub fn is_shuffle(&self) -> bool {
        self.as_shuffle().is_some()
    }

    pub fn is_go(&self) -> bool {
        self.as_go().is_some()
    }

    pub fn is_un1(&self) -> bool {
        self.as_un1().is_some()
    }

    pub fn is_un2(&self) -> bool {
        self.as_un2().is_some()
    }

    pub fn is_bye(&self) -> bool {
        self.as_bye().is_some()
    }

    pub fn is_taikyoku(&self) -> bool {
        self.as_taikyoku().is_some()
    }

    pub fn is_init(&self) -> bool {
        self.as_init().is_some()
    }

    pub fn is_reach1(&self) -> bool {
        self.as_reach1().is_some()
    }

    pub fn is_reach2(&self) -> bool {
        self.as_reach2().is_some()
    }

    pub fn is_n(&self) -> bool {
        self.as_n().is_some()
    }

    pub fn is_dora(&self) -> bool {
        self.as_dora().is_some()
    }

    pub fn is_agari(&self) -> bool {
        self.as_agari().is_some()
    }

    pub fn is_ryuukyoku(&self) -> bool {
        self.as_ryuukyoku().is_some()
    }

    pub fn is_draw(&self) -> bool {
        self.as_draw().is_some()
    }

    pub fn is_discard(&self) -> bool {
        self.as_discard().is_some()
    }
}

impl std::str::FromStr for Hai {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u8>().map(Hai).map_err(|_| ParseError::InvalidHaiNumber)
    }
}

impl std::str::FromStr for Player {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u8>().map(Player).map_err(|_| ParseError::InvalidPlayerNumber)
    }
}

impl std::str::FromStr for TenhouRank {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rank = s.parse::<u8>().map_err(|_| ParseError::InvalidTenhouRank)?;
        TenhouRank::from_u8(rank).ok_or(ParseError::InvalidTenhouRank)
    }
}

impl std::str::FromStr for ExtraRyuukyokuReason {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "yao9" => ExtraRyuukyokuReason::KyuusyuKyuuhai,
            "reach4" => ExtraRyuukyokuReason::SuuchaRiichi,
            "ron3" => ExtraRyuukyokuReason::SanchaHoura,
            "kan4" => ExtraRyuukyokuReason::SuukanSanra,
            "kaze4" => ExtraRyuukyokuReason::SuufuuRenda,
            "nm" => ExtraRyuukyokuReason::NagashiMangan,
            _ => return Err(ParseError::InvalidExtraRyuukyokuReason),
        })
    }
}

impl Default for Meld {
    fn default() -> Self {
        Meld::Ankan { hai: Hai::default() }
    }
}
