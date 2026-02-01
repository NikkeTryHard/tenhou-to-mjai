use crate::model::*;
use num_traits::FromPrimitive;
use percent_encoding::percent_decode_str;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MjlogError {
    #[error(transparent)]
    XmlError(#[from] quick_xml::errors::Error),
    #[error(transparent)]
    XmlInvalidAttribute(#[from] quick_xml::events::attributes::AttrError),
    #[error(transparent)]
    ObjectParseError(#[from] ParseError),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Invalid Ryuukyoku Type: {0}")]
    InvalidRyuukyokuReason(String),
    #[error("Invalid tenhou rank: {0}")]
    InvalidTenhouRank(String),
    #[error("Not found attribute: {0}")]
    AttributeNotFound(String),
    #[error("Version not defined")]
    VersionNotDefined,
    #[error("Invalid reach step: {0}")]
    InvalidReachStep(u8),
    #[error("The number of valid names is either one or four. Actual: {0}")]
    InvalidNameNum(usize),
    #[error("Invalid ba length: {0}")]
    InvalidBaLength(usize),
    #[error("Invalid ten length: {0}")]
    InvalidTenLength(usize),
    #[error("Invalid yaku number: {0}")]
    InvalidYakuNum(u8),
    #[error("Invalid agari rank: {0}")]
    InvalidScoreRank(u8),
    #[error("Invalid owari")]
    InvalidOwari,
    #[error("Process instruction is not supported.")]
    UnexpectedPI,
    #[error("CData is not supported.")]
    UnexpectedCData,
    #[error("Text is not supported.")]
    UnexpectedText,
    #[error("Pei nuki is not supported.")]
    UnexpectedPeiNuki,
    #[error("Unexpected eof")]
    UnexpectedEof,
    #[error("Unexpected tag: {0}")]
    UnexpectedTag(String),
}

pub type MjlogResult<T> = Result<T, MjlogError>;

fn get_partition_even_odd<T: Clone>(v: &[T]) -> (Vec<T>, Vec<T>) {
    (v.iter().step_by(2).cloned().collect(), v.iter().skip(1).step_by(2).cloned().collect())
}

fn parse_vec<T: std::str::FromStr>(v: &[String]) -> Result<Vec<T>, T::Err> {
    v.iter().map(|x| x.parse()).collect()
}

fn parse_csv<T: std::str::FromStr>(x: &str) -> Result<Vec<T>, T::Err> {
    x.split(',').map(|x| x.parse()).collect()
}

fn decode_percent_encoding(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().to_string()
}

fn try_get_attribute_str(e: &BytesStart, attr_name: &str) -> MjlogResult<Option<String>> {
    let attr_opt = e.try_get_attribute(attr_name)?;
    if attr_opt.is_none() {
        return Ok(None);
    }
    let attr = attr_opt.unwrap();
    let unescaped_value = attr.unescape_value()?;
    Ok(Some(unescaped_value.to_string()))
}

fn try_get_attribute_value<T: std::str::FromStr>(e: &BytesStart, attr_name: &str) -> MjlogResult<Option<T>> {
    let s_opt = try_get_attribute_str(e, attr_name)?;
    if s_opt.is_none() {
        return Ok(None);
    }
    let s = s_opt.unwrap();
    let value = s.parse::<T>().map_err(|_| MjlogError::ParseError(s))?;
    Ok(Some(value))
}

fn try_get_attribute_csv<T: std::str::FromStr>(e: &BytesStart, attr_name: &str) -> MjlogResult<Option<Vec<T>>> {
    let s_opt = try_get_attribute_str(e, attr_name)?;
    if s_opt.is_none() {
        return Ok(None);
    }
    let s = s_opt.unwrap();
    let csv = parse_csv(&s).map_err(|_| MjlogError::ParseError(s))?;
    Ok(Some(csv))
}

fn get_attribute_str(e: &BytesStart, attr_name: &str) -> MjlogResult<String> {
    try_get_attribute_str(e, attr_name)?.ok_or(MjlogError::AttributeNotFound(attr_name.to_string()))
}

fn get_attribute_value<T: std::str::FromStr>(e: &BytesStart, attr_name: &str) -> MjlogResult<T> {
    try_get_attribute_value(e, attr_name)?.ok_or(MjlogError::AttributeNotFound(attr_name.to_string()))
}

fn get_attribute_csv<T: std::str::FromStr>(e: &BytesStart, attr_name: &str) -> MjlogResult<Vec<T>> {
    try_get_attribute_csv(e, attr_name)?.ok_or(MjlogError::AttributeNotFound(attr_name.to_string()))
}

fn conv_shuffle(e: &BytesStart) -> MjlogResult<Action> {
    let seed = get_attribute_str(e, "seed")?;
    Ok(Action::SHUFFLE(ActionSHUFFLE { seed }))
}

fn conv_go(e: &BytesStart) -> MjlogResult<Action> {
    let t: u32 = get_attribute_value(e, "type")?;
    let lobby = get_attribute_value(e, "lobby")?;
    let room_type_index = (t & 0x20) >> 4 | (t & 0x80) >> 7;

    let settings = GameSettings {
        vs_human: (t & 0x01) != 0,
        no_red: (t & 0x02) != 0,
        no_kuitan: (t & 0x04) != 0,
        hanchan: (t & 0x08) != 0,
        sanma: (t & 0x10) != 0,
        soku: (t & 0x40) != 0,
        room: TenhouRoom::from_u8(room_type_index as u8).unwrap(), // always succeeds because there are enough bits
    };

    Ok(Action::GO(ActionGO { settings, lobby }))
}

fn conv_uv(e: &BytesStart) -> MjlogResult<Action> {
    let names = [
        try_get_attribute_str(e, "n0")?.map(|s| decode_percent_encoding(&s)),
        try_get_attribute_str(e, "n1")?.map(|s| decode_percent_encoding(&s)),
        try_get_attribute_str(e, "n2")?.map(|s| decode_percent_encoding(&s)),
        try_get_attribute_str(e, "n3")?.map(|s| decode_percent_encoding(&s)),
    ];

    let name_num = names.iter().filter(|x| x.is_some()).count();
    if name_num == 4 {
        // In the initial state, all values from n0 to n3 are valid.
        // Even in a three-player game, n3 is an empty string.
        let dan = get_attribute_csv(e, "dan")?;
        let rate = get_attribute_csv(e, "rate")?;
        let sx = get_attribute_csv(e, "sx")?;

        Ok(Action::UN1(ActionUN1 {
            names: names.iter().map(|x| x.clone().unwrap()).collect(),
            dan,
            rate,
            sx,
        }))
    } else if name_num == 1 {
        // When reconnecting, only one of the values among n0 to n3 is valid.
        let who = names.iter().position(|x| x.is_some()).unwrap();
        Ok(Action::UN2(ActionUN2 {
            who: Player::new(who as u8),
            name: names[who].clone().unwrap(),
        }))
    } else {
        Err(MjlogError::InvalidNameNum(name_num))
    }
}

fn conv_bye(e: &BytesStart) -> MjlogResult<Action> {
    let who = get_attribute_value(e, "who")?;

    Ok(Action::BYE(ActionBYE { who }))
}

fn conv_taikyoku(e: &BytesStart) -> MjlogResult<Action> {
    let oya = get_attribute_value(e, "oya")?;

    Ok(Action::TAIKYOKU(ActionTAIKYOKU { oya }))
}

fn conv_init(e: &BytesStart) -> MjlogResult<Action> {
    let seed: Vec<u8> = get_attribute_csv(e, "seed")?;
    let ten = get_attribute_csv(e, "ten")?;
    let oya = get_attribute_value(e, "oya")?;
    let hai0 = get_attribute_csv(e, "hai0")?;
    let hai1 = get_attribute_csv(e, "hai1")?;
    let hai2 = get_attribute_csv(e, "hai2")?;
    let hai3 = get_attribute_csv(e, "hai3")?; // Note: sanma has also hai3, but contains empty string

    Ok(Action::INIT(ActionINIT {
        seed: InitSeed {
            kyoku: seed[0],
            honba: seed[1],
            kyoutaku: seed[2],
            dice: (seed[3], seed[4]),
            dora_hyouji: Hai::new(seed[5]),
        },
        ten,
        oya,
        hai: vec![hai0, hai1, hai2, hai3],
    }))
}

fn conv_reach(e: &BytesStart) -> MjlogResult<Action> {
    let step = get_attribute_value(e, "step")?;
    let who = get_attribute_value(e, "who")?;

    match step {
        1 => Ok(Action::REACH1(ActionREACH1 { who })),
        2 => {
            let ten = get_attribute_csv(e, "ten")?;
            Ok(Action::REACH2(ActionREACH2 { who, ten }))
        }
        _ => Err(MjlogError::InvalidReachStep(step)),
    }
}

fn conv_meld_from_u16(m: u16) -> MjlogResult<Meld> {
    // who called?
    let dir = Direction::from_u8((m & 0x3) as u8).unwrap();

    if m & 0x04 != 0 {
        // Chii
        //
        // * 123/234/345/.../789  (7 patterns)
        // * m/p/s (3 patterns)
        // * min/mid/max (3 patterns)
        //
        // 7*3*3 == 63 (6bits)
        let pattern = ((m & 0xfc00) >> 10) as u8;

        // 0 == min
        // 1 == mid
        // 2 == max
        let called_position = pattern % 3;

        // 0..6 (1..7)
        let min_number = (pattern / 3) % 7;

        // 0 == m
        // 1 == p
        // 2 == s
        let kind = (pattern / 3) / 7;

        // hai offset
        let offset_min = ((m & 0x0018) >> 3) as u8;
        let offset_mid = ((m & 0x0060) >> 5) as u8;
        let offset_max = ((m & 0x0180) >> 7) as u8;

        // result
        let pict_type = kind * 9 + min_number;
        let base = pict_type * 4;
        let h_min = base + offset_min;
        let h_mid = base + offset_mid + 4;
        let h_max = base + offset_max + 8;

        Ok(Meld::Chii {
            combination: (Hai::new(h_min), Hai::new(h_mid), Hai::new(h_max)),
            called_position,
        })
    } else if (m & 0x08 != 0) || (m & 0x10 != 0) {
        // Pon(0x08) / Kakan(0x10)
        //
        // * 111/222/.../999mps (9 pattern * 3)
        // * 111/222/.../777z (7 pattern)
        //
        // (9*3+7)*3 == 102 (7bits)
        let pattern = ((m & 0xfe00) >> 9) as u8;
        let unused_hai_offset = ((m & 0x60) >> 5) as usize;
        let called_index = (pattern % 3) as usize;

        // 1..9m1..9p1..9s1..7z
        let pict_type = pattern / 3;
        let base = pict_type * 4;
        let mut same_pict_hais = [Hai::new(base), Hai::new(base + 1), Hai::new(base + 2), Hai::new(base + 3)];
        same_pict_hais.swap(3, unused_hai_offset);
        let combination = (same_pict_hais[0], same_pict_hais[1], same_pict_hais[2]);
        let unused_hai = same_pict_hais[3];
        let called_hai = same_pict_hais[called_index];

        if m & 0x10 != 0 {
            Ok(Meld::Kakan {
                dir,
                combination,
                called: called_hai,
                added: unused_hai,
            })
        } else {
            Ok(Meld::Pon {
                dir,
                combination,
                called: called_hai,
                unused: unused_hai,
            })
        }
    } else if m & 0x20 != 0 {
        // North(not supported currently)
        return Err(MjlogError::UnexpectedPeiNuki);
    } else {
        // Daiminkan or Ankan
        let hai = Hai::new(((m & 0xff00) >> 8) as u8);
        if dir == Direction::SelfSeat {
            Ok(Meld::Ankan { hai })
        } else {
            Ok(Meld::Daiminkan { dir, hai })
        }
    }
}

fn conv_n(e: &BytesStart) -> MjlogResult<Action> {
    let who = get_attribute_value(e, "who")?;
    let m = get_attribute_value(e, "m")?;
    Ok(Action::N(ActionN { who, m: conv_meld_from_u16(m)? }))
}

fn conv_dora(e: &BytesStart) -> MjlogResult<Action> {
    let hai = get_attribute_value(e, "hai")?;
    Ok(Action::DORA(ActionDORA { hai }))
}

fn conv_owari(e: &BytesStart) -> MjlogResult<Option<(Vec<GamePoint>, Vec<f64>)>> {
    let owari_csv_opt: Option<Vec<String>> = try_get_attribute_csv(e, "owari")?;

    if let Some(owari_csv) = owari_csv_opt {
        let (final_points_str, final_results_str) = get_partition_even_odd(&owari_csv);
        let final_points = parse_vec(&final_points_str).map_err(|_| MjlogError::InvalidOwari)?;
        let final_results = parse_vec(&final_results_str).map_err(|_| MjlogError::InvalidOwari)?;
        Ok(Some((final_points, final_results)))
    } else {
        Ok(None)
    }
}

fn conv_yaku(x: u8) -> MjlogResult<Yaku> {
    Yaku::from_u8(x).ok_or(MjlogError::InvalidYakuNum(x))
}

fn conv_score_rank(x: u8) -> MjlogResult<ScoreRank> {
    ScoreRank::from_u8(x).ok_or(MjlogError::InvalidScoreRank(x))
}

fn conv_yaku_pair(chunk: &[u8]) -> MjlogResult<(Yaku, u8)> {
    assert_eq!(chunk.len(), 2);

    let yaku = Yaku::from_u8(chunk[0]).ok_or(MjlogError::InvalidYakuNum(chunk[0]))?;
    let han = chunk[1];

    Ok((yaku, han))
}

fn conv_agari(e: &BytesStart) -> MjlogResult<Action> {
    let ba = get_attribute_csv(e, "ba")?;
    let hai = get_attribute_csv(e, "hai")?;
    let m_vec: Vec<u16> = try_get_attribute_csv(e, "m")?.unwrap_or(vec![]);
    let machi = get_attribute_value(e, "machi")?;
    let ten: Vec<u32> = get_attribute_csv(e, "ten")?;
    let yaku_vec: Vec<u8> = try_get_attribute_csv(e, "yaku")?.unwrap_or(vec![]);
    let yakuman_vec: Vec<u8> = try_get_attribute_csv(e, "yakuman")?.unwrap_or(vec![]);
    let dora_hai = get_attribute_csv(e, "doraHai")?;
    let dora_hai_ura = try_get_attribute_csv(e, "doraHaiUra")?.unwrap_or(vec![]);
    let who = get_attribute_value(e, "who")?;
    let from_who = get_attribute_value(e, "fromWho")?;
    let pao_who = try_get_attribute_value(e, "paoWho")?;
    let (before_points, delta_points) = get_partition_even_odd(&get_attribute_csv(e, "sc")?);
    let owari = conv_owari(e)?;

    if ba.len() != 2 {
        return Err(MjlogError::InvalidBaLength(ba.len()));
    }

    if ten.len() != 3 {
        return Err(MjlogError::InvalidTenLength(ten.len()));
    }

    let m = m_vec.into_iter().map(conv_meld_from_u16).collect::<MjlogResult<Vec<Meld>>>()?;
    let score_rank = conv_score_rank(ten[2] as u8)?;
    let yaku = yaku_vec.chunks_exact(2).map(conv_yaku_pair).collect::<MjlogResult<Vec<(Yaku, u8)>>>()?;
    let yakuman = yakuman_vec.into_iter().map(conv_yaku).collect::<MjlogResult<Vec<Yaku>>>()?;

    let agari = ActionAGARI {
        honba: ba[0],
        kyoutaku: ba[1],
        hai,
        m,
        machi,
        fu: ten[0] as u8,
        net_score: ten[1],
        score_rank,
        yaku,
        yakuman,
        dora_hai,
        dora_hai_ura,
        who,
        from_who,
        pao_who,
        before_points,
        delta_points,
        owari,
    };

    Ok(Action::AGARI(agari))
}

fn conv_ryuukyoku(e: &BytesStart) -> MjlogResult<Action> {
    let ba = get_attribute_csv(e, "ba")?;
    let hai0 = try_get_attribute_csv(e, "hai0")?;
    let hai1 = try_get_attribute_csv(e, "hai1")?;
    let hai2 = try_get_attribute_csv(e, "hai2")?;
    let hai3 = try_get_attribute_csv(e, "hai3")?;
    let (before_points, delta_points) = get_partition_even_odd(&get_attribute_csv(e, "sc")?);
    let type_str_opt: Option<String> = try_get_attribute_str(e, "type")?;
    let owari = conv_owari(e)?;

    if ba.len() != 2 {
        return Err(MjlogError::InvalidBaLength(ba.len()));
    }

    let ryuukyoku = ActionRYUUKYOKU {
        honba: ba[0],
        kyoutaku: ba[1],
        before_points,
        delta_points,
        hai0,
        hai1,
        hai2,
        hai3,
        reason: type_str_opt.map(|x| x.parse()).transpose()?,
        owari,
    };

    Ok(Action::RYUUKYOKU(ryuukyoku))
}

fn parse_hai_tag(n: &[u8]) -> Option<Action> {
    if n.is_empty() {
        return None;
    }

    let first_char = n[0] as char;
    let index = ['T', 'U', 'V', 'W', 'D', 'E', 'F', 'G'].iter().position(|c| *c == first_char)? as u8;
    let hai_str = std::str::from_utf8(&n[1..]).ok()?;
    let hai = hai_str.parse::<Hai>().ok()?;
    if 136 <= hai.to_u8() {
        return None;
    }

    if index < 4 {
        Some(Action::DRAW(ActionDRAW { who: Player::new(index), hai }))
    } else {
        Some(Action::DISCARD(ActionDISCARD { who: Player::new(index - 4), hai }))
    }
}

fn conv_action(e: &BytesStart) -> MjlogResult<Action> {
    let event = match e.name().as_ref() {
        b"SHUFFLE" => conv_shuffle(e)?,
        b"GO" => conv_go(e)?,
        b"UN" => conv_uv(e)?,
        b"BYE" => conv_bye(e)?,
        b"TAIKYOKU" => conv_taikyoku(e)?,
        b"INIT" => conv_init(e)?,
        b"REACH" => conv_reach(e)?,
        b"N" => conv_n(e)?,
        b"DORA" => conv_dora(e)?,
        b"AGARI" => conv_agari(e)?,
        b"RYUUKYOKU" => conv_ryuukyoku(e)?,
        x => parse_hai_tag(x).ok_or(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string()))?,
    };
    Ok(event)
}

fn conv_mjloggm<R: std::io::BufRead>(reader: &mut Reader<R>, e: &BytesStart) -> MjlogResult<Mjlog> {
    let ver = get_attribute_value(e, "ver")?;

    let mut actions = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Decl(_) => continue,
            Event::DocType(_) => continue,
            Event::Comment(_) => continue,
            Event::Eof => return Err(MjlogError::UnexpectedEof),
            Event::PI(_) => return Err(MjlogError::UnexpectedPI),
            Event::CData(_) => return Err(MjlogError::UnexpectedCData),
            Event::Text(_) => return Err(MjlogError::UnexpectedText),
            Event::Start(e) => return Err(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string())),
            Event::Empty(e) => actions.push(conv_action(&e)?),
            Event::End(e) if e.as_ref() == b"mjloggm" => return Ok(Mjlog { ver, actions }),
            Event::End(e) => return Err(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string())),
        }
    }
}

pub fn parse_mjlogs(text: &str) -> MjlogResult<Vec<Mjlog>> {
    let mut reader = Reader::from_reader(text.as_ref());

    // Ignore spaces for xmllint
    reader.config_mut().trim_text(true);

    // Convert all event types
    let mut mjlogs = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Decl(_) => continue,
            Event::DocType(_) => continue,
            Event::Comment(_) => continue,
            Event::Eof => return Ok(mjlogs),
            Event::PI(_) => return Err(MjlogError::UnexpectedPI),
            Event::CData(_) => return Err(MjlogError::UnexpectedCData),
            Event::Text(_) => return Err(MjlogError::UnexpectedText),
            Event::Start(e) => {
                if e.name().as_ref() != b"mjloggm" {
                    return Err(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string()));
                }

                mjlogs.push(conv_mjloggm(&mut reader, &e)?);
            }
            Event::Empty(e) => return Err(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string())),
            Event::End(e) => return Err(MjlogError::UnexpectedTag(String::from_utf8_lossy(e.name().as_ref()).to_string())),
        }
    }
}
