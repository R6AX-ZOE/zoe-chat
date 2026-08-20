//! 前端"重启服务"命令的宿主实现(经 daemon 钩子触发,见 lib.rs setup 第 1 步)。
//!
//! Android:经 JNI 调 `MainActivity.restartApp()` —— 重建 Activity + 杀进程
//! (Android 标准冷启动方式)。冷启动后守护进程以 PIN 用户无 `--pin` 启动 →
//! 锁定模式 → 锁定屏立即出现(设置 PIN 后用户即可看到完整流程)。
//! 桌面/其它平台:仅内嵌 Tauri 场景,直接返回不可用。

#[cfg(target_os = "android")]
pub fn app_restart() -> Result<(), String> {
    use jni::objects::JObject;
    use jni::JavaVM;
    use ndk_context::android_context;

    let ctx = android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("java vm: {e}"))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("attach jvm: {e}"))?;
    // 用活动运行时类(而非 find_class)调用静态方法 —— 规避 AppClassLoader
    // 下 find_class 可能失败的问题;class 引用随活动全局句柄,勿让 JObject Drop 释放
    let activity = unsafe { JObject::from_raw(ctx.context() as jni::sys::jobject) };
    let class = env
        .get_object_class(&activity)
        .map_err(|e| format!("get class: {e}"))?;
    let _raw = activity.into_raw();
    env.call_static_method(class, "restartApp", "()V", &[])
        .map_err(|e| format!("call restartApp: {e}"))?;
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn app_restart() -> Result<(), String> {
    Err("restart is only available on the Android app".to_string())
}