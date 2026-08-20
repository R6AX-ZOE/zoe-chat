//! 统一自绘 SVG 图标集:24×24 viewBox、stroke 1.5、圆角线帽/线接。
//! 设计约束:禁止使用 emoji 作图标。装饰图标带 aria-hidden。

use leptos::prelude::InnerHtmlAttribute;
use leptos::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Send,
    Plus,
    Back,
    Gear,
    Sun,
    Moon,
    Monitor,
    Shield,
    Users,
    Chat,
    Copy,
    Check,
    X,
    Lock,
    Key,
    Radio,
    Globe,
    Bluetooth,
    Trash,
    Dots,
    Qr,
    Download,
    Ban,
    Verify,
    Power,
    Logout,
    Paperclip,
    Restart,
}

impl Icon {
    fn paths(self) -> &'static str {
        match self {
            Icon::Send => "<path d=\"M21 3 10.5 13.5\"/><path d=\"M21 3 14 21l-3.5-7.5L3 10 21 3Z\"/>",
            Icon::Plus => "<path d=\"M12 5v14\"/><path d=\"M5 12h14\"/>",
            Icon::Back => "<path d=\"M19 12H5\"/><path d=\"m12 19-7-7 7-7\"/>",
            Icon::Gear => "<circle cx=\"12\" cy=\"12\" r=\"3.2\"/><path d=\"M12 2v2.2M12 19.8V22M4.9 4.9l1.6 1.6M17.5 17.5l1.6 1.6M2 12h2.2M19.8 12H22M4.9 19.1l1.6-1.6M17.5 6.5l1.6-1.6\"/>",
            Icon::Sun => "<circle cx=\"12\" cy=\"12\" r=\"4\"/><path d=\"M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4\"/>",
            Icon::Moon => "<path d=\"M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z\"/>",
            Icon::Monitor => "<rect x=\"2.5\" y=\"4\" width=\"19\" height=\"12.5\" rx=\"2\"/><path d=\"M8.5 20.5h7M12 16.5v4\"/>",
            Icon::Shield => "<path d=\"M12 3 20 6.6v5.2c0 4.3-3.3 7.5-8 9.2-4.7-1.7-8-4.9-8-9.2V6.6L12 3Z\"/><path d=\"m8.8 11.8 2.4 2.4 4.2-4.4\"/>",
            Icon::Users => "<circle cx=\"9\" cy=\"8\" r=\"3.4\"/><path d=\"M3.2 20a5.8 5.8 0 0 1 11.6 0\"/><circle cx=\"17\" cy=\"9.4\" r=\"2.6\"/><path d=\"M15.4 15.6a4.6 4.6 0 0 1 5.4 4.4\"/>",
            Icon::Chat => "<path d=\"M21 11.6a8.4 8.4 0 0 1-12.4 7.4L3 21l2-5.6A8.4 8.4 0 1 1 21 11.6Z\"/>",
            Icon::Copy => "<rect x=\"9\" y=\"9\" width=\"11\" height=\"11\" rx=\"2\"/><path d=\"M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1\"/>",
            Icon::Check => "<path d=\"m4.5 12.5 5 5L19.5 6.5\"/>",
            Icon::X => "<path d=\"M6 6l12 12M18 6 6 18\"/>",
            Icon::Lock => "<rect x=\"5\" y=\"11\" width=\"14\" height=\"9\" rx=\"2\"/><path d=\"M8 11V8a4 4 0 0 1 8 0v3\"/>",
            Icon::Key => "<circle cx=\"8\" cy=\"15\" r=\"4\"/><path d=\"m10.8 12.2 9.2-9.2M16 6.5l2.5 2.5M12.8 9.2l2.5 2.5\"/>",
            Icon::Radio => "<circle cx=\"12\" cy=\"12\" r=\"2\"/><path d=\"M16.2 7.8a6 6 0 0 1 0 8.4M7.8 7.8a6 6 0 0 0 0 8.4M19 5a10 10 0 0 1 0 14M5 5a10 10 0 0 0 0 14\"/>",
            Icon::Globe => "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18\"/>",
            Icon::Bluetooth => "<path d=\"m7 7 10 10L12 21V3l5 4L7 17\"/>",
            Icon::Trash => "<path d=\"M3 6h18\"/><path d=\"M8 6V4.5A1.5 1.5 0 0 1 9.5 3h5A1.5 1.5 0 0 1 16 4.5V6\"/><path d=\"m6 6 1 13.5A2 2 0 0 0 9 21.5h6a2 2 0 0 0 2-2L18 6\"/>",
            Icon::Dots => "<circle cx=\"5.5\" cy=\"12\" r=\"1.4\"/><circle cx=\"12\" cy=\"12\" r=\"1.4\"/><circle cx=\"18.5\" cy=\"12\" r=\"1.4\"/>",
            Icon::Qr => "<rect x=\"3.5\" y=\"3.5\" width=\"7\" height=\"7\" rx=\"1\"/><rect x=\"13.5\" y=\"3.5\" width=\"7\" height=\"7\" rx=\"1\"/><rect x=\"3.5\" y=\"13.5\" width=\"7\" height=\"7\" rx=\"1\"/><path d=\"M13.5 13.5h2.5v2.5h-2.5zM18 13.5H20.5V16H18zM13.5 18H16v2.5h-2.5z\"/>",
            Icon::Download => "<path d=\"M12 3v12\"/><path d=\"m7 10 5 5 5-5\"/><path d=\"M4 21h16\"/>",
            Icon::Ban => "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"m5.6 5.6 12.8 12.8\"/>",
            Icon::Verify => "<rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"5\"/><path d=\"m8.5 12.5 2.5 2.5 4.5-5\"/>",
            Icon::Power => "<path d=\"M12 2.5V12\"/><path d=\"M6.3 5.3a8 8 0 1 0 11.4 0\"/>",
            Icon::Logout => "<path d=\"M9 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h4\"/><path d=\"M16 17l5-5-5-5\"/><path d=\"M21 12H9\"/>",
            Icon::Paperclip => "<path d=\"m20.6 11.8-8.6 8.5a5 5 0 0 1-7.1-7L14.4 3.6a3.4 3.4 0 0 1 4.8 4.8l-9.5 9.4a1.8 1.8 0 0 1-2.5-2.5l8.4-8.3\"/>",
            Icon::Restart => "<path d=\"M4.5 12a7.5 7.5 0 1 0 2.2-5.3\"/><path d=\"M4 3.5V8h4.5\"/>",
        }
    }
}

/// 内联 SVG 图标组件(当前颜色描边)。
/// 注:Leptos 的 svg 元素不支持 inner_html 属性,故以字符串构造完整 SVG 经
/// span 的 inner_html 注入(浏览器 HTML 解析器可正确解析内嵌 svg 子树)。
#[component]
pub fn IconView(icon: Icon, #[prop(default = 20)] size: u32) -> impl IntoView {
    let svg = format!(
        "<svg viewBox=\"0 0 24 24\" width=\"{size}\" height=\"{size}\" fill=\"none\" \
         stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\" aria-hidden=\"true\">{}</svg>",
        icon.paths()
    );
    view! { <span inner_html=svg></span> }
}
