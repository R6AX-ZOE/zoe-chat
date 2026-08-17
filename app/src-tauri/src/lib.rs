//! zoe-mobile:Tauri 2 移动端,嵌入 zoe-core + zoe-transport(ble-mobile)。
//!
//! 复用原则:移动侧不复制 Linux 侧逻辑 —— 帧构造/解析/分片/去重/TTL/MeshOverlay
//! 全部来自 `zoe-transport::ble`(与 zoe-cli/daemon 同一份代码,feature ble-mobile 门控);
//! 本 crate 只做命令层薄封装。

use zoe_transport::ble::{frame_chunks, parse_frame};

mod bridge;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            app_info,
            hello_frame,
            bridge::start_bridge,
            bridge::stop_bridge,
            bridge::set_echo,
            bridge::bridge_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running zoe-mobile");
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    app_version: String,
    zoe_core_version: String,
    frame_example_hex: String,
}

/// 版本信息 + 示例帧 hex(M0 验收:装机能显示版本+帧 hex)。
/// 注意:command 函数不要加 pub —— #[tauri::command] 会生成同名 `__cmd__*` 宏,
/// pub 触发其再导出,rustc 报 E0255 defined multiple times(实测)。
#[tauri::command]
fn app_info() -> AppInfo {
    let frames = frame_chunks([0x42; 8], 3, b"zoe-mobile").expect("frame_chunks");
    AppInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        zoe_core_version: zoe_core::VERSION.to_string(),
        frame_example_hex: hex::encode(&frames[0]),
    }
}

/// 帧 构造→解析 回路冒烟(M0 验收)。复用 Linux 侧同一份 frame_chunks/parse_frame。
#[tauri::command]
fn hello_frame() -> String {
    let data: &[u8] = b"zoe-mobile hello";
    let frames = frame_chunks([0x42; 8], 3, data).expect("frame_chunks");
    let (hdr, payload) = parse_frame(&frames[0]).expect("parse_frame");
    format!(
        "构造→解析 OK\n帧 hex: {}\nmsg_id={} ttl={} 片 {}/{} 数据 {}B",
        hex::encode(&frames[0]),
        hex::encode(hdr.msg_id),
        hdr.ttl,
        hdr.chunk_idx + 1,
        hdr.total,
        payload.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_frame_roundtrip() {
        let out = hello_frame();
        assert!(out.contains("构造→解析 OK"), "{out}");
        assert!(out.contains("ttl=3"), "{out}");
        assert!(out.contains("msg_id=4242424242424242"), "{out}");
    }

    #[test]
    fn app_info_reports_versions() {
        let info = app_info();
        assert_eq!(info.zoe_core_version, zoe_core::VERSION);
        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
        assert!(!info.frame_example_hex.is_empty());
        // 与 zoe-transport 共享测试一致:帧首字节 = magic 0x5A
        assert!(info.frame_example_hex.starts_with("5a"));
    }
}
