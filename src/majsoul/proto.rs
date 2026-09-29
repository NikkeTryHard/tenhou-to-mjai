//! Manual protobuf decoder for Majsoul game records.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::io::Read;

/// Decode a varint from buffer, return (value, `bytes_consumed`)
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
        if shift >= 64 {
            anyhow::bail!("Varint too long");
        }
    }
    Ok((value, pos))
}

/// Checked `u64` → `u32` for hostile wire values (field numbers, versions,
/// seats); out-of-range values bail instead of truncating.
pub fn u32_checked(v: u64, what: &'static str) -> Result<u32> {
    u32::try_from(v).map_err(|_| anyhow::anyhow!("{what} out of range: {v}"))
}

/// Checked `u64` → `usize` for wire lengths; out-of-range bails (the
/// bounds check after each site stays the real guard on 64-bit).
fn usize_checked(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| anyhow::anyhow!("length out of range: {v}"))
}

/// Decode a protobuf `int32` varint. Negatives arrive sign-extended to 64
/// bits, so low-32-bit reinterpretation is the decode, not a lossy cast.
#[allow(clippy::cast_possible_truncation)]
pub fn i32_from_varint(v: u64) -> i32 {
    v as i32
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

        let field_number = match u32_checked(tag >> 3, "field number") {
            Ok(n) => n,
            Err(e) => return Some(Err(e)),
        };
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
                        let len = match usize_checked(len) {
                            Ok(len) => len,
                            Err(e) => return Some(Err(e)),
                        };
                        let end = self.pos + len;
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
                return Some(Err(anyhow::anyhow!("Unknown wire type: {wire_type}")));
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

/// Decode packed repeated varints from length-delimited data
pub fn decode_packed_varints(data: &[u8]) -> Result<Vec<i32>> {
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (val, n) = decode_varint(&data[pos..])?;
        result.push(i32_from_varint(val));
        pos += n;
    }
    Ok(result)
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

/// Decoded `ResGameRecord`
#[derive(Debug)]
pub struct GameRecord {
    // Decoded for identity/debugging but never read downstream (callers use
    // `player_names`/`records`); underscore-prefixed until wired.
    pub _uuid: String,
    // u64 preserves the full wire-varint range; values fit Unix seconds so the DB boundary `as i64` cast is lossless.
    /// Unix seconds (DB boundary converts `as i64`).
    pub _start_time: u64,
    pub player_names: Vec<String>,
    pub records: Vec<RecordAction>,
}

/// Decode `ResGameRecord` from raw protobuf bytes
pub fn decode_game_record(raw: &[u8]) -> Result<GameRecord> {
    let mut uuid = String::new();
    let mut start_time = 0u64;
    let mut player_names = Vec::new();
    let mut compressed_data: Option<Vec<u8>> = None;
    let mut data_url: Option<String> = None;

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
                                anyhow::bail!("Game record error code: {code}");
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
                        2 if inner.wire_type == 0 => start_time = extract_varint(inner.data)?,
                        11 if inner.wire_type == 2 => {
                            // accounts (repeated PlayerAccount) - extract nickname (field 3)
                            for acct_field in FieldIterator::new(inner.data) {
                                let acct_field = acct_field?;
                                if acct_field.number == 3 && acct_field.wire_type == 2 {
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
                data_url = Some(extract_string(field.data));
            }
            _ => {}
        }
    }

    // Decompress and decode records
    let records = if let Some(data_bytes) = compressed_data {
        // Field 4 (data) is a Wrapper message: {name: string, data: bytes}
        // The wrapper's data contains GameDetailRecords
        let (_wrapper_name, wrapper_data) = decode_wrapper(&data_bytes)?;

        // Parse GameDetailRecords: {records: repeated bytes (field 1),
        //                           version: uint32 (field 2),
        //                           actions: repeated GameAction (field 3)}
        let mut version = 0u32;
        let mut raw_records: Vec<Vec<u8>> = Vec::new();
        let mut actions_data: Vec<Vec<u8>> = Vec::new();

        for field in FieldIterator::new(&wrapper_data) {
            let field = field?;
            match (field.number, field.wire_type) {
                (1, 2) => raw_records.push(field.data.to_vec()),
                (2, 0) => version = u32_checked(extract_varint(field.data)?, "version")?,
                (3, 2) => actions_data.push(field.data.to_vec()),
                _ => {}
            }
        }

        if version < 210_715 && !raw_records.is_empty() {
            // Old format: each record is a Wrapper (or gzipped Wrapper list)
            decode_old_format_records(&raw_records)?
        } else if !actions_data.is_empty() {
            // New format: each action has a `result` field (field 3) which is a Wrapper
            decode_new_format_actions(&actions_data)?
        } else {
            Vec::new()
        }
    } else if let Some(url) = data_url {
        anyhow::bail!("game record {uuid} has no inline data; data_url fallback not implemented: {url}");
    } else {
        Vec::new()
    };
    Ok(GameRecord {
        _uuid: uuid,
        _start_time: start_time,
        player_names,
        records,
    })
}

/// Decode old-format `GameDetailRecords` (version < 210715)
/// Each record in the list is a serialized Wrapper message, possibly gzipped
/// (magic `1f 8b` raws are gunzipped first via the shared gzip path).
fn decode_old_format_records(raw_records: &[Vec<u8>]) -> Result<Vec<RecordAction>> {
    let mut records = Vec::new();
    for raw in raw_records {
        // Gunzip `1f 8b` raws (wires the old gzip helper into this path).
        let buf: Vec<u8> = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
            let mut decoder = GzDecoder::new(&raw[..]);
            let mut decompressed = Vec::new();
            decoder
                .read_to_end(&mut decompressed)
                .context("Failed to decompress game records")?;
            decompressed
        } else {
            raw.clone()
        };
        // Each record bytes is a Wrapper {name: string, data: bytes}
        let (name, data) = decode_wrapper(&buf)?;
        if !name.is_empty() {
            records.push(RecordAction { name, data });
        }
    }
    Ok(records)
}

/// Decode new-format `GameDetailRecords` (version >= 210715)
/// Each action is a `GameAction` message with `result` field containing a Wrapper
fn decode_new_format_actions(actions_data: &[Vec<u8>]) -> Result<Vec<RecordAction>> {
    let mut records = Vec::new();
    for action_bytes in actions_data {
        // GameAction: {type: uint32 (field 1), result: bytes (field 3)}
        let mut result_data: Option<Vec<u8>> = None;
        for field in FieldIterator::new(action_bytes) {
            let field = field?;
            if field.number == 3 && field.wire_type == 2 {
                result_data = Some(field.data.to_vec());
            }
        }

        if let Some(result) = result_data {
            if !result.is_empty() {
                let (name, data) = decode_wrapper(&result)?;
                if !name.is_empty() {
                    records.push(RecordAction { name, data });
                }
            }
        }
    }
    Ok(records)
}

/// Decode a fetchGameRecordList-style response into
/// `(uuid, player_id, start_time, mode)` rows.
///
/// The exact wire schema for the list endpoint is not yet captured from live
/// traffic; this stub bails loudly rather than inventing field numbers.
/// Callers (fetch-public) wire inserts behind it so real parsing flows once
/// the schema lands.
pub fn decode_game_record_list(_raw: &[u8]) -> Result<Vec<(String, i64, i64, i32)>> {
    anyhow::bail!("fetchGameRecordList decoding not implemented: needs live response capture");
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

    #[test]
    fn test_decode_packed_varints_negative_int32() {
        // Protobuf `int32` negatives arrive sign-extended to 64 bits; the
        // decoder must reinterpret the low 32 bits (checked `try_from`
        // would wrongly reject them).
        let neg1000 = 0u64.wrapping_sub(1000);
        let mut buf = Vec::new();
        let mut v = neg1000;
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            buf.push(b);
            if v == 0 {
                break;
            }
        }
        assert_eq!(buf.len(), 10, "negative int32 must be 10-byte varint");
        assert_eq!(decode_packed_varints(&buf).unwrap(), vec![-1000]);
        assert_eq!(i32_from_varint(neg1000), -1000);
    }

    #[test]
    fn test_decode_packed_varints_nonzero() {
        // Packed deltas: e.g. [1000, -1000] as varints in one length-delimited blob.
        // Build via encode: use decode_varint round-trip through FieldIterator path.
        // Here we test decode_packed_varints directly with known bytes:
        // 1000 => E8 07, 1 => 01.
        let packed = vec![0xE8, 0x07, 0x01];
        let v = decode_packed_varints(&packed).unwrap();
        assert_eq!(v, vec![1000, 1]);
        assert!(v.iter().any(|&x| x != 0));
    }

    #[test]
    fn test_data_url_only_bails_loudly() {
        fn push_ld(buf: &mut Vec<u8>, bytes: &[u8]) {
            let mut n = bytes.len() as u64;
            loop {
                let mut b = (n & 0x7f) as u8;
                n >>= 7;
                if n != 0 {
                    b |= 0x80;
                }
                buf.push(b);
                if n == 0 {
                    break;
                }
            }
            buf.extend_from_slice(bytes);
        }
        let uuid = "test-uuid-123";
        let url = "https://example.com/data.gz";
        let mut head = vec![0x0a];
        push_ld(&mut head, uuid.as_bytes());
        let mut raw = vec![0x1a];
        push_ld(&mut raw, &head);
        raw.push(0x2a);
        push_ld(&mut raw, url.as_bytes());
        let err = decode_game_record(&raw).unwrap_err().to_string();
        assert!(err.contains(uuid), "missing uuid in: {err}");
        assert!(err.contains(url), "missing url in: {err}");
    }
}
