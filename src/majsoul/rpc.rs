use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, warn};
use uuid::Uuid;

/// Origin URL for CN server WebSocket connections.
pub const CN_ORIGIN: &str = "https://game.maj-soul.com";
/// Origin URL for EN/JP server WebSocket connections.
pub const EN_ORIGIN: &str = "https://mahjongsoul.game.yo-star.com";

/// Per-server Origin header value for Majsoul WebSocket connections.
// The gateway validates the WS Origin against its own host; sending the wrong server's Origin fails the handshake.
pub fn origin_for_server(server: &str) -> &'static str {
    match server {
        "cn" => CN_ORIGIN,
        _ => EN_ORIGIN,
    }
}

/// Classified Majsoul RPC error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MajsoulError {
    /// Server is rate-limiting us; caller should back off / lower rate.
    RateLimited,
    /// Client version rejected; caller should rediscover gateway and retry (bounded).
    VersionMismatch,
    /// Any other fatal server error code.
    Fatal(i64),
}

/// Classify a numeric Majsoul error code.
///
/// Code 151 is the version-mismatch code (gateway rediscovery path).
/// All other non-zero codes are treated as fatal except for the known
/// rate-limit code 103 (too many requests), which maps to [`MajsoulError::RateLimited`].
pub fn classify_error(code: i64) -> MajsoulError {
    match code {
        151 => MajsoulError::VersionMismatch,
        // Observed rate-limit code on Lobby endpoints.
        103 => MajsoulError::RateLimited,
        n => MajsoulError::Fatal(n),
    }
}

/// Extract the numeric error code from a `fetchGameRecord error {code}: {uuid}`
/// style message. Prefers the `error {code}` token; falls back to the first
/// standalone digit run so messages like `"oops 2151"` parse as 2151.
/// Returns `None` when no numeric code is present. Classification is always
/// numeric, so `"2151"` maps to `Fatal(2151)`, never the 151 path.
pub fn parse_error_code_from_message(msg: &str) -> Option<i64> {
    // Look for "error <digits>" token boundary.
    if let Some(idx) = msg.find("error") {
        let rest = msg[idx + "error".len()..].trim_start_matches([' ', ':']);
        let mut digits = String::new();
        for c in rest.chars() {
            if c.is_ascii_digit() || (digits.is_empty() && c == '-') {
                digits.push(c);
            } else {
                break;
            }
        }
        if !digits.is_empty() && digits != "-" {
            if let Ok(n) = digits.parse::<i64>() {
                return Some(n);
            }
        }
    }
    // Fallback: first whitespace-delimited token that is all digits
    // (UUID segments stay glued to dashes/hex, so they never match).
    for tok in msg.split_whitespace() {
        let t = tok.trim_matches(|c: char| !c.is_ascii_digit());
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = t.parse::<i64>() {
                return Some(n);
            }
        }
    }
    None
}

/// Classify an anyhow error produced by [`MajsoulRpc::fetch_game_record`].
/// Returns `None` when the message carries no numeric code.
pub fn classify_fetch_error(err: &anyhow::Error) -> Option<MajsoulError> {
    parse_error_code_from_message(&err.to_string()).map(classify_error)
}

/// In-flight RPC response channel endpoints keyed by request index.
type PendingTx = oneshot::Sender<Result<Vec<u8>, String>>;
type PendingRx = oneshot::Receiver<Result<Vec<u8>, String>>;
type PendingMap = HashMap<u32, PendingTx>;

/// Checked `u64` → `usize` for hostile wire lengths; out-of-range bails
/// (the bounds check after each site stays the real guard on 64-bit).
fn usize_checked(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| anyhow::anyhow!("length out of range: {v}"))
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Simple protobuf Wrapper encoder/decoder
mod wrapper {
    use anyhow::Result;

    pub fn encode(name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: name (string)
        buf.push(0x0a);
        encode_varint(&mut buf, name.len() as u64);
        buf.extend_from_slice(name.as_bytes());
        // Field 2: data (bytes)
        if !data.is_empty() {
            buf.push(0x12);
            encode_varint(&mut buf, data.len() as u64);
            buf.extend_from_slice(data);
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<(String, Vec<u8>)> {
        // Decoded via multi-byte varint tags so field numbers >= 16 work.
        let mut name = String::new();
        let mut data = Vec::new();
        let mut pos = 0;
        while pos < buf.len() {
            let (tag, tag_len) = decode_varint(&buf[pos..])?;
            pos += tag_len;
            let field_num = tag >> 3;
            let wire_type = (tag & 0x07) as u8;
            match (field_num, wire_type) {
                (1, 2) => {
                    let (len, n) = decode_varint(&buf[pos..])?;
                    pos += n;
                    let end = pos + super::usize_checked(len)?;
                    if end > buf.len() {
                        anyhow::bail!("Buffer overflow");
                    }
                    name = String::from_utf8_lossy(&buf[pos..end]).to_string();
                    pos = end;
                }
                (2, 2) => {
                    let (len, n) = decode_varint(&buf[pos..])?;
                    pos += n;
                    let end = pos + super::usize_checked(len)?;
                    if end > buf.len() {
                        anyhow::bail!("Buffer overflow");
                    }
                    data = buf[pos..end].to_vec();
                    pos = end;
                }
                (_, 0) => {
                    let (_, n) = decode_varint(&buf[pos..])?;
                    pos += n;
                }
                (_, 1) => {
                    if pos + 8 > buf.len() {
                        anyhow::bail!("Buffer overflow in fixed64");
                    }
                    pos += 8;
                }
                (_, 2) => {
                    let (len, n) = decode_varint(&buf[pos..])?;
                    pos += n;
                    let end = pos + super::usize_checked(len)?;
                    if end > buf.len() {
                        anyhow::bail!("Buffer overflow");
                    }
                    pos = end;
                }
                (_, 5) => {
                    if pos + 4 > buf.len() {
                        anyhow::bail!("Buffer overflow in fixed32");
                    }
                    pos += 4;
                }
                _ => anyhow::bail!("Unsupported wire type {wire_type}"),
            }
        }
        Ok((name, data))
    }

    pub fn encode_varint(buf: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    pub fn decode_varint(buf: &[u8]) -> Result<(u64, usize)> {
        let mut value: u64 = 0;
        let mut shift = 0;
        let mut pos = 0;
        loop {
            if pos >= buf.len() {
                anyhow::bail!("Unexpected end in varint");
            }
            let byte = buf[pos];
            pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok((value, pos))
    }
}

mod requests {
    use super::wrapper::encode_varint;

    pub fn fetch_game_record(uuid: &str, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: game_uuid
        encode_string(&mut buf, 1, uuid);
        // Field 2: client_version_string
        encode_string(&mut buf, 2, version);
        buf
    }

    /// Fetch public game record list from ranked rooms
    /// type: 0=all, 1=Bronze, 2=Silver, 3=Gold, 4=Jade, 5=Throne
    pub fn fetch_game_record_list(start: u32, count: u32, room_type: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: start (pagination offset)
        buf.push(0x08);
        encode_varint(&mut buf, u64::from(start));
        // Field 2: count (number of records)
        buf.push(0x10);
        encode_varint(&mut buf, u64::from(count));
        // Field 3: type (room type)
        buf.push(0x18);
        encode_varint(&mut buf, u64::from(room_type));
        buf
    }

    /// Fetch live games list (spectatable)
    pub fn fetch_game_live_list(filter_id: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: filter_id (0 = all, or specific room)
        buf.push(0x08);
        encode_varint(&mut buf, u64::from(filter_id));
        buf
    }

    /// Build `ReqLogin` for CN native login (username/password)
    /// Field numbers from protobuf: account=1, password=2, device=4, `random_key=5`,
    /// `gen_access_token=7`, `currency_platforms=8`, `client_version_string=11`
    pub fn build_login_request(
        account: &str,
        password_hash: &str,
        random_key: &str,
        version: &str,
    ) -> Vec<u8> {
        let mut buf = Vec::new();

        // Field 1: account (string)
        encode_string(&mut buf, 1, account);
        // Field 2: password (string, hashed)
        encode_string(&mut buf, 2, password_hash);
        // Field 4: device { is_browser: true }
        encode_nested_device_simple(&mut buf);
        // Field 5: random_key (string)
        encode_string(&mut buf, 5, random_key);
        // Field 7: gen_access_token (bool = true)
        encode_bool(&mut buf, 7, true);
        // Field 8: currency_platforms (repeated int32 = [2])
        encode_varint_field(&mut buf, 8, 2);
        // Field 11: client_version_string
        encode_string(&mut buf, 11, version);

        buf
    }

    /// Build loginBeat request
    pub fn build_login_beat_request(contract: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_string(&mut buf, 1, contract);
        buf
    }

    fn encode_string(buf: &mut Vec<u8>, field: u32, value: &str) {
        let tag = (u64::from(field) << 3) | 2;
        encode_varint(buf, tag);
        encode_varint(buf, value.len() as u64);
        buf.extend_from_slice(value.as_bytes());
    }

    fn encode_bool(buf: &mut Vec<u8>, field: u32, value: bool) {
        let tag = u64::from(field) << 3;
        encode_varint(buf, tag);
        buf.push(u8::from(value));
    }

    fn encode_varint_field(buf: &mut Vec<u8>, field: u32, value: u64) {
        let tag = u64::from(field) << 3;
        encode_varint(buf, tag);
        encode_varint(buf, value);
    }

    /// Encode device message with just `is_browser` = true (for native login)
    fn encode_nested_device_simple(buf: &mut Vec<u8>) {
        // Field 4: device is_browser=true (field 5); tag (5 << 3) | 0 = 0x28
        let inner = vec![0x28, 0x01];

        let tag = (u64::from(4u32) << 3) | 2;
        encode_varint(buf, tag);
        encode_varint(buf, inner.len() as u64);
        buf.extend(inner);
    }

    /// Build `ReqRequestConnection` for route handshake (required before login)
    /// Field numbers from protobuf: type=2, `route_id=3`, timestamp=4
    pub fn build_request_connection(route_id: &str, timestamp: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 2: type = 3 (varint)
        encode_varint_field(&mut buf, 2, 3);
        // Field 3: route_id (string)
        encode_string(&mut buf, 3, route_id);
        // Field 4: timestamp (varint, milliseconds)
        encode_varint_field(&mut buf, 4, timestamp);
        buf
    }
}

pub struct MajsoulRpc {
    write: Arc<Mutex<futures::stream::SplitSink<WsStream, Message>>>,
    pending: Arc<Mutex<PendingMap>>,
    req_idx: AtomicU32,
    _read_task: tokio::task::JoinHandle<()>,
}

impl MajsoulRpc {
    pub async fn connect(endpoint: &str, origin: &str) -> Result<Self> {
        let mut request = endpoint.into_client_request()?;
        request
            .headers_mut()
            .insert("Origin", origin.parse().context("Invalid origin")?);

        debug!("Connecting to {} (origin {})", endpoint, origin);
        let (ws_stream, _) = connect_async(request)
            .await
            .context("WebSocket connect failed")?;

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(Mutex::new(write));
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));

        let pending_clone = Arc::clone(&pending);
        let read_task = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Binary(data)) if data.len() >= 3 => {
                        if data[0] == 3 {
                            // RESPONSE
                            let idx = u32::from_le_bytes([data[1], data[2], 0, 0]);
                            match wrapper::decode(&data[3..]) {
                                Ok((_, response_data)) => {
                                    let mut pending = pending_clone.lock().await;
                                    if let Some(tx) = pending.remove(&idx) {
                                        let _ = tx.send(Ok(response_data));
                                    } else {
                                        debug!("Response for unknown idx {}", idx);
                                    }
                                }
                                Err(e) => {
                                    warn!("Failed to decode response idx {}: {}", idx, e);
                                    let mut pending = pending_clone.lock().await;
                                    if let Some(tx) = pending.remove(&idx) {
                                        let _ = tx.send(Err(format!("decode failed: {e}")));
                                    }
                                }
                            }
                        } else {
                            // Server-push frame (not a response): log, do not drop silently.
                            debug!("Server-push frame kind {} ({} bytes)", data[0], data.len());
                        }
                    }
                    Ok(Message::Binary(data)) => {
                        debug!("Short binary frame ({} bytes)", data.len());
                    }
                    Ok(Message::Text(text)) => {
                        debug!("Server-push text frame ({} bytes)", text.len());
                    }
                    Ok(Message::Close(_)) => {
                        debug!("WebSocket closed; draining {} pendings", pending_clone.lock().await.len());
                        let mut pending = pending_clone.lock().await;
                        for (_, tx) in pending.drain() {
                            let _ = tx.send(Err("connection closed".to_string()));
                        }
                        break;
                    }
                    Err(e) => {
                        warn!("WebSocket error: {}; draining pendings", e);
                        let mut pending = pending_clone.lock().await;
                        for (_, tx) in pending.drain() {
                            let _ = tx.send(Err("connection closed".to_string()));
                        }
                        break;
                    }
                    _ => {}
                }
            }
        });

        debug!("Connected to Majsoul gateway");
        Ok(Self {
            write,
            pending,
            req_idx: AtomicU32::new(1),
            _read_task: read_task,
        })
    }

    /// Number of in-flight RPCs (test hook).
    /// Kept for the D4 demux invariant (forced timeout leaves `pending` empty);
    /// never called in production, so the dead-code lint is suppressed here.
    #[allow(dead_code)]
    pub async fn pending_len(&self) -> usize {
        self.pending.lock().await.len()
    }

    pub async fn call(&self, method: &str, request_data: &[u8]) -> Result<Vec<u8>> {
        // Allocate a non-zero, currently-unused index (insert-if-absent retry).
        let (idx, rx) = {
            let mut chosen: Option<(u32, PendingRx)> = None;
            for _ in 0..1024 {
                let cand = self.req_idx.fetch_add(1, Ordering::SeqCst);
                // Gateway request index lives in 1..60007 (0 is never a valid in-flight key); wrap + skip 0 keeps the demux map sound.
                let id = cand % 60007;
                if id == 0 {
                    continue;
                }
                let (tx, rx) = oneshot::channel();
                {
                    let mut guard = self.pending.lock().await;
                    if guard.contains_key(&id) {
                        continue;
                    }
                    guard.insert(id, tx);
                }
                chosen = Some((id, rx));
                break;
            }
            match chosen {
                Some(v) => v,
                None => anyhow::bail!("RPC index space exhausted"),
            }
        };
        // `idx` is `cand % 60007`, so it always fits `u16`; the checked
        // conversion only guards future changes to the modulus.
        let idx_bytes = u16::try_from(idx)
            .map_err(|_| anyhow::anyhow!("RPC index out of range: {idx}"))?
            .to_le_bytes();

        let wrapped = wrapper::encode(method, request_data);
        let mut packet = vec![0x02];
        packet.extend_from_slice(&idx_bytes);
        packet.extend_from_slice(&wrapped);

        if let Err(e) = self
            .write
            .lock()
            .await
            .send(Message::Binary(packet.into()))
            .await
        {
            self.pending.lock().await.remove(&idx);
            return Err(e).context("WebSocket send failed");
        }

        debug!("Sent RPC: {} (idx={})", method, idx);

        let response = Self::await_response(&self.pending, idx, rx, std::time::Duration::from_secs(30)).await?;
        Ok(response)
    }

    async fn await_response(
        pending: &Arc<Mutex<PendingMap>>,
        idx: u32,
        rx: PendingRx,
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>> {
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(data))) => Ok(data),
            Ok(Ok(Err(e))) => anyhow::bail!("{e}"),
            Ok(Err(_)) => {
                pending.lock().await.remove(&idx);
                anyhow::bail!("connection closed");
            }
            Err(_) => {
                pending.lock().await.remove(&idx);
                anyhow::bail!("RPC timeout");
            }
        }
    }
    pub async fn fetch_game_record(&self, uuid: &str, version: &str) -> Result<Vec<u8>> {
        let request = requests::fetch_game_record(uuid, version);
        let response = self.call(".lq.Lobby.fetchGameRecord", &request).await?;
        // Check for error: direct (08 XX) or nested (0a LL 08 XX)
        if let Some(code) = Self::extract_error_code(&response) {
            if code != 0 {
                anyhow::bail!("fetchGameRecord error {code}: {uuid}");
            }
        }
        debug!("Fetched game record: {} ({} bytes)", uuid, response.len());
        Ok(response)
    }

    /// Fetch public game list from ranked rooms (Throne, Jade, Gold, etc.)
    /// `room_type`: 0=all, 1=Bronze, 2=Silver, 3=Gold, 4=Jade, 5=Throne
    pub async fn fetch_game_record_list(&self, start: u32, count: u32, room_type: u32) -> Result<Vec<u8>> {
        let request = requests::fetch_game_record_list(start, count, room_type);
        let response = self.call(".lq.Lobby.fetchGameRecordList", &request).await?;
        if let Some(code) = Self::extract_error_code(&response) {
            if code != 0 {
                anyhow::bail!("fetchGameRecordList error {code}");
            }
        }
        debug!("Fetched game record list ({} bytes)", response.len());
        Ok(response)
    }

    /// Fetch live games (spectatable)
    pub async fn fetch_game_live_list(&self, filter_id: u32) -> Result<Vec<u8>> {
        let request = requests::fetch_game_live_list(filter_id);
        let response = self.call(".lq.Lobby.fetchGameLiveList", &request).await?;
        if let Some(code) = Self::extract_error_code(&response) {
            if code != 0 {
                anyhow::bail!("fetchGameLiveList error {code}");
            }
        }
        debug!("Fetched live game list ({} bytes)", response.len());
        Ok(response)
    }

    /// Perform route connection handshake (required before login)
    pub async fn route_connect(&self, route_id: &str) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Current epoch millis always fits `u64`; saturate rather than fail
        // if the clock ever reports otherwise.
        let timestamp = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);

        debug!("Sending route connection (route_id: {}, timestamp: {})", route_id, timestamp);

        let request = requests::build_request_connection(route_id, timestamp);
        let response = self.call(".lq.Route.requestConnection", &request).await?;

        // Check for error via numeric decode (multi-byte safe).
        if let Some(code) = Self::extract_error_code(&response) {
            if code != 0 {
                anyhow::bail!("Route connection failed (error {code})");
            }
        }

        debug!("Route connection established");
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.write.lock().await.close().await?;
        Ok(())
    }

    /// Login with username/password (CN server native auth)
    pub async fn login_native(&self, username: &str, password: &str, version: &str, route_id: &str) -> Result<()> {
        use crate::majsoul::auth::hash_password;

        // Step 1: Route connection handshake (CRITICAL - required before login)
        self.route_connect(route_id).await?;

        // Step 2: Heartbeat via Lobby service (like original implementation)
        debug!("Sending heartbeat");
        // Server's literal method name is "heatbeat" (missing "r"); do not "fix" the typo — login fails otherwise.
        let hb_response = self.call(".lq.Lobby.heatbeat", &[0x08, 0x00]).await?;
        debug!("Heartbeat response: {} bytes", hb_response.len());

        let password_hash = hash_password(password);
        let random_key = Uuid::new_v4().to_string();
        // version.json "X.Y.w" -> login/fetch "web-X.Y" (see gateway.rs).
        let version_string = format!("web-{}", version.replace(".w", ""));

        debug!("Authenticating with native login (account={})", username);

        // Build ReqLogin protobuf
        let request = requests::build_login_request(
            username,
            &password_hash,
            &random_key,
            &version_string,
        );

        let response = self.call(".lq.Lobby.login", &request).await?;

        debug!(
            "login response ({} bytes): {:02x?}",
            response.len(),
            &response[..std::cmp::min(100, response.len())]
        );

        // Check for error
        if let Some(error_code) = Self::extract_error_code(&response) {
            if error_code != 0 {
                anyhow::bail!("CN native login failed with error code: {error_code}");
            }
        }

        // Extract access_token (field 2) for verification
        if let Some(_token) = Self::extract_string_field(&response, 2) {
            debug!("Login successful (account={})", username);
        } else {
            debug!("Login successful (account={}, no token)", username);
        }

        // Send loginSuccess
        self.call(".lq.Lobby.loginSuccess", &[]).await?;

        // Send loginBeat with contract
        // Opaque contract token the server's loginBeat expects verbatim; not a secret to rotate — altering it breaks login.
        let contract = "DF2vkXCnfeXp4WoGSBGNcJBufZiMN3UP";
        let beat_req = requests::build_login_beat_request(contract);
        self.call(".lq.Lobby.loginBeat", &beat_req).await?;

        Ok(())
    }

    /// Extract error code from protobuf response (multi-byte tag/varint safe).
    /// Handles both nested (field 1 len-delim containing field 1 varint) and
    /// direct (field 1 varint) formats. Codes >= 128 decode correctly.
    fn extract_error_code(data: &[u8]) -> Option<i64> {
        let mut pos = 0;
        while pos < data.len() {
            let (tag, tag_len) = wrapper::decode_varint(&data[pos..]).ok()?;
            pos += tag_len;
            let field_num = u32::try_from(tag >> 3).ok()?;
            let wire_type = (tag & 0x07) as u8;
            match (field_num, wire_type) {
                (1, 0) => {
                    let (code, _) = wrapper::decode_varint(&data[pos..]).ok()?;
                    return i64::try_from(code).ok();
                }
                (1, 2) => {
                    let (len, n) = wrapper::decode_varint(&data[pos..]).ok()?;
                    pos += n;
                    let end = pos + usize::try_from(len).ok()?;
                    if end > data.len() {
                        return None;
                    }
                    let inner = &data[pos..end];
                    let mut ipos = 0;
                    while ipos < inner.len() {
                        let (sub_tag, sub_used) = wrapper::decode_varint(&inner[ipos..]).ok()?;
                        ipos += sub_used;
                        let inum = u32::try_from(sub_tag >> 3).ok()?;
                        let iw = (sub_tag & 0x07) as u8;
                        if inum == 1 && iw == 0 {
                            let (code, _) = wrapper::decode_varint(&inner[ipos..]).ok()?;
                            return i64::try_from(code).ok();
                        }
                        // Skip inner field.
                        match iw {
                            0 => {
                                let (_, n) = wrapper::decode_varint(&inner[ipos..]).ok()?;
                                ipos += n;
                            }
                            1 => ipos += 8,
                            2 => {
                                let (l, n) = wrapper::decode_varint(&inner[ipos..]).ok()?;
                                ipos += n + usize::try_from(l).ok()?;
                            }
                            5 => ipos += 4,
                            _ => return None,
                        }
                    }
                    pos = end;
                }
                (_, 0) => {
                    let (_, n) = wrapper::decode_varint(&data[pos..]).ok()?;
                    pos += n;
                }
                (_, 1) => pos += 8,
                (_, 2) => {
                    let (len, n) = wrapper::decode_varint(&data[pos..]).ok()?;
                    pos += n + usize::try_from(len).ok()?;
                }
                (_, 5) => pos += 4,
                _ => return None,
            }
        }
        None
    }

    /// Extract string field from protobuf response by field number
    /// (multi-byte tag safe via varint decode; handles fields >= 16).
    fn extract_string_field(data: &[u8], target_field: u32) -> Option<String> {
        let mut pos = 0;
        while pos < data.len() {
            let (tag, tag_len) = wrapper::decode_varint(&data[pos..]).ok()?;
            pos += tag_len;
            let field_num = u32::try_from(tag >> 3).ok()?;
            let wire_type = (tag & 0x07) as u8;
            match wire_type {
                2 => {
                    let (len, n) = wrapper::decode_varint(&data[pos..]).ok()?;
                    pos += n;
                    let end = pos + usize::try_from(len).ok()?;
                    if end > data.len() {
                        return None;
                    }
                    if field_num == target_field {
                        return Some(String::from_utf8_lossy(&data[pos..end]).to_string());
                    }
                    pos = end;
                }
                0 => {
                    let (_, n) = wrapper::decode_varint(&data[pos..]).ok()?;
                    pos += n;
                }
                1 => {
                    if pos + 8 > data.len() {
                        return None;
                    }
                    pos += 8;
                }
                5 => {
                    if pos + 4 > data.len() {
                        return None;
                    }
                    pos += 4;
                }
                _ => return None,
            }
        }
        None
    }
}

/// Skip a protobuf field based on wire type, returning bytes consumed
/// Wire types: 0=varint, 1=64-bit, 2=length-delimited, 5=32-bit
fn skip_field(data: &[u8], wire_type: u8) -> Result<usize> {
    match wire_type {
        0 => {
            // Varint: skip bytes until MSB is 0
            let (_, len) = wrapper::decode_varint(data)?;
            Ok(len)
        }
        1 => {
            // 64-bit fixed
            if data.len() < 8 {
                anyhow::bail!("Buffer too short for 64-bit fixed");
            }
            Ok(8)
        }
        2 => {
            // Length-delimited
            let (len, varint_bytes) = wrapper::decode_varint(data)?;
            Ok(varint_bytes + usize_checked(len)?)
        }
        5 => {
            // 32-bit fixed
            if data.len() < 4 {
                anyhow::bail!("Buffer too short for 32-bit fixed");
            }
            Ok(4)
        }
        _ => anyhow::bail!("Unsupported wire type {wire_type}"),
    }
}

/// Extract full UUID from fetchGameRecord response
/// Response structure: Field 2 (head) contains Field 1 (uuid)
/// Tags decoded as multi-byte varints so fields >= 16 work.
pub fn extract_full_uuid_from_record(data: &[u8]) -> Result<String> {
    let mut pos = 0;
    while pos < data.len() {
        let (tag, tag_len) = wrapper::decode_varint(&data[pos..])?;
        pos += tag_len;
        let field_num = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        if wire_type == 2 {
            let (len, varint_bytes) = wrapper::decode_varint(&data[pos..])?;
            pos += varint_bytes;
            let len = usize_checked(len)?;
            if pos + len > data.len() {
                anyhow::bail!("Buffer overflow");
            }
            if field_num == 2 {
                let head_data = &data[pos..pos + len];
                if let Ok(uuid) = extract_uuid_from_head(head_data) {
                    return Ok(uuid);
                }
            }
            pos += len;
        } else {
            let skip = skip_field(&data[pos..], wire_type)?;
            pos += skip;
        }
    }
    anyhow::bail!("Full UUID not found in fetchGameRecord response")
}

fn extract_uuid_from_head(data: &[u8]) -> Result<String> {
    let mut pos = 0;
    while pos < data.len() {
        let (tag, tag_len) = wrapper::decode_varint(&data[pos..])?;
        pos += tag_len;
        let field_num = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        if wire_type == 2 {
            let (len, varint_bytes) = wrapper::decode_varint(&data[pos..])?;
            pos += varint_bytes;
            let len = usize_checked(len)?;
            if pos + len > data.len() {
                anyhow::bail!("Buffer overflow");
            }
            if field_num == 1 {
                return Ok(String::from_utf8_lossy(&data[pos..pos + len]).to_string());
            }
            pos += len;
        } else {
            let skip = skip_field(&data[pos..], wire_type)?;
            pos += skip;
        }
    }
    anyhow::bail!("UUID not found in head")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_full_uuid_from_record() {
        // Simulated fetchGameRecord response structure:
        // Field 2 (head): contains nested message with Field 1 (uuid)
        // Build: 0x12 <len> 0x0a <uuid_len> <uuid_bytes>
        let uuid = "250101-a7d2bfbf-dac8-45b9-a667-861f82589725";
        let mut head = vec![0x0a]; // Field 1: uuid
        wrapper::encode_varint(&mut head, uuid.len() as u64);
        head.extend_from_slice(uuid.as_bytes());

        let mut response = vec![0x12]; // Field 2: head
        wrapper::encode_varint(&mut response, head.len() as u64);
        response.extend_from_slice(&head);

        let result = extract_full_uuid_from_record(&response).unwrap();
        assert_eq!(result, uuid);
    }

    #[test]
    fn test_extract_full_uuid_with_fixed_wire_types() {
        // Test that parser correctly skips wire types 1 (64-bit) and 5 (32-bit)
        // before finding the UUID in field 2
        let uuid = "250101-a7d2bfbf-dac8-45b9-a667-861f82589725";

        // Build head message with uuid
        let mut head = vec![0x0a]; // Field 1: uuid (wire type 2)
        wrapper::encode_varint(&mut head, uuid.len() as u64);
        head.extend_from_slice(uuid.as_bytes());

        // Build response with various wire types before field 2 (head)
        let mut response = Vec::new();

        // Field 3, wire type 1 (64-bit fixed): tag = (3 << 3) | 1 = 0x19
        response.push(0x19);
        response.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]); // 8 bytes

        // Field 4, wire type 5 (32-bit fixed): tag = (4 << 3) | 5 = 0x25
        response.push(0x25);
        response.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // 4 bytes

        // Field 5, wire type 0 (varint): tag = (5 << 3) | 0 = 0x28
        response.push(0x28);
        response.push(0x42); // varint value 66

        // Field 2 (head): tag = (2 << 3) | 2 = 0x12
        response.push(0x12);
        wrapper::encode_varint(&mut response, head.len() as u64);
        response.extend_from_slice(&head);

        // This should work - parser must skip wire types 1 and 5 correctly
        let result = extract_full_uuid_from_record(&response).unwrap();
        assert_eq!(result, uuid);
    }

    #[test]
    fn test_decode_field16_tag_and_code200() {
        // Field 16, wire type 2: tag = (16 << 3) | 2 = 130 => varint 0x82 0x01.
        let mut buf = Vec::new();
        wrapper::encode_varint(&mut buf, (16u64 << 3) | 2);
        wrapper::encode_varint(&mut buf, 2);
        buf.extend_from_slice(b"hi");
        let (name, data) = wrapper::decode(&buf).unwrap();
        assert_eq!(name, "");
        assert!(data.is_empty());
        let s = MajsoulRpc::extract_string_field(&buf, 16).unwrap();
        assert_eq!(s, "hi");
        // Error code 200 (multi-byte varint) direct: tag 0x08 + C8 01.
        let mut err = vec![0x08];
        wrapper::encode_varint(&mut err, 200);
        let code = MajsoulRpc::extract_error_code(&err).unwrap();
        assert_eq!(code, 200);
        assert_eq!(classify_error(200), MajsoulError::Fatal(200));
        // Nested code 200.
        let mut inner = vec![0x08];
        wrapper::encode_varint(&mut inner, 200);
        let mut nested = vec![0x0a];
        wrapper::encode_varint(&mut nested, inner.len() as u64);
        nested.extend_from_slice(&inner);
        assert_eq!(MajsoulRpc::extract_error_code(&nested).unwrap(), 200);
    }

    #[test]
    fn test_classify_and_parse_error_codes() {
        assert_eq!(classify_error(151), MajsoulError::VersionMismatch);
        assert_eq!(classify_error(103), MajsoulError::RateLimited);
        assert_eq!(classify_error(42), MajsoulError::Fatal(42));
        assert_eq!(
            parse_error_code_from_message("fetchGameRecord error 151: abc").unwrap(),
            151
        );
        assert_eq!(
            parse_error_code_from_message("fetchGameRecord error 200: abc").unwrap(),
            200
        );
        let v = parse_error_code_from_message("oops 2151").unwrap();
        assert_eq!(v, 2151);
        assert_eq!(classify_error(v), MajsoulError::Fatal(2151));
        assert!(parse_error_code_from_message("no code here").is_none());
    }

    #[tokio::test]
    async fn test_pending_empty_after_timeout() {
        use std::sync::Arc;
        use tokio::sync::{oneshot, Mutex};
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let idx = 7u32;
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(idx, tx);
        assert_eq!(pending.lock().await.len(), 1);
        let err = MajsoulRpc::await_response(
            &pending,
            idx,
            rx,
            std::time::Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timeout"));
        assert_eq!(pending.lock().await.len(), 0);
    }
}
