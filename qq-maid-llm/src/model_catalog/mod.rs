//! Effective Model Catalog：只负责“模型元数据”的领域模块。
//!
//! 数据流：
//! `embedded catalog / valid cache` → `official compatibility patches` →
//! `config/models.json local overrides` → `Effective Model Catalog`。
//!
//! 边界约束：
//! - Catalog 只描述模型元数据（上下文窗口、模态、能力声明、价格、状态等），
//!   不包含 Base URL、认证头、API Key、Adapter 类型等 Connection/Credential 安全字段；
//!   这些仍由 `agent.toml` / 配置中心与 `LlmConfig` 管理。
//! - Catalog Provider 是 Models.dev 等上游用来组织模型元数据的供应商身份，独立于
//!   运行时 Provider Connection。`google` 不会被改写成 `gemini`，`zhipuai` 也不会被
//!   改写成 `bigmodel`；后续由 Connection 映射层按连接规则解释，而不是在 Catalog 内改写。
//! - 规范化 Catalog 的每项能力只保留 `advertised`（上游声明）。`adapter_supported`
//!   属于 Connection/Adapter 解析阶段，不由远程 Catalog 控制，也不允许 `models.json`
//!   伪造；`verified` / `effective` 属于后续 resolved capability，本基础层不承接。
//! - Catalog 不作为 Route 白名单：Route 中引用但 Catalog 未收录的模型按
//!   “未知模型 / 能力未知”处理，不阻断启动。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::LlmError;

pub mod converter;

/// 当前 Catalog 规范化 Schema 版本；不兼容变更时递增。
pub const CATALOG_SCHEMA_VERSION: u32 = 1;
/// Models.dev 快照转换器的固定版本标识；只随转换规则变更递增。
pub const CONVERTER_VERSION: &str = "modelsdev-converter-v1";
/// 本地 `models.json` 的 Schema 版本。
const LOCAL_SCHEMA_VERSION: u32 = 1;

/// 编译期嵌入的规范化 Catalog 快照，保证 Release 离线启动，构建时不联网。
const EMBEDDED_SNAPSHOT_BYTES: &[u8] = include_bytes!("../../assets/model-catalog.json");

/// 本地覆盖文件中禁止出现的字段名（大小写不敏感、精确匹配键名）。
/// 这些字段属于 Connection / Credential / Route / 业务工具权限，
/// 一律由 `agent.toml` 与配置中心管理，Catalog 无权触碰。
const FORBIDDEN_LOCAL_KEYS: &[&str] = &[
    "adapter",
    "api_key",
    "api_key_env",
    "auth",
    "auth_header",
    "auth_scheme",
    "base_url",
    "baseurl",
    "command",
    "credential",
    "credentials",
    "enabled_tools",
    "provider_adapter",
    "route",
    "routes",
    "script",
    "shell",
    "tool_whitelist",
    "tools",
    "url",
];

// ---- Catalog Provider ID ----

/// Catalog 自己的 Provider ID。
///
/// 它只代表模型目录中的供应商身份，不经过运行时
/// [`ModelProvider`](crate::provider::types::ModelProvider) 的 `google -> gemini`、
/// `glm/zhipu -> bigmodel` 等 Connection alias。一个 Catalog Provider 后续可以映射到
/// 多个 Connection，但 ID 本身不因连接规则被改写。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CatalogProviderId(String);

impl CatalogProviderId {
    /// 解析并校验 Catalog Provider ID；只做小写归一化，不应用任何 Connection alias。
    pub fn parse(value: &str) -> Result<Self, LlmError> {
        let normalized = value.trim().to_ascii_lowercase();
        if !is_valid_provider_id(&normalized) {
            return Err(LlmError::config(format!(
                "Catalog provider id `{value}` 非法：只允许小写字母开头，包含字母/数字/下划线/连字符"
            )));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CatalogProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn is_valid_provider_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

// ---- 嵌入快照 / 官方补丁共用的规范化 Schema ----

/// 规范化 Catalog 快照：内置资产、将来的远程缓存与官方兼容补丁共用此结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub schema_version: u32,
    pub source: CatalogSource,
    #[serde(default)]
    pub providers: Vec<CatalogProvider>,
}

/// 快照来源元数据；所有字段都必须来自真实生成过程，不允许占位值。
/// `source_hash` 是转换器实际输入字节的 SHA-256，`source_version` 是上游版本/commit。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
    pub name: String,
    pub upstream_license: String,
    pub source_url: String,
    pub source_version: String,
    pub fetched_at: String,
    pub source_hash: String,
    pub converter_version: String,
}

/// Catalog Provider 分组：模型元数据的供应商身份，不承载任何连接配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogProvider {
    pub id: CatalogProviderId,
    pub name: String,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// 规范化目录模型；`enabled` 是本地覆盖语义，不属于可远程控制的模型元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub modalities: ModelModalities,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub status: ModelStatus,
    #[serde(default)]
    pub price: Option<ModelPrice>,
}

/// 模型输入 / 输出模态；白名单覆盖 Models.dev 当前文本聊天相关模态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Pdf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ModelModalities {
    #[serde(default)]
    pub input: Vec<Modality>,
    #[serde(default)]
    pub output: Vec<Modality>,
}

/// 能力声明集合。Catalog 基础层每项只有 `advertised` 上游声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub reasoning: CapabilityClaim,
    #[serde(default)]
    pub tool_calling: CapabilityClaim,
    #[serde(default)]
    pub vision: CapabilityClaim,
}

/// 单项能力的上游声明；只允许 Catalog 声明存在与否。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CapabilityClaim {
    #[serde(default)]
    pub advertised: bool,
}

/// 模型生命周期状态；与本地 `enabled`（覆盖开关）正交。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    #[default]
    Active,
    Beta,
    Deprecated,
}

/// 计费信息（每百万 token 美元价）；仅作展示参考。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ModelPrice {
    #[serde(default)]
    pub input_per_million_usd: Option<f64>,
    #[serde(default)]
    pub output_per_million_usd: Option<f64>,
}

// ---- 字段级 provenance ----

/// 字段 / 能力声明的数据来源层。
///
/// 模型整体不会被打上单一 `Local` 标签；本地只覆盖存在字段，未覆盖字段继续指向
/// `Catalog` / `OfficialPatch`，从而避免“改了一个字段就变成整个模型都来自本地”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    #[default]
    Catalog,
    OfficialPatch,
    LocalOverride,
}

/// 模型逐字段来源；`capabilities` 再按能力项细分，用于后续展示声明来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelProvenance {
    pub display_name: FieldSource,
    pub context_window: FieldSource,
    pub max_output_tokens: FieldSource,
    pub modalities: FieldSource,
    pub status: FieldSource,
    pub price: FieldSource,
    pub capabilities: CapabilityProvenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilityProvenance {
    pub reasoning: FieldSource,
    pub tool_calling: FieldSource,
    pub vision: FieldSource,
}

impl ModelProvenance {
    fn from_catalog() -> Self {
        Self::default()
    }

    fn from_patch() -> Self {
        Self {
            display_name: FieldSource::OfficialPatch,
            context_window: FieldSource::OfficialPatch,
            max_output_tokens: FieldSource::OfficialPatch,
            modalities: FieldSource::OfficialPatch,
            status: FieldSource::OfficialPatch,
            price: FieldSource::OfficialPatch,
            capabilities: CapabilityProvenance {
                reasoning: FieldSource::OfficialPatch,
                tool_calling: FieldSource::OfficialPatch,
                vision: FieldSource::OfficialPatch,
            },
        }
    }

    fn from_local_override() -> Self {
        Self {
            display_name: FieldSource::LocalOverride,
            context_window: FieldSource::LocalOverride,
            max_output_tokens: FieldSource::LocalOverride,
            modalities: FieldSource::LocalOverride,
            status: FieldSource::LocalOverride,
            price: FieldSource::LocalOverride,
            capabilities: CapabilityProvenance {
                reasoning: FieldSource::LocalOverride,
                tool_calling: FieldSource::LocalOverride,
                vision: FieldSource::LocalOverride,
            },
        }
    }
}

// ---- Effective Catalog 输出模型 ----

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveCatalogProvider {
    pub id: CatalogProviderId,
    pub name: String,
    pub models: Vec<EffectiveCatalogModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveCatalogModel {
    pub id: String,
    pub display_name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub modalities: ModelModalities,
    pub capabilities: ModelCapabilities,
    pub status: ModelStatus,
    /// 本地禁用后的有效状态；Catalog 基础层不直接承载该字段，默认保持启用。
    pub enabled: bool,
    pub price: Option<ModelPrice>,
    pub provenance: ModelProvenance,
}

// ---- config/models.json 本地覆盖 Schema ----

/// `config/models.json` 顶层结构；字段全部可选，按“存在的字段覆盖”语义合并。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalModelOverrides {
    #[serde(default = "default_local_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub models: Vec<LocalModelEntry>,
}

/// 本地覆盖 / 追加条目；所有元数据字段可选，缺失即继承目录值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalModelEntry {
    /// Catalog Provider ID；不受运行时 `google -> gemini` 等 alias 影响。
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
    #[serde(default)]
    pub capabilities: Option<LocalCapabilities>,
    #[serde(default)]
    pub status: Option<ModelStatus>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub price: Option<ModelPrice>,
}

/// 本地能力的部分覆盖；缺省能力项保持目录值不变。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct LocalCapabilities {
    #[serde(default)]
    pub reasoning: Option<CapabilityClaim>,
    #[serde(default)]
    pub tool_calling: Option<CapabilityClaim>,
    #[serde(default)]
    pub vision: Option<CapabilityClaim>,
}

fn default_local_schema_version() -> u32 {
    LOCAL_SCHEMA_VERSION
}

// ---- 解析与校验 ----

/// 解析规范化 Catalog 快照字节（嵌入资产、缓存或官方补丁均复用此入口）。
/// 解析或校验失败返回带明确中文信息的配置错误，绝不 panic。
pub fn parse_snapshot(bytes: &[u8]) -> Result<CatalogSnapshot, LlmError> {
    let snapshot: CatalogSnapshot = serde_json::from_slice(bytes)
        .map_err(|error| LlmError::config(format!("内置模型目录快照解析失败：{error}")))?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub(crate) fn validate_snapshot(snapshot: &CatalogSnapshot) -> Result<(), LlmError> {
    if snapshot.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(LlmError::config(format!(
            "内置模型目录快照 schema_version 不受支持：{}（当前支持 {}）",
            snapshot.schema_version, CATALOG_SCHEMA_VERSION
        )));
    }
    if snapshot.source.name.trim().is_empty()
        || snapshot.source.upstream_license.trim().is_empty()
        || snapshot.source.source_url.trim().is_empty()
        || snapshot.source.source_version.trim().is_empty()
        || snapshot.source.fetched_at.trim().is_empty()
        || snapshot.source.source_hash.trim().is_empty()
        || snapshot.source.converter_version.trim().is_empty()
    {
        return Err(LlmError::config(
            "内置模型目录快照来源元数据不完整，必须来自真实生成过程",
        ));
    }
    let mut provider_ids = Vec::new();
    for provider in &snapshot.providers {
        // 反序列化只保证字符串外观，这里再做一次类型级校验，防止调用方手工构造非法值。
        CatalogProviderId::parse(provider.id.as_str()).map_err(|_| {
            LlmError::config(format!(
                "内置模型目录存在非法 provider id：`{}`",
                provider.id.as_str()
            ))
        })?;
        provider_ids.push(provider.id.as_str().to_owned());
        let mut model_ids = Vec::new();
        for model in &provider.models {
            if model.id.trim().is_empty() || model.display_name.trim().is_empty() {
                return Err(LlmError::config(format!(
                    "内置模型目录 provider `{}` 存在缺少 id 或 display_name 的模型",
                    provider.id
                )));
            }
            model_ids.push(model.id.clone());
        }
        ensure_unique(
            &model_ids,
            &format!("内置模型目录 provider `{}` 的模型 id", provider.id),
        )?;
    }
    ensure_unique(&provider_ids, "内置模型目录的 provider id")?;
    Ok(())
}

fn ensure_unique(values: &[String], label: &str) -> Result<(), LlmError> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            return Err(LlmError::config(format!("{label} 重复：`{value}`")));
        }
    }
    Ok(())
}

/// 递归检查本地配置 JSON 中是否出现禁止字段（Connection / Credential / Route /
/// 工具权限等）。命中即拒绝并明确报错，绝不静默忽略。
fn reject_forbidden_local_keys(value: &serde_json::Value) -> Result<(), LlmError> {
    let mut found = std::collections::BTreeSet::new();
    collect_forbidden_keys(value, &mut found);
    if found.is_empty() {
        Ok(())
    } else {
        Err(LlmError::config(format!(
            "models.json 含有 Catalog 禁止的字段（Base URL / Credential / Adapter / Route / \
             工具权限等均由 agent.toml 与配置中心管理）：{}",
            found.into_iter().collect::<Vec<_>>().join(", ")
        )))
    }
}

fn collect_forbidden_keys(
    value: &serde_json::Value,
    found: &mut std::collections::BTreeSet<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if FORBIDDEN_LOCAL_KEYS
                    .iter()
                    .any(|bad| bad.eq_ignore_ascii_case(key))
                {
                    found.insert(key.to_ascii_lowercase());
                }
                collect_forbidden_keys(child, found);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_forbidden_keys(item, found);
            }
        }
        _ => {}
    }
}

/// 解析 `models.json` 字节为本地覆盖结构；非法或禁止字段一律报错。
pub fn parse_local_overrides(bytes: &[u8]) -> Result<LocalModelOverrides, LlmError> {
    let raw: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| LlmError::config(format!("models.json 解析失败：{error}")))?;
    reject_forbidden_local_keys(&raw)?;
    let overrides: LocalModelOverrides = serde_json::from_value(raw)
        .map_err(|error| LlmError::config(format!("models.json Schema 校验失败：{error}")))?;
    if overrides.schema_version != LOCAL_SCHEMA_VERSION {
        return Err(LlmError::config(format!(
            "models.json schema_version 不受支持：{}（当前支持 {}）",
            overrides.schema_version, LOCAL_SCHEMA_VERSION
        )));
    }
    for entry in &overrides.models {
        if entry.id.trim().is_empty() {
            return Err(LlmError::config("models.json 存在缺少 id 的模型条目"));
        }
        CatalogProviderId::parse(&entry.provider).map_err(|_| {
            LlmError::config(format!(
                "models.json 条目 `{}` 的 provider 非法：`{}`",
                entry.id, entry.provider
            ))
        })?;
    }
    Ok(overrides)
}

/// 从磁盘读取本地覆盖文件；文件不存在时返回 `None`（使用纯内置目录），
/// 存在但非法时明确报错，不静默跳过，也不改写用户文件。
pub fn load_local_overrides(path: &Path) -> Result<Option<LocalModelOverrides>, LlmError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|error| {
        LlmError::config(format!(
            "models.json 读取失败（{}）：{error}",
            path.display()
        ))
    })?;
    parse_local_overrides(&bytes).map(Some)
}

// ---- Effective Catalog 合并 ----

/// 合并后的确定性 Effective Model Catalog。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveModelCatalog {
    base_source: CatalogSource,
    providers: Vec<EffectiveCatalogProvider>,
}

impl EffectiveModelCatalog {
    /// 以 `base`（latest_valid_cache 或 embedded snapshot，二选一）为基线，
    /// 依次叠加官方兼容补丁与本地 `models.json` 覆盖。结果按 provider id、
    /// model id 稳定排序，保证可复现。
    pub fn build(
        base: &CatalogSnapshot,
        official_patches: &[CatalogSnapshot],
        local: Option<&LocalModelOverrides>,
    ) -> Result<Self, LlmError> {
        validate_snapshot(base)?;
        for patch in official_patches {
            validate_snapshot(patch)?;
        }

        let mut providers: Vec<EffectiveCatalogProvider> = Vec::new();
        for provider in &base.providers {
            let target = ensure_effective_provider(&mut providers, provider);
            for model in &provider.models {
                target.models.push(effective_from_catalog(model));
            }
        }

        // 官方兼容补丁以完整规范化模型记录覆盖基线；local 覆盖在其后按字段生效。
        for patch in official_patches {
            for provider in &patch.providers {
                let target = ensure_effective_provider(&mut providers, provider);
                for model in &provider.models {
                    upsert_patch_model(target, model);
                }
            }
        }

        if let Some(local) = local {
            for entry in &local.models {
                let provider_id = CatalogProviderId::parse(&entry.provider).map_err(|_| {
                    LlmError::config(format!(
                        "models.json 条目 `{}` 的 provider 非法：`{}`",
                        entry.id, entry.provider
                    ))
                })?;
                let target = ensure_effective_provider_id(
                    &mut providers,
                    &provider_id,
                    provider_id.as_str(),
                );
                upsert_local_entry(target, entry);
            }
        }

        // 稳定排序：provider 与 model 均按 id 字典序，保证合并结果确定可审查。
        providers.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        for provider in &mut providers {
            provider.models.sort_by(|a, b| a.id.cmp(&b.id));
        }

        Ok(Self {
            base_source: base.source.clone(),
            providers,
        })
    }

    /// 使用编译期内置快照作为基线构建目录（本阶段尚无远程缓存）。
    pub fn from_embedded(local: Option<&LocalModelOverrides>) -> Result<Self, LlmError> {
        Self::build(&parse_snapshot(EMBEDDED_SNAPSHOT_BYTES)?, &[], local)
    }

    /// 基线快照来源元数据（缓存接入后反映缓存来源）。
    pub fn base_source(&self) -> &CatalogSource {
        &self.base_source
    }

    pub fn providers(&self) -> &[EffectiveCatalogProvider] {
        &self.providers
    }

    /// 按 Catalog Provider ID 查询；未收录时返回 `None`。
    pub fn find_by_provider(
        &self,
        provider: &CatalogProviderId,
        model_id: &str,
    ) -> Option<&EffectiveCatalogModel> {
        let model_id = model_id.trim();
        if model_id.is_empty() {
            return None;
        }
        self.providers
            .iter()
            .find(|candidate| candidate.id == *provider)
            .and_then(|provider| provider.models.iter().find(|model| model.id == model_id))
    }

    /// 无 Provider 时只允许唯一匹配；同一 model id 出现在多个 Provider 中时返回
    /// `None`（歧义），绝不隐式选择字典序第一个 Provider。
    pub fn find(
        &self,
        provider: Option<&CatalogProviderId>,
        model_id: &str,
    ) -> Option<&EffectiveCatalogModel> {
        match provider {
            Some(provider) => self.find_by_provider(provider, model_id),
            None => self.find_unique(model_id),
        }
    }

    /// 全局唯一 model id 查询；歧义或不存在均返回 `None`。
    pub fn find_unique(&self, model_id: &str) -> Option<&EffectiveCatalogModel> {
        let model_id = model_id.trim();
        if model_id.is_empty() {
            return None;
        }
        let mut matches = self.providers.iter().flat_map(|provider| {
            provider
                .models
                .iter()
                .filter(move |model| model.id == model_id)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first)
    }

    /// 模型启用状态：`None` 表示目录未收录或查询歧义（未知模型），不等于禁用。
    pub fn is_model_enabled(
        &self,
        provider: Option<&CatalogProviderId>,
        model_id: &str,
    ) -> Option<bool> {
        self.find(provider, model_id).map(|model| model.enabled)
    }

    /// 迭代所有未被本地禁用的模型条目（含 deprecated/beta，状态由调用方区分）。
    pub fn active_models(&self) -> impl Iterator<Item = &EffectiveCatalogModel> {
        self.providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .filter(|model| model.enabled)
    }
}

fn ensure_effective_provider<'a>(
    providers: &'a mut Vec<EffectiveCatalogProvider>,
    source: &CatalogProvider,
) -> &'a mut EffectiveCatalogProvider {
    ensure_effective_provider_id(providers, &source.id, &source.name)
}

fn ensure_effective_provider_id<'a>(
    providers: &'a mut Vec<EffectiveCatalogProvider>,
    provider_id: &CatalogProviderId,
    display_name: &str,
) -> &'a mut EffectiveCatalogProvider {
    if let Some(index) = providers.iter().position(|p| p.id == *provider_id) {
        return &mut providers[index];
    }
    providers.push(EffectiveCatalogProvider {
        id: provider_id.clone(),
        name: display_name.to_owned(),
        models: Vec::new(),
    });
    let last = providers.len() - 1;
    &mut providers[last]
}

fn effective_from_catalog(model: &CatalogModel) -> EffectiveCatalogModel {
    EffectiveCatalogModel {
        id: model.id.clone(),
        display_name: model.display_name.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        modalities: model.modalities.clone(),
        capabilities: model.capabilities,
        status: model.status,
        enabled: true,
        price: model.price,
        provenance: ModelProvenance::from_catalog(),
    }
}

fn upsert_patch_model(target: &mut EffectiveCatalogProvider, model: &CatalogModel) {
    let patched = EffectiveCatalogModel {
        id: model.id.clone(),
        display_name: model.display_name.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        modalities: model.modalities.clone(),
        capabilities: model.capabilities,
        status: model.status,
        // 官方兼容补丁不控制本地禁用状态；保留已由 local 决定的结果。
        enabled: target
            .models
            .iter()
            .find(|candidate| candidate.id == model.id)
            .is_none_or(|candidate| candidate.enabled),
        price: model.price,
        provenance: ModelProvenance::from_patch(),
    };
    if let Some(existing) = target
        .models
        .iter_mut()
        .find(|candidate| candidate.id == model.id)
    {
        *existing = patched;
    } else {
        target.models.push(patched);
    }
}

/// 应用本地覆盖：存在的字段覆盖，缺失字段继承目录值；模型不存在则追加，
/// `enabled=false` 时保留条目并标记禁用。
fn upsert_local_entry(target: &mut EffectiveCatalogProvider, entry: &LocalModelEntry) {
    if let Some(existing) = target.models.iter_mut().find(|m| m.id == entry.id) {
        if let Some(value) = &entry.display_name {
            existing.display_name = value.clone();
            existing.provenance.display_name = FieldSource::LocalOverride;
        }
        if let Some(value) = entry.context_window {
            existing.context_window = Some(value);
            existing.provenance.context_window = FieldSource::LocalOverride;
        }
        if let Some(value) = entry.max_output_tokens {
            existing.max_output_tokens = Some(value);
            existing.provenance.max_output_tokens = FieldSource::LocalOverride;
        }
        if let Some(value) = &entry.modalities {
            existing.modalities = value.clone();
            existing.provenance.modalities = FieldSource::LocalOverride;
        }
        if let Some(value) = &entry.capabilities {
            if let Some(claim) = value.reasoning {
                existing.capabilities.reasoning = claim;
                existing.provenance.capabilities.reasoning = FieldSource::LocalOverride;
            }
            if let Some(claim) = value.tool_calling {
                existing.capabilities.tool_calling = claim;
                existing.provenance.capabilities.tool_calling = FieldSource::LocalOverride;
            }
            if let Some(claim) = value.vision {
                existing.capabilities.vision = claim;
                existing.provenance.capabilities.vision = FieldSource::LocalOverride;
            }
        }
        if let Some(value) = entry.status {
            existing.status = value;
            existing.provenance.status = FieldSource::LocalOverride;
        }
        if let Some(value) = entry.enabled {
            existing.enabled = value;
        }
        if let Some(value) = &entry.price {
            existing.price = Some(*value);
            existing.provenance.price = FieldSource::LocalOverride;
        }
        return;
    }

    let capabilities = entry.capabilities.clone().unwrap_or_default();
    target.models.push(EffectiveCatalogModel {
        id: entry.id.clone(),
        display_name: entry
            .display_name
            .clone()
            .unwrap_or_else(|| entry.id.clone()),
        context_window: entry.context_window,
        max_output_tokens: entry.max_output_tokens,
        modalities: entry.modalities.clone().unwrap_or_default(),
        capabilities: ModelCapabilities {
            reasoning: capabilities.reasoning.unwrap_or_default(),
            tool_calling: capabilities.tool_calling.unwrap_or_default(),
            vision: capabilities.vision.unwrap_or_default(),
        },
        status: entry.status.unwrap_or_default(),
        enabled: entry.enabled.unwrap_or(true),
        price: entry.price,
        provenance: ModelProvenance::from_local_override(),
    });
}

#[cfg(test)]
mod tests;
