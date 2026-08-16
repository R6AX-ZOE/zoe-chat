//! zoe-chat 核心库:身份、信封、MLS 会话、存储。
//!
//! 纯逻辑 + 本地 SQLite;传输与网络在 zoe-transport / zoe-daemon。

pub mod envelope;
pub mod identity;
pub mod mls;
pub mod storage;
