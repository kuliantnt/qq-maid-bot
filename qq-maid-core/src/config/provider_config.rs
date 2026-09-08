//! Agent Provider 配置到 LLM crate 配置的窄转换层。
//!
//! Provider 元数据保留在 `agent.toml`，凭证只按声明的环境变量名从受管环境解析，
//! 避免把 API Key 写回 Agent 文档。

use qq_maid_llm::config::{
    HttpAuthConfig, OpenAiCompatibleProviderConfig, OpenAiResponsesProviderConfig, ProviderMode,
};

use super::{
    agent::{AgentProviderKind, AgentRuntimeConfig},
    env_optional,
};

pub(super) fn builtin_api_key(prefix: &str) -> Result<Option<String>, crate::error::LlmError> {
    Ok(if super::env_bool(&format!("{prefix}_ENABLED"), true)? {
        env_optional(&format!("{prefix}_API_KEY"))
    } else {
        None
    })
}

pub(super) fn validate_builtin_connection_environment(
    agent: &AgentRuntimeConfig,
    environment: &std::collections::HashMap<String, String>,
) -> Result<(), crate::error::LlmError> {
    let _guard = super::ValidationEnvironmentGuard::install(environment.clone());
    // AppConfig 固定使用 Auto；LLM_PROVIDER 已移除，不能在诊断中恢复另一套入口。
    validate_builtin_connections(agent, &ProviderMode::Auto)
}

/// 内置连接保留原存储来源；停用仍检查全部模型/搜索引用，不能变成候选链静默跳过。
pub(super) fn validate_builtin_connections(
    agent: &AgentRuntimeConfig,
    mode: &ProviderMode,
) -> Result<(), crate::error::LlmError> {
    use qq_maid_llm::provider::types::{ModelId, ModelProvider};
    let mut disabled = Vec::new();
    for name in ["openai", "deepseek", "bigmodel", "gemini"] {
        if !super::env_bool(&format!("{}_ENABLED", name.to_ascii_uppercase()), true)? {
            disabled.push(name);
        }
    }
    let default_provider = mode.default_provider();
    let mut invalid = Vec::new();
    let mut check = |model: &ModelId, location: String, search: bool| {
        let provider = model.provider.as_ref().unwrap_or(if search {
            &ModelProvider::OpenAi
        } else {
            &default_provider
        });
        if disabled.contains(&provider.as_str()) {
            invalid.push(format!(
                "{location}: provider `{}` is disabled",
                provider.as_str()
            ));
        }
    };
    for (name, route) in agent.configured_model_routes() {
        for (index, model) in route.candidates().iter().enumerate() {
            check(model, format!("{name}.candidates[{index}]"), false);
        }
    }
    if let Some(document) = agent.document() {
        for (name, route) in &document.tools.web_search.routes {
            let location = format!("tools.web_search.routes.{name}");
            check(
                &ModelId::parse_config(&route.model, &location)?,
                location,
                true,
            );
        }
    }
    invalid.sort();
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(crate::error::LlmError::config(invalid.join("; ")))
    }
}

pub(super) fn llm_provider_configs(
    agent_config: &AgentRuntimeConfig,
) -> (
    Vec<OpenAiCompatibleProviderConfig>,
    Vec<OpenAiResponsesProviderConfig>,
) {
    let mut compatible = Vec::new();
    let mut responses = Vec::new();
    for provider in agent_config.provider_configs() {
        // Agent 解析已拒绝所有停用项的路线引用，此处只装配启用连接。
        if !provider.enabled {
            continue;
        }
        let auth = HttpAuthConfig {
            header: provider.auth_header,
            scheme: provider.auth_scheme,
        };
        let api_key = env_optional(&provider.api_key_env);
        match provider.kind {
            AgentProviderKind::OpenAiCompatible => {
                compatible.push(OpenAiCompatibleProviderConfig {
                    id: provider.id,
                    base_url: provider.base_url,
                    api_key_env: provider.api_key_env,
                    api_key,
                    auth,
                    request_timeout_seconds: provider.request_timeout_seconds,
                });
            }
            AgentProviderKind::OpenAiResponses => {
                responses.push(OpenAiResponsesProviderConfig {
                    id: provider.id,
                    base_url: provider.base_url,
                    api_key_env: provider.api_key_env,
                    api_key,
                    auth,
                    request_timeout_seconds: provider.request_timeout_seconds,
                    chat_fallback: provider.chat_fallback,
                });
            }
        }
    }
    (compatible, responses)
}
