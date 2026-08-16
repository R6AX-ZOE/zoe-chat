//! 统一信封(Envelope):所有传输层搬运的传输无关格式。
//!
//! 布局(docs/envelope.md):version | flags | msg_type | group_id_len | group_id |
//! epoch(u32 BE) | sender(u32 BE) | seq(u64 BE) | payload_len(u32 BE) | payload | hash(32)
//! `hash = SHA-256(前序全部字节)`,解码时验证,作为去重键与完整性校验。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const VERSION: u8 = 0x01;

pub const FLAG_ACK: u8 = 0b0000_0001;
pub const FLAG_MULTIPATH: u8 = 0b0000_0010;
pub const FLAG_FRAGMENTED: u8 = 0b0000_0100;

pub const MSG_PRIVATE: u8 = 0;
pub const MSG_PROPOSAL: u8 = 1;
pub const MSG_COMMIT: u8 = 2;
pub const MSG_WELCOME: u8 = 3;
pub const MSG_KEY_PACKAGE: u8 = 4;
pub const MSG_CONTROL: u8 = 5;

pub const HASH_LEN: usize = 32;
pub const MAX_GROUP_ID_LEN: usize = 255;
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024; // 16 MiB 上限,防超长分配

#[derive(Error, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("truncated envelope")]
    Truncated,
    #[error("invalid version {0}")]
    BadVersion(u8),
    #[error("invalid group_id length {0}")]
    BadGroupIdLen(usize),
    #[error("invalid payload length {0}")]
    BadPayloadLen(usize),
    #[error("hash mismatch")]
    HashMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub flags: u8,
    pub msg_type: u8,
    pub group_id: Vec<u8>,
    pub epoch: u32,
    pub sender: u32,
    pub seq: u64,
    pub payload: Vec<u8>,
    pub hash: [u8; HASH_LEN],
}

impl Envelope {
    /// 构造信封并计算 hash。
    pub fn new(
        flags: u8,
        msg_type: u8,
        group_id: Vec<u8>,
        epoch: u32,
        sender: u32,
        seq: u64,
        payload: Vec<u8>,
    ) -> Self {
        let body = Self::encode_body(flags, msg_type, &group_id, epoch, sender, seq, &payload);
        let hash: [u8; HASH_LEN] = Sha256::digest(&body).into();
        Self {
            flags,
            msg_type,
            group_id,
            epoch,
            sender,
            seq,
            payload,
            hash,
        }
    }

    fn encode_body(
        flags: u8,
        msg_type: u8,
        group_id: &[u8],
        epoch: u32,
        sender: u32,
        seq: u64,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(1 + 1 + 1 + 1 + group_id.len() + 4 + 4 + 8 + 4 + payload.len());
        assert!(group_id.len() <= MAX_GROUP_ID_LEN, "group_id too long");
        assert!(payload.len() <= MAX_PAYLOAD_LEN, "payload too long");
        out.push(VERSION);
        out.push(flags);
        out.push(msg_type);
        out.push(group_id.len() as u8);
        out.extend_from_slice(group_id);
        out.extend_from_slice(&epoch.to_be_bytes());
        out.extend_from_slice(&sender.to_be_bytes());
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut body = Self::encode_body(
            self.flags,
            self.msg_type,
            &self.group_id,
            self.epoch,
            self.sender,
            self.seq,
            &self.payload,
        );
        body.extend_from_slice(&self.hash);
        body
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        let mut r = Reader::new(bytes);
        let version = r.u8()?;
        if version != VERSION {
            return Err(EnvelopeError::BadVersion(version));
        }
        let flags = r.u8()?;
        let msg_type = r.u8()?;
        let group_id_len = r.u8()? as usize;
        let group_id = r.bytes(group_id_len)?;
        let epoch = r.u32()?;
        let sender = r.u32()?;
        let seq = r.u64()?;
        let payload_len = r.u32()? as usize;
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(EnvelopeError::BadPayloadLen(payload_len));
        }
        let payload = r.bytes(payload_len)?;
        let hash: [u8; HASH_LEN] = r
            .bytes(HASH_LEN)?
            .try_into()
            .map_err(|_| EnvelopeError::Truncated)?;
        if !r.is_done() {
            return Err(EnvelopeError::Truncated);
        }
        let body = Self::encode_body(flags, msg_type, group_id, epoch, sender, seq, payload);
        let expect: [u8; HASH_LEN] = Sha256::digest(&body).into();
        if hash != expect {
            return Err(EnvelopeError::HashMismatch);
        }
        Ok(Self {
            flags,
            msg_type,
            group_id: group_id.to_vec(),
            epoch,
            sender,
            seq,
            payload: payload.to_vec(),
            hash,
        })
    }
}

/// 大端字节读取器。
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn u8(&mut self) -> Result<u8, EnvelopeError> {
        let b = self.bytes(1)?;
        Ok(b[0])
    }

    fn u32(&mut self) -> Result<u32, EnvelopeError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes(b.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, EnvelopeError> {
        let b = self.bytes(8)?;
        Ok(u64::from_be_bytes(b.try_into().unwrap()))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], EnvelopeError> {
        if self.buf.len() - self.pos < n {
            return Err(EnvelopeError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn is_done(&self) -> bool {
        self.pos == self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Envelope {
        Envelope::new(
            FLAG_MULTIPATH,
            MSG_PRIVATE,
            b"group-0001".to_vec(),
            3,
            7,
            42,
            b"mls ciphertext bytes".to_vec(),
        )
    }

    #[test]
    fn roundtrip() {
        let env = sample();
        let bytes = env.encode();
        let decoded = Envelope::decode(&bytes).unwrap();
        assert_eq!(decoded, env);
    }

    #[test]
    fn roundtrip_empty_payload() {
        let env = Envelope::new(0, MSG_CONTROL, vec![], 0, 0, 1, vec![]);
        assert_eq!(Envelope::decode(&env.encode()).unwrap(), env);
    }

    #[test]
    fn tampered_hash_rejected() {
        let mut bytes = sample().encode();
        let n = bytes.len();
        bytes[n - 1] ^= 0x01;
        assert_eq!(Envelope::decode(&bytes), Err(EnvelopeError::HashMismatch));
    }

    #[test]
    fn tampered_payload_rejected() {
        let mut bytes = sample().encode();
        let n = bytes.len();
        bytes[n - HASH_LEN - 1] ^= 0x01; // payload 末字节
        assert_eq!(Envelope::decode(&bytes), Err(EnvelopeError::HashMismatch));
    }

    #[test]
    fn truncation_rejected() {
        let bytes = sample().encode();
        for cut in 0..bytes.len() {
            assert_eq!(
                Envelope::decode(&bytes[..cut]),
                Err(EnvelopeError::Truncated),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut bytes = sample().encode();
        bytes.push(0);
        assert_eq!(Envelope::decode(&bytes), Err(EnvelopeError::Truncated));
    }

    #[test]
    fn unknown_version_rejected() {
        let mut bytes = sample().encode();
        bytes[0] = 0x02;
        assert!(matches!(
            Envelope::decode(&bytes),
            Err(EnvelopeError::BadVersion(2))
        ));
    }

    #[test]
    fn oversized_payload_len_rejected() {
        // 手工构造:伪造超大 payload_len
        let env = sample();
        let mut bytes = vec![VERSION, env.flags, env.msg_type, 0];
        bytes.extend_from_slice(&env.epoch.to_be_bytes());
        bytes.extend_from_slice(&env.sender.to_be_bytes());
        bytes.extend_from_slice(&env.seq.to_be_bytes());
        bytes.extend_from_slice(&(MAX_PAYLOAD_LEN as u32 + 1).to_be_bytes());
        assert!(matches!(
            Envelope::decode(&bytes),
            Err(EnvelopeError::BadPayloadLen(_))
        ));
    }

    #[test]
    fn hash_stability() {
        // 相同输入 → 相同 hash(去重键依赖)
        let a = sample();
        let b = sample();
        assert_eq!(a.hash, b.hash);
    }
}
