//! MLS 会话封装(openmls 0.8.1,锁版本)。
//!
//! 设备级 MLS 身份(签名钥 + BasicCredential + CredentialWithKey)与会话操作:
//! 建群、加入(StagedWelcome)、加人/移除/update、消息加解密。
//! M0 不持久化;M1 经 openmls StorageProvider 接入 SQLite。

use openmls::group::{GroupId, MlsGroup, MlsGroupJoinConfig, StagedWelcome};
use openmls::prelude::tls_codec::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use thiserror::Error;

pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

#[derive(Error, Debug)]
pub enum MlsError {
    #[error("openmls error: {0}")]
    OpenMls(String),
    #[error("message is not a welcome")]
    NotWelcome,
    #[error("invalid key package: {0}")]
    BadKeyPackage(String),
}

fn str_err<E: std::fmt::Display>(e: E) -> MlsError {
    MlsError::OpenMls(e.to_string())
}

/// 设备级 MLS 身份。
pub struct MlsIdentity {
    name: String,
    signer: SignatureKeyPair,
    identifier: Vec<u8>,
}

impl MlsIdentity {
    /// 从 32 字节种子构造(种子由用户身份派生或随机生成)。
    pub fn new(name: &str, seed: &[u8; 32]) -> Result<Self, MlsError> {
        let signing = ed25519_dalek::SigningKey::from_bytes(seed);
        let signer = SignatureKeyPair::from_raw(
            SignatureScheme::ED25519,
            signing.to_bytes().to_vec(),
            signing.verifying_key().to_bytes().to_vec(),
        );
        Ok(Self {
            name: name.to_string(),
            signer,
            identifier: name.as_bytes().to_vec(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn signer(&self) -> &SignatureKeyPair {
        &self.signer
    }

    pub fn signature_public_key(&self) -> &[u8] {
        self.signer.public()
    }

    fn credential_with_key(&self) -> CredentialWithKey {
        CredentialWithKey {
            credential: BasicCredential::new(self.identifier.clone()).into(),
            signature_key: self.signer.public().into(),
        }
    }
}

/// 处理一条 MLS 消息的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum Processed {
    /// 应用消息(解密后的明文)
    Message(Vec<u8>),
    /// 群组变更(commit/update/remove 等,已应用)
    GroupChange,
}

/// 一个群组的 MLS 会话。
pub struct MlsSession {
    group: MlsGroup,
}

impl MlsSession {
    /// 创建新群组(创建者为唯一成员)。
    ///
    /// 启用 `use_ratchet_tree_extension`:Welcome 携带 ratchet tree,
    /// 加入者无需带外获取树(无服务器架构的关键;大群组下 Welcome 体积
    /// 随成员数增长,M4 评估 DMLS 时可再优化)。
    pub fn create_group(
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
        group_id: &[u8],
    ) -> Result<Self, MlsError> {
        let group = MlsGroup::builder()
            .with_group_id(GroupId::from_slice(group_id))
            .use_ratchet_tree_extension(true)
            .build(provider, id.signer(), id.credential_with_key())
            .map_err(str_err)?;
        Ok(Self { group })
    }

    /// 生成本设备的 KeyPackage(TLS 序列化,用于被邀请)。
    pub fn new_key_package(
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
    ) -> Result<Vec<u8>, MlsError> {
        let bundle = KeyPackage::builder()
            .build(CIPHERSUITE, provider, id.signer(), id.credential_with_key())
            .map_err(str_err)?;
        bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(str_err)
    }

    /// 解析并验证对方 KeyPackage 字节。
    pub fn key_package_from_bytes(
        provider: &impl OpenMlsProvider,
        bytes: &[u8],
    ) -> Result<KeyPackage, MlsError> {
        let kp_in = KeyPackageIn::tls_deserialize(&mut &bytes[..]).map_err(str_err)?;
        kp_in
            .validate(provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::BadKeyPackage(e.to_string()))
    }

    /// 加入群组(消费 Welcome 消息)。
    pub fn join(provider: &impl OpenMlsProvider, welcome: &[u8]) -> Result<Self, MlsError> {
        let (msg, _rest) = MlsMessageIn::tls_deserialize_bytes(welcome).map_err(str_err)?;
        let welcome = match msg.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(MlsError::NotWelcome),
        };
        let join_config = MlsGroupJoinConfig::builder().build();
        let staged = StagedWelcome::new_from_welcome(provider, &join_config, welcome, None)
            .map_err(str_err)?;
        let group = staged.into_group(provider).map_err(str_err)?;
        Ok(Self { group })
    }

    /// 添加成员(协调者)。返回 (commit 字节, Welcome 字节, 新 epoch)。
    ///
    /// commit 需广播给既有成员;Welcome 发给新成员。本方法自动合并自身
    /// 的 pending commit(协调者即提交者)。
    pub fn add_member(
        &mut self,
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
        kp: &KeyPackage,
    ) -> Result<(Vec<u8>, Vec<u8>, u64), MlsError> {
        let (commit, welcome, _group_info) = self
            .group
            .add_members(provider, id.signer(), std::slice::from_ref(kp))
            .map_err(str_err)?;
        let commit_bytes = commit.tls_serialize_detached().map_err(str_err)?;
        let welcome_bytes = welcome.tls_serialize_detached().map_err(str_err)?;
        self.group.merge_pending_commit(provider).map_err(str_err)?;
        Ok((commit_bytes, welcome_bytes, self.epoch()))
    }

    /// 成员自我更新(轮换密钥,恢复 PCS)。返回 commit 消息字节。
    pub fn self_update(
        &mut self,
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
    ) -> Result<Vec<u8>, MlsError> {
        let bundle = self
            .group
            .self_update(provider, id.signer(), LeafNodeParameters::default())
            .map_err(str_err)?;
        let bytes = bundle.commit().tls_serialize_detached().map_err(str_err)?;
        self.group.merge_pending_commit(provider).map_err(str_err)?;
        Ok(bytes)
    }

    /// 移除成员(协调者)。返回 commit 消息字节。
    pub fn remove_member(
        &mut self,
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, MlsError> {
        let (commit, _welcome, _group_info) = self
            .group
            .remove_members(provider, id.signer(), &[LeafNodeIndex::new(leaf_index)])
            .map_err(str_err)?;
        let bytes = commit.tls_serialize_detached().map_err(str_err)?;
        self.group.merge_pending_commit(provider).map_err(str_err)?;
        Ok(bytes)
    }

    /// 加密应用消息,返回 TLS 序列化的 MLS 私密消息。
    pub fn encrypt(
        &mut self,
        provider: &impl OpenMlsProvider,
        id: &MlsIdentity,
        msg: &[u8],
    ) -> Result<Vec<u8>, MlsError> {
        let out = self
            .group
            .create_message(provider, id.signer(), msg)
            .map_err(str_err)?;
        out.tls_serialize_detached().map_err(str_err)
    }

    /// 处理入站 MLS 消息(私密消息 → 明文;commit → 应用群组变更)。
    ///
    /// openmls 0.8:收到 commit 后组状态为 PendingCommit,必须显式
    /// `merge_staged_commit` 才推进 epoch(本方法自动完成)。
    pub fn process(
        &mut self,
        provider: &impl OpenMlsProvider,
        bytes: &[u8],
    ) -> Result<Processed, MlsError> {
        let (msg, _rest) = MlsMessageIn::tls_deserialize_bytes(bytes).map_err(str_err)?;
        let proto = msg.try_into_protocol_message().map_err(str_err)?;
        let processed = self
            .group
            .process_message(provider, proto)
            .map_err(str_err)?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                Ok(Processed::Message(app.into_bytes()))
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                self.group
                    .merge_staged_commit(provider, *staged)
                    .map_err(str_err)?;
                Ok(Processed::GroupChange)
            }
            _ => Ok(Processed::GroupChange),
        }
    }

    /// 从持久化状态加载群组会话(provider.storage() 为写穿式存储)。
    pub fn load(
        provider: &impl OpenMlsProvider,
        group_id: &[u8],
    ) -> Result<Option<MlsSession>, MlsError> {
        let group =
            MlsGroup::load(provider.storage(), &GroupId::from_slice(group_id)).map_err(str_err)?;
        Ok(group.map(|group| Self { group }))
    }

    pub fn epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// 本设备在 MLS 树中的 leaf 序号(信封 sender 字段 / 成员名解析用)。
    pub fn own_leaf_index(&self) -> u32 {
        self.group.own_leaf_index().u32()
    }

    pub fn members(&self) -> Vec<u32> {
        self.group.members().map(|m| m.index.u32()).collect()
    }

    pub fn group_id(&self) -> Vec<u8> {
        self.group.group_id().to_vec()
    }
}
