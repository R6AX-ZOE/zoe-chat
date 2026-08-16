# Termux 真机 BLE 联调指南

> 目标:在 Android 手机(Termux)上对 zoe-chat 的 BLE GATT 网状覆盖层做**真机验证**,
> 并跑通"Linux 节点 ↔ 手机节点"的完整链路。
> 配套脚本:`scripts/termux/`;手机端 GATT 测试页:`tools/ble-gatt-test/index.html`;
> Linux 端联调命令:`zoe-cli ble adv|scan|connect`(feature `ble-linux`)。

## 1. 角色矩阵(先看清谁能干什么)

| 设备 | BLE 能力 | 说明 |
|---|---|---|
| Linux 电脑/树莓派(BlueZ) | 广播 + GATT 服务端 + 扫描 + 连接 | 生产驱动 `ble-linux`(bluer),`zoe-cli ble` 可直接用;**唯一能承担 GATT 服务端(被连接)角色的节点** |
| Windows 电脑 | 广播(仅广告)+ 扫描 + 连接 | `ble-windows` 驱动:可被手机**扫描到**(zoe-device),可做 central;**GATT 服务端受 UWP 限制不可用**(GattServiceProvider 需包标识),手机连不上 |
| Android 手机 + Termux | **仅扫描**(termux-api) | Android 用自家蓝牙栈,**无 BlueZ**,`bluer` 无法工作(`target_os=android` 不编译 `ble-linux`) |
| Android 手机 + Chrome | Web Bluetooth:扫描/连接/GATT 收发 | 现成的真机 GATT 客户端,无需 root、无需写 App |
| Android 手机 + nRF Connect | 完整 GATT 客户端(手动) | 备选,支持自定义服务/特性 |

结论:**Termux 端做"扫描验证 + 协议栈测试 + 守护进程真机运行";GATT 收发用 Chrome
Web Bluetooth 测试页(或 nRF Connect);对端 GATT 服务端由 Linux 节点承担。
只有 Windows + 手机时:Windows 可广播让手机"看见",但完整 GATT 链路仍需要一台
Linux 节点(树莓派/旧笔记本/虚拟机直通蓝牙均可)。**

## 2. 推荐拓扑(方案 A:验证完整 GATT 链路)

```
┌─────────────────────────┐          BLE          ┌──────────────────────────┐
│ Linux 节点(peripheral)  │ ◄─────── GATT ──────► │ Android 手机(central)    │
│ zoe-cli ble adv --echo  │  写 7a5e0002…         │ Chrome 打开               │
│ 广播名 zoe-device       │  通知 7a5e0003…       │ tools/ble-gatt-test/      │
└─────────────────────────┘                       └──────────────────────────┘
        ▲                                                   ▲
        │ 验证点1:termux-api 扫描到广播(ble-scan.sh)          │
        └─────────────────────────────────────────────────────┘
```

验证点:
1. **广播可达**:手机 `ble-scan.sh` 能看到 `zoe-device`(或自定义名),带 RSSI;
2. **GATT 收发**:手机测试页连接后,写入测试帧 → Linux 端打印解析结果;开启 `--echo` 后
   手机收到回显帧,页面显示往返 RTT;
3. **帧格式**:测试页可生成单分片帧与 600B 两分片帧,对照 docs/envelope.md §2.1;
4. **协议栈**:`build.sh --test` 在手机本地跑 BLE mesh overlay mock 测试(分片/重组/去重/TTL)。

## 3. 完整操作步骤

### 3.1 Linux 端准备(peripheral)

```sh
# 确认 BlueZ 与适配器可用
bluetoothctl show                      # 应列出控制器,Powered: yes
hciconfig hci0 up                      # 必要时

# 构建带 BLE 的 CLI(Linux 主机)
cargo build -p zoe-cli --features ble-linux

# 启动广播 + GATT 服务,回显模式(联调用)
target/debug/zoe-cli ble adv --name zoe-device --echo
# 其它命令:ble scan / ble connect <MAC> --send <HEX>
```

> **只有 Windows 电脑时**(无法提供 GATT 服务端):
> ```sh
> cargo build -p zoe-cli --features ble-windows
> target/debug/zoe-cli ble adv --name zoe-device
> # 手机 termux-bluetooth-scan / Web Bluetooth 选择器能看到 zoe-device(验证广播),
> # 但连接 GATT 会失败(Windows 无服务端,属预期);
> # Windows 也可做 central:`zoe-cli ble scan` / `ble connect <MAC> --send <HEX>`
> # 去连手机侧 nRF Connect 模拟的 peripheral。
> ```

### 3.2 手机端准备(Termux)

```sh
pkg install -y git
git clone https://github.com/R6AX-ZOE/zoe-chat.git
cd zoe-chat
bash scripts/termux/setup-termux.sh        # 安装 rust/termux-api 等(一次)
```

权限(关键,漏了扫不到设备):
- 系统设置 → 应用 → **Termux:API** → 权限 → 授予**附近设备**(Android 12+)/**位置**(Android 6-11);
- 打开**蓝牙**与**定位**开关;屏幕保持亮起(Android 8.1+ 熄屏停止 BLE 扫描)。

### 3.3 验证广播(termux-api 扫描)

```sh
bash scripts/termux/ble-scan.sh --filter zoe --count 3
# 或等待模式:对端开始广播后
bash scripts/termux/ble-scan.sh --wait-for zoe-device --timeout 60
```

看到类似输出即通过验证点 1:

```
18:AA:BB:CC:DD:EE   zoe-device   RSSI -55
```

### 3.4 GATT 收发(Chrome Web Bluetooth)

1. Linux 端保持 `zoe-cli ble adv --echo` 运行;
2. 把 `tools/ble-gatt-test/index.html` 弄到手机上(任选):
   - `git clone` 后直接在 Termux 里 `python -m http.server 8000`,手机 Chrome 打开
     `http://127.0.0.1:8000/tools/ble-gatt-test/`(Termux 内访问本机端口可行);
   - 或复制该单文件到手机文件管理,用 Chrome 打开 `file://` 路径;
   - 或推到任意静态托管;
3. 点**扫描并连接**(Chrome 会弹蓝牙授权,选 `zoe-device`);
4. 点**生成测试帧** → **发送**:Linux 端应打印 `[rx] ... frame msg_id=... chunk=1/1`;
5. 点 **echo 往返测试**:日志出现 RX 回显帧与 RTT(毫秒);
6. 点**分片帧 A/B** 分别发送:Linux 端应显示 `chunk=1/2`、`chunk=2/2`(MeshOverlay
   分片格式真机校验)。

> 备选:nRF Connect App → 按服务 UUID 过滤 → 连接 → 写 7a5e0002…(勾选
> Write Without Response 前可先试 Write)、订阅 7a5e0003… 通知,手动粘贴 hex 帧。
> 注意 nRF Connect 需**先写一帧再订阅通知**(旧版顺序),新版先订阅也兼容——
> 我们的驱动已修复"先订阅后写入"场景(notifier 孤儿槽)。

### 3.5 协议栈测试(手机本地)

```sh
bash scripts/termux/build.sh --test
# cargo test -p zoe-transport --features ble-linux
# → BLE mesh overlay(mock 驱动)5 项测试在真机 Android 上通过
```

### 3.6 完整应用栈(方案 B:手机跑守护进程,Wi-Fi 互通)

BLE 驱动在 Android 上不可用,但**整个应用栈**可以在手机上跑(libp2p net 传输):

```sh
bash scripts/termux/build.sh
bash scripts/termux/run-daemon.sh --port 8787
```

- 手机浏览器访问 `http://127.0.0.1:8787`(Termux 内直接可用);
- 电脑访问需 SSH 端口转发(手机开 sshd):
  `ssh -L 127.0.0.1:8787:127.0.0.1:8787 u0_aXXX@手机IP`,
  然后电脑浏览器打开 `http://127.0.0.1:8787`;
- 两台设备同一 Wi-Fi 时,libp2p mDNS 自动互发现;跨网手动拨号/打洞见 README。

## 4. 方案 C(可选,不推荐):root + BlueZ 直跑 ble-linux

`pkg install bluez` 后 bluetoothctl 通常报 **No default controller available**:
Android 内核未编译 BlueZ 驱动、控制器被厂商 HAL 独占。仅当设备刷了带 BlueZ 的
自定义内核/PostmarketOS 类系统时才可能工作;若可行,则 `ble-linux` 驱动可直接编译
运行(bluer 纯 Rust 实现 D-Bus,无额外系统库)。默认不要依赖此路线。

## 5. 帧格式速查(docs/envelope.md §2.1)

```
[magic 0x5A | msg_id 8B | ttl 1B | chunk_idx 1B | total 2B | data ≤499B]
服务   7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01
写     7a5e0002-2e4c-4a31-9b6c-3c2a0e5f6a01(客户端→服务端)
通知   7a5e0003-2e4c-4a31-9b6c-3c2a0e5f6a01(服务端→客户端)
```

单分片测试帧示例(`hello` 载荷):
`5a 0102030405060708 01 00 0001 68656c6c6f`

## 6. 故障排查

| 现象 | 原因与处理 |
|---|---|
| `termux-bluetooth-scan` 无输出/报错 | Termux:API 权限未授予;定位未开;蓝牙未开;屏幕熄灭(Android 8.1+ 熄屏停扫) |
| 扫到设备但名字为空 | 广播未带 local name(部分 Windows 版本不发 LocalName);用 MAC 匹配(`--filter 18:AA`),或 Chrome 测试页留空名称前缀按服务 UUID 过滤 |
| 扫描不到端广播 | 适配器未 up/未 discoverable;`zoe-cli ble adv` 需保持运行;两台设备距离过远;信道拥堵换位置 |
| Chrome 弹窗里没有 zoe-device | 页面按服务 UUID 过滤,对端必须用我们的 `SERVICE_UUID` 广播(`ble adv` 已内置);或改名前缀过滤;Windows 广播为非可连接广告,能看到但连接会失败 |
| 写特性报错 | MTU 不足(Android 通常协商 517B,帧 ≤512B);连错设备;先断开重连 |
| 订阅通知后收不到回显 | 确认对端 `--echo`;旧版 nRF 需先写一帧;驱动已兼容先订阅场景 |
| Windows 广播后手机能扫到但连不上 | 属预期:Windows 桌面应用无 GATT 服务端(GattServiceProvider 需 UWP);GATT 链路测试需 Linux 节点 |
| `pkg install bluez` 失败/不可用 | 属预期,Android 无 BlueZ;走 termux-api + Web Bluetooth 路线 |
| 手机端 `cargo build` 慢 | 首次编译依赖多,耐心等待;`--release` 更慢但产物更小 |

## 7. 命令速查

```sh
# Linux 端(全角色)
cargo build -p zoe-cli --features ble-linux
zoe-cli ble adv --name zoe-device --echo        # peripheral + 回显
zoe-cli ble scan --timeout 10                    # 扫描
zoe-cli ble connect AA:BB:CC:DD:EE:FF --send 5a01020304050607080100000168656c6c6f

# Windows 端(仅广播/扫描/连接)
cargo build -p zoe-cli --features ble-windows
zoe-cli ble adv --name zoe-device                # 仅广播:手机可扫描到,连不上
zoe-cli ble connect AA:BB:CC:DD:EE:FF            # central:连接手机侧模拟外设

# 手机端(Termux)
bash scripts/termux/setup-termux.sh
bash scripts/termux/ble-scan.sh --filter zoe --count 3
bash scripts/termux/ble-scan.sh --wait-for zoe-device
bash scripts/termux/build.sh --test
bash scripts/termux/run-daemon.sh --port 8787
```
