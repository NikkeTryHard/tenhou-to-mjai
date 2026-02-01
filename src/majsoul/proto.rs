//! Manual protobuf decoder for Majsoul game records.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::io::Read;

/// Decode a varint from buffer, return (value, bytes_consumed)
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
        if shift >= 64 {
            anyhow::bail!("Varint too long");
        }
    }
    Ok((value, pos))
}

/// Parse a protobuf message, extracting fields by number
pub struct FieldIterator<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> FieldIterator<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

#[derive(Debug)]
pub struct Field<'a> {
    pub number: u32,
    pub wire_type: u8,
    pub data: &'a [u8],
}

impl<'a> Iterator for FieldIterator<'a> {
    type Item = Result<Field<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }

        // Read tag as varint (not just single byte for larger field numbers)
        let (tag, tag_len) = match decode_varint(&self.buf[self.pos..]) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        self.pos += tag_len;

        let field_number = (tag >> 3) as u32;
        let wire_type = (tag & 0x07) as u8;

        let data_start = self.pos;
        let data = match wire_type {
            0 => {
                // Varint
                match decode_varint(&self.buf[self.pos..]) {
                    Ok((_, n)) => {
                        self.pos += n;
                        &self.buf[data_start..self.pos]
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
            1 => {
                // Fixed64
                if self.pos + 8 > self.buf.len() {
                    return Some(Err(anyhow::anyhow!("Buffer overflow in fixed64")));
                }
                self.pos += 8;
                &self.buf[data_start..self.pos]
            }
            2 => {
                // Length-delimited
                match decode_varint(&self.buf[self.pos..]) {
                    Ok((len, n)) => {
                        self.pos += n;
                        let end = self.pos + len as usize;
                        if end > self.buf.len() {
                            return Some(Err(anyhow::anyhow!("Buffer overflow in length-delimited")));
                        }
                        let data = &self.buf[self.pos..end];
                        self.pos = end;
                        data
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
            5 => {
                // Fixed32
                if self.pos + 4 > self.buf.len() {
                    return Some(Err(anyhow::anyhow!("Buffer overflow in fixed32")));
                }
                self.pos += 4;
                &self.buf[data_start..self.pos]
            }
            _ => {
                return Some(Err(anyhow::anyhow!("Unknown wire type: {}", wire_type)));
            }
        };

        Some(Ok(Field {
            number: field_number,
            wire_type,
            data,
        }))
    }
}

/// Extract a string field from length-delimited data
pub fn extract_string(data: &[u8]) -> String {
    String::from_utf8_lossy(data).to_string()
}

/// Extract a varint as u64 from varint-encoded data
pub fn extract_varint(data: &[u8]) -> Result<u64> {
    let (val, _) = decode_varint(data)?;
    Ok(val)
}

/// Decode Wrapper message: {name: string (field 1), data: bytes (field 2)}
pub fn decode_wrapper(buf: &[u8]) -> Result<(String, Vec<u8>)> {
    let mut name = String::new();
    let mut data = Vec::new();

    for field in FieldIterator::new(buf) {
        let field = field?;
        match field.number {
            1 if field.wire_type == 2 => name = extract_string(field.data),
            2 if field.wire_type == 2 => data = field.data.to_vec(),
            _ => {}
        }
    }

    Ok((name, data))
}

/// A single game action (decoded from Wrapper)
#[derive(Debug, Clone)]
pub struct RecordAction {
    pub name: String,
    pub data: Vec<u8>,
}

/// Decoded ResGameRecord
#[derive(Debug)]
pub struct GameRecord {
    pub uuid: String,
    pub start_time: u32,
    pub player_names: Vec<String>,
    pub records: Vec<RecordAction>,
}

/// Decode ResGameRecord from raw protobuf bytes
pub fn decode_game_record(raw: &[u8]) -> Result<GameRecord> {
    let mut uuid = String::new();
    let mut start_time = 0u32;
    let mut player_names = Vec::new();
    let mut compressed_data: Option<Vec<u8>> = None;

    for field in FieldIterator::new(raw) {
        let field = field?;
        match field.number {
            1 if field.wire_type == 2 => {
                // Error message - check if non-empty
                if !field.data.is_empty() {
                    for inner in FieldIterator::new(field.data) {
                        let inner = inner?;
                        if inner.number == 1 && inner.wire_type == 0 {
                            let code = extract_varint(inner.data)?;
                            if code != 0 {
                                anyhow::bail!("Game record error code: {}", code);
                            }
                        }
                    }
                }
            }
            3 if field.wire_type == 2 => {
                // head (GameRecordHeader)
                for inner in FieldIterator::new(field.data) {
                    let inner = inner?;
                    match inner.number {
                        1 if inner.wire_type == 2 => uuid = extract_string(inner.data),
                        2 if inner.wire_type == 0 => start_time = extract_varint(inner.data)? as u32,
                        4 if inner.wire_type == 2 => {
                            // accounts (repeated PlayerAccount) - extract nickname (field 2)
                            for acct_field in FieldIterator::new(inner.data) {
                                let acct_field = acct_field?;
                                if acct_field.number == 2 && acct_field.wire_type == 2 {
                                    player_names.push(extract_string(acct_field.data));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            4 if field.wire_type == 2 => {
                // data (compressed GameDetailRecords)
                compressed_data = Some(field.data.to_vec());
            }
            5 if field.wire_type == 2 => {
                // data_url (string) - alternative way to get data
                // We skip this for now, as we expect inline data
            }
            _ => {}
        }
    }

    // Decompress and decode records
    let records = if let Some(compressed) = compressed_data {
        decode_game_detail_records(&compressed)?
    } else {
        Vec::new()
    };

    Ok(GameRecord {
        uuid,
        start_time,
        player_names,
        records,
    })
}

/// Decode GameDetailRecords (gzipped, contains repeated Wrapper messages)
fn decode_game_detail_records(compressed: &[u8]) -> Result<Vec<RecordAction>> {
    // Decompress
    let mut decoder = GzDecoder::new(compressed);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .context("Failed to decompress game records")?;

    let mut records = Vec::new();

    // Parse the decompressed protobuf
    for field in FieldIterator::new(&decompressed) {
        let field = field?;
        if field.number == 1 && field.wire_type == 2 {
            // Each record is a Wrapper message
            let (name, data) = decode_wrapper(field.data)?;
            records.push(RecordAction { name, data });
        }
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_varint() {
        assert_eq!(decode_varint(&[0x00]).unwrap(), (0, 1));
        assert_eq!(decode_varint(&[0x01]).unwrap(), (1, 1));
        assert_eq!(decode_varint(&[0x7f]).unwrap(), (127, 1));
        assert_eq!(decode_varint(&[0x80, 0x01]).unwrap(), (128, 2));
        assert_eq!(decode_varint(&[0xac, 0x02]).unwrap(), (300, 2));
    }

    #[test]
    fn test_decode_wrapper() {
        // Field 1 (string "test"): 0a 04 t e s t
        // Field 2 (bytes [1,2,3]): 12 03 01 02 03
        let buf = vec![0x0a, 0x04, b't', b'e', b's', b't', 0x12, 0x03, 0x01, 0x02, 0x03];
        let (name, data) = decode_wrapper(&buf).unwrap();
        assert_eq!(name, "test");
        assert_eq!(data, vec![1, 2, 3]);
    }
}
