//! route 配置解析与 provider 可用性预检。
//!
//! 在 `build_provider` 阶段统一校验：
//! * 单 provider 模式下所有 specialty route 都必须落在该 provider 上；
//! * auto 模式下根据 route 实际引用的 provider 计算需要初始化的 provider 集合，
//!   缺少 API key 的 provider 会在启动时告警并跳过；
//! * auto 模式保留旧的「单 OpenAI 主模型自动追加 DeepSeek fallback」兼容行为。

use crate::{
    config::LlmConfig,
    error::LlmError,
    provider::{deepseek, types::ModelId},
};

use super::types::{ModelProvider, ModelRoute};

/// auto 模式的默认候选链。
///
/// 兼容旧的 `LLM_PROVIDER=auto` 行为：单个 OpenAI/裸主模型在可恢复失败时，
/// 仍可降级到 `DEEPSEEK_MODEL`。用户显式写多个候选时则严格按配置顺序执行。
pub(crate) fn auto_default_route(config: &LlmConfig) -> Result<ModelRoute, LlmError> {
    let mut candidates = config.model_route.candidates().to_vec();
    if candidates.len() == 1
        && config
            .deepseek_api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && candidates[0].provider != Some(ModelProvider::DeepSeek)
    {
        let deepseek_model = deepseek::deepseek_config_model(&config.deepseek_model)?;
        candidates.push(ModelId {
            provider: Some(ModelProvider::DeepSeek),
            name: deepseek_model,
        });
    }
    ModelRoute::from_candidates(candidates)
}

/// 返回所有需要初始化 provider 实例的 named route 列表。
///
/// provider 初始化必须使用 auto 模式的实际默认链（来自 [`auto_default_route`]），
/// 才能保留单 OpenAI 主模型自动追加 DeepSeek fallback 的兼容行为，
/// 因此这里会把 `LLM_MODEL` 项替换为 `default_route`。
pub(crate) fn auto_provider_routes(
    config: &LlmConfig,
    default_route: &ModelRoute,
) -> Result<Vec<(String, ModelRoute)>, LlmError> {
    let mut routes = config.configured_model_routes.clone();
    if let Some((_, route)) = routes.iter_mut().find(|(name, _)| *name == "LLM_MODEL") {
        // provider 初始化必须使用 auto 模式的实际默认链，才能保留单 OpenAI
        // 主模型自动追加 DeepSeek fallback 的兼容行为。
        *route = default_route.clone();
    }
    Ok(routes)
}

/// 收集所有 named route 实际引用到的 provider，按固定顺序去重。
///
/// 顺序固定为 OpenAI -> DeepSeek -> BigModel，保证 `build_provider` 构造的
/// provider 列表与原实现一致。
pub(crate) fn provider_kinds_for_routes(
    routes: &[(String, ModelRoute)],
    default_provider: ModelProvider,
) -> Vec<ModelProvider> {
    [
        ModelProvider::OpenAi,
        ModelProvider::DeepSeek,
        ModelProvider::BigModel,
    ]
    .into_iter()
    .filter(|provider| {
        routes
            .iter()
            .any(|(_, route)| route_uses_provider(route, *provider, default_provider))
    })
    .collect()
}

/// 收集 auto 模式下具备 API key、可以初始化的 provider。
///
/// 候选链允许写多个 provider 做 fallback；缺少某个 provider 的 API key 时，
/// 启动阶段只告警并跳过该 provider，运行时候选链会继续尝试后续可用候选。
pub(crate) fn available_provider_kinds_for_routes(
    config: &LlmConfig,
    routes: &[(String, ModelRoute)],
    default_provider: ModelProvider,
) -> Vec<ModelProvider> {
    provider_kinds_for_routes(routes, default_provider)
        .into_iter()
        .filter(|provider| {
            if provider_api_key_configured(config, *provider) {
                return true;
            }
            let route_names = route_names_using_provider(routes, *provider, default_provider);
            tracing::warn!(
                provider = provider.as_str(),
                routes = route_names.join(", "),
                "configured model routes reference provider without API key; skipping provider in auto mode"
            );
            false
        })
        .collect()
}

fn provider_api_key_configured(config: &LlmConfig, provider: ModelProvider) -> bool {
    match provider {
        ModelProvider::OpenAi => config.openai_api_key.as_deref(),
        ModelProvider::DeepSeek => config.deepseek_api_key.as_deref(),
        ModelProvider::BigModel => config.bigmodel_api_key.as_deref(),
    }
    .is_some_and(|value| !value.trim().is_empty())
}

fn route_names_using_provider(
    routes: &[(String, ModelRoute)],
    provider: ModelProvider,
    default_provider: ModelProvider,
) -> Vec<&str> {
    routes
        .iter()
        .filter_map(|(name, route)| {
            route_uses_provider(route, provider, default_provider).then_some(name.as_str())
        })
        .collect()
}

/// 单 provider 模式下校验某条 route 的所有候选都落在该 provider 上。
///
/// 候选未显式声明 provider 时使用 `default_provider` 兜底，行为与原实现一致。
pub(crate) fn ensure_route_supported(
    route: &ModelRoute,
    supported: ModelProvider,
    default_provider: ModelProvider,
    name: &str,
) -> Result<(), LlmError> {
    for candidate in route.candidates() {
        let provider = candidate.provider.unwrap_or(default_provider);
        if provider != supported {
            return Err(LlmError::config(format!(
                "{name} candidate `{}` requires provider `{}`, but LLM_PROVIDER is `{}`",
                candidate.to_request_model(),
                provider.as_str(),
                supported.as_str()
            )));
        }
    }
    Ok(())
}

/// 判定一条 route 是否引用了某个 provider。
///
/// 候选未显式声明 provider 时使用 `default_provider` 兜底，与 [`ensure_route_supported`] 语义一致。
pub(crate) fn route_uses_provider(
    route: &ModelRoute,
    provider: ModelProvider,
    default_provider: ModelProvider,
) -> bool {
    route
        .candidates()
        .iter()
        .any(|candidate| candidate.provider.unwrap_or(default_provider) == provider)
}
