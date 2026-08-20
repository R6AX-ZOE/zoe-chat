# 存储 schema 与持久化策略 v0.1

SQLite(WAL 模式),`rusqlite`。**密文必存,明文可清除**。

## 1. Schema

```sql
PRAGMA journal_mode=WAL;

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL              -- schema_version、口令盐、启动 token 哈希等
);

CREATE TABLE identity (             -- 单用户(每安装一份)
  user_id       BLOB PRIMARY KEY,  -- Ed25519 身份公钥
  secret        BLOB NOT NULL,     -- 私钥(0600 文件权限;可选 argon2 口令加密)
  created_at    INTEGER NOT NULL
);

CREATE TABLE devices (
  device_id     BLOB PRIMARY KEY,  -- 设备 MLS 签名公钥
  user_id       BLOB NOT NULL REFERENCES identity(user_id),
  user_sig      BLOB NOT NULL,     -- 用户身份对设备凭据的签名
  created_at    INTEGER NOT NULL,
  revoked_at    INTEGER            -- NULL=有效
);

CREATE TABLE peers (
  peer_id       BLOB PRIMARY KEY,  -- 对方用户身份公钥哈希(32B 指纹)
  fingerprint   BLOB NOT NULL,     -- Safety-Number 风格指纹
  display_name  TEXT,
  trust_status  INTEGER NOT NULL DEFAULT 0, -- 0=TOFU 1=已带外验证 2=已阻止
  bt_addrs      TEXT,              -- JSON:[BLE 地址]
  net_addrs     TEXT,              -- JSON:[libp2p PeerId/multiaddr]
  keypkg        BLOB,              -- 最近一次 KeyPackage(带过期时间语义)
  first_seen    INTEGER, last_seen INTEGER
);

CREATE TABLE key_packages (        -- 本设备已生成、待消费的 KeyPackage
  kp_ref        BLOB PRIMARY KEY,  -- KeyPackageRef
  body          BLOB NOT NULL,     -- KeyPackage TLS 序列化
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  used_at       INTEGER            -- 一次性:消费后标记
);

CREATE TABLE groups (
  group_id      BLOB PRIMARY KEY,
  name          TEXT,
  epoch         INTEGER NOT NULL DEFAULT 0,
  state         BLOB NOT NULL,     -- openmls 群组状态序列化(见 §2)
  coordinator   BLOB,              -- 协调者 device_id;NULL=本机
  created_at    INTEGER NOT NULL
);

CREATE TABLE messages (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  msg_hash      BLOB UNIQUE NOT NULL,   -- Envelope hash(去重键)
  envelope      BLOB NOT NULL,          -- 完整 Envelope(密文必存)
  group_id      BLOB NOT NULL,
  epoch         INTEGER NOT NULL,
  sender        BLOB,                   -- 发送方设备指纹(解码后回填)
  seq           INTEGER,
  direction     INTEGER NOT NULL,       -- 0=收 1=发
  status        INTEGER NOT NULL DEFAULT 0, -- 0=待投递 1=已送达 2=已读 3=失败
  plaintext     BLOB,                   -- 解密缓存,可清除(见 §3)
  received_at   INTEGER, delivered_at INTEGER
);
CREATE INDEX idx_msg_group ON messages(group_id, epoch, seq);
CREATE INDEX idx_msg_hash   ON messages(msg_hash);

CREATE TABLE pending_proposals (        -- 协调者缓冲(结构变更)
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  group_id      BLOB NOT NULL,
  body          BLOB NOT NULL,          -- Proposal Envelope
  created_at    INTEGER NOT NULL
);

CREATE TABLE transport_state (
  peer_id       BLOB NOT NULL,
  transport     TEXT NOT NULL,          -- 'ble' | 'lan' | 'net'
  addr          TEXT,
  last_ok       INTEGER, quality        INTEGER,
  PRIMARY KEY (peer_id, transport)
);
```

## 1.1 用户注册表(users.db)

多用户引入后,数据目录含两处持久化:**根目录 `users.db`(注册表)** + **每用户独立子目录 `users/<id>/`**(各自 `zoe.db`/`mls.db`/`token`)。用户切换 = 指定 `--user <id>`,运行时只开该账号的数据集。旧版明文数据迁移为 `default` 用户(`kind=plain`,`dir` 指向原数据目录,子目录布局不回溯迁移文件)。

```sql
CREATE TABLE users (
  user_id      TEXT PRIMARY KEY,  -- 8 字节十六进制(Ed25519 身份的 user_id)
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL,     -- 'plain' | 'pin'
  dir          TEXT NOT NULL,     -- 运行时数据子目录,如 users/<id>/
  seed_enc     BLOB,              -- PIN 用户:salt||nonce||ciphertext(种子密文);plain 用户为 NULL
  pin_verifier BLOB,              -- PIN 用户:argon2id PHC 串(校验用,不存 PIN 明文);plain 用户为 NULL
  created_at   INTEGER NOT NULL,
  last_used    INTEGER NOT NULL   -- 最近激活时间;CLI activate 会更新
);
```

- **PIN 派生**:`argon2id(PIN)` 分别派生两路——① 校验串(`password_hash` PHC 格式,恒时比较),② 种子加密密钥(KDF 盐随机 16B,密文格式 `salt||nonce(12)||ciphertext`,XChaCha20-Poly1305,见 `crates/zoe-core/src/storage.rs` `encrypt_seed`/`decrypt_seed`)。
- **明文种子零落盘**:新建 PIN 用户种子 = 随机 Ed25519 身份,仅以密文存 `users.db`;用户 `zoe.db` 的 `meta` 表写入 `seed_enc=1` 标记("种子已迁出注册表持有"),此后该用户的身份读取一律走注册表解密,不再写回明文。
- **set-pin 路径**:CLI/API 以激活用户运行时身份种子重新加密 → 原子更新 `seed_enc`+`pin_verifier`+`kind='pin'`,写入独立事务。
- 备份范围不变:仅身份助记词(登录 `default` 明文账号使用);PIN 用户密钥不随助记词备份(优先恢复为明文账号,需要时可对恢复的账号 set-pin)。

## 2. openmls 持久化

- openmls 提供 `StorageProvider` 抽象(KeyPackage/群组状态/PSK 等)。
- **M0 简化**:将 `MlsGroup` 状态导出为 blob 存入 `groups.state`,整组读写;每次 epoch 推进后原子更新(同一事务)。
- **M1 完善**:实现 `StorageProvider`(rusqlite 后端),按 openmls 的条目粒度持久化,消除整组 blob 的读写放大与并发风险。
- 版本策略:openmls **锁版本**(pre-1.0,API 变动频繁);`meta` 记录 `openmls_version`,升级时迁移或拒绝启动(失败安全)。

## 3. 明文缓存与清除

- `messages.plaintext` 仅为 UI 渲染缓存;设置项"清除明文"批量置 NULL(密文保留,可重解密——仅对仍持有会话密钥的群组)。
- 设备密钥加密:默认文件权限 0600;可选设置口令后以 argon2id 派生密钥加密 `identity.secret` 与设备私钥。
- 备份范围:仅身份助记词(24 词 BIP39);`identity` 支持助记词重建,重建后需重新扫码授权设备、重新加入群组(与 §DESIGN 一致)。

## 4. 事务与并发

- 单写者(守护进程内部单线程持有连接,WAL 允许多读)。
- 写操作全部走显式事务;`groups.state` 更新与 `messages` 插入同事务,保证 epoch 推进不丢消息。
