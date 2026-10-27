//! Mux frames (§8): each Noise transport plaintext is exactly one frame,
//! `type u8 ‖ stream_id u32 BE ‖ payload`.

use serde::{Deserialize, Serialize};

pub const OPEN: u8 = 0x01;
pub const DATA: u8 = 0x02;
pub const FIN: u8 = 0x03;
pub const RESET: u8 = 0x04;
pub const RESPONSE: u8 = 0x05;
pub const WINDOW: u8 = 0x06;
pub const PING: u8 = 0x07;
pub const PONG: u8 = 0x08;
pub const GOAWAY: u8 = 0x09;

/// RESET codes (§12).
pub mod reset {
    pub const CANCEL: u32 = 0;
    pub const REFUSED: u32 = 1;
    pub const PROTOCOL: u32 = 2;
    pub const FLOW_CONTROL: u32 = 3;
    pub const INTERNAL: u32 = 4;
}

/// GOAWAY codes (§12).
pub mod goaway {
    pub const NORMAL: u32 = 0;
    pub const REVOKED: u32 = 1;
    pub const PROTOCOL: u32 = 2;
    pub const LIFETIME: u32 = 3;
    pub const SHUTTING_DOWN: u32 = 4;
}

pub const HEADER_LEN: usize = 5;
/// Max request/response head (§8).
pub const MAX_HEAD: usize = 16 * 1024;
/// Max DATA payload (§8).
pub const MAX_DATA: usize = 16_384;
/// Initial per-stream, per-direction send window (§8).
pub const INITIAL_WINDOW: u32 = 262_144;
/// Concurrent streams per session (§8).
pub const MAX_STREAMS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub ty: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortFrame;

impl Frame {
    pub fn new(ty: u8, stream: u32, payload: Vec<u8>) -> Self {
        Self { ty, stream, payload }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push(self.ty);
        out.extend_from_slice(&self.stream.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self, ShortFrame> {
        if b.len() < HEADER_LEN {
            return Err(ShortFrame);
        }
        Ok(Self { ty: b[0], stream: u32::from_be_bytes([b[1], b[2], b[3], b[4]]), payload: b[HEADER_LEN..].to_vec() })
    }

    pub fn open(stream: u32, head: &RequestHead) -> Self {
        Self::new(OPEN, stream, serde_json::to_vec(head).expect("head"))
    }
    pub fn response(stream: u32, head: &ResponseHead) -> Self {
        Self::new(RESPONSE, stream, serde_json::to_vec(head).expect("head"))
    }
    pub fn data(stream: u32, bytes: &[u8]) -> Self {
        Self::new(DATA, stream, bytes.to_vec())
    }
    pub fn fin(stream: u32) -> Self {
        Self::new(FIN, stream, Vec::new())
    }
    pub fn reset(stream: u32, code: u32, reason: &str) -> Self {
        Self::new(RESET, stream, code_payload(code, reason))
    }
    pub fn window(stream: u32, inc: u32) -> Self {
        Self::new(WINDOW, stream, inc.to_be_bytes().to_vec())
    }
    pub fn ping(data: [u8; 8]) -> Self {
        Self::new(PING, 0, data.to_vec())
    }
    pub fn pong(data: &[u8]) -> Self {
        Self::new(PONG, 0, data.to_vec())
    }
    pub fn goaway(code: u32, reason: &str) -> Self {
        Self::new(GOAWAY, 0, code_payload(code, reason))
    }

    /// The `u32` at the start of a RESET / GOAWAY / WINDOW payload.
    pub fn code(&self) -> Option<u32> {
        let b: [u8; 4] = self.payload.get(..4)?.try_into().ok()?;
        Some(u32::from_be_bytes(b))
    }
}

fn code_payload(code: u32, reason: &str) -> Vec<u8> {
    let mut p = code.to_be_bytes().to_vec();
    p.extend_from_slice(reason.as_bytes());
    p
}

/// Request head (OPEN payload, §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// Response head (RESPONSE payload, §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// Hop-by-hop headers that MUST NOT cross the mux (§9).
pub fn is_hop_by_hop(name: &str) -> bool {
    matches!(name, "connection" | "keep-alive" | "transfer-encoding" | "te" | "upgrade" | "host") || name.starts_with("proxy-")
}
