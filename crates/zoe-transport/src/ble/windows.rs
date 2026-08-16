//! Windows BLE 驱动:btleplug 0.12(central)+ windows-rs 广告发布器。
//! feature `ble-windows`,仅 Windows 编译。
//!
//! 角色能力:
//! - 广播:BluetoothLEAdvertisementPublisher(名称 + 服务 UUID)—— 手机等
//!   扫描方可"看见"本节点(zoe-device);
//! - 主动连接(central):扫描 + GATT 读写/通知(btleplug);
//! - 被连接(GATT server)不可用:Windows 桌面应用无法托管 GATT 服务端
//!   (GattServiceProvider 仅 UWP/需包标识),故 Windows 节点只能被扫描到,
//!   不能接受手机连接;完整 GATT 服务端角色由 Linux 节点承担(见 docs/DESIGN.md §6.2)。
//! 真机验证需 Windows 10 1709+ 且带蓝牙适配器。

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
        let publisher = BluetoothLEAdvertisementPublisher::new()
            .map_err(|e| BleError(format!("create publisher: {e}")))?;
        let adv = publisher
            .Advertisement()
            .map_err(|e| BleError(format!("get advertisement: {e}")))?;
        adv.ServiceUuids()
            .map_err(|e| BleError(format!("service uuids: {e}")))?
            .Append(windows::core::GUID::from_u128(SERVICE_UUID.as_u128()))
            .map_err(|e| BleError(format!("append service uuid: {e}")))?;

        // legacy 广播载荷上限 31B:Flags(3B)+ 128 位服务 UUID(18B)+ 名称(2+N)B。
        // 名称 > 8 字符即超限,Start() 抛 E_INVALIDARG(0x80070057)。
        // 策略:1) legacy+名称 → 2) 扩展广播+名称(上限 1650B,需控制器支持)
        //      → 3) legacy 去名称(仅 UUID,21B,必定可广播)。
        let name_set = adv
            .SetLocalName(&windows::core::HSTRING::from(name))
            .is_ok();
        let mut prior = String::new();

        if name_set {
            match publisher.Start() {
                Ok(()) => {
                    self.set_publisher(publisher);
                    return Ok(());
                }
                Err(e) => prior = format!("legacy with name: {e}; "),
            }
            // 尝试扩展广播(控制器不支持时 Start 失败,继续回退)
            let _ = publisher.SetUseExtendedAdvertisement(true);
            match publisher.Start() {
                Ok(()) => {
                    self.set_publisher(publisher);
                    return Ok(());
                }
                Err(e) => prior = format!("{prior}extended with name: {e}; "),
            }
            let _ = publisher.SetUseExtendedAdvertisement(false);
        }

        // 最终回退:不广播名称(部分 Windows 版本本就不发送 LocalName)
        let _ = adv.SetLocalName(&windows::core::HSTRING::new());
        match publisher.Start() {
            Ok(()) => {
                self.set_publisher(publisher);
                Ok(())
            }
            Err(e) => Err(BleError(format!(
                "publisher start failed: {e} (prior attempts: {prior}fallback without name)"
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
