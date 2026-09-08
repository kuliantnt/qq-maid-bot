//! Models.dev → 规范化 Catalog 快照的固定开发期转换器。
//!
//! 转换器只读取固定上游 Provider（`openai` / `deepseek` / `google` / `zhipuai`），
//! 只提取项目需要的白名单字段，输出按 Provider / Model id 稳定排序，并基于转换器
//! 实际输入字节计算 SHA-256。生成、校验和嵌入都在仓库内完成，Release 构建不联网。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    CATALOG_SCHEMA_VERSION, CONVERTER_VERSION, CapabilityClaim, CatalogModel, CatalogProvider,
    CatalogProviderId, CatalogSnapshot, CatalogSource, Modality, ModelCapabilities,
    ModelModalities, ModelPrice, ModelStatus, validate_snapshot,
};
use crate::error::LlmError;

/// 内置快照当前支持的 Models.dev Provider 白名单。
/// 这里使用 Models.dev 原始 Provider ID（如 `google`、`zhipuai`），
/// 不做 `google -> gemini`、`zhipuai -> bigmodel` 的运行时 Connection 改写。
pub const ALLOWED_UPSTREAM_PROVIDERS: &[&str] = &["openai", "deepseek", "google", "zhipuai"];

/// 每次真实抓取生成一次的开发期来源清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsDevSourceManifest {
    pub source_url: String,
    pub source_version: String,
    pub fetched_at: String,
    pub upstream_license: String,
}

/// 把固定输入与来源清单转换为规范化快照字节。
///
/// `input` 必须是从 Models.dev 原始 API 响应保留下来的真实 Provider 对象，
/// 不允许手工改写；`source_hash` 由本函数根据输入字节计算。
pub fn convert_snapshot(
    input: &[u8],
    manifest: &ModelsDevSourceManifest,
) -> Result<Vec<u8>, LlmError> {
    if input.is_empty() {
        return Err(LlmError::config("Models.dev 转换输入为空"));
    }
    if manifest.source_url.trim().is_empty()
        || manifest.source_version.trim().is_empty()
        || manifest.fetched_at.trim().is_empty()
        || manifest.upstream_license.trim().is_empty()
    {
        return Err(LlmError::config(
            "Models.dev 来源清单缺少 source_url / source_version / fetched_at / license",
        ));
    }

    let root: Value = serde_json::from_slice(input)
        .map_err(|error| LlmError::config(format!("Models.dev 输入 JSON 解析失败：{error}")))?;
    let root_object = root
        .as_object()
        .ok_or_else(|| LlmError::config("Models.dev 输入必须是 Provider 对象"))?;

    let mut providers = Vec::new();
    for upstream_provider_id in ALLOWED_UPSTREAM_PROVIDERS {
        let provider_value = root_object.get(*upstream_provider_id).ok_or_else(|| {
            LlmError::config(format!(
                "Models.dev 输入缺少白名单 Provider `{upstream_provider_id}`"
            ))
        })?;
        providers.push(convert_provider(upstream_provider_id, provider_value)?);
    }
    // 输出稳定排序，避免上游对象顺序进入规范化资产。
    providers.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    for provider in &mut providers {
        provider.models.sort_by(|a, b| a.id.cmp(&b.id));
    }

    let snapshot = CatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        source: CatalogSource {
            name: "models.dev".to_owned(),
            upstream_license: manifest.upstream_license.clone(),
            source_url: manifest.source_url.clone(),
            source_version: manifest.source_version.clone(),
            fetched_at: manifest.fetched_at.clone(),
            source_hash: sha256_hex(input),
            converter_version: CONVERTER_VERSION.to_owned(),
        },
        providers,
    };
    validate_snapshot(&snapshot)?;

    let mut bytes = serde_json::to_vec_pretty(&snapshot)
        .map_err(|error| LlmError::config(format!("Catalog 快照序列化失败：{error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn convert_provider(
    upstream_provider_id: &str,
    provider_value: &Value,
) -> Result<CatalogProvider, LlmError> {
    let provider = provider_value.as_object().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{upstream_provider_id}` 不是对象"
        ))
    })?;
    let nested_id = required_string(provider, upstream_provider_id, "id")?;
    if nested_id != upstream_provider_id {
        return Err(LlmError::config(format!(
            "Models.dev Provider 外层键 `{upstream_provider_id}` 与内层 id `{nested_id}` 不一致"
        )));
    }
    let display_name = required_string(provider, upstream_provider_id, "name")?;
    let models_value = provider.get("models").ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{upstream_provider_id}` 缺少 models"
        ))
    })?;
    let models_object = models_value.as_object().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{upstream_provider_id}` 的 models 不是对象"
        ))
    })?;

    let mut models = Vec::with_capacity(models_object.len());
    for model_value in models_object.values() {
        models.push(convert_model(upstream_provider_id, model_value)?);
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(CatalogProvider {
        id: CatalogProviderId::parse(upstream_provider_id)?,
        name: display_name.to_owned(),
        models,
    })
}

fn convert_model(provider_id: &str, model_value: &Value) -> Result<CatalogModel, LlmError> {
    let model = model_value.as_object().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 的模型不是对象"
        ))
    })?;
    let id = required_string(model, provider_id, "id")?;
    let display_name = required_string(model, provider_id, "name")?;

    let modalities = model
        .get("modalities")
        .map(|value| convert_modalities(provider_id, id, value))
        .transpose()?
        .unwrap_or_default();

    let context_window = limit_value(model, provider_id, id, "context")?;
    let max_output_tokens = limit_value(model, provider_id, id, "output")?;
    let reasoning = bool_value(model, "reasoning").unwrap_or(false);
    let tool_calling = bool_value(model, "tool_call").unwrap_or(false);
    let vision = modalities.input.contains(&Modality::Image);
    let price = convert_price(model, provider_id, id)?;

    Ok(CatalogModel {
        id: id.to_owned(),
        display_name: display_name.to_owned(),
        context_window,
        max_output_tokens,
        modalities,
        capabilities: ModelCapabilities {
            reasoning: CapabilityClaim {
                advertised: reasoning,
            },
            tool_calling: CapabilityClaim {
                advertised: tool_calling,
            },
            vision: CapabilityClaim { advertised: vision },
        },
        status: convert_status(model, provider_id, id)?,
        price,
    })
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    provider_id: &str,
    key: &str,
) -> Result<&'a str, LlmError> {
    object.get(key).and_then(Value::as_str).ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型字段缺少字符串 `{key}`"
        ))
    })
}

fn bool_value(object: &serde_json::Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

fn limit_value(
    model: &serde_json::Map<String, Value>,
    provider_id: &str,
    model_id: &str,
    key: &str,
) -> Result<Option<u64>, LlmError> {
    let Some(limit) = model.get("limit") else {
        return Ok(None);
    };
    let limit = limit.as_object().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 limit 不是对象"
        ))
    })?;
    let Some(value) = limit.get(key) else {
        return Ok(None);
    };
    let value = value.as_u64().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 limit.{key} 不是整数"
        ))
    })?;
    // Models.dev 用 0 表示无限制/不适用；转换为 None 比输出伪 0 上下文更诚实。
    Ok((value > 0).then_some(value))
}

fn convert_modalities(
    provider_id: &str,
    model_id: &str,
    value: &Value,
) -> Result<ModelModalities, LlmError> {
    let modalities = value.as_object().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 modalities 不是对象"
        ))
    })?;
    let input = convert_modality_list(provider_id, model_id, "input", modalities.get("input"))?;
    let output = convert_modality_list(provider_id, model_id, "output", modalities.get("output"))?;
    Ok(ModelModalities { input, output })
}

fn convert_modality_list(
    provider_id: &str,
    model_id: &str,
    list_name: &str,
    value: Option<&Value>,
) -> Result<Vec<Modality>, LlmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 modalities.{list_name} 不是数组"
        ))
    })?;
    let mut modalities = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(LlmError::config(format!(
                "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 modalities.{list_name} 含非字符串"
            )));
        };
        let modality = match name {
            "text" => Modality::Text,
            "image" => Modality::Image,
            "audio" => Modality::Audio,
            "video" => Modality::Video,
            "pdf" => Modality::Pdf,
            other => {
                return Err(LlmError::config(format!(
                    "Models.dev Provider `{provider_id}` 模型 `{model_id}` 含未知模态 `{other}`，\
                     需要先更新白名单"
                )));
            }
        };
        if !modalities.contains(&modality) {
            modalities.push(modality);
        }
    }
    // 模态列表也保持稳定顺序，避免上游数组顺序造成快照 diff。
    modalities.sort_by_key(|modality| modality.as_str());
    Ok(modalities)
}

impl Modality {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Pdf => "pdf",
        }
    }
}

fn convert_status(
    model: &serde_json::Map<String, Value>,
    provider_id: &str,
    model_id: &str,
) -> Result<ModelStatus, LlmError> {
    let Some(value) = model.get("status") else {
        return Ok(ModelStatus::Active);
    };
    let Some(status) = value.as_str() else {
        return Err(LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 status 不是字符串"
        )));
    };
    match status {
        "deprecated" => Ok(ModelStatus::Deprecated),
        "beta" => Ok(ModelStatus::Beta),
        // Models.dev 多数模型不写 status；显式未知状态应让转换失败而不是悄悄改成 Active。
        other => Err(LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 含未支持状态 `{other}`，\
             需要先扩展 ModelStatus"
        ))),
    }
}

fn convert_price(
    model: &serde_json::Map<String, Value>,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<ModelPrice>, LlmError> {
    let Some(value) = model.get("cost") else {
        return Ok(None);
    };
    let Some(cost) = value.as_object() else {
        return Err(LlmError::config(format!(
            "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 cost 不是对象"
        )));
    };
    let input = cost
        .get("input")
        .map(price_number)
        .transpose()
        .map_err(|error| {
            LlmError::config(format!(
                "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 cost.input：{error}"
            ))
        })?;
    let output = cost
        .get("output")
        .map(price_number)
        .transpose()
        .map_err(|error| {
            LlmError::config(format!(
                "Models.dev Provider `{provider_id}` 模型 `{model_id}` 的 cost.output：{error}"
            ))
        })?;
    if input.is_none() && output.is_none() {
        return Ok(None);
    }
    Ok(Some(ModelPrice {
        input_per_million_usd: input,
        output_per_million_usd: output,
    }))
}

fn price_number(value: &Value) -> Result<f64, String> {
    value.as_f64().ok_or_else(|| "不是数字".to_owned())
}

fn sha256_hex(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_ignores_non_whitelisted_provider_fields() {
        let input = br#"{
          "openai": {
            "id": "openai",
            "name": "OpenAI",
            "env": ["OPENAI_API_KEY"],
            "api": "https://api.openai.com/v1",
            "models": {
              "gpt-test": {
                "id": "gpt-test",
                "name": "GPT Test",
                "attachment": true,
                "reasoning": true,
                "tool_call": true,
                "temperature": false,
                "modalities": {"input": ["text", "image"], "output": ["text"]},
                "limit": {"context": 128000, "output": 16384},
                "cost": {"input": 1.25, "output": 10}
              }
            }
          },
          "deepseek": {
            "id": "deepseek",
            "name": "DeepSeek",
            "models": {}
          },
          "google": {
            "id": "google",
            "name": "Google",
            "models": {}
          },
          "zhipuai": {
            "id": "zhipuai",
            "name": "Zhipu AI",
            "models": {}
          }
        }"#;
        let manifest = ModelsDevSourceManifest {
            source_url: "https://models.dev/api.json".to_owned(),
            source_version: "fixture".to_owned(),
            fetched_at: "2026-01-01T00:00:00Z".to_owned(),
            upstream_license: "MIT".to_owned(),
        };
        let bytes = convert_snapshot(input, &manifest).unwrap();
        let snapshot: CatalogSnapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(snapshot.source.name, "models.dev");
        assert_eq!(snapshot.source.source_hash, sha256_hex(input));
        assert_eq!(snapshot.providers.len(), 4);
        let provider = snapshot
            .providers
            .iter()
            .find(|provider| provider.id.as_str() == "openai")
            .expect("openai provider 应存在");
        let model = &provider.models[0];
        assert_eq!(model.context_window, Some(128000));
        assert_eq!(model.max_output_tokens, Some(16384));
        assert!(model.capabilities.tool_calling.advertised);
        assert!(model.capabilities.vision.advertised);
        assert_eq!(
            model.modalities.input,
            vec![Modality::Image, Modality::Text]
        );
        assert_eq!(
            model.price.map(|p| p.input_per_million_usd),
            Some(Some(1.25))
        );
    }
}
