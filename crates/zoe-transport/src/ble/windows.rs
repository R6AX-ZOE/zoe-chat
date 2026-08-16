//! Windows BLE 驱动:btleplug 0.12(central,WinRT)+ windows-rs 广告发布器。
//! feature `ble-windows`,仅 Windows 编译。
//!
//! 已知限制(见 docs/DESIGN.md §6.2):Windows 的 GATT server 角色
//! (被连接方)暂未实现 —— Windows 节点主动连接 Linux/其他外设;
//! Windows↔Windows 近场互通需等 GATT server 角色(M2.5)。
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

use crate::ble::{BleAddr, BleConn, BleDriver, BleError, BlePeer};

pub const SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_u128(0x7a5e_0001_2e4c_4a31_9b6c_3c2a_0e5f_6a01);
pub const WRITE_CHAR_UUID: uuid::Uuid = uuid::Uuid::from_u128(0x7a5e_0002_2e4c_4a31_9b6c_3c2a_0e5f_6a01);
pub const NOTIFY_CHAR_UUID: uuid::Uuid = uuid::Uuid::from_u128(0x7a5e_0003_2e4c_4a31_9b6c_3c2a_0e5f_6a01);

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

    async fn start_advertising(&self, _name: &str) -> Result<(), BleError> {
        // 当前 windows-rs SDK 绑定中 BluetoothLEAdvertisementPublisher 的
        // Advertisement 属性为只读(构造后不可配置 payload),广告角色暂不可用。
        // 设计取舍(见 docs/DESIGN.md §6.2):Windows 节点以 central 身份
        // 主动连接 Linux/其他外设;Windows↔Windows 近场需等 GATT server
        // 角色与可配置广告(M2.5)。
        Err(BleError(
            "windows advertising not available in this SDK binding; \
             use a Linux node as BLE peripheral (see docs/DESIGN.md §6.2)"
                .to_string(),
        ))
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
        // Windows GATT server 角色(GattServiceProvider)尚未实现:
        // Windows 节点以 central 身份主动连接(M2.5 补服务端角色)。
        Err(BleError(
            "windows gatt server role not implemented yet".to_string(),
        ))
    }
}
