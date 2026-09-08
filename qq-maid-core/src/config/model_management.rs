//! Connection 身份与目录身份的显式边界；未知连接绝不按 URL 或模型名猜品牌。
use super::AgentRuntimeConfig;
use qq_maid_llm::{
    LlmError,
    model_catalog::{CatalogProviderId, EffectiveModelCatalog},
    provider::types::{ModelId, ModelProvider},
};

pub(super) fn catalog_identity(connection: &str) -> (&str, bool) {
    match connection {
        "openai" => ("openai", true),
        "deepseek" => ("deepseek", true),
        "gemini" => ("google", true),
        "bigmodel" => ("zhipuai", true),
        other => (other, false),
    }
}

/// 只拒绝本地明确禁用；目录缺失、生命周期或能力声明不参与路由合法性。
pub(super) fn validate_disabled_models(
    agent: &AgentRuntimeConfig,
    catalog: &EffectiveModelCatalog,
) -> Result<(), LlmError> {
    let mut invalid = Vec::new();
    let mut check = |model: &ModelId, location: String, search: bool| -> Result<(), LlmError> {
        let default = if search {
            ModelProvider::OpenAi
        } else {
            qq_maid_llm::config::ProviderMode::Auto.default_provider()
        };
        let connection = model.provider.as_ref().unwrap_or(&default).as_str();
        let (identity, _) = catalog_identity(connection);
        let provider = CatalogProviderId::parse(identity)?;
        if catalog.is_model_enabled(Some(&provider), &model.name) == Some(false) {
            invalid.push(format!(
                "{location}: model `{connection}:{}` is disabled",
                model.name
            ));
        }
        Ok(())
    };
    for (name, route) in agent.configured_model_routes() {
        for (index, model) in route.candidates().iter().enumerate() {
            check(model, format!("{name}.candidates[{index}]"), false)?;
        }
    }
    if let Some(document) = agent.document() {
        for (name, route) in &document.tools.web_search.routes {
            let location = format!("tools.web_search.routes.{name}");
            check(
                &ModelId::parse_config(&route.model, &location)?,
                location,
                true,
            )?;
        }
    }
    invalid.sort();
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(LlmError::config(invalid.join("; ")))
    }
}
