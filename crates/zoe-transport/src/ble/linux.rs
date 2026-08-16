//! Linux BLE 驱动:bluer(BlueZ D-Bus)。feature `ble-linux`,仅 Linux 编译。
//!
//! - 客户端角色:扫描 + 连接 + GATT 读写/通知;
//! - 服务端角色:本地 GATT 应用(Service/Characteristic),写入即入站帧,
//!   通知即出站帧 —— 每远端设备一个 `LinuxConn`。
//! 注:真机验证需 Linux + 蓝牙适配器(CI 无硬件,仅编译验证)。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicNotify, CharacteristicNotifyMethod,
    CharacteristicWrite, CharacteristicWriteMethod, Service,
};
use bluer::{Adapter, Address, Session};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::ble::{
    BleAddr, BleConn, BleDriver, BleError, BlePeer, NOTIFY_CHAR_UUID, SERVICE_UUID, WRITE_CHAR_UUID,
};

fn err(e: impl std::fmt::Display) -> BleError {
    BleError(e.to_string())
}

pub struct LinuxDriver {
    _session: Arc<Session>,
    adapter: Adapter,
    adv: Mutex<Option<AdvertisementHandle>>,
    incoming: Mutex<Option<mpsc::Sender<LinuxConn>>>,
}

impl LinuxDriver {
    pub async fn new() -> Result<Self, BleError> {
        let session = Arc::new(Session::new().await.map_err(err)?);
        let adapter = session.default_adapter().await.map_err(err)?;
        adapter.set_powered(true).await.map_err(err)?;
        Ok(Self {
            _session: session,
            adapter,
            adv: Mutex::new(None),
            incoming: Mutex::new(None),
        })
    }
}

// ---------------------------------------------------------------------------
// 连接
// ---------------------------------------------------------------------------

/// 客户端连接:远端 GATT 特性(写 + 通知流)。
/// 通知流包 tokio Mutex:`write(&self)` 的 future 捕获 `&self`,要求本类型 Sync,
/// 而 `dyn Stream` 本身不 Sync —— 包一层后 `TokioMutex<T: Send>` 即 Sync。
pub struct LinuxClientConn {
    addr: BleAddr,
    write_char: bluer::gatt::remote::Characteristic,
    notify: TokioMutex<Pin<Box<dyn futures_util::Stream<Item = Vec<u8>> + Send>>>,
}

/// 服务端连接:远端设备对本地特性的写入流 + 通知器槽位。
/// 写入流包 tokio Mutex(理由同上:`mpsc::Receiver` 不 Sync)。
/// 槽位用 tokio Mutex(guard 可跨 await 保持 Send)且与全局共享同一 Arc:
/// "先订阅后写入"与"先写入后订阅"两种顺序都可用。
pub struct LinuxServerConn {
    addr: BleAddr,
    writes: TokioMutex<mpsc::Receiver<Vec<u8>>>,
    notifier: Arc<TokioMutex<Option<bluer::gatt::local::CharacteristicNotifier>>>,
}

pub enum LinuxConn {
    Client(LinuxClientConn),
    Server(LinuxServerConn),
}

impl BleConn for LinuxConn {
    fn peer_addr(&self) -> BleAddr {
        match self {
            LinuxConn::Client(c) => c.addr.clone(),
            LinuxConn::Server(s) => s.addr.clone(),
        }
    }

    async fn write(&self, frame: &[u8]) -> Result<(), BleError> {
        match self {
            LinuxConn::Client(c) => {
                // 克隆出特性再 await,避免把整个 conn 借入 future
                let ch = c.write_char.clone();
                ch.write(frame).await.map_err(err)
            }
            LinuxConn::Server(s) => {
                let slot = s.notifier.clone();
                let mut guard = slot.lock().await;
                match guard.as_mut() {
                    Some(notifier) => notifier.notify(frame.to_vec()).await.map_err(err),
                    None => Err(BleError("no notify subscriber".to_string())),
                }
            }
        }
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>, BleError> {
        match self {
            LinuxConn::Client(c) => {
                let mut notify = c.notify.lock().await;
                Ok(notify.next().await)
            }
            LinuxConn::Server(s) => {
                let mut writes = s.writes.lock().await;
                Ok(writes.recv().await)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BleDriver 实现
// ---------------------------------------------------------------------------

impl BleDriver for LinuxDriver {
    type Conn = LinuxConn;

    fn driver_name(&self) -> &'static str {
        "bluer"
    }

    async fn start_advertising(&self, name: &str) -> Result<(), BleError> {
        // 先停旧广告(重复注册会被 BlueZ 忽略)
        self.stop_advertising().await?;
        let adv = Advertisement {
            service_uuids: [SERVICE_UUID].into_iter().collect(),
            local_name: Some(name.to_string()),
            discoverable: Some(true),
            discoverable_timeout: Some(Duration::from_secs(0)),
            ..Default::default()
        };
        let handle = self.adapter.advertise(adv).await.map_err(err)?;
        *self.adv.lock().unwrap() = Some(handle);
        Ok(())
    }

    async fn stop_advertising(&self) -> Result<(), BleError> {
        *self.adv.lock().unwrap() = None; // Drop 即注销
        Ok(())
    }

    async fn scan(&self, timeout: Duration) -> Result<Vec<BlePeer>, BleError> {
        let mut devices = self.adapter.discover_devices().await.map_err(err)?;
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(timeout, devices.next()).await {
                Ok(Some(bluer::AdapterEvent::DeviceAdded(addr))) => {
                    let Ok(device) = self.adapter.device(addr) else {
                        continue;
                    };
                    let name = device.name().await.ok().flatten().unwrap_or_default();
                    // 统一为 6 字节 MAC 表示(与 windows.rs 一致)
                    if let Ok(baddr) = BleAddr::from_mac_str(&addr.to_string()) {
                        out.push(BlePeer { addr: baddr, name });
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        Ok(out)
    }

    async fn connect(&self, addr: &BleAddr) -> Result<Self::Conn, BleError> {
        let address: Address = addr.to_mac().parse().map_err(err)?;
        let device = self.adapter.device(address).map_err(err)?;
        device.connect().await.map_err(err)?;
        // 发现我们的服务与特性
        let mut write_char = None;
        let mut notify_char = None;
        for service in device.services().await.map_err(err)? {
            if service.uuid().await.map_err(err)? != SERVICE_UUID {
                continue;
            }
            for ch in service.characteristics().await.map_err(err)? {
                let uuid = ch.uuid().await.map_err(err)?;
                if uuid == WRITE_CHAR_UUID {
                    write_char = Some(ch);
                } else if uuid == NOTIFY_CHAR_UUID {
                    notify_char = Some(ch);
                }
            }
        }
        let write_char =
            write_char.ok_or_else(|| BleError("write characteristic not found".to_string()))?;
        let notify_char =
            notify_char.ok_or_else(|| BleError("notify characteristic not found".to_string()))?;
        let notify = TokioMutex::new(Box::pin(notify_char.notify().await.map_err(err)?));
        Ok(LinuxConn::Client(LinuxClientConn {
            addr: addr.clone(),
            write_char,
            notify,
        }))
    }

    async fn listen(&self) -> Result<mpsc::Receiver<Self::Conn>, BleError> {
        let (incoming_tx, incoming_rx) = mpsc::channel(32);
        *self.incoming.lock().unwrap() = Some(incoming_tx.clone());

        // 每远端设备注册表:写帧通道 + 通知器槽位。
        // Receiver 随首次写入生成的 ServerConn 交付给 listen 消费者(仅一次)。
        // 通知器槽位:所有设备条目与全局共享同一个 Arc<tokio Mutex> ——
        // CharacteristicNotifier 不可 Clone,只能存一份;共享后"先订阅后写入"
        // 与"先写入后订阅"两种顺序都可用(tokio Mutex 的 guard 可跨 await)。
        type NotifierSlot = Arc<TokioMutex<Option<bluer::gatt::local::CharacteristicNotifier>>>;
        type Entry = (
            mpsc::Sender<Vec<u8>>,
            Mutex<Option<TokioMutex<mpsc::Receiver<Vec<u8>>>>>,
            NotifierSlot,
        );
        let shared: NotifierSlot = Arc::new(TokioMutex::new(None));
        let state: Arc<Mutex<HashMap<String, Entry>>> = Arc::new(Mutex::new(HashMap::new()));

        // 写入特性:客户端写入即入站帧
        let write_state = Arc::clone(&state);
        let shared_for_write = Arc::clone(&shared);
        let incoming_for_write = incoming_tx.clone();
        let write_char = Characteristic {
            uuid: WRITE_CHAR_UUID,
            write: Some(CharacteristicWrite {
                write: true,
                write_without_response: true,
                method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                    let state = Arc::clone(&write_state);
                    let shared = Arc::clone(&shared_for_write);
                    let incoming = incoming_for_write.clone();
                    async move {
                        let key = req.device_address.to_string();
                        let addr = BleAddr::from_mac_str(&key)
                            .unwrap_or_else(|_| BleAddr(key.as_bytes().to_vec()));
                        let (tx, rx_taken, notifier) = {
                            let mut map = state.lock().unwrap();
                            let entry = map.entry(key.clone()).or_insert_with(|| {
                                let (tx, rx) = mpsc::channel(128);
                                (tx, Mutex::new(Some(TokioMutex::new(rx))), shared.clone())
                            });
                            let tx = entry.0.clone();
                            // 首次写入:取走写帧接收端(交付 ServerConn)
                            let rx_taken = entry.1.lock().unwrap().take();
                            (tx, rx_taken, entry.2.clone())
                        };
                        let _ = tx.try_send(value);
                        if let Some(rx) = rx_taken {
                            let _ = incoming.try_send(LinuxConn::Server(LinuxServerConn {
                                addr,
                                writes: rx,
                                notifier,
                            }));
                        }
                        Ok(())
                    }
                    .boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        };

        // 通知特性:客户端订阅时把 notifier 写入全局共享槽位(供所有 ServerConn 写)。
        let shared_for_notify = Arc::clone(&shared);
        let notify_char = Characteristic {
            uuid: NOTIFY_CHAR_UUID,
            notify: Some(CharacteristicNotify {
                notify: true,
                method: CharacteristicNotifyMethod::Fun(Box::new(
                    move |notifier: bluer::gatt::local::CharacteristicNotifier| {
                        let shared = Arc::clone(&shared_for_notify);
                        async move {
                            *shared.lock().await = Some(notifier);
                        }
                        .boxed()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let app = Application {
            services: vec![Service {
                uuid: SERVICE_UUID,
                primary: true,
                characteristics: vec![write_char, notify_char],
                ..Default::default()
            }],
            ..Default::default()
        };

        let _app_handle = self
            .adapter
            .serve_gatt_application(app)
            .await
            .map_err(err)?;
        Ok(incoming_rx)
    }
}
