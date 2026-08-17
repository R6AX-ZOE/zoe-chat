# Termux 真机 BLE 联调指南

> 目标:在 Android 手机(Termux)上对 zoe-chat 的 BLE GATT 网状覆盖层做**真机验证**,
> 并跑通"Linux 节点 ↔ 手机节点"的完整链路。
> 配套脚本:`scripts/termux/`;手机端 GATT 测试页:`tools/ble-gatt-test/index.html`;
> 手机 peripheral App:`android-old/`(zoe BLE 服务端,推荐);Linux 端联调命令:
> `zoe-cli ble adv|scan|connect`(feature `ble-linux`)。

## 1. 角色矩阵(先看清谁能干什么)

| 设备 | BLE 能力 | 说明 |
|---|---|---|
| Linux 电脑/树莓派(BlueZ) | 广播 + GATT 服务端 + 扫描 + 连接 | 生产驱动 `ble-linux`(bluer),`zoe-cli ble` 全角色可用;**唯一能直接承担 GATT 服务端(被连接)角色的节点** |
| Windows 电脑 | **仅 central**:扫描 + 连接 | `ble-windows` 驱动;**广播不可用**(`BluetoothLEAdvertisementPublisher` 需包标识中的 bluetooth 能力,桌面进程 Start 报 0x80070057;已用 `ble diag` 实测验证,适配器/无线电/载荷均正常仍失败);GATT 服务端同样需 UWP |
| Android 手机 + Termux | **仅扫描**(termux-api) | Android 用自家蓝牙栈,**无 BlueZ**,`bluer` 无法工作(`target_os=android` 不编译 `ble-linux`) |
| Android 手机 + zoe App | **peripheral 模拟**(广播 + GATT 服务端) | 本仓库 `android-old/` 工程(纯 Kotlin,零依赖):广播 zoe 服务、收帧解析、echo 回显 —— **没有 Linux 时的推荐方案**;APK 由 Android Studio 或 CI 构建 |
| Android 手机 + nRF Connect | peripheral 模拟 + GATT 客户端(手动) | 免费 App 临时替代,需手动配置服务/特性;zoe App 是正式方案 |
| Android 手机 + Chrome | Web Bluetooth:扫描/连接/GATT 收发 | 现成的真机 GATT 客户端;**注意 Web Bluetooth 没有广播 API,浏览器只能当 central** |
| 电脑 Chrome | Web Bluetooth:连接/GATT 收发 | 电脑浏览器也可当 central(前提:服务端在广播 zoe 服务 UUID) |

结论:
- **有 Linux 节点**:按方案 A(Linux peripheral + 手机 Chrome/nRF 测试);
- **只有 Windows + 手机**:按方案 A'(**手机 zoe App 当 peripheral**,Windows
  `zoe-cli ble scan/connect` 或**电脑 Chrome** 当 central)—— 无需 root、无需
  管理员权限、无需改证书/注册表,验证完整的 GATT 收发链路;
- Termux 端始终做"扫描验证 + 协议栈测试 + 守护进程真机运行"。

## 2. 推荐拓扑

### 方案 A(有 Linux 节点:验证完整 GATT 链路)

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

### 方案 A'(只有 Windows + 手机:手机 zoe App 模拟 peripheral)

```
┌──────────────────────────────┐       BLE       ┌─────────────────────────────┐
│ Android 手机 (peripheral)    │ ◄──── GATT ────► │ Windows 电脑 (central)      │
│ zoe BLE App(android-old/ 工程)   │  写 7a5e0002…   │ zoe-cli ble connect <MAC>   │
│ 广播 zoe 服务 + echo 回显     │  通知 7a5e0003…  │ 或电脑 Chrome 打开           │
└──────────────────────────────┘                 │ tools/ble-gatt-test/        │
        ▲                                        └─────────────────────────────┘
        │ 可选:Termux ble-scan.sh 也能扫描到该广播
        └────────────────────────────────────────────
```

验证点(两种方案通用):
1. **广播可达**:Termux `ble-scan.sh`(或 Chrome 选择器)能看到 `zoe-device`(或自定义名),带 RSSI;
2. **GATT 收发**:central 端写入测试帧 → peripheral 端打印/显示解析结果;peripheral 回发
   通知帧 → central 收到(方案 A 用 `--echo` 自动回显并测 RTT);
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

> **只有 Windows 电脑时**(不能广播、没有 GATT 服务端):
> 用**手机 zoe BLE App 模拟 peripheral**(见 §3.4 步骤 0),Windows 做 central:
> ```sh
> cargo build -p zoe-cli --features ble-windows
> target/debug/zoe-cli ble scan --timeout 10              # 找到手机的 MAC
> target/debug/zoe-cli ble connect AA:BB:CC:DD:EE:FF --send 5a01020304050607080100000168656c6c6f
> # 手机 App 日志显示 [收] 帧...;echo 自动回发 → Windows 打印 [rx]
> ```
> 或电脑 Chrome 打开 `tools/ble-gatt-test/index.html` 直接连手机(纯浏览器方案)。

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

### 3.4 GATT 收发

**步骤 0(方案 A' 专用):手机运行 zoe BLE App(peripheral)**

1. 构建并安装 APK(任选):
   - Android Studio 打开 `android-old/` → 连接手机(USB 调试)→ Run;
   - 或命令行 `cd android-old && gradle wrapper && ./gradlew assembleDebug` 后
     `adb install -r app/build/outputs/apk/debug/app-debug.apk`
     (也可从 GitHub Actions 的 Android Build 工作流下载 APK 产物);
2. 打开 App → 授权"附近的设备"(Android 12+)/定位(Android 12 以下)→ 点**启动**;
3. App 开始广播(只带服务 UUID,不带设备名 —— 128 位 UUID + 系统蓝牙名
   易超 31 字节 legacy 载荷上限导致 `ADVERTISE_FAILED_DATA_TOO_LARGE`)
   并托管 GATT 服务;echo 开关默认开启。

> 临时替代(无 APK 时):nRF Connect App → **Advertiser** 页新建广播(服务 UUID
> `7a5e0001-2e4c-4a31-9b6c-3c2a0e5f6a01`,名称随意)→ **Server** 页新建服务
> (写特性 `7a5e0002-…` 属性 Write/Write Without Response;通知特性
> `7a5e0003-…` 属性 Notify)→ 两页都保持运行。注意 nRF Connect 回程需
> 在 Server 页手动发通知(没有自动 echo)。

**步骤 1(方案 A):Linux 端保持 `zoe-cli ble adv --echo` 运行;**
**方案 A' 则跳到此步的 central 侧(Windows zoe-cli 或电脑 Chrome)。**

**步骤 2(手机侧测试页):** 把 `tools/ble-gatt-test/index.html` 弄到手机上(任选):
   - `git clone` 后直接在 Termux 里 `python -m http.server 8000`,手机 Chrome 打开
     `http://127.0.0.1:8000/tools/ble-gatt-test/`(Termux 内访问本机端口可行);
   - 或复制该单文件到手机文件管理,用 Chrome 打开 `file://` 路径;
   - 或推到任意静态托管;
   (电脑 Chrome 测试:直接 `file://` 或任意静态服务打开同一页面即可。)

**步骤 3(连接与收发):**
3. 点**扫描并连接**(Chrome 会弹蓝牙授权,选手机名/`zoe-device`);
4. 点**生成测试帧** → **发送**:对端应打印/显示 `[rx] ... frame msg_id=... chunk=1/1`
   (Linux 端打印解析结果;zoe App 日志显示 `[收] 帧 ...`);
5. 回程验证(echo):
   - 方案 A:点 **echo 往返测试**,日志出现 RX 回显帧与 RTT(毫秒);
   - 方案 A':zoe App 默认自动 echo,收到帧即回发 —— 测试页日志出现 RX 帧、
     App 日志显示 `[发] 通知已送达`;`zoe-cli ble connect` 同样能收到回显;
6. 点**分片帧 A/B** 分别发送:对端应显示 `chunk=1/2`、`chunk=2/2`(MeshOverlay
   分片格式真机校验)。

> 备选(nRF Connect 当 central):按服务 UUID 过滤 → 连接 → 写 7a5e0002…、
> 订阅 7a5e0003… 通知,手动粘贴 hex 帧。
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
| 扫到设备但名字为空 | 广播未带 local name;用 MAC 匹配(`--filter 18:AA`),或 Chrome 测试页留空名称前缀按服务 UUID 过滤。`zoe-cli ble scan` 输出带 `⭐ zoe 设备` 标记的就是手机 App |
| 找不到手机的"蓝牙地址" | Android 广播用的是随机私有地址(RPA),与手机设置里的蓝牙地址不同,且重启广播后可能变化;以扫描结果为准,连接前重新扫描 |
| 扫描不到端广播 | 对端广播未启动(nRF Connect Advertiser 要运行);两台设备距离过远;信道拥堵换位置;手机与 Windows 已配对时部分系统会屏蔽其广播(设置里删除配对) |
| Chrome 弹窗里没有 zoe-device | 页面按服务 UUID 过滤,对端必须广播我们的 `SERVICE_UUID`(nRF Connect Advertiser 里添加;`ble adv` 已内置);或改名前缀过滤 |
| 连接报 `peripheral not found` | Windows 下 btleplug 只能连接被扫描发现过的设备;`ble connect` 已内置 20s 扫描等待,仍失败 = 对端未在广播或 RPA 已变(重新 `ble scan` 取最新 ⭐ 地址) |
| 写特性报错 | MTU 不足(Android 通常协商 517B,帧 ≤512B);连错设备;先断开重连 |
| 订阅通知后收不到回显 | 方案 A 确认对端 `--echo`;方案 A' 需在 nRF Connect Server 页手动发通知;旧版 nRF 需先写一帧;驱动已兼容先订阅场景 |
| Windows `ble adv` 报 `参数错误 (0x80070057)` | **属预期**:Windows 桌面进程无 bluetooth 能力(需包标识),广播被系统拒绝;适配器/无线电/载荷均正常(可用 `zoe-cli ble diag` 复核)。改用方案 A':手机 nRF Connect 模拟 peripheral + Windows/Chrome 做 central |
| `pkg install bluez` 失败/不可用 | 属预期,Android 无 BlueZ;走 termux-api + Web Bluetooth/nRF Connect 路线 |
| 手机端 `cargo build` 慢 | 首次编译依赖多,耐心等待;`--release` 更慢但产物更小 |

## 7. 命令速查

```sh
# Linux 端(全角色)
cargo build -p zoe-cli --features ble-linux
zoe-cli ble adv --name zoe-device --echo        # peripheral + 回显
zoe-cli ble scan --timeout 10                    # 扫描
zoe-cli ble connect AA:BB:CC:DD:EE:FF --send 5a01020304050607080100000168656c6c6f

# Windows 端(仅 central;广播/服务端需 UWP 包标识,不可用)
cargo build -p zoe-cli --features ble-windows
zoe-cli ble diag                               # 诊断:适配器/无线电/广播能力(只读)
zoe-cli ble scan --timeout 10                  # 扫描手机 nRF Connect 模拟的外设
zoe-cli ble connect AA:BB:CC:DD:EE:FF          # central:连接 + 收发帧
# 或电脑 Chrome 打开 tools/ble-gatt-test/index.html 直接连接

# 手机端(Termux)
bash scripts/termux/setup-termux.sh
bash scripts/termux/ble-scan.sh --filter zoe --count 3
bash scripts/termux/ble-scan.sh --wait-for zoe-device
bash scripts/termux/build.sh --test
bash scripts/termux/run-daemon.sh --port 8787
```
