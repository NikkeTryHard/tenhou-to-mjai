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

    fn decode_varint(buf: &[u8]) -> Result<(u64, usize)> {
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
                return self.login_with_access_token(token, version, server).await;
            }
        } else {
            // No uid, try as direct access_token
            return self.login_with_access_token(token, version, server).await;
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
        self.login_with_access_token(&access_token, version, "en").await
    }

    async fn login_with_access_token(&self, access_token: &str, version: &str, server: &str) -> Result<Vec<u8>> {
        // Step 1: oauth2Check to validate token
        info!("Checking token with oauth2Check (server: {}, type: {})", server, requests::auth_type_for_server(server));
        let check_request = requests::oauth2_check(access_token, server);
        let check_response = self.call(".lq.Lobby.oauth2Check", &check_request).await?;
        debug!("oauth2Check response: {:02x?}", &check_response[..std::cmp::min(50, check_response.len())]);

        // For CN/TW/HK with JWT token: exchange via oauth2Auth first
        // JWT starts with "eyJ", UUID tokens are 36 chars
        let is_jwt = access_token.starts_with("eyJ") && access_token.len() > 100;

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
                // CN server error 109: two-step OAuth - extract liqi_access_token and retry
                if code == 109 {
                    if let Some(liqi_token) = Self::extract_liqi_token(&response) {
                        info!("TW/HK two-step OAuth: got liqi_access_token, retrying with type 16...");
                        let retry_request = requests::oauth2_login_with_type(&liqi_token, version, 16);
                        let retry_response = self.call(".lq.Lobby.oauth2Login", &retry_request).await?;
                        info!("oauth2Login retry response ({} bytes)", retry_response.len());
                        // Check retry response for errors
                        if retry_response.len() >= 4 && retry_response[0] == 0x0a && retry_response[2] == 0x08 && retry_response[3] != 0 {
                            anyhow::bail!("CN login retry failed (error {})", retry_response[3]);
                        }
                        info!("Login successful (CN two-step)");
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

    pub async fn close(self) -> Result<()> {
        self.write.lock().await.close().await?;
        Ok(())
    }
}
