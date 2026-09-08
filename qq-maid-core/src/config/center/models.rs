//! 模型元数据沿用 models.json 与配置中心原子写入，不参与请求能力决策。
use super::{ConfigCenter, ConfigCenterError, managed_file};
use crate::config::model_management::{catalog_identity, validate_disabled_models};
use qq_maid_llm::model_catalog::{
    CatalogSnapshot, EffectiveModelCatalog, LocalModelOverrides, parse_local_overrides,
};
use serde_json::{Value, json};
use std::path::PathBuf;

impl ConfigCenter {
    fn models_path(&self) -> PathBuf {
        self.external_environment
            .get("MODELS_CONFIG_FILE")
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("config/models.json"))
    }

    pub(super) fn load_models(&self) -> Result<(String, LocalModelOverrides), ConfigCenterError> {
        let bytes = managed_file::read_regular_file(&self.models_path())?;
        let revision = bytes
            .as_deref()
            .map(managed_file::revision)
            .unwrap_or_else(|| "missing".into());
        let local = parse_local_overrides(
            bytes
                .as_deref()
                .unwrap_or(b"{\"schema_version\":1,\"models\":[]}"),
        )
        .map_err(|e| ConfigCenterError::invalid(e.message))?;
        Ok((revision, local))
    }

    pub fn connection_model_metadata(&self, id: &str) -> Result<Value, ConfigCenterError> {
        let canonical = id.to_ascii_lowercase();
        let (identity, builtin) = catalog_identity(&canonical);
        if !builtin
            && !self
                .agent_file
                .as_ref()
                .map(|f| f.current_runtime())
                .transpose()?
                .is_some_and(|agent| {
                    agent
                        .provider_configs()
                        .iter()
                        .any(|p| p.id.as_str() == canonical)
                })
        {
            return Err(ConfigCenterError::invalid("Connection 不存在"));
        }
        let (revision, local) = self.load_models()?;
        let embedded = EffectiveModelCatalog::from_embedded(None)
            .map_err(|e| ConfigCenterError::invalid(e.message))?;
        // 自定义连接只展示本地明确登记的条目，即使名称碰巧等于 Catalog Provider。
        let catalog = if builtin {
            EffectiveModelCatalog::from_embedded(Some(&local))
        } else {
            EffectiveModelCatalog::build(
                &CatalogSnapshot {
                    schema_version: 1,
                    source: embedded.base_source().clone(),
                    providers: vec![],
                },
                &[],
                Some(&local),
            )
        }
        .map_err(|e| ConfigCenterError::invalid(e.message))?;
        let models = catalog
            .providers()
            .iter()
            .find(|p| p.id.as_str() == identity)
            .map(|p| p.models.clone())
            .unwrap_or_default();
        let overrides: Vec<_> = local
            .models
            .iter()
            .filter(|m| m.provider.as_str() == identity)
            .collect();
        Ok(
            json!({"revision": revision, "provider": identity, "models": models, "overrides": overrides,
            "catalog_source": if builtin { Some(embedded.base_source()) } else { None },
            "verified_capabilities": "unknown", "apply_mode": "restart"}),
        )
    }

    pub fn update_model_override(
        &self,
        id: &str,
        expected_revision: &str,
        entry: Value,
    ) -> Result<(), ConfigCenterError> {
        let _guard = self
            .mutation_lock
            .lock()
            .map_err(|_| ConfigCenterError::io("配置写锁不可用"))?;
        managed_file::ensure_expected_revision(
            &self.models_path(),
            expected_revision,
            "models.json",
        )?;
        let metadata = self.connection_model_metadata(id)?;
        let parsed = parse_local_overrides(
            json!({"schema_version": 1, "models": [entry]})
                .to_string()
                .as_bytes(),
        )
        .map_err(|e| ConfigCenterError::invalid(e.message))?;
        let entry = parsed
            .models
            .into_iter()
            .next()
            .ok_or_else(|| ConfigCenterError::invalid("缺少模型条目"))?;
        if entry.provider.as_str() != metadata["provider"].as_str().unwrap_or_default() {
            return Err(ConfigCenterError::invalid(
                "模型 Provider 与 Connection 的明确身份不一致",
            ));
        }
        // 必须可以无歧义地写入 provider:model-id 候选链。
        if entry.id.trim() != entry.id || entry.id.chars().any(|c| c.is_control() || c == ',') {
            return Err(ConfigCenterError::invalid(
                "模型 ID 含有非法路线分隔符或空白",
            ));
        }
        validate_metadata(&entry)?;
        let (_, mut local) = self.load_models()?;
        local
            .models
            .retain(|m| !(m.provider == entry.provider && m.id == entry.id));
        local.models.push(entry);
        let catalog = EffectiveModelCatalog::from_embedded(Some(&local))
            .map_err(|e| ConfigCenterError::invalid(e.message))?;
        if let Some(agent) = self
            .agent_file
            .as_ref()
            .map(|f| f.current_runtime())
            .transpose()?
        {
            validate_disabled_models(&agent, &catalog)
                .map_err(|e| ConfigCenterError::invalid(e.message))?;
        }
        let bytes = serde_json::to_vec_pretty(&local)
            .map_err(|e| ConfigCenterError::invalid(e.to_string()))?;
        if bytes.len() > 1024 * 1024 {
            return Err(ConfigCenterError::invalid("models.json 超出 1 MiB"));
        }
        managed_file::atomic_write_if_revision(
            &self.models_path(),
            &bytes,
            expected_revision,
            "models.json",
        )
    }
}

/// Web 输入不能保存无意义的 token 数或负价格；不会把声明转成请求参数。
fn validate_metadata(
    entry: &qq_maid_llm::model_catalog::LocalModelEntry,
) -> Result<(), ConfigCenterError> {
    if entry.id.len() > 512
        || entry.display_name.as_ref().is_some_and(|name| {
            name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control)
        })
    {
        return Err(ConfigCenterError::invalid(
            "模型 ID / 显示名称过长或含非法字符",
        ));
    }
    if entry.context_window == Some(0) || entry.max_output_tokens == Some(0) {
        return Err(ConfigCenterError::invalid(
            "上下文窗口和最大输出必须为正整数",
        ));
    }
    if entry.price.as_ref().is_some_and(|price| {
        [price.input_per_million_usd, price.output_per_million_usd]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite() || value < 0.0)
    }) {
        return Err(ConfigCenterError::invalid("模型价格必须为非负有限数字"));
    }
    Ok(())
}
