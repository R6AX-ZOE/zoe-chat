//! 消息内容格式:文件消息的结构化明文。
//!
//! 文本消息保持与既有版本一致的裸 UTF-8 明文;文件消息使用带魔数的
//! 结构化格式,与旧消息可共存(`decode_file` 对非文件明文返回 None):
//!
//! ```text
//! [0x02 magic][0x01 kind=file]
//! [u32 BE name_len][name(UTF-8)]
//! [u64 BE size]
//! [u32 BE mime_len][mime(ASCII/UTF-8)]
//! [u32 BE data_len][data]
//! ```
//!
//! 限制:文件名 ≤ 255 字节、mime ≤ 128 字节、文件 ≤ `FILE_MAX`(8 MiB)。
//! 小文件(≤ `FILE_AUTO_MAX` = 1 MiB)接收端自动落盘("自动下载")。

use thiserror::Error;

/// 结构化明文魔数。
pub const MAGIC: u8 = 0x02;
/// kind = 文件。
pub const KIND_FILE: u8 = 0x01;

/// 单条文件消息上限(8 MiB;信封载荷上限 16 MiB 之下留 MLS 开销余量)。
pub const FILE_MAX: usize = 8 * 1024 * 1024;
/// 小文件自动下载阈值(1 MiB):接收端解密后自动写入本地 files/ 目录。
pub const FILE_AUTO_MAX: usize = 1024 * 1024;

pub const MAX_NAME_LEN: usize = 255;
pub const MAX_MIME_LEN: usize = 128;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ContentError {
    #[error("file too large: {0} bytes (max {FILE_MAX})")]
    FileTooLarge(usize),
    #[error("bad file name length {0}")]
    BadNameLen(usize),
    #[error("bad mime length {0}")]
    BadMimeLen(usize),
    #[error("truncated file content")]
    Truncated,
}

/// 文件消息内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub data: Vec<u8>,
}

/// 编码文件消息明文。
pub fn encode_file(name: &str, mime: &str, data: &[u8]) -> Result<Vec<u8>, ContentError> {
    if data.len() > FILE_MAX {
        return Err(ContentError::FileTooLarge(data.len()));
    }
    let name_b = name.as_bytes();
    let mime_b = mime.as_bytes();
    if name_b.len() > MAX_NAME_LEN {
        return Err(ContentError::BadNameLen(name_b.len()));
    }
    if mime_b.len() > MAX_MIME_LEN {
        return Err(ContentError::BadMimeLen(mime_b.len()));
    }
    let mut out = Vec::with_capacity(10 + name_b.len() + 8 + 4 + mime_b.len() + 4 + data.len());
    out.push(MAGIC);
    out.push(KIND_FILE);
    out.extend_from_slice(&(name_b.len() as u32).to_be_bytes());
    out.extend_from_slice(name_b);
    out.extend_from_slice(&(data.len() as u64).to_be_bytes());
    out.extend_from_slice(&(mime_b.len() as u32).to_be_bytes());
    out.extend_from_slice(mime_b);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

/// 解码文件消息明文;非结构化/非文件明文返回 None(文本消息走裸 UTF-8)。
pub fn decode_file(pt: &[u8]) -> Option<FileContent> {
    if pt.len() < 2 || pt[0] != MAGIC || pt[1] != KIND_FILE {
        return None;
    }
    let mut pos = 2usize;
    let name_len = read_u32(pt, &mut pos)? as usize;
    if name_len > MAX_NAME_LEN {
        return None;
    }
    let name = std::str::from_utf8(pt.get(pos..pos + name_len)?)
        .ok()?
        .to_string();
    pos += name_len;
    let size = read_u64(pt, &mut pos)?;
    let mime_len = read_u32(pt, &mut pos)? as usize;
    if mime_len > MAX_MIME_LEN {
        return None;
    }
    let mime = std::str::from_utf8(pt.get(pos..pos + mime_len)?)
        .ok()?
        .to_string();
    pos += mime_len;
    let data_len = read_u32(pt, &mut pos)? as usize;
    let data = pt.get(pos..pos + data_len)?.to_vec();
    if pos + data_len != pt.len() {
        return None;
    }
    // size 与 data_len 双写校验(冗余字段一致性)
    if size != data_len as u64 {
        return None;
    }
    Some(FileContent {
        name,
        mime,
        size,
        data,
    })
}

fn read_u32(b: &[u8], pos: &mut usize) -> Option<u32> {
    let s = b.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_be_bytes(s.try_into().ok()?))
}

fn read_u64(b: &[u8], pos: &mut usize) -> Option<u64> {
    let s = b.get(*pos..*pos + 8)?;
    *pos += 8;
    Some(u64::from_be_bytes(s.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_roundtrip() {
        let pt = encode_file("报告.pdf", "application/pdf", &[0x25, 0x50, 0x44, 0x46]).unwrap();
        let f = decode_file(&pt).unwrap();
        assert_eq!(f.name, "报告.pdf");
        assert_eq!(f.mime, "application/pdf");
        assert_eq!(f.size, 4);
        assert_eq!(f.data, vec![0x25, 0x50, 0x44, 0x46]);
    }

    #[test]
    fn empty_file_roundtrip() {
        let pt = encode_file("empty.bin", "", &[]).unwrap();
        let f = decode_file(&pt).unwrap();
        assert_eq!(f.name, "empty.bin");
        assert_eq!(f.size, 0);
        assert!(f.data.is_empty());
    }

    #[test]
    fn text_plaintext_is_not_file() {
        assert!(decode_file(b"hello world").is_none());
        assert!(decode_file(b"").is_none());
    }

    #[test]
    fn truncated_rejected() {
        let pt = encode_file("a.txt", "text/plain", b"abc").unwrap();
        for cut in 0..pt.len() {
            assert_eq!(decode_file(&pt[..cut]), None, "cut at {cut}");
        }
    }

    #[test]
    fn tampered_size_rejected() {
        let pt = encode_file("a.txt", "text/plain", b"abc").unwrap();
        let mut bad = pt.clone();
        // 篡改 size 字段(magic+kind 后:name_len+name+size 偏移),与 data_len 不一致即拒绝
        let size_pos = 2 + 4 + 5; // "a.txt"
        bad[size_pos] ^= 0x01;
        assert!(decode_file(&bad).is_none());
    }

    #[test]
    fn oversize_rejected() {
        let data = vec![0u8; FILE_MAX + 1];
        assert_eq!(
            encode_file("big.bin", "application/octet-stream", &data),
            Err(ContentError::FileTooLarge(FILE_MAX + 1))
        );
    }
}
