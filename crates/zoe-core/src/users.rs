//! 多用户注册表:每安装一个注册表(`data_dir/users.db`),多用户并存。
//!
//! - `Pin` 用户:独立数据目录 `users/<user_id_hex>/`(`zoe.db`+`mls.db`),
//!   身份种子仅以 `argon2id(PIN) → ChaCha20-Poly1305` 加密形态保存在注册表
//!   (`seed_enc` = salt||nonce||ciphertext;`pin_verifier` = argon2id PHC 串)。
//! - `Plain` 用户:兼容旧版单用户布局(数据在 `data_dir` 根,种子明文存
//!   `identity` 表)——首次启动自动迁移旧数据为 `default` 用户;可通过
//!   `set_pin` 升级为 `Pin` 用户(升级后种子重加密,不再落明文)。
//!
//! 切换用户 = 修改 `last_used` 后重启守护进程(v1 约定,文档见 README)。

use std::path::{Path, PathBuf};

use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Connection};
use thiserror::Error;

use crate::storage::{encrypt_seed, Db, ZoeStorage, SEED_ENC_SALT_LEN};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("invalid pin: must be 4..=100 chars")]
    BadPin,
    #[error("pin verification failed: {0}")]
    BadPinHash(String),
    #[error("user {0} is plain (no pin set)")]
    NotPinProtected(String),
    #[error("invalid identity seed length {0}")]
    BadSeed(usize),
}

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserKind {
    /// PIN 保护:种子加密存于注册表。
    Pin,
    /// 明文:旧版/默认用户,种子存于本用户 zoe.db 的 identity 表。
    Plain,
}

impl UserKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            UserKind::Pin => "pin",
            UserKind::Plain => "plain",
        }
    }
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pin" => Some(UserKind::Pin),
            "plain" => Some(UserKind::Plain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub user_id: [u8; 32],
    pub name: String,
    pub kind: UserKind,
    /// 相对 data_dir 的数据目录:Plain = 空(根目录);Pin = `users/<hex>`。
    pub dir: PathBuf,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

impl User {
    /// 用户数据目录绝对路径。
    pub fn data_path(&self, data_dir: &Path) -> PathBuf {
        data_dir.join(&self.dir)
    }
}

/// 最小 PIN 校验(4..=100 字符)。
pub const PIN_MIN: usize = 4;
pub const PIN_MAX: usize = 100;

fn validate_pin(pin: &str) -> Result<(), RegistryError> {
    if pin.len() < PIN_MIN || pin.len() > PIN_MAX {
        return Err(RegistryError::BadPin);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

const REGISTRY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  user_id       BLOB PRIMARY KEY,
  name          TEXT NOT NULL,
  kind          TEXT NOT NULL,
  dir           TEXT NOT NULL,
  seed_enc      BLOB,
  pin_verifier  TEXT,
  created_at    INTEGER NOT NULL,
  last_used     INTEGER
);
"#;

pub const REGISTRY_DB: &str = "users.db";

#[derive(Clone)]
pub struct UserRegistry {
    conn: std::sync::Arc<std::sync::Mutex<Connection>>,
    /// 数据目录(zoe.db 等所在根)。
    data_dir: PathBuf,
}

impl UserRegistry {
    /// 打开(必要时创建)`data_dir/users.db`。
    pub fn open(data_dir: &Path) -> Result<Self, RegistryError> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join(REGISTRY_DB))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(REGISTRY_SCHEMA)?;
        Ok(Self {
            conn: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            data_dir: data_dir.to_path_buf(),
        })
    }

    // --- 查询 ---

    pub fn list(&self) -> Result<Vec<User>, RegistryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, name, kind, dir, seed_enc, pin_verifier, created_at, last_used
             FROM users ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<i64>>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, kind, dir, _seed_enc, _verifier, created, last_used) = row?;
            let user_id: [u8; 32] =
                id.clone().try_into().map_err(|_| RegistryError::BadSeed(id.len()))?;
            let kind = UserKind::from_str(&kind).ok_or_else(|| {
                RegistryError::NotFound(format!("unknown kind {kind} for user {name}"))
            })?;
            let _ = (id, kind);
            out.push(User {
                user_id,
                name,
                kind,
                dir: PathBuf::from(dir),
                created_at: created,
                last_used,
            });
        }
        Ok(out)
    }

    pub fn get(&self, user_id: &[u8]) -> Result<User, RegistryError> {
        self.list()?
            .into_iter()
            .find(|u| u.user_id == user_id)
            .ok_or_else(|| RegistryError::NotFound(hex::encode(user_id)))
    }

    /// 最近使用用户(v1 默认选中)。
    pub fn most_recent(&self) -> Result<Option<User>, RegistryError> {
        Ok(self.list()?.into_iter().max_by_key(|u| u.last_used).or_else(|| {
            // 无 last_used 记录时退回最先创建者(迁移场景)
            self.list()
                .ok()
                .and_then(|all| all.into_iter().min_by_key(|u| u.created_at))
        }))
    }

    pub fn set_last_used(&self, user_id: &[u8]) -> Result<(), RegistryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET last_used = ?2 WHERE user_id = ?1",
            params![user_id, now_i64()],
        )?;
        Ok(())
    }

    // --- 创建 ---

/// 创建 PIN 保护用户:独立数据目录 + 加密种子 + argon2id 校验串。
/// 返回新用户。
pub fn add_pin_user(
    &self,
    name: &str,
    pin: &str,
    seed: &[u8; 32],
) -> Result<User, RegistryError> {
    validate_pin(pin)?;
    if name.trim().is_empty() || name.len() > 64 {
        return Err(RegistryError::BadPin); // 复用:非法名称同样拒绝
    }

    let mut user_id = [0u8; 32];
    OsRng.fill_bytes(&mut user_id);
    let dir = PathBuf::from(format!("users/{}", hex::encode(user_id)));
    let user_dir = self.data_dir.join(&dir);
    std::fs::create_dir_all(&user_dir)?;

    // 密码学:派生密钥加密种子 + 生成 PIN 校验串
    let mut salt = [0u8; SEED_ENC_SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let seed_enc = encrypt_seed(seed, pin, &salt)?;
    let verifier = pin_verifier(pin)?;

    // 初始化用户 zoe.db(空身份表;身份只以加密形态在注册表)与 mls.db
    let db = Db::open(&user_dir.join("zoe.db"))?;
    let storage = ZoeStorage::new(db);
    storage.set_meta("seed_enc", "1")?;
    storage.set_meta("user_id", &hex::encode(user_id))?;
    let _ = crate::storage::ZoeProvider::new(&user_dir.join("mls.db"))?;

    self.insert(
        &user_id,
        name,
        UserKind::Pin,
        &dir,
        Some(seed_enc),
        Some(verifier),
        now_i64(),
    )
}

/// 创建明文用户(旧版布局,数据在 data_dir 根;种子由调用方负责写入
/// 该用户 zoe.db 的 identity 表 —— 本方法只登记)。
pub fn add_plain_user(&self, name: &str, created_at: i64) -> Result<User, RegistryError> {
    if name.trim().is_empty() || name.len() > 64 {
        return Err(RegistryError::BadPin);
    }
    let mut user_id = [0u8; 32];
    OsRng.fill_bytes(&mut user_id);
    self.insert(
        &user_id,
        name,
        UserKind::Plain,
        &PathBuf::new(),
        None,
        None,
        created_at,
    )
}

#[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        user_id: &[u8; 32],
        name: &str,
        kind: UserKind,
        dir: &Path,
        seed_enc: Option<Vec<u8>>,
        verifier: Option<String>,
        created_at: i64,
    ) -> Result<User, RegistryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO users (user_id, name, kind, dir, seed_enc, pin_verifier, created_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                user_id.as_slice(),
                name,
                kind.as_str(),
                dir.to_string_lossy().to_string(),
                seed_enc,
                verifier,
                created_at
            ],
        )?;
        Ok(User {
            user_id: *user_id,
            name: name.to_string(),
            kind,
            dir: dir.to_path_buf(),
            created_at,
            last_used: Some(now_i64()),
        })
    }

    // --- PIN 验证 / 种子解密 / 升级 ---

    /// 校验 PIN(恒时比较交给 argon2;错误返回 Ok(false) 不泄露原因)。
    pub fn verify_pin(&self, user_id: &[u8], pin: &str) -> Result<bool, RegistryError> {
        let user = self.get(user_id)?;
        if user.kind != UserKind::Pin {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let verifier: Option<String> = conn.query_row(
            "SELECT pin_verifier FROM users WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )?;
        let Some(phc) = verifier else {
            return Ok(false);
        };
        verify_pin_hash(pin, &phc)
    }

    /// 解密身份种子(PIN 用户)。错误 PIN 返回 Err(BadPinHash)。
    pub fn decrypt_seed(&self, user_id: &[u8], pin: &str) -> Result<[u8; 32], RegistryError> {
        let user = self.get(user_id)?;
        if user.kind != UserKind::Pin {
            return Err(RegistryError::NotPinProtected(hex::encode(user_id)));
        }
        let conn = self.conn.lock().unwrap();
        let seed_enc: Option<Vec<u8>> = conn.query_row(
            "SELECT seed_enc FROM users WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )?;
        let Some(blob) = seed_enc else {
            return Err(RegistryError::NotFound(hex::encode(user_id)));
        };
        crate::storage::decrypt_seed(&blob, pin).map_err(|_| {
            RegistryError::BadPinHash(hex::encode(user_id))
        })
    }

    /// 升级为 PIN 保护(或将 PIN 更换为新值):重新加密种子并换校验串。
    /// 要求调用方持有当前身份的种子(即已解锁)。返回新的 pin_verifier。
    pub fn set_pin(&self, user_id: &[u8], new_pin: &str, seed: &[u8; 32]) -> Result<(), RegistryError> {
        validate_pin(new_pin)?;
        let mut salt = [0u8; SEED_ENC_SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let seed_enc = encrypt_seed(seed, new_pin, &salt)?;
        let verifier = pin_verifier(new_pin)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE users SET kind = 'pin', seed_enc = ?2, pin_verifier = ?3 WHERE user_id = ?1",
            params![user_id, seed_enc, verifier],
        )?;
        Ok(())
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

// ---------------------------------------------------------------------------
// argon2id PHC 工具
// ---------------------------------------------------------------------------

fn pin_verifier(pin: &str) -> Result<String, RegistryError> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    use argon2::Argon2;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| RegistryError::BadPinHash(e.to_string()))
}

fn verify_pin_hash(pin: &str, phc: &str) -> Result<bool, RegistryError> {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;
    let parsed = PasswordHash::new(phc).map_err(|e| RegistryError::BadPinHash(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(pin.as_bytes(), &parsed)
        .is_ok())
}

fn now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityKeyPair;
    use std::sync::Once;

    static TMP_DIRS: Once = Once::new();

    fn tmp_dir(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join("zoe-registry-tests");
        TMP_DIRS.call_once(|| {
            let _ = std::fs::remove_dir_all(&base);
        });
        let dir = base.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registry_empty() {
        let reg = UserRegistry::open(&tmp_dir("empty")).unwrap();
        assert!(reg.list().unwrap().is_empty());
        assert!(reg.most_recent().unwrap().is_none());
    }

    #[test]
    fn pin_user_roundtrip() {
        let dir = tmp_dir("pin");
        let reg = UserRegistry::open(&dir).unwrap();
        let seed = IdentityKeyPair::generate().seed();
        let user = reg.add_pin_user("alice", "123456", &seed).unwrap();
        assert_eq!(user.kind, UserKind::Pin);
        assert!(user.data_path(&dir).join("zoe.db").exists());
        assert!(user.data_path(&dir).join("mls.db").exists());

        // 校验通过
        assert!(reg.verify_pin(&user.user_id, "123456").unwrap());
        // 错误 PIN 拒绝
        assert!(!reg.verify_pin(&user.user_id, "000000").unwrap());
        // 种子可解回
        let dec = reg.decrypt_seed(&user.user_id, "123456").unwrap();
        assert_eq!(dec, seed);
        assert!(reg.decrypt_seed(&user.user_id, "bad-pin").is_err());
        // last_used
        assert!(reg.most_recent().unwrap().unwrap().user_id == user.user_id);
    }

    #[test]
    fn plain_user_and_upgrade() {
        let dir = tmp_dir("plain");
        let reg = UserRegistry::open(&dir).unwrap();
        let user = reg.add_plain_user("default", 1000).unwrap();
        assert_eq!(user.kind, UserKind::Plain);
        assert_eq!(user.dir, PathBuf::new());
        assert!(!reg.verify_pin(&user.user_id, "whatever").unwrap());
        assert!(reg.decrypt_seed(&user.user_id, "x").is_err());

        // 升级 → pin
        let seed = IdentityKeyPair::generate().seed();
        reg.set_pin(&user.user_id, "9876", &seed).unwrap();
        let u2 = reg.get(&user.user_id).unwrap();
        assert_eq!(u2.kind, UserKind::Pin);
        assert!(reg.verify_pin(&user.user_id, "9876").unwrap());
        assert_eq!(reg.decrypt_seed(&user.user_id, "9876").unwrap(), seed);
    }

    #[test]
    fn pin_policy() {
        let reg = UserRegistry::open(&tmp_dir("policy")).unwrap();
        let seed = IdentityKeyPair::generate().seed();
        assert!(reg.add_pin_user("a", "12", &seed).is_err()); // 过短
        assert!(reg.add_pin_user("a", &"x".repeat(101), &seed).is_err()); // 过长
        assert!(reg.add_pin_user("", "1234", &seed).is_err()); // 空名
        assert!(reg.add_pin_user("ok", "1234", &seed).is_ok());
    }

    #[test]
    fn multiple_users_isolated() {
        let dir = tmp_dir("multi");
        let reg = UserRegistry::open(&dir).unwrap();
        let s1 = IdentityKeyPair::generate().seed();
        let s2 = IdentityKeyPair::generate().seed();
        let u1 = reg.add_pin_user("alice", "1111", &s1).unwrap();
        let u2 = reg.add_pin_user("bob", "2222", &s2).unwrap();
        assert_eq!(reg.list().unwrap().len(), 2);
        assert_ne!(u1.user_id, u2.user_id);
        assert!(u1.data_path(&dir).exists());
        assert!(u2.data_path(&dir).exists());
        // 各自解密互不干扰
        assert_eq!(reg.decrypt_seed(&u1.user_id, "1111").unwrap(), s1);
        assert_eq!(reg.decrypt_seed(&u2.user_id, "2222").unwrap(), s2);
        assert!(reg.decrypt_seed(&u1.user_id, "2222").is_err());
    }
}