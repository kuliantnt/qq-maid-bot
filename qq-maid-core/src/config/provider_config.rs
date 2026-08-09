//! Agent Provider 配置到 LLM crate 配置的窄转换层。
//!
//! Provider 元数据保留在 `agent.toml`，凭证只按声明的环境变量名从受管环境解析，
//! 避免把 API Key 写回 Agent 文档。

use qq_maid_llm::config::{
    HttpAuthConfig, OpenAiCompatibleProviderConfig, OpenAiResponsesProviderConfig,
};

use super::{
    agent::{AgentProviderKind, AgentRuntimeConfig},
    env_optional,
};

pub(super) fn llm_provider_configs(
    agent_config: &AgentRuntimeConfig,
) -> (
    Vec<OpenAiCompatibleProviderConfig>,
    Vec<OpenAiResponsesProviderConfig>,
) {
    let mut compatible = Vec::new();
    let mut responses = Vec::new();
    for provider in agent_config.provider_configs() {
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
