//! Bounded JSONL framing and OMP protocol-v2 chunk reassembly.

use std::io::BufRead;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;

use crate::HarnessError;

pub const MAX_FRAME: usize = 64 * 1024 * 1024;
pub const OMP_LINE_LIMIT: usize = 1024 * 1024;
const MAX_CHUNKS: u64 = 65_536;

pub fn read_frame(reader: &mut impl BufRead, limit: usize) -> Result<Option<Value>, HarnessError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|e| HarnessError::Protocol(format!("reading agent output failed: {e}")))?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(HarnessError::Protocol("agent output ended mid-frame".into()));
        }
        let (take, done) = match available.iter().position(|&byte| byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        if line.len() + take > limit {
            return Err(HarnessError::Protocol("agent frame exceeds limit".into()));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if done {
            break;
        }
    }
    let text = line.strip_suffix(b"\n").unwrap_or(&line);
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    if text.iter().all(u8::is_ascii_whitespace) {
        return read_frame(reader, limit);
    }
    serde_json::from_slice(text)
        .map(Some)
        .map_err(|e| HarnessError::Protocol(format!("agent output is not JSON: {e}")))
}

struct PendingChunk {
    id: String,
    count: u64,
    length: usize,
    next: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct Chunks {
    pending: Option<PendingChunk>,
}

impl Chunks {
    pub fn accept(&mut self, frame: Value) -> Result<Option<Value>, HarnessError> {
        if frame["type"] != "rpc_chunk" {
            if self.pending.is_some() {
                return Err(HarnessError::Protocol("OMP chunk sequence interrupted".into()));
            }
            return Ok(Some(frame));
        }
        let id = frame["chunkId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| protocol("missing chunkId"))?;
        let index = frame["index"].as_u64().ok_or_else(|| protocol("missing index"))?;
        let count = frame["count"]
            .as_u64()
            .filter(|count| (1..=MAX_CHUNKS).contains(count))
            .ok_or_else(|| protocol("invalid chunk count"))?;
        let length = frame["byteLength"]
            .as_u64()
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length <= MAX_FRAME)
            .ok_or_else(|| protocol("invalid or oversized byteLength"))?;
        let data = frame["data"]
            .as_str()
            .filter(|data| !data.is_empty())
            .ok_or_else(|| protocol("missing chunk data"))?;
        let data = STANDARD
            .decode(data)
            .map_err(|_| protocol("chunk data is not base64"))?;
        let mut pending = match self.pending.take() {
            None if index == 0 => PendingChunk {
                id: id.into(),
                count,
                length,
                next: 0,
                bytes: Vec::with_capacity(length),
            },
            Some(pending)
                if pending.id == id
                    && pending.count == count
                    && pending.length == length
                    && pending.next == index => pending,
            _ => return Err(protocol("chunk order or metadata is not contiguous")),
        };
        if pending.bytes.len() + data.len() > pending.length {
            return Err(protocol("chunk data exceeds declared length"));
        }
        pending.bytes.extend_from_slice(&data);
        pending.next += 1;
        if pending.next < pending.count {
            self.pending = Some(pending);
            return Ok(None);
        }
        if pending.bytes.len() != pending.length {
            return Err(protocol("chunk length does not match declaration"));
        }
        serde_json::from_slice(&pending.bytes)
            .map(Some)
            .map_err(|e| protocol(format!("reassembled frame is not JSON: {e}")))
    }

    pub fn finish(&mut self) -> Result<(), HarnessError> {
        if self.pending.take().is_some() {
            Err(protocol("EOF while an OMP chunk sequence was pending"))
        } else {
            Ok(())
        }
    }
}

fn protocol(message: impl Into<String>) -> HarnessError {
    HarnessError::Protocol(message.into())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::json;

    use super::*;

    #[test]
    fn bounded_frames_distinguish_blank_lines_and_cutoff() {
        let mut input = Cursor::new(b"{\"a\":1}\r\n\n{\"b\":2}\n".to_vec());
        assert_eq!(read_frame(&mut input, 64).unwrap(), Some(json!({"a": 1})));
        assert_eq!(read_frame(&mut input, 64).unwrap(), Some(json!({"b": 2})));
        assert_eq!(read_frame(&mut input, 64).unwrap(), None);
        let mut cut = Cursor::new(b"{\"a\":".to_vec());
        assert!(read_frame(&mut cut, 64).is_err());
    }

    #[test]
    fn chunks_reassemble_and_reject_gaps_or_eof() {
        let payload = br#"{"type":"message_end","n":1}"#;
        let (first, second) = payload.split_at(10);
        let chunk = |index, data: &[u8]| {
            json!({
                "type": "rpc_chunk", "chunkId": "c", "index": index,
                "count": 2, "byteLength": payload.len(),
                "data": STANDARD.encode(data)
            })
        };
        let mut chunks = Chunks::default();
        assert!(chunks.accept(chunk(0, first)).unwrap().is_none());
        assert_eq!(chunks.accept(chunk(1, second)).unwrap(), Some(json!({"type":"message_end","n":1})));
        let mut gap = Chunks::default();
        assert!(gap.accept(chunk(1, second)).is_err());
        let mut eof = Chunks::default();
        eof.accept(chunk(0, first)).unwrap();
        assert!(eof.finish().is_err());
    }
}
