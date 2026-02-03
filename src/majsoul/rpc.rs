use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

const MS_HOST: &str = "https://game.maj-soul.com";

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
        let mut pos = 0;
        let mut name = String::new();
        let mut data = Vec::new();

        while pos < buf.len() {
            let tag = buf[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            if wire_type != 2 {
                if wire_type == 0 {
                    while pos < buf.len() && buf[pos] & 0x80 != 0 {
                        pos += 1;
                    }
                    pos += 1;
                }
                continue;
            }

            let (len, bytes_read) = decode_varint(&buf[pos..])?;
            pos += bytes_read;
            let end = pos + len as usize;
            if end > buf.len() {
                anyhow::bail!("Buffer overflow");
            }

            match field_num {
                1 => name = String::from_utf8_lossy(&buf[pos..end]).to_string(),
                2 => data = buf[pos..end].to_vec(),
                _ => {}
            }
            pos = end;
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
            value |= ((byte & 0x7f) as u64) << shift;
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
    use uuid::Uuid;

    /// Get auth type for server
    /// EN/JP (Yostar) = 7, TW/HK (Google) = 16, CN (native) = 0
    pub fn auth_type_for_server(server: &str) -> u8 {
        match server {
            "cn" => 16,  // TW/HK uses Google OAuth type 16 (same domain as CN)
            "en" | "jp" => 7,  // Yostar OAuth
            _ => 7,
        }
    }

    /// oauth2Auth request for EN/JP servers (type 7)
    /// Exchanges code-uid for access_token
    pub fn oauth2_auth(code: &str, uid: &str, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type = 7
        buf.push(0x08);
        buf.push(0x07);
        // Field 2: code (GUID)
        buf.push(0x12);
        encode_varint(&mut buf, code.len() as u64);
        buf.extend_from_slice(code.as_bytes());
        // Field 3: uid
        buf.push(0x1a);
        encode_varint(&mut buf, uid.len() as u64);
        buf.extend_from_slice(uid.as_bytes());
        // Field 8: client_version_string
        let version_str = format!("web-{}", version);
        buf.push(0x42);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        buf
    }

    /// oauth2Auth for CN/TW/HK - exchanges Google token for liqi_access_token
    pub fn oauth2_auth_for_cn(access_token: &str, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type = 16 (Google OAuth for CN/TW/HK)
        buf.push(0x08);
        buf.push(0x10); // 16
        // Field 2: code (the Google access_token from localStorage)
        buf.push(0x12);
        encode_varint(&mut buf, access_token.len() as u64);
        buf.extend_from_slice(access_token.as_bytes());
        // Field 3: uid (empty for Google OAuth)
        buf.push(0x1a);
        buf.push(0x00);
        // Field 8: client_version_string
        let version_str = format!("web-{}", version);
        buf.push(0x42);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        buf
    }

    /// oauth2Auth for CN/TW/HK with Google JWT (type 16 for TW/HK Google OAuth)
    pub fn oauth2_auth_for_cn_jwt(id_token: &str, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type = 16 (Google OAuth for TW/HK)
        buf.push(0x08);
        buf.push(0x10); // 16
        // Field 2: code (the Google id_token JWT)
        buf.push(0x12);
        encode_varint(&mut buf, id_token.len() as u64);
        buf.extend_from_slice(id_token.as_bytes());
        // Field 3: uid (empty)
        buf.push(0x1a);
        buf.push(0x00);
        // Field 8: client_version_string
        let version_str = format!("web-{}", version);
        buf.push(0x42);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        buf
    }

    pub fn oauth2_check(access_token: &str, server: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type (7 for Yostar, 16 for TW/HK Google)
        let auth_type = auth_type_for_server(server);
        buf.push(0x08);
        buf.push(auth_type);
        // Field 2: access_token
        buf.push(0x12);
        encode_varint(&mut buf, access_token.len() as u64);
        buf.extend_from_slice(access_token.as_bytes());
        buf
    }

    /// Native login with uid + token (from URL redirect)
    pub fn login_with_token(token: &str, uid: u64, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: account (uid as string)
        let uid_str = uid.to_string();
        buf.push(0x0a);
        encode_varint(&mut buf, uid_str.len() as u64);
        buf.extend_from_slice(uid_str.as_bytes());
        // Field 2: password (empty for token login)
        buf.push(0x12);
        buf.push(0x00);
        // Field 3: reconnect = false
        buf.push(0x18);
        buf.push(0x00);
        // Field 4: device
        let device = encode_device();
        buf.push(0x22);
        encode_varint(&mut buf, device.len() as u64);
        buf.extend_from_slice(&device);
        // Field 5: random_key
        let random_key = Uuid::new_v4().to_string();
        buf.push(0x2a);
        encode_varint(&mut buf, random_key.len() as u64);
        buf.extend_from_slice(random_key.as_bytes());
        // Field 6: client_version
        let version_str = format!("web-{}", version);
        let mut client_version = Vec::new();
        client_version.push(0x0a);
        encode_varint(&mut client_version, version_str.len() as u64);
        client_version.extend_from_slice(version_str.as_bytes());
        buf.push(0x32);
        encode_varint(&mut buf, client_version.len() as u64);
        buf.extend_from_slice(&client_version);
        // Field 9: type = 0 (token login)
        buf.push(0x48);
        buf.push(0x00);
        // Field 10: access_token
        buf.push(0x52);
        encode_varint(&mut buf, token.len() as u64);
        buf.extend_from_slice(token.as_bytes());
        buf
    }

    pub fn oauth2_login(access_token: &str, version: &str, server: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type (7 for Yostar, 16 for TW/HK Google)
        let auth_type = auth_type_for_server(server);
        buf.push(0x08);
        buf.push(auth_type);
        // Field 2: access_token (FIXED: was incorrectly field 4)
        buf.push(0x12);
        encode_varint(&mut buf, access_token.len() as u64);
        buf.extend_from_slice(access_token.as_bytes());
        // Field 3: reconnect = false (FIXED: was incorrectly field 5)
        buf.push(0x18);
        buf.push(0x00);
        // Field 4: device - nested message (FIXED: was incorrectly field 6)
        let device = encode_device();
        buf.push(0x22);
        encode_varint(&mut buf, device.len() as u64);
        buf.extend_from_slice(&device);
        // Field 5: random_key (UUID) (FIXED: was incorrectly field 7)
        let random_key = Uuid::new_v4().to_string();
        buf.push(0x2a);
        encode_varint(&mut buf, random_key.len() as u64);
        buf.extend_from_slice(random_key.as_bytes());
        // Field 6: client_version - nested message (FIXED: was incorrectly field 8)
        let version_str = format!("web-{}", version);
        let mut client_version = Vec::new();
        client_version.push(0x0a); // field 1 = resource
        encode_varint(&mut client_version, version_str.len() as u64);
        client_version.extend_from_slice(version_str.as_bytes());
        buf.push(0x32);
        encode_varint(&mut buf, client_version.len() as u64);
        buf.extend_from_slice(&client_version);
        // Field 10: client_version_string (tag 0x52)
        buf.push(0x52);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        buf
    }

    /// oauth2_login with explicit type (for two-step retry with type 0)
    pub fn oauth2_login_with_type(access_token: &str, version: &str, auth_type: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: type
        buf.push(0x08);
        buf.push(auth_type);
        // Field 2: access_token
        buf.push(0x12);
        encode_varint(&mut buf, access_token.len() as u64);
        buf.extend_from_slice(access_token.as_bytes());
        // Field 3: reconnect = false
        buf.push(0x18);
        buf.push(0x00);
        // Field 4: device
        let device = encode_device();
        buf.push(0x22);
        encode_varint(&mut buf, device.len() as u64);
        buf.extend_from_slice(&device);
        // Field 5: random_key
        let random_key = Uuid::new_v4().to_string();
        buf.push(0x2a);
        encode_varint(&mut buf, random_key.len() as u64);
        buf.extend_from_slice(random_key.as_bytes());
        // Field 6: client_version
        let version_str = format!("web-{}", version);
        let mut client_version = Vec::new();
        client_version.push(0x0a);
        encode_varint(&mut client_version, version_str.len() as u64);
        client_version.extend_from_slice(version_str.as_bytes());
        buf.push(0x32);
        encode_varint(&mut buf, client_version.len() as u64);
        buf.extend_from_slice(&client_version);
        // Field 10: client_version_string
        buf.push(0x52);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        buf
    }

    fn encode_device() -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: platform = "pc" (was incorrectly field 2)
        buf.push(0x0a);
        buf.push(0x02);
        buf.extend_from_slice(b"pc");
        // Field 2: hardware = "pc" (was incorrectly field 3)
        buf.push(0x12);
        buf.push(0x02);
        buf.extend_from_slice(b"pc");
        // Field 3: os = "pc" (was incorrectly field 4 with "windows")
        buf.push(0x1a);
        buf.push(0x02);
        buf.extend_from_slice(b"pc");
        // Field 4: os_version = "" (was incorrectly field 5)
        buf.push(0x22);
        buf.push(0x00);
        // Field 5: is_browser = true (was incorrectly field 6)
        buf.push(0x28);
        buf.push(0x01);
        // Field 6: software = "Chrome" (was incorrectly field 7)
        buf.push(0x32);
        buf.push(0x06);
        buf.extend_from_slice(b"Chrome");
        // Field 7: sale_platform = "web" (was incorrectly field 8)
        buf.push(0x3a);
        buf.push(0x03);
        buf.extend_from_slice(b"web");
        buf
    }

    pub fn fetch_game_record(uuid: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(0x0a);
        encode_varint(&mut buf, uuid.len() as u64);
        buf.extend_from_slice(uuid.as_bytes());
        buf
    }

    /// Fetch public game record list from ranked rooms
    /// type: 0=all, 1=Bronze, 2=Silver, 3=Gold, 4=Jade, 5=Throne
    pub fn fetch_game_record_list(start: u32, count: u32, room_type: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: start (pagination offset)
        buf.push(0x08);
        encode_varint(&mut buf, start as u64);
        // Field 2: count (number of records)
        buf.push(0x10);
        encode_varint(&mut buf, count as u64);
        // Field 3: type (room type)
        buf.push(0x18);
        encode_varint(&mut buf, room_type as u64);
        buf
    }

    /// Fetch live games list (spectatable)
    pub fn fetch_game_live_list(filter_id: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 1: filter_id (0 = all, or specific room)
        buf.push(0x08);
        encode_varint(&mut buf, filter_id as u64);
        buf
    }

    /// Build ReqLogin for CN native login (username/password)
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
        // Field 8: random_key (string)
        encode_string(&mut buf, 8, random_key);
        // Field 9: gen_access_token (bool = true)
        encode_bool(&mut buf, 9, true);
        // Field 10: currency_platforms (repeated int32 = [2])
        encode_varint_field(&mut buf, 10, 2);
        // Field 12: client_version_string
        encode_string(&mut buf, 12, version);

        buf
    }

    /// Build loginBeat request
    pub fn build_login_beat_request(contract: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_string(&mut buf, 1, contract);
        buf
    }

    fn encode_string(buf: &mut Vec<u8>, field: u32, value: &str) {
        let tag = (field << 3) | 2;
        encode_varint(buf, tag as u64);
        encode_varint(buf, value.len() as u64);
        buf.extend_from_slice(value.as_bytes());
    }

    fn encode_bool(buf: &mut Vec<u8>, field: u32, value: bool) {
        let tag = (field << 3) | 0;
        encode_varint(buf, tag as u64);
        buf.push(if value { 1 } else { 0 });
    }

    fn encode_varint_field(buf: &mut Vec<u8>, field: u32, value: u64) {
        let tag = (field << 3) | 0;
        encode_varint(buf, tag as u64);
        encode_varint(buf, value);
    }

    /// Encode device message with just is_browser = true (for native login)
    fn encode_nested_device_simple(buf: &mut Vec<u8>) {
        // Field 4: device message with is_browser = true (field 5 in device)
        let mut inner = Vec::new();
        // Field 5: is_browser = true
        inner.push(0x28); // (5 << 3) | 0
        inner.push(0x01);

        let tag = (4 << 3) | 2;
        encode_varint(buf, tag as u64);
        encode_varint(buf, inner.len() as u64);
        buf.extend(inner);
    }
}

pub struct MajsoulRpc {
    write: Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>,
    pending: Arc<Mutex<HashMap<u16, oneshot::Sender<Vec<u8>>>>>,
    req_idx: AtomicU16,
    _read_task: tokio::task::JoinHandle<()>,
}

impl MajsoulRpc {
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let mut request = endpoint.into_client_request()?;
        request
            .headers_mut()
            .insert("Origin", MS_HOST.parse().unwrap());

        info!("Connecting to {}", endpoint);
        let (ws_stream, _) = connect_async(request)
            .await
            .context("WebSocket connect failed")?;

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(Mutex::new(write));
        let pending: Arc<Mutex<HashMap<u16, oneshot::Sender<Vec<u8>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let pending_clone = Arc::clone(&pending);
        let read_task = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Binary(data)) if data.len() >= 3 => {
                        if data[0] == 3 {
                            // RESPONSE
                            let idx = u16::from_le_bytes([data[1], data[2]]);
                            if let Ok((_, response_data)) = wrapper::decode(&data[3..]) {
                                let mut pending = pending_clone.lock().await;
                                if let Some(tx) = pending.remove(&idx) {
                                    let _ = tx.send(response_data);
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!("WebSocket closed");
                        break;
                    }
                    Err(e) => {
                        warn!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        info!("Connected to Majsoul gateway");
        Ok(Self {
            write,
            pending,
            req_idx: AtomicU16::new(1),
            _read_task: read_task,
        })
    }

    pub async fn call(&self, method: &str, request_data: &[u8]) -> Result<Vec<u8>> {
        let idx = self.req_idx.fetch_add(1, Ordering::SeqCst) % 60007;

        let wrapped = wrapper::encode(method, request_data);
        let mut packet = vec![0x02];
        packet.extend_from_slice(&idx.to_le_bytes());
        packet.extend_from_slice(&wrapped);

        let (tx, rx) = oneshot::channel();
        {
            self.pending.lock().await.insert(idx, tx);
        }
        {
            self.write
                .lock()
                .await
                .send(Message::Binary(packet.into()))
                .await?;
        }

        debug!("Sent RPC: {} (idx={})", method, idx);

        let response = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .context("RPC timeout")?
            .context("RPC channel closed")?;
        Ok(response)
    }

    /// Login with code-uid format (for EN/JP servers)
    /// Format: "{code}-{uid}" where code is from localStorage 'dddddcv' and uid is account_id
    pub async fn login(&self, token: &str, version: &str, server: &str) -> Result<Vec<u8>> {
        // Step 0: Send heartbeat first (required to establish session)
        info!("Sending heartbeat");
        let hb_response = self.call(".lq.Lobby.heatbeat", &[0x08, 0x00]).await?;
        debug!("Heartbeat response: {} bytes", hb_response.len());

        // Parse token: code-uid format
        let (code, uid) = if token.contains('-') && token.len() > 40 {
            // Assume format: {guid}-{uid} where guid has hyphens
            let last_dash = token.rfind('-').unwrap();
            let potential_uid = &token[last_dash + 1..];
            if potential_uid.chars().all(|c| c.is_ascii_digit()) {
                (&token[..last_dash], potential_uid)
            } else {
                // No uid suffix, use token directly as access_token
                return self.login_with_access_token(token, version, server, None).await;
            }
        } else {
            // No uid, try as direct access_token
            return self.login_with_access_token(token, version, server, None).await;
        };

        info!("Authenticating with oauth2Auth (code={}, uid={})", &code[..8], uid);

        // Step 1: oauth2Auth to exchange code+uid for access_token
        let auth_request = requests::oauth2_auth(code, uid, version);
        let auth_response = self.call(".lq.Lobby.oauth2Auth", &auth_request).await?;

        debug!(
            "oauth2Auth response ({} bytes): {:02x?}",
            auth_response.len(),
            &auth_response[..std::cmp::min(50, auth_response.len())]
        );

        // Check for error in oauth2Auth response
        if auth_response.len() >= 4 && auth_response[0] == 0x0a && auth_response[2] == 0x08 {
            let err_code = auth_response[3];
            if err_code != 0 {
                anyhow::bail!(
                    "oauth2Auth failed (error {}). Code may be expired.\n\
                     The 'dddddcv' code gets invalidated after use.\n\
                     You need to logout and login again in browser to get a fresh code.",
                    err_code
                );
            }
        }

        // Parse access_token from response
        let access_token = Self::parse_access_token(&auth_response)?;
        info!("Got access_token: {}...", &access_token[..8.min(access_token.len())]);

        // Step 2: oauth2Login with the real access_token
        self.login_with_access_token(&access_token, version, "en", None).await
    }

    async fn login_with_access_token(&self, access_token: &str, version: &str, server: &str, uid: Option<u64>) -> Result<Vec<u8>> {
        // Step 1: oauth2Check to validate token
        info!("Checking token with oauth2Check (server: {}, type: {})", server, requests::auth_type_for_server(server));
        let check_request = requests::oauth2_check(access_token, server);
        let check_response = self.call(".lq.Lobby.oauth2Check", &check_request).await?;
        debug!("oauth2Check response: {:02x?}", &check_response[..std::cmp::min(50, check_response.len())]);

        // For CN/TW/HK with JWT token: exchange via oauth2Auth first
        // JWT starts with "eyJ", UUID tokens are 36 chars, URL tokens are 32 hex
        let is_jwt = access_token.starts_with("eyJ") && access_token.len() > 100;
        let is_url_token = access_token.len() == 32 && access_token.chars().all(|c| c.is_ascii_hexdigit());

        // URL tokens (from redirect) use native login, not oauth2Login
        if is_url_token {
            info!("Using URL token with native login (uid: {})", uid.unwrap_or(0));
            let login_request = requests::login_with_token(access_token, uid.unwrap_or(0), version);
            let response = self.call(".lq.Lobby.login", &login_request).await?;

            // Check for error
            if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
                anyhow::bail!("Native login failed (error {})", response[1]);
            }
            info!("Login successful (native token)");
            return Ok(response);
        }

        let final_token = if server == "cn" && is_jwt {
            info!("CN: exchanging Google JWT via oauth2Auth (type 20)...");
            let auth_request = requests::oauth2_auth_for_cn_jwt(access_token, version);
            let auth_response = self.call(".lq.Lobby.oauth2Auth", &auth_request).await?;
            debug!("oauth2Auth response: {:02x?}", &auth_response[..std::cmp::min(100, auth_response.len())]);

            // Parse liqi_access_token from response (field 2)
            match Self::parse_access_token(&auth_response) {
                Ok(liqi_token) => {
                    info!("Got liqi_access_token from oauth2Auth: {}...", &liqi_token[..8.min(liqi_token.len())]);
                    liqi_token
                }
                Err(_) => {
                    if auth_response.len() >= 4 && auth_response[0] == 0x0a && auth_response[2] == 0x08 && auth_response[3] != 0 {
                        anyhow::bail!("oauth2Auth failed (error {}). JWT may be expired.", auth_response[3]);
                    }
                    access_token.to_string()
                }
            }
        } else {
            access_token.to_string()
        };

        // Step 2: oauth2Login
        info!("Authenticating with oauth2Login");
        let login_request = requests::oauth2_login(&final_token, version, server);
        let response = self.call(".lq.Lobby.oauth2Login", &login_request).await?;

        // Debug: dump response hex
        info!(
            "oauth2Login response ({} bytes): {:02x?}",
            response.len(),
            &response[..std::cmp::min(100, response.len())]
        );

        // Check for error - can be nested in field 1 (Error message)
        // Format: 0a <len> 08 <error_code> ... or direct 08 <error_code>
        let error_code = if response.len() >= 4 && response[0] == 0x0a {
            // Nested error message in field 1
            if response[2] == 0x08 {
                Some(response[3])
            } else {
                None
            }
        } else if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
            // Direct error code
            Some(response[1])
        } else {
            None
        };

        if let Some(code) = error_code {
            if code != 0 {
                // Error 109: two-step OAuth - extract liqi_access_token and retry with same type
                if code == 109 {
                    if let Some(liqi_token) = Self::extract_liqi_token(&response) {
                        let auth_type = requests::auth_type_for_server(server);
                        info!("Two-step OAuth: got liqi_access_token, retrying with type {}...", auth_type);
                        let retry_request = requests::oauth2_login_with_type(&liqi_token, version, auth_type);
                        let retry_response = self.call(".lq.Lobby.oauth2Login", &retry_request).await?;
                        info!("oauth2Login retry response ({} bytes)", retry_response.len());
                        // Check retry response for errors
                        if retry_response.len() >= 4 && retry_response[0] == 0x0a && retry_response[2] == 0x08 && retry_response[3] != 0 {
                            anyhow::bail!("Login retry failed (error {})", retry_response[3]);
                        }
                        info!("Login successful (two-step)");
                        return Ok(retry_response);
                    }
                }
                anyhow::bail!(
                    "Login failed (error {}). Token may be expired.\n\
                     Get a fresh token from browser: localStorage.getItem('ssssoooodd')\n\
                     Your token: ssssoooodd (access_token), NOT dddddcv",
                    code
                );
            }
        }
        info!("Login successful");
        Ok(response)
    }

    /// Parse access_token from oauth2Auth response
    fn parse_access_token(data: &[u8]) -> Result<String> {
        let mut pos = 0;
        while pos < data.len() {
            let tag = data[pos];
            pos += 1;
            let field_num = tag >> 3;
            let wire_type = tag & 0x07;

            if wire_type == 2 {
                // Length-delimited
                let mut len: usize = 0;
                let mut shift = 0;
                while pos < data.len() {
                    let b = data[pos];
                    pos += 1;
                    len |= ((b & 0x7f) as usize) << shift;
                    if b & 0x80 == 0 {
                        break;
                    }
                    shift += 7;
                }
                if field_num == 2 && pos + len <= data.len() {
                    // Field 2 is access_token
                    return Ok(String::from_utf8_lossy(&data[pos..pos + len]).to_string());
                }
                pos += len;
            } else if wire_type == 0 {
                // Varint - skip
                while pos < data.len() && data[pos] & 0x80 != 0 {
                    pos += 1;
                }
                pos += 1;
            } else {
                anyhow::bail!("Unexpected wire type {}", wire_type);
            }
        }
        anyhow::bail!("access_token not found in oauth2Auth response");
    }

    /// Extract liqi_access_token from CN error 109 response
    /// Response contains JSON like {"type":7,"liqi_access_token":"uuid-here"}
    fn extract_liqi_token(data: &[u8]) -> Option<String> {
        // Find JSON in response (field 4, tag 0x22)
        let json_str = String::from_utf8_lossy(data);
        if let Some(start) = json_str.find("\"liqi_access_token\":\"") {
            let start = start + 21;
            if let Some(end) = json_str[start..].find('"') {
                return Some(json_str[start..start + end].to_string());
            }
        }
        None
    }

    pub async fn fetch_game_record(&self, uuid: &str) -> Result<Vec<u8>> {
        let request = requests::fetch_game_record(uuid);
        let response = self.call(".lq.Lobby.fetchGameRecord", &request).await?;
        if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
            anyhow::bail!("fetchGameRecord error {}: {}", response[1], uuid);
        }
        debug!("Fetched game record: {} ({} bytes)", uuid, response.len());
        Ok(response)
    }

    /// Fetch public game list from ranked rooms (Throne, Jade, Gold, etc.)
    /// room_type: 0=all, 1=Bronze, 2=Silver, 3=Gold, 4=Jade, 5=Throne
    pub async fn fetch_game_record_list(&self, start: u32, count: u32, room_type: u32) -> Result<Vec<u8>> {
        let request = requests::fetch_game_record_list(start, count, room_type);
        let response = self.call(".lq.Lobby.fetchGameRecordList", &request).await?;
        if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
            anyhow::bail!("fetchGameRecordList error {}", response[1]);
        }
        debug!("Fetched game record list ({} bytes)", response.len());
        Ok(response)
    }

    /// Fetch live games (spectatable)
    pub async fn fetch_game_live_list(&self, filter_id: u32) -> Result<Vec<u8>> {
        let request = requests::fetch_game_live_list(filter_id);
        let response = self.call(".lq.Lobby.fetchGameLiveList", &request).await?;
        if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
            anyhow::bail!("fetchGameLiveList error {}", response[1]);
        }
        debug!("Fetched live game list ({} bytes)", response.len());
        Ok(response)
    }

    pub async fn close(self) -> Result<()> {
        self.write.lock().await.close().await?;
        Ok(())
    }

    /// Login with username/password (CN server native auth)
    pub async fn login_native(&self, username: &str, password: &str, version: &str) -> Result<()> {
        use crate::majsoul::auth::hash_password;

        // Step 0: Send heartbeat first (required to establish session)
        info!("Sending heartbeat");
        let hb_response = self.call(".lq.Lobby.heatbeat", &[0x08, 0x00]).await?;
        debug!("Heartbeat response: {} bytes", hb_response.len());

        let password_hash = hash_password(password);
        let random_key = Uuid::new_v4().to_string();
        let version_string = format!("web-{}", version.replace(".w", ""));

        info!("Authenticating with native login (account={})", username);

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
                anyhow::bail!("CN native login failed with error code: {}", error_code);
            }
        }

        // Extract access_token (field 2) for verification
        if let Some(token) = Self::extract_string_field(&response, 2) {
            info!("CN native login successful (token: {}...)", &token[..8.min(token.len())]);
        } else {
            info!("CN native login successful");
        }

        // Send loginSuccess
        self.call(".lq.Lobby.loginSuccess", &[]).await?;

        // Send loginBeat with contract
        let contract = "DF2vkXCnfeXp4WoGSBGNcJBufZiMN3UP";
        let beat_req = requests::build_login_beat_request(contract);
        self.call(".lq.Lobby.loginBeat", &beat_req).await?;

        Ok(())
    }

    /// Extract error code from protobuf response
    /// Handles both nested (0a <len> 08 <code>) and direct (08 <code>) formats
    fn extract_error_code(data: &[u8]) -> Option<u8> {
        if data.len() >= 4 && data[0] == 0x0a && data[2] == 0x08 {
            // Nested error in field 1
            Some(data[3])
        } else if data.len() >= 2 && data[0] == 0x08 {
            // Direct error code
            Some(data[1])
        } else {
            None
        }
    }

    /// Extract string field from protobuf response by field number
    fn extract_string_field(data: &[u8], target_field: u32) -> Option<String> {
        let mut pos = 0;
        while pos < data.len() {
            let tag = data[pos];
            pos += 1;
            let field_num = (tag >> 3) as u32;
            let wire_type = tag & 0x07;

            if wire_type == 2 {
                // Length-delimited
                let mut len: usize = 0;
                let mut shift = 0;
                while pos < data.len() {
                    let b = data[pos];
                    pos += 1;
                    len |= ((b & 0x7f) as usize) << shift;
                    if b & 0x80 == 0 {
                        break;
                    }
                    shift += 7;
                }
                if field_num == target_field && pos + len <= data.len() {
                    return Some(String::from_utf8_lossy(&data[pos..pos + len]).to_string());
                }
                pos += len;
            } else if wire_type == 0 {
                // Varint - skip
                while pos < data.len() && data[pos] & 0x80 != 0 {
                    pos += 1;
                }
                pos += 1;
            } else {
                // Unknown wire type, stop parsing
                break;
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
            Ok(varint_bytes + len as usize)
        }
        5 => {
            // 32-bit fixed
            if data.len() < 4 {
                anyhow::bail!("Buffer too short for 32-bit fixed");
            }
            Ok(4)
        }
        _ => anyhow::bail!("Unsupported wire type {}", wire_type),
    }
}

/// Extract full UUID from fetchGameRecord response
/// Response structure: Field 2 (head) contains Field 1 (uuid)
pub fn extract_full_uuid_from_record(data: &[u8]) -> Result<String> {
    let mut pos = 0;

    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type == 2 {
            // Length-delimited
            let (len, varint_bytes) = wrapper::decode_varint(&data[pos..])?;
            pos += varint_bytes;
            let len = len as usize;

            if field_num == 2 && pos + len <= data.len() {
                // Field 2 is head - parse nested message for uuid (field 1)
                let head_data = &data[pos..pos + len];
                if let Ok(uuid) = extract_uuid_from_head(head_data) {
                    return Ok(uuid);
                }
            }
            pos += len;
        } else {
            // Skip field based on wire type
            let skip = skip_field(&data[pos..], wire_type)?;
            pos += skip;
        }
    }
    anyhow::bail!("Full UUID not found in fetchGameRecord response")
}

fn extract_uuid_from_head(data: &[u8]) -> Result<String> {
    let mut pos = 0;

    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;

        if wire_type == 2 {
            // Length-delimited
            let (len, varint_bytes) = wrapper::decode_varint(&data[pos..])?;
            pos += varint_bytes;
            let len = len as usize;

            if field_num == 1 && pos + len <= data.len() {
                // Field 1 is uuid
                return Ok(String::from_utf8_lossy(&data[pos..pos + len]).to_string());
            }
            pos += len;
        } else {
            // Skip field based on wire type
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
}
