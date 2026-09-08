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
//! - 能力字段区分 `advertised`（上游目录声明）、`adapter_supported`（本项目 adapter
//!   实测支持）、`verified`（是否经过验证）与 `effective`（最终可信结论）。
//!   远程目录声明 Tool Calling 等能力不会自动改变 Agent 行为，Tool Loop 白名单
//!   仍由 core 的场景配置决定。
//! - Catalog 不作为 Route 白名单：Route 中引用但 Catalog 未收录的模型按
//!   “未知模型 / 能力未知”处理，不阻断启动。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::types::ModelProvider;

/// 当前 Catalog 规范化 Schema 版本；不兼容变更时递增。
pub const CATALOG_SCHEMA_VERSION: u32 = 1;
/// Models.dev 快照转换器的固定版本标识，保证快照可复现。
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

/// 快照来源元数据；`source_hash` 标识上游原始数据批次，便于审查与缓存失效判断。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
    pub name: String,
    pub upstream_license: String,
    pub fetched_at: String,
    pub source_hash: String,
    pub converter_version: String,
}

/// Catalog 中的 Provider：仅是模型元数据的分组键，不代表任何连接配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// 单个模型的元数据条目。
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
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub price: Option<ModelPrice>,
    #[serde(default)]
    pub origin: CatalogOrigin,
}

/// 模型输入 / 输出模态；只保留项目实际关心的 text 与 image。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ModelModalities {
    #[serde(default)]
    pub input: Vec<Modality>,
    #[serde(default)]
    pub output: Vec<Modality>,
}

/// 能力声明集合；每项能力单独携带可信度信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub reasoning: CapabilityClaim,
    #[serde(default)]
    pub tool_calling: CapabilityClaim,
    #[serde(default)]
    pub vision: CapabilityClaim,
}

/// 单项能力的可信度模型。
///
/// Models.dev 等上游目录只能提供 `advertised`；`adapter_supported` 由本项目
/// adapter 实测填充；`verified` 表示是否完成验证。只有 `verified != unknown`
/// 时 `effective()` 才给出结论，避免伪造验证成功。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct CapabilityClaim {
    #[serde(default)]
    pub advertised: bool,
    #[serde(default)]
    pub adapter_supported: Option<bool>,
    #[serde(default)]
    pub verified: Verified,
}

impl CapabilityClaim {
    /// 最终可信结论：仅在验证完成后给出，未知时返回 `None`。
    pub fn effective(&self) -> Option<bool> {
        match self.verified {
            Verified::Yes => Some(self.advertised && self.adapter_supported.unwrap_or(true)),
            Verified::No => Some(false),
            Verified::Unknown => None,
        }
    }
}

/// 能力验证状态；快照中的远程声明一律保持 `unknown`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Verified {
    #[default]
    Unknown,
    Yes,
    No,
}

/// 模型生命周期状态；与 `enabled`（本地禁用开关）正交。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    #[default]
    Active,
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

/// 条目数据来源，用于追溯每条模型元数据是目录自带、官方补丁还是本地覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CatalogOrigin {
    #[default]
    Embedded,
    OfficialPatch,
    Local,
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

fn default_true() -> bool {
    true
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

fn validate_snapshot(snapshot: &CatalogSnapshot) -> Result<(), LlmError> {
    if snapshot.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(LlmError::config(format!(
            "内置模型目录快照 schema_version 不受支持：{}（当前支持 {}）",
            snapshot.schema_version, CATALOG_SCHEMA_VERSION
        )));
    }
    if snapshot.source.converter_version.is_empty() || snapshot.source.source_hash.is_empty() {
        return Err(LlmError::config(
            "内置模型目录快照缺少 source_hash 或 converter_version 元数据",
        ));
    }
    let mut provider_ids = Vec::new();
    for provider in &snapshot.providers {
        if provider.id.trim().is_empty() {
            return Err(LlmError::config("内置模型目录存在空 provider id"));
        }
        provider_ids.push(provider.id.clone());
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
        ModelProvider::parse_prefix(&entry.provider).map_err(|error| {
            LlmError::config(format!(
                "models.json 条目 `{}` 的 provider 非法：{}",
                entry.id, error.message
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
    providers: Vec<CatalogProvider>,
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

        let mut providers = base.providers.clone();
        for patch in official_patches {
            for patch_provider in &patch.providers {
                let target = ensure_provider(&mut providers, patch_provider);
                for model in &patch_provider.models {
                    upsert_model(target, model.clone(), CatalogOrigin::OfficialPatch);
                }
            }
        }
        if let Some(local) = local {
            for entry in &local.models {
                let provider_id = ModelProvider::parse_prefix(&entry.provider)
                    .map_err(|error| {
                        LlmError::config(format!(
                            "models.json 条目 `{}` 的 provider 非法：{}",
                            entry.id, error.message
                        ))
                    })?
                    .as_str()
                    .to_owned();
                let target = ensure_provider_named(&mut providers, &provider_id);
                upsert_local_entry(target, entry);
            }
        }

        // 稳定排序：provider 与 model 均按 id 字典序，保证合并结果确定可审查。
        providers.sort_by(|a, b| a.id.cmp(&b.id));
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

    pub fn providers(&self) -> &[CatalogProvider] {
        &self.providers
    }

    /// 查询模型元数据；未收录时返回 `None`（未知模型 / 能力未知），
    /// 调用方不得将其视为启动或请求失败。
    pub fn find(&self, provider: Option<&str>, model_id: &str) -> Option<&CatalogModel> {
        let model_id = model_id.trim();
        if model_id.is_empty() {
            return None;
        }
        match provider.map(str::trim).filter(|value| !value.is_empty()) {
            Some(provider_id) => self
                .providers
                .iter()
                .find(|candidate| candidate.id == provider_id.to_ascii_lowercase())
                .and_then(|provider| provider.models.iter().find(|model| model.id == model_id)),
            None => self
                .providers
                .iter()
                .find_map(|provider| provider.models.iter().find(|model| model.id == model_id)),
        }
    }

    /// 模型启用状态：`None` 表示目录未收录（未知模型），不等于禁用。
    pub fn is_model_enabled(&self, provider: Option<&str>, model_id: &str) -> Option<bool> {
        self.find(provider, model_id).map(|model| model.enabled)
    }

    /// 迭代所有未被本地禁用的模型条目（含 deprecated，状态由调用方区分）。
    pub fn active_models(&self) -> impl Iterator<Item = &CatalogModel> {
        self.providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .filter(|model| model.enabled)
    }
}

fn ensure_provider<'a>(
    providers: &'a mut Vec<CatalogProvider>,
    patch_provider: &CatalogProvider,
) -> &'a mut CatalogProvider {
    ensure_provider_named(providers, &patch_provider.id)
}

fn ensure_provider_named<'a>(
    providers: &'a mut Vec<CatalogProvider>,
    provider_id: &str,
) -> &'a mut CatalogProvider {
    if let Some(index) = providers.iter().position(|p| p.id == provider_id) {
        return &mut providers[index];
    }
    providers.push(CatalogProvider {
        id: provider_id.to_owned(),
        name: provider_id.to_owned(),
        models: Vec::new(),
    });
    let last = providers.len() - 1;
    &mut providers[last]
}

fn upsert_model(target: &mut CatalogProvider, mut model: CatalogModel, origin: CatalogOrigin) {
    model.origin = origin;
    if let Some(existing) = target.models.iter_mut().find(|m| m.id == model.id) {
        *existing = model;
    } else {
        target.models.push(model);
    }
}

/// 应用本地覆盖：存在的字段覆盖，缺失字段继承目录值；模型不存在则追加，
/// `enabled=false` 时保留条目并标记禁用。
fn upsert_local_entry(target: &mut CatalogProvider, entry: &LocalModelEntry) {
    if let Some(existing) = target.models.iter_mut().find(|m| m.id == entry.id) {
        if let Some(value) = &entry.display_name {
            existing.display_name = value.clone();
        }
        if let Some(value) = entry.context_window {
            existing.context_window = Some(value);
        }
        if let Some(value) = entry.max_output_tokens {
            existing.max_output_tokens = Some(value);
        }
        if let Some(value) = &entry.modalities {
            existing.modalities = value.clone();
        }
        if let Some(value) = &entry.capabilities {
            if let Some(claim) = value.reasoning {
                existing.capabilities.reasoning = claim;
            }
            if let Some(claim) = value.tool_calling {
                existing.capabilities.tool_calling = claim;
            }
            if let Some(claim) = value.vision {
                existing.capabilities.vision = claim;
            }
        }
        if let Some(value) = entry.status {
            existing.status = value;
        }
        if let Some(value) = entry.enabled {
            existing.enabled = value;
        }
        if let Some(value) = &entry.price {
            existing.price = Some(*value);
        }
        existing.origin = CatalogOrigin::Local;
        return;
    }

    let capabilities = entry.capabilities.clone().unwrap_or_default();
    target.models.push(CatalogModel {
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
        origin: CatalogOrigin::Local,
    });
}

#[cfg(test)]
mod tests;
