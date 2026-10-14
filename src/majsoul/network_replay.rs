//! Unity OAuth replay retrieval: one session, sequential requests, no quota retries.
use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::{fs::{File, OpenOptions}, io::{BufRead, BufReader, Write}, path::Path, time::{Duration, Instant}};
use super::{browser_auth::SavedAuth, proto::{FieldIterator, decode_varint}, rpc::MajsoulRpc};

pub struct Config<'a> {
    pub auth: &'a Path,
    pub input: &'a Path,
    pub raw: &'a Path,
    pub output: Option<&'a Path>,
    pub log: Option<&'a Path>,
    pub rank_index: Option<&'a Path>,
    pub endpoint: &'a str,
    pub resource: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    pub origin: &'a str,
    pub route_id: &'a str,
    pub oauth_type: u64,
    pub tag: &'a str,
    pub delay_ms: u64,
    pub limit: usize,
}
fn stamp() -> String { Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true) }
fn varint(buf: &mut Vec<u8>, mut n: u64) {
    while n >= 128 { buf.push((n & 127) as u8 | 128); n >>= 7; }
    buf.push(n.to_le_bytes()[0]);
}
fn number(buf: &mut Vec<u8>, id: u64, n: u64) { varint(buf, id << 3); varint(buf, n); }
fn bytes(buf: &mut Vec<u8>, id: u64, data: &[u8]) {
    varint(buf, (id << 3) | 2); varint(buf, data.len() as u64); buf.extend_from_slice(data);
}
fn field(data: &[u8], id: u32) -> Result<Option<&[u8]>> {
    for f in FieldIterator::new(data) { let f = f?; if f.number == id { return Ok(Some(f.data)); } }
    Ok(None)
}
fn error_code(data: &[u8]) -> Result<u64> {
    let Some(error) = field(data, 1)? else { return Ok(0); };
    let Some(code) = field(error, 1)? else { return Ok(0); };
    Ok(decode_varint(code)?.0)
}
fn log(file: &mut Option<File>, value: &Value) -> Result<()> {
    if let Some(file) = file {
        serde_json::to_writer(&mut *file, value)?; file.write_all(b"\n")?; file.sync_data()?;
    }
    Ok(())
}
fn native_uuid(line: &str) -> Result<String> {
    let value = if line.starts_with('{') {
        let row: Value = serde_json::from_str(line)?;
        row["paipu"].as_str().or_else(|| row["uuid"].as_str()).context("JSON row requires paipu or uuid")?.to_owned()
    } else { line.to_owned() };
    let value = value.split_once("paipu=").map_or(value.as_str(), |(_, tail)| tail);
    let value = value.split(['&', '#']).next().unwrap_or(value);
    let value = value.split_once("_a").map_or(value, |(id, _)| id);
    let bare = if value.len() == 43 && value.as_bytes()[..6].iter().all(u8::is_ascii_digit) && value.as_bytes()[6] == b'-' { &value[7..] } else { value };
    uuid::Uuid::parse_str(bare).context("A full UUID is required; short Koromo IDs cannot be downloaded")?;
    Ok(value.to_owned())
}

pub async fn run(cfg: Config<'_>) -> Result<()> {
    let auth: SavedAuth = serde_json::from_reader(File::open(cfg.auth)?)?;
    anyhow::ensure!(!auth.access_token.is_empty(), "saved token is empty");
    std::fs::create_dir_all(cfg.raw)?;
    if let Some(output) = cfg.output { std::fs::create_dir_all(output)?; }
    let mut last_attempt = None;
    if let Some(path) = cfg.log {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) { std::fs::create_dir_all(parent)?; }
        if path.exists() {
            for line in BufReader::new(File::open(path)?).lines() {
                let row: Value = serde_json::from_str(&line?)?;
                if let Some(time) = row["request_started_at"].as_str() {
                    last_attempt = Some(chrono::DateTime::parse_from_rfc3339(time)?.with_timezone(&Utc));
                }
            }
        }
    }
    let mut trials = cfg.log.map(|path| OpenOptions::new().create(true).append(true).open(path)).transpose()?;
    let session = uuid::Uuid::new_v4().to_string();
    log(&mut trials, &json!({"event":"session_started","session_id":session,"time":stamp(),"endpoint":cfg.endpoint,"resource":cfg.resource,"package":cfg.package,"client_version":cfg.version}))?;
    let rpc = MajsoulRpc::connect(cfg.endpoint, cfg.origin).await?;
    let outcome: Result<()> = async {
        // Unity Web uses route type 1 and seconds, unlike the legacy native handshake.
        let mut route = Vec::new();
        number(&mut route, 2, 1); bytes(&mut route, 3, cfg.route_id.as_bytes());
        number(&mut route, 4, u64::try_from(Utc::now().timestamp())?); bytes(&mut route, 6, b"Web");
        let response = rpc.call(".lq.Route.requestConnection", &route).await?;
        let code = error_code(&response)?;
        anyhow::ensure!(code == 0, "Unity route rejected with code {code}");
        let mut login = Vec::new();
        number(&mut login, 1, cfg.oauth_type); bytes(&mut login, 2, auth.access_token.as_bytes());
        let mut device = Vec::new(); bytes(&mut device, 1, b"pc"); bytes(&mut login, 4, &device);
        let mut version = Vec::new(); bytes(&mut version, 1, cfg.resource.as_bytes()); bytes(&mut version, 2, cfg.package.as_bytes()); bytes(&mut login, 6, &version);
        bytes(&mut login, 10, cfg.version.as_bytes()); bytes(&mut login, 11, cfg.tag.as_bytes());
        let response = rpc.call(".lq.Lobby.oauth2Login", &login).await?;
        login.fill(0);
        let code = error_code(&response)?;
        let account = field(&response, 2)?.map(decode_varint).transpose()?.map(|(n, _)| n);
        log(&mut trials, &json!({"event":"login_finished","session_id":session,"time":stamp(),"account_id":account,"error_code":code}))?;
        anyhow::ensure!(code == 0, "OAuth login rejected with code {code}");
        let mut previous = last_attempt;
        let mut attempted = 0;
        let mut quota = false;
        for line in BufReader::new(File::open(cfg.input)?).lines() {
            let line = line?;
            let line = line.trim(); if line.is_empty() { continue; }
            let uuid = native_uuid(line)?;
            if cfg.raw.join(format!("{uuid}.pb")).exists() { continue; }
            if cfg.limit != 0 && attempted >= cfg.limit { break; }
            let mut heartbeat = Vec::new(); number(&mut heartbeat, 1, 8); number(&mut heartbeat, 3, 11); number(&mut heartbeat, 4, 8);
            rpc.call(".lq.Route.heartbeat", &heartbeat).await?;
            let now = Utc::now(); let start = Instant::now();
            let attempt = uuid::Uuid::new_v4().to_string();
            let base = json!({"session_id":session,"attempt_id":attempt,"account_id":account,"uuid":uuid,"game_date_code":if uuid.len()==43 {Some(&uuid[..6])}else{None},"request_started_at":now.to_rfc3339_opts(SecondsFormat::Millis,true),"seconds_since_previous_attempt":previous.and_then(|p: chrono::DateTime<Utc>| (now-p).to_std().ok()).map(|d| d.as_secs_f64())});
            previous = Some(now);
            let mut row = base.clone(); row["event"] = "request_started".into(); log(&mut trials, &row)?;
            let mut request = Vec::new(); bytes(&mut request, 1, uuid.as_bytes()); bytes(&mut request, 2, cfg.version.as_bytes());
            let result = rpc.call(".lq.Lobby.fetchGameRecord", &request).await;
            row["event"] = "request_finished".into(); row["request_finished_at"] = stamp().into(); row["duration_seconds"] = start.elapsed().as_secs_f64().into();
            attempted += 1;
            match result {
                Err(error) => { row["transport_ok"] = false.into(); log(&mut trials, &row)?; return Err(error.context("replay transport failed")); }
                Ok(mut raw) => {
                    let code = error_code(&raw)?;
                    row["transport_ok"] = true.into(); row["error_code"] = code.into(); row["response_bytes"] = raw.len().into();
                    row["inline_replay_bytes"] = field(&raw,4)?.map_or(0, <[u8]>::len).into();
                    log(&mut trials, &row)?;
                    if code == 540 { quota = true; break; }
                    if code != 0 {
                        tracing::warn!(%uuid, code, "Replay rejected");
                        tokio::time::sleep(Duration::from_millis(cfg.delay_ms)).await;
                        continue;
                    }
                    if field(&raw,4)?.is_none() {
                        let url = std::str::from_utf8(field(&raw,5)?.context("Replay response contains no data")?)?;
                        let data = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?.get(url).send().await?.error_for_status()?.bytes().await?;
                        bytes(&mut raw,4,&data);
                    }
                    let dest = cfg.raw.join(format!("{uuid}.pb"));
                    let temp = cfg.raw.join(format!("{uuid}.pb.part"));
                    std::fs::write(&temp, &raw)?; std::fs::rename(temp,dest)?;
                }
            }
            tokio::time::sleep(Duration::from_millis(cfg.delay_ms)).await;
        }
        let (converted, failed) = if let Some(output) = cfg.output {
            super::convert::convert_raw_files(cfg.raw, output, false, cfg.rank_index)?
        } else { (0, 0) };
        log(&mut trials,&json!({"event":"session_finished","session_id":session,"account_id":account,"time":stamp(),"attempted":attempted,"quota_limited":quota,"converted":converted,"conversion_failed":failed}))?;
        anyhow::ensure!(failed == 0, "{failed} replay conversions failed");
        if quota { anyhow::bail!("Replay retrieval quota exceeded (540); stopped without retries"); }
        Ok(())
    }.await;
    let close = rpc.close().await;
    if outcome.is_err() {
        log(&mut trials, &json!({"event":"session_error","session_id":session,"time":stamp(),"details":"Command failed; inspect error code events and terminal output"}))?;
    }
    outcome?; close?; Ok(())
}
