//! Windows BLE 驱动:btleplug 0.12(central)+ windows-rs 广告发布器。
//! feature `ble-windows`,仅 Windows 编译。
//!
//! 角色能力:
//! - 主动连接(central):扫描 + GATT 读写/通知(btleplug)—— 真机联调主路线;
//! - 广播:BluetoothLEAdvertisementPublisher 在**未打包的桌面进程**中不可用
//!   (该 API 需要包标识中的 bluetooth 能力声明,Start() 抛 0x80070057
//!   E_INVALIDARG;适配器/无线电/载荷均正常时同样失败,已用 ble diag 验证)。
//!   若确需 Windows 广播,只能走 MSIX 打包 + 管理员信任证书路线(本项目不提供);
//! - 被连接(GATT server)同样需 UWP(GattServiceProvider),不可用。
//! 真机联调拓扑见 docs/termux-ble.md:手机 nRF Connect 模拟 peripheral,
//! Windows `zoe-cli ble scan/connect` 或电脑 Chrome Web Bluetooth 做 central。

#![cfg(all(feature = "ble-windows", windows))]

use std::sync::Mutex;
use std::time::Duration;

use btleplug::api::{
    Central, Characteristic as BtCharacteristic, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use windows::Devices::Bluetooth::Advertisement::BluetoothLEAdvertisementPublisher;

use crate::ble::{
    BleAddr, BleConn, BleDriver, BleError, BlePeer, NOTIFY_CHAR_UUID, SERVICE_UUID,
    WRITE_CHAR_UUID,
};

fn err(e: impl std::fmt::Display) -> BleError {
    BleError(e.to_string())
}

pub struct WindowsDriver {
    adapter: Adapter,
    _publisher: Mutex<Option<BluetoothLEAdvertisementPublisher>>,
}

impl WindowsDriver {
    pub async fn new() -> Result<Self, BleError> {
        let manager = Manager::new().await.map_err(err)?;
        let adapter = manager
            .adapters()
            .await
            .map_err(err)?
            .into_iter()
            .next()
            .ok_or_else(|| BleError("no bluetooth adapter".to_string()))?;
        Ok(Self {
            adapter,
            _publisher: Mutex::new(None),
        })
    }

    fn set_publisher(&self, p: BluetoothLEAdvertisementPublisher) {
        *self._publisher.lock().unwrap() = Some(p);
    }

    /// 尝试以给定参数启动一个全新 publisher;返回已启动的 publisher。
    fn try_start(
        service: uuid::Uuid,
        name: Option<&str>,
        extended: bool,
    ) -> Result<BluetoothLEAdvertisementPublisher, String> {
        let publisher = BluetoothLEAdvertisementPublisher::new()
            .map_err(|e| format!("create publisher: {e}"))?;
        if extended {
            publisher
                .SetUseExtendedAdvertisement(true)
                .map_err(|e| format!("set extended: {e}"))?;
        }
        let adv = publisher
            .Advertisement()
            .map_err(|e| format!("get advertisement: {e}"))?;
        adv.ServiceUuids()
            .map_err(|e| format!("service uuids: {e}"))?
            .Append(windows::core::GUID::from_u128(service.as_u128()))
            .map_err(|e| format!("append service uuid: {e}"))?;
        if let Some(n) = name {
            adv.SetLocalName(&windows::core::HSTRING::from(n))
                .map_err(|e| format!("set local name: {e}"))?;
        }
        publisher.Start().map_err(|e| format!("start: {e}"))?;
        Ok(publisher)
    }

    /// 阻塞等待 WinRT 异步操作完成。windows-future 的 IAsyncOperation Future
    /// 非 Send(BleDriver 要求 Send),且其 Async trait 为私有不可导入,
    /// CLI 场景下轮询 IAsyncInfo::Status 等待即可。
    fn wait_async<T: windows::core::RuntimeType>(
        op: &windows_future::IAsyncOperation<T>,
    ) -> Result<T, windows::core::Error> {
        use windows::core::Interface;
        let info: windows_future::IAsyncInfo = op.cast()?;
        loop {
            match info.Status()? {
                windows_future::AsyncStatus::Completed => return op.GetResults(),
                windows_future::AsyncStatus::Error => {
                    return Err(windows::core::Error::from(info.ErrorCode()?))
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// 诊断:适配器 / 无线电状态 / 广播能力 / 最小载荷广播实测。
    /// 用 `zoe-cli ble diag` 调用;用于定位 Start() 的 E_INVALIDARG 类问题。
    pub async fn diag() -> String {
        use btleplug::api::Central as _;
        let mut out = String::new();
        out.push_str("== zoe-cli ble diag (Windows) ==\n");

        // 1) 适配器(btleplug 视角)
        match Manager::new().await {
            Ok(manager) => match manager.adapters().await {
                Ok(adapters) if adapters.is_empty() => {
                    out.push_str("[适配器] 未找到蓝牙适配器\n");
                }
                Ok(adapters) => {
                    for (i, a) in adapters.iter().enumerate() {
                        let info = a
                            .adapter_info()
                            .await
                            .unwrap_or_else(|e| format!("<{e}>"));
                        let state = a
                            .adapter_state()
                            .await
                            .unwrap_or(btleplug::api::CentralState::Unknown);
                        out.push_str(&format!("[适配器 {}] {info} | 状态: {state:?}\n", i + 1));
                    }
                }
                Err(e) => out.push_str(&format!("[适配器] 枚举失败: {e}\n")),
            },
            Err(e) => out.push_str(&format!("[适配器] 初始化失败: {e}\n")),
        }

        // 2) 无线电状态
        match Self::radio_state().await {
            Ok(Some(state)) => out.push_str(&format!("[无线电] 蓝牙无线电状态: {state:?}\n")),
            Ok(None) => out.push_str("[无线电] 未找到蓝牙无线电(虚拟/直通适配器?)\n"),
            Err(e) => out.push_str(&format!("[无线电] 查询失败: {e}\n")),
        }

        // 3) 适配器能力(系统视角)
        match Self::adapter_capabilities().await {
            Ok(caps) => out.push_str(&caps),
            Err(e) => out.push_str(&format!("[能力] 查询失败: {e}\n")),
        }

        // 4) 最小载荷广播实测(仅服务 UUID,21B)。
        //    未打包桌面进程无 bluetooth 能力,预期 Start() 报 0x80070057 ——
        //    这正是 Windows 广播不可用的根因(需 MSIX 打包,本项目不做)。
        out.push_str("[实测] 尝试最小载荷广播(仅服务 UUID)...\n");
        out.push_str("[实测] 注:未打包进程预期失败(0x80070057,缺 bluetooth 能力);\n");
        out.push_str("[实测]     若意外成功,说明你的系统允许桌面广播。\n");
        match Self::try_start(SERVICE_UUID, None, false) {
            Ok(p) => {
                let mut status = "未知";
                for _ in 0..10 {
                    match p.Status() {
                        Ok(s) => {
                            status = match s.0 {
                                2 => "Started(广播已激活)",
                                0 => "Aborted(被系统中止,通常是不支持广播)",
                                4 => "Stopped(停止)",
                                1 => "Waiting(等待中)",
                                _ => "未知",
                            };
                            if s.0 == 0 || s.0 == 2 || s.0 == 4 {
                                break;
                            }
                        }
                        Err(_) => {}
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                out.push_str(&format!("[实测] 广播状态: {status}\n"));
                if status.starts_with("Started") {
                    out.push_str("[实测] 手机此时应能扫描到本机(服务 UUID 7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01)\n");
                }
                let _ = p.Stop();
            }
            Err(e) => out.push_str(&format!("[实测] Start() 失败: {e}\n")),
        }

        out
    }

    /// 查询蓝牙无线电状态(不改动);无蓝牙无线电时返回 None。
    async fn radio_state() -> Result<Option<windows::Devices::Radios::RadioState>, String> {
        use windows::Devices::Radios::{Radio, RadioKind};
        let radios = Self::wait_async(&Radio::GetRadiosAsync().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let count = radios.Size().map_err(|e| e.to_string())?;
        for i in 0..count {
            let radio = radios.GetAt(i).map_err(|e| e.to_string())?;
            if radio.Kind().map_err(|e| e.to_string())? == RadioKind::Bluetooth {
                return Ok(Some(radio.State().map_err(|e| e.to_string())?));
            }
        }
        Ok(None)
    }

    /// 系统视角的适配器广播能力(Devices.Bluetooth)。
    async fn adapter_capabilities() -> Result<String, String> {
        use windows::Devices::Bluetooth::BluetoothAdapter;
        let adapter = Self::wait_async(
            &BluetoothAdapter::GetDefaultAsync().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let le = adapter.IsLowEnergySupported().map_err(|e| e.to_string())?;
        let periph = adapter
            .IsPeripheralRoleSupported()
            .map_err(|e| e.to_string())?;
        let central = adapter
            .IsCentralRoleSupported()
            .map_err(|e| e.to_string())?;
        let ext = adapter
            .IsExtendedAdvertisingSupported()
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "[能力] LE: {le} | 外设角色(广播): {periph} | 中央角色: {central} | 扩展广播: {ext}\n\
             [能力] 外设角色为 false 时,系统会拒绝任何广播(Start 报 0x80070057 参数错误)\n"
        ))
    }

    /// 检查蓝牙无线电状态;关闭时提示手动开启(只检测,不修改系统设置)。
    /// 注:Windows 桌面进程广播本就不可能(见 start_advertising 注释),
    /// 此检查仅为给出更明确的报错信息。
    async fn ensure_radio_on() -> Result<(), String> {
        use windows::Devices::Radios::RadioState;
        match Self::radio_state().await {
            Ok(Some(RadioState::On)) => Ok(()),
            Ok(Some(RadioState::Disabled)) => {
                Err("蓝牙无线电处于禁用状态(设备管理器),无法广播".to_string())
            }
            Ok(Some(_)) => Err("蓝牙已关闭,请手动打开:设置 → 蓝牙和与其他设备".to_string()),
            Ok(None) => Ok(()),
            Err(e) => Err(format!("查询蓝牙无线电状态失败: {e}")),
        }
    }
}

/// Windows 连接:远端 Peripheral + 写特性 + 通知流(由后台任务汇入通道)。
pub struct WindowsConn {
    addr: BleAddr,
    peripheral: Peripheral,
    write_char: BtCharacteristic,
    notify_rx: mpsc::Receiver<Vec<u8>>,
}

impl BleConn for WindowsConn {
    fn peer_addr(&self) -> BleAddr {
        self.addr.clone()
    }

    async fn write(&self, frame: &[u8]) -> Result<(), BleError> {
        self.peripheral
            .write(&self.write_char, frame, WriteType::WithoutResponse)
            .await
            .map_err(err)
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>, BleError> {
        Ok(self.notify_rx.recv().await)
    }
}

impl BleDriver for WindowsDriver {
    type Conn = WindowsConn;

    fn driver_name(&self) -> &'static str {
        "btleplug-win"
    }

    async fn start_advertising(&self, name: &str) -> Result<(), BleError> {
        self.stop_advertising().await?;
        // 无线电关闭时给出明确提示(只检测,不修改系统设置)
        match Self::ensure_radio_on().await {
            Ok(()) => {}
            Err(e) => eprintln!("ble: 提示: {e}"),
        }

        // legacy 广播载荷上限 31B:Flags(3B)+ 128 位服务 UUID(18B)+ 名称(2+N)B,
        // 名称 > 8 字符即超限。每个尝试都用全新 publisher(避免状态残留),失败逐级回退:
        //   1) legacy + 名称    2) 扩展广播 + 名称(上限 1650B,需控制器支持)
        //   3) legacy 无名称(仅 UUID,21B,必定合法;部分 Windows 本就不发 LocalName)
        let mut prior = String::new();

        if !name.is_empty() {
            match Self::try_start(SERVICE_UUID, Some(name), false) {
                Ok(p) => {
                    self.set_publisher(p);
                    return Ok(());
                }
                Err(e) => prior = format!("legacy with name: {e}; "),
            }
            match Self::try_start(SERVICE_UUID, Some(name), true) {
                Ok(p) => {
                    self.set_publisher(p);
                    return Ok(());
                }
                Err(e) => prior = format!("{prior}extended with name: {e}; "),
            }
        }

        match Self::try_start(SERVICE_UUID, None, false) {
            Ok(p) => {
                self.set_publisher(p);
                Ok(())
            }
            Err(e) => Err(BleError(format!(
                "publisher start failed: {e} (prior: {prior}fallback without name); \
                 运行 `zoe-cli ble diag` 查看适配器能力与无线电状态"
            ))),
        }
    }

    async fn stop_advertising(&self) -> Result<(), BleError> {
        if let Some(p) = self._publisher.lock().unwrap().take() {
            p.Stop().map_err(err)?;
        }
        Ok(())
    }

    async fn scan(&self, timeout: Duration) -> Result<Vec<BlePeer>, BleError> {
        self.adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(err)?;
        tokio::time::sleep(timeout).await;
        let _ = self.adapter.stop_scan().await;
        let mut out = Vec::new();
        for p in self.adapter.peripherals().await.map_err(err)? {
            if let Ok(Some(props)) = p.properties().await {
                out.push(BlePeer {
                    addr: BleAddr(props.address.into_inner().to_vec()),
                    name: props.local_name.unwrap_or_default(),
                });
            }
        }
        Ok(out)
    }

    async fn connect(&self, addr: &BleAddr) -> Result<Self::Conn, BleError> {
        let bytes = addr.0.clone();
        if bytes.len() != 6 {
            return Err(BleError("bad mac length".to_string()));
        }
        let mac = btleplug::api::BDAddr::from(<[u8; 6]>::try_from(bytes.as_slice()).unwrap());
        let peripheral: Peripheral = self
            .adapter
            .peripherals()
            .await
            .map_err(err)?
            .into_iter()
            .find(|p| p.address() == mac)
            .ok_or_else(|| BleError("peripheral not found".to_string()))?;

        peripheral.connect().await.map_err(err)?;
        peripheral.discover_services().await.map_err(err)?;

        let mut write_char = None;
        let mut notify_char = None;
        for ch in peripheral.characteristics() {
            if ch.uuid == WRITE_CHAR_UUID {
                write_char = Some(ch.clone());
            } else if ch.uuid == NOTIFY_CHAR_UUID {
                notify_char = Some(ch.clone());
            }
        }
        let write_char = write_char.ok_or_else(|| BleError("write characteristic missing".to_string()))?;
        let notify_char = notify_char.ok_or_else(|| BleError("notify characteristic missing".to_string()))?;

        peripheral.subscribe(&notify_char).await.map_err(err)?;

        // 通知流 → mpsc(按特性 UUID 过滤)
        let (tx, rx) = mpsc::channel(128);
        let mut notifications = peripheral.notifications().await.map_err(err)?;        tokio::spawn(async move {
            while let Some(ValueNotification { uuid, value, .. }) = notifications.next().await {
                if uuid == NOTIFY_CHAR_UUID {
                    let _ = tx.try_send(value);
                }
            }
        });

        Ok(WindowsConn {
            addr: BleAddr(bytes),
            peripheral,
            write_char,
            notify_rx: rx,
        })
    }

    async fn listen(&self) -> Result<mpsc::Receiver<Self::Conn>, BleError> {
        // Windows 桌面应用无法托管 GATT 服务端(GattServiceProvider 仅 UWP/
        // 需包标识),此角色由 Linux 节点承担;Windows 节点用 adv/scan/connect。
        Err(BleError(
            "windows gatt server role requires UWP (GattServiceProvider), \
             not available in desktop binary; Windows 节点可广播/扫描/连接, \
             不能接受 GATT 连接"
                .to_string(),
        ))
    }
}
