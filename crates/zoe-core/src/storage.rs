//! 存储层:应用 SQLite schema(docs/storage.md)+ openmls 持久化 provider。
//!
//! 分工:
//! - `ZoeStorage`:应用数据(身份、对端、群组元数据、消息、设置),rusqlite。
//! - `ZoeProvider`:openmls 的 OpenMlsProvider —— RustCrypto 密码实现 +
//!   openmls 官方 SqliteStorage(群组状态/KeyPackage/凭据等密码学状态)。
//! - 种子口令加密:argon2id 派生密钥 + ChaCha20-Poly1305(可选)。

use std::path::Path;
use std::sync::{Arc, Mutex};

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use openmls_rust_crypto::RustCrypto;
use openmls_sqlite_storage::{Codec, SqliteStorageProvider};
use openmls_traits::OpenMlsProvider;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Connection};
use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

pub const SCHEMA_VERSION: &str = "1";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS identity (
  user_id       BLOB PRIMARY KEY,
  secret        BLOB NOT NULL,
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
  device_id     BLOB PRIMARY KEY,
  user_id       BLOB NOT NULL,
  user_sig      BLOB NOT NULL,
  created_at    INTEGER NOT NULL,
  revoked_at    INTEGER
);
CREATE TABLE IF NOT EXISTS peers (
  peer_id       BLOB PRIMARY KEY,
  fingerprint   BLOB NOT NULL,
  display_name  TEXT,
  trust_status  INTEGER NOT NULL DEFAULT 0,
  first_seen    INTEGER NOT NULL,
  last_seen     INTEGER
);
CREATE TABLE IF NOT EXISTS key_packages (
  kp_ref        BLOB PRIMARY KEY,
  body          BLOB NOT NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL,
  used_at       INTEGER
);
CREATE TABLE IF NOT EXISTS groups (
  group_id      BLOB PRIMARY KEY,
  name          TEXT,
  epoch         INTEGER NOT NULL DEFAULT 0,
  coordinator   BLOB,
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  msg_hash      BLOB UNIQUE NOT NULL,
  envelope      BLOB NOT NULL,
  group_id      BLOB NOT NULL,
  epoch         INTEGER NOT NULL,
  sender        BLOB,
  seq           INTEGER,
  direction     INTEGER NOT NULL DEFAULT 0,
  status        INTEGER NOT NULL DEFAULT 0,
  plaintext     BLOB,
  received_at   INTEGER NOT NULL,
  delivered_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_msg_group ON messages(group_id, epoch, seq);
CREATE INDEX IF NOT EXISTS idx_msg_hash ON messages(msg_hash);
CREATE TABLE IF NOT EXISTS pending_proposals (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  group_id      BLOB NOT NULL,
  body          BLOB NOT NULL,
  created_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS transport_state (
  peer_id       BLOB NOT NULL,
  transport     TEXT NOT NULL,
  addr          TEXT,
  last_ok       INTEGER,
  quality       INTEGER,
  PRIMARY KEY (peer_id, transport)
);
"#;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("argon2 error: {0}")]
    Argon2(String),
    #[error("aead error")]
    Aead,
    #[error("no identity in storage (run init first)")]
    NoIdentity,
    #[error("invalid seed length {0}")]
    BadSeed(usize),
    #[error("openmls storage error: {0}")]
    OpenMlsStorage(String),
}

/// 共享数据库连接(WAL,单写者)。
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Arc<Self>, StorageError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
        }))
    }

    pub fn open_in_memory() -> Result<Arc<Self>, StorageError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
        }))
    }
}

// ---------------------------------------------------------------------------
// 应用层存储
// ---------------------------------------------------------------------------

pub struct ZoeStorage {
    db: Arc<Db>,
}

impl ZoeStorage {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    // --- meta / settings ---

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    // --- identity ---

    pub fn set_identity(&self, seed: &[u8; 32], created_at: i64) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO identity (user_id, secret, created_at) VALUES (?1, ?2, ?3)",
            params![seed, seed, created_at],
        )?;
        Ok(())
    }

    /// 返回 (身份种子, created_at)。
    pub fn identity(&self) -> Result<Option<([u8; 32], i64)>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT secret, created_at FROM identity LIMIT 1")?;
        let mut rows = stmt.query([])?;
        let row = rows.next()?;
        match row {
            None => Ok(None),
            Some(r) => {
                let secret: Vec<u8> = r.get(0)?;
                let created_at: i64 = r.get(1)?;
                if secret.len() != 32 {
                    return Err(StorageError::BadSeed(secret.len()));
                }
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&secret);
                Ok(Some((seed, created_at)))
            }
        }
    }

    // --- peers ---

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_peer(
        &self,
        peer_id: &[u8],
        fingerprint: &[u8],
        display_name: &str,
        trust_status: i64,
        first_seen: i64,
    ) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO peers (peer_id, fingerprint, display_name, trust_status, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(peer_id) DO UPDATE SET
               display_name = excluded.display_name,
               trust_status = excluded.trust_status,
               last_seen = excluded.last_seen",
            params![peer_id, fingerprint, display_name, trust_status, first_seen],
        )?;
        Ok(())
    }

    pub fn peers(&self) -> Result<Vec<PeerRecord>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT peer_id, fingerprint, display_name, trust_status, first_seen, last_seen FROM peers ORDER BY first_seen",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PeerRecord {
                peer_id: r.get(0)?,
                fingerprint: r.get(1)?,
                display_name: r.get(2)?,
                trust_status: r.get(3)?,
                first_seen: r.get(4)?,
                last_seen: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // --- groups ---

    pub fn create_group(
        &self,
        group_id: &[u8],
        name: &str,
        epoch: u64,
        coordinator: Option<&[u8]>,
        created_at: i64,
    ) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO groups (group_id, name, epoch, coordinator, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![group_id, name, epoch as i64, coordinator, created_at],
        )?;
        Ok(())
    }

    pub fn update_group_epoch(&self, group_id: &[u8], epoch: u64) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "UPDATE groups SET epoch = ?1 WHERE group_id = ?2",
            params![epoch as i64, group_id],
        )?;
        Ok(())
    }

    pub fn set_group_name(&self, group_id: &[u8], name: &str) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "UPDATE groups SET name = ?1 WHERE group_id = ?2",
            params![name, group_id],
        )?;
        Ok(())
    }

    pub fn groups(&self) -> Result<Vec<GroupRecord>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT group_id, name, epoch, coordinator, created_at FROM groups ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(GroupRecord {
                group_id: r.get(0)?,
                name: r.get(1)?,
                epoch: r.get::<_, i64>(2)? as u64,
                coordinator: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn group(&self, group_id: &[u8]) -> Result<Option<GroupRecord>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT group_id, name, epoch, coordinator, created_at FROM groups WHERE group_id = ?1",
        )?;
        let mut rows = stmt.query(params![group_id])?;
        let row = rows.next()?;
        match row {
            None => Ok(None),
            Some(r) => Ok(Some(GroupRecord {
                group_id: r.get(0)?,
                name: r.get(1)?,
                epoch: r.get::<_, i64>(2)? as u64,
                coordinator: r.get(3)?,
                created_at: r.get(4)?,
            })),
        }
    }

    // --- messages ---

    #[allow(clippy::too_many_arguments)]
    pub fn insert_message(
        &self,
        msg_hash: &[u8],
        envelope: &[u8],
        group_id: &[u8],
        epoch: u64,
        sender: Option<&[u8]>,
        seq: Option<u64>,
        direction: i64,
        status: i64,
        plaintext: Option<&[u8]>,
        received_at: i64,
    ) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO messages
               (msg_hash, envelope, group_id, epoch, sender, seq, direction, status, plaintext, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                msg_hash,
                envelope,
                group_id,
                epoch as i64,
                sender,
                seq.map(|s| s as i64),
                direction,
                status,
                plaintext,
                received_at
            ],
        )?;
        Ok(())
    }

    pub fn messages(
        &self,
        group_id: &[u8],
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<Vec<MessageRecord>, StorageError> {
        let conn = self.db.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT id, msg_hash, envelope, group_id, epoch, sender, seq, direction, status, plaintext, received_at
             FROM messages WHERE group_id = ?",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(group_id.to_vec())];
        if let Some(before) = before_id {
            sql.push_str(" AND id < ?");
            args.push(Box::new(before));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(Box::new(limit as i64));
        let conn_ref = conn;
        let mut stmt = conn_ref.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |r| {
            Ok(MessageRecord {
                id: r.get(0)?,
                msg_hash: r.get(1)?,
                envelope: r.get(2)?,
                group_id: r.get(3)?,
                epoch: r.get::<_, i64>(4)? as u64,
                sender: r.get(5)?,
                seq: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                direction: r.get(7)?,
                status: r.get(8)?,
                plaintext: r.get(9)?,
                received_at: r.get(10)?,
            })
        })?;
        let mut out = rows.collect::<Result<Vec<_>, _>>()?;
        out.reverse();
        Ok(out)
    }

    pub fn update_message_status(&self, msg_hash: &[u8], status: i64) -> Result<(), StorageError> {
        let conn = self.db.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET status = ?1 WHERE msg_hash = ?2",
            params![status, msg_hash],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub peer_id: Vec<u8>,
    pub fingerprint: Vec<u8>,
    pub display_name: Option<String>,
    pub trust_status: i64,
    pub first_seen: i64,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GroupRecord {
    pub group_id: Vec<u8>,
    pub name: Option<String>,
    pub epoch: u64,
    pub coordinator: Option<Vec<u8>>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub id: i64,
    pub msg_hash: Vec<u8>,
    pub envelope: Vec<u8>,
    pub group_id: Vec<u8>,
    pub epoch: u64,
    pub sender: Option<Vec<u8>>,
    pub seq: Option<u64>,
    pub direction: i64,
    pub status: i64,
    pub plaintext: Option<Vec<u8>>,
    pub received_at: i64,
}

// ---------------------------------------------------------------------------
// openmls provider:密码学状态持久化(官方 SqliteStorageProvider)
// ---------------------------------------------------------------------------

/// serde_json 编解码(Codec trait 的自定义实现)。
#[derive(Default)]
pub struct JsonCodec;

impl Codec for JsonCodec {
    type Error = serde_json::Error;

    fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(value)
    }

    fn from_slice<T: DeserializeOwned>(slice: &[u8]) -> Result<T, Self::Error> {
        serde_json::from_slice(slice)
    }
}

/// 组合 provider:RustCrypto 密码/随机实现 + 官方 SQLite 存储。
///
/// 注意:rusqlite Connection 是 Send 而非 Sync,`ZoeProvider` 需在互斥下
/// 使用(守护进程中用 `Mutex<ZoeProvider>` 串行化 MLS 操作)。
pub struct ZoeProvider {
    crypto: RustCrypto,
    rand: RustCrypto,
    storage: SqliteStorageProvider<JsonCodec, Connection>,
}

impl ZoeProvider {
    pub fn new(mls_db: &std::path::Path) -> Result<Self, StorageError> {
        let conn = Connection::open(mls_db)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut storage = SqliteStorageProvider::<JsonCodec, Connection>::new(conn);
        storage
            .run_migrations()
            .map_err(|e| StorageError::OpenMlsStorage(e.to_string()))?;
        Ok(Self {
            crypto: RustCrypto::default(),
            rand: RustCrypto::default(),
            storage,
        })
    }
}

impl OpenMlsProvider for ZoeProvider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = SqliteStorageProvider<JsonCodec, Connection>;

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.rand
    }

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }
}

// ---------------------------------------------------------------------------
// 身份种子口令加密(可选):argon2id → ChaCha20-Poly1305
// 输出格式: salt(16) || nonce(12) || ciphertext
// ---------------------------------------------------------------------------

pub const SEED_ENC_SALT_LEN: usize = 16;
pub const SEED_ENC_NONCE_LEN: usize = 12;

pub fn encrypt_seed(
    seed: &[u8; 32],
    password: &str,
    salt: &[u8; SEED_ENC_SALT_LEN],
) -> Result<Vec<u8>, StorageError> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| StorageError::Argon2(e.to_string()))?;
    let cipher = ChaCha20Poly1305::new(&Key::try_from(&key[..]).map_err(|_| StorageError::Aead)?);
    let mut nonce_bytes = [0u8; SEED_ENC_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ct = cipher
        .encrypt(
            &Nonce::try_from(&nonce_bytes[..]).map_err(|_| StorageError::Aead)?,
            Payload {
                msg: seed,
                aad: b"zoe-chat-identity-v1",
            },
        )
        .map_err(|_| StorageError::Aead)?;
    let mut out = Vec::with_capacity(salt.len() + nonce_bytes.len() + ct.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt_seed(blob: &[u8], password: &str) -> Result<[u8; 32], StorageError> {
    if blob.len() < SEED_ENC_SALT_LEN + SEED_ENC_NONCE_LEN + 16 {
        return Err(StorageError::BadSeed(blob.len()));
    }
    let (salt, rest) = blob.split_at(SEED_ENC_SALT_LEN);
    let (nonce, ct) = rest.split_at(SEED_ENC_NONCE_LEN);
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| StorageError::Argon2(e.to_string()))?;
    let cipher = ChaCha20Poly1305::new(&Key::try_from(&key[..]).map_err(|_| StorageError::Aead)?);
    let pt = cipher
        .decrypt(
            &Nonce::try_from(nonce).map_err(|_| StorageError::Aead)?,
            Payload {
                msg: ct,
                aad: b"zoe-chat-identity-v1",
            },
        )
        .map_err(|_| StorageError::Aead)?;
    if pt.len() != 32 {
        return Err(StorageError::BadSeed(pt.len()));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&pt);
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let s = ZoeStorage::new(db);
        s.set_meta("ui_theme", "dark").unwrap();
        assert_eq!(s.get_meta("ui_theme").unwrap(), Some("dark".to_string()));
        assert_eq!(s.get_meta("missing").unwrap(), None);
    }

    #[test]
    fn identity_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let s = ZoeStorage::new(db);
        assert!(s.identity().unwrap().is_none());
        s.set_identity(&[0x42; 32], 1234).unwrap();
        let (seed, created) = s.identity().unwrap().unwrap();
        assert_eq!(seed, [0x42; 32]);
        assert_eq!(created, 1234);
    }

    #[test]
    fn group_and_messages_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let s = ZoeStorage::new(db);
        let gid = b"group-1".to_vec();
        s.create_group(&gid, "test", 3, None, 1).unwrap();
        s.insert_message(
            b"hash1",
            b"env1",
            &gid,
            3,
            Some(b"sender"),
            Some(1),
            0,
            0,
            Some(b"hi"),
            10,
        )
        .unwrap();
        s.insert_message(
            b"hash2",
            b"env2",
            &gid,
            3,
            Some(b"sender"),
            Some(2),
            0,
            0,
            Some(b"yo"),
            11,
        )
        .unwrap();
        let msgs = s.messages(&gid, 100, None).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].plaintext.as_deref(), Some(b"hi".as_slice()));
        assert_eq!(msgs[1].plaintext.as_deref(), Some(b"yo".as_slice()));
        let groups = s.groups().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].epoch, 3);
        s.update_group_epoch(&gid, 4).unwrap();
        assert_eq!(s.group(&gid).unwrap().unwrap().epoch, 4);
    }

    #[test]
    fn seed_encryption_roundtrip() {
        let seed = [0x77; 32];
        let salt = [0x11; SEED_ENC_SALT_LEN];
        let blob = encrypt_seed(&seed, "correct horse", &salt).unwrap();
        assert_eq!(decrypt_seed(&blob, "correct horse").unwrap(), seed);
        assert!(decrypt_seed(&blob, "wrong password").is_err());
        // 篡改密文 → 认证失败
        let mut tampered = blob.clone();
        let n = tampered.len();
        tampered[n - 1] ^= 1;
        assert!(decrypt_seed(&tampered, "correct horse").is_err());
    }
}
