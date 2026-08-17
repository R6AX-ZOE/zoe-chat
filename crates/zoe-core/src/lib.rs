//! zoe-chat 核心库:身份、信封、MLS 会话、存储。
//!
//! 纯逻辑 + 本地 SQLite;传输与网络在 zoe-transport / zoe-daemon。

/// 本 crate 版本(CARGO_PKG_VERSION 编译期注入),供宿主(如 app/ 移动端)显示。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod envelope;
pub mod identity;
pub mod mls;
pub mod storage;
