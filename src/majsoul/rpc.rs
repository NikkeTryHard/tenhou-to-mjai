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

    pub fn oauth2_login(access_token: &str, version: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        // Field 4: access_token
        buf.push(0x22);
        encode_varint(&mut buf, access_token.len() as u64);
        buf.extend_from_slice(access_token.as_bytes());
        // Field 8: client_version_string
        let version_str = format!("web-{}", version);
        buf.push(0x42);
        encode_varint(&mut buf, version_str.len() as u64);
        buf.extend_from_slice(version_str.as_bytes());
        // Field 10: currency_platforms
        buf.push(0x50);
        buf.push(0x02);
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

    pub async fn login(&self, access_token: &str, version: &str) -> Result<Vec<u8>> {
        let request = requests::oauth2_login(access_token, version);
        let response = self.call(".lq.Lobby.oauth2Login", &request).await?;
        if response.len() >= 2 && response[0] == 0x08 && response[1] != 0 {
            anyhow::bail!("Login failed with error code: {}", response[1]);
        }
        info!("Login successful");
        Ok(response)
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
