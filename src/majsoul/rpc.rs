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
    /// Field numbers from protobuf: account=1, password=2, device=4, random_key=5,
    /// gen_access_token=7, currency_platforms=8, client_version_string=11
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

    /// Build ReqRequestConnection for route handshake (required before login)
    /// Field numbers from protobuf: type=2, route_id=3, timestamp=4
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

    /// Build ReqHeartbeat for Route.heartbeat
    pub fn build_heartbeat() -> Vec<u8> {
        let mut buf = Vec::new();
        encode_varint_field(&mut buf, 1, 0);  // delay
        encode_varint_field(&mut buf, 2, 0);  // no_operation_counter
        encode_varint_field(&mut buf, 3, 11); // platform (Web)
        encode_varint_field(&mut buf, 4, 0);  // network_quality
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

        debug!("Connecting to {}", endpoint);
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
                        debug!("WebSocket closed");
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

        debug!("Connected to Majsoul gateway");
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

    pub async fn fetch_game_record(&self, uuid: &str, version: &str) -> Result<Vec<u8>> {
        let request = requests::fetch_game_record(uuid, version);
        let response = self.call(".lq.Lobby.fetchGameRecord", &request).await?;
        // Check for error: direct (08 XX) or nested (0a LL 08 XX)
        if let Some(code) = Self::extract_error_code(&response) {
            if code != 0 {
                anyhow::bail!("fetchGameRecord error {}: {}", code, uuid);
            }
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

    /// Perform route connection handshake (required before login)
    pub async fn route_connect(&self, route_id: &str) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        debug!("Sending route connection (route_id: {}, timestamp: {})", route_id, timestamp);

        let request = requests::build_request_connection(route_id, timestamp);
        let response = self.call(".lq.Route.requestConnection", &request).await?;

        // Check for error
        if response.len() >= 4 && response[0] == 0x0a && response[2] == 0x08 {
            let err_code = response[3];
            if err_code != 0 {
                anyhow::bail!("Route connection failed (error {})", err_code);
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
        let hb_response = self.call(".lq.Lobby.heatbeat", &[0x08, 0x00]).await?;
        debug!("Heartbeat response: {} bytes", hb_response.len());

        let password_hash = hash_password(password);
        let random_key = Uuid::new_v4().to_string();
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
                anyhow::bail!("CN native login failed with error code: {}", error_code);
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
