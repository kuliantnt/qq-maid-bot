//! route 配置解析与 provider 可用性预检。
//!
//! 在 `build_provider` 阶段统一校验：
//! * 单 provider 模式下所有 specialty route 都必须落在该 provider 上；
//! * auto 模式下根据 route 实际引用的 provider 计算需要初始化的 provider 集合，
//!   缺少 API key 的候选可以跳过，但每条 route 必须至少保留一个可用 provider；
//! * auto 模式保留旧的「单 OpenAI 主模型自动追加 DeepSeek fallback」兼容行为。

use crate::{
    config::{LlmConfig, ProviderMode},
    error::LlmError,
    provider::{deepseek, types::ModelId},
};

use super::types::{ModelProvider, ModelRoute};

/// `build_provider` 与配置中心候选校验共享的纯配置计划。
///
/// 这里只检查 route、Provider 声明和凭证是否足以完成初始化，不创建 HTTP client，
/// 更不会发起任何上游请求。
pub(crate) struct ProviderBuildPlan {
    pub(crate) default_route: ModelRoute,
    pub(crate) provider_routes: Vec<(String, ModelRoute)>,
    pub(crate) provider_kinds: Vec<ModelProvider>,
}

pub(crate) fn provider_build_plan(config: &LlmConfig) -> Result<ProviderBuildPlan, LlmError> {
    let configured_custom_providers = config
        .openai_compatible_providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    ensure_custom_providers_declared(
        &config.configured_model_routes,
        &configured_custom_providers,
    )?;

    let (default_provider, default_route, provider_routes) = match config.provider {
        ProviderMode::OpenAi => (
            ModelProvider::OpenAi,
            config.model_route.clone(),
            config.configured_model_routes.clone(),
        ),
        ProviderMode::DeepSeek => (
            ModelProvider::DeepSeek,
            config.model_route.clone(),
            config.configured_model_routes.clone(),
        ),
        ProviderMode::BigModel => (
            ModelProvider::BigModel,
            config.model_route.clone(),
            config.configured_model_routes.clone(),
        ),
        ProviderMode::Gemini => (
            ModelProvider::Gemini,
            config.model_route.clone(),
            config.configured_model_routes.clone(),
        ),
        ProviderMode::Auto => {
            let default_route = auto_default_route(config)?;
            let routes = auto_provider_routes(config, &default_route)?;
            (ModelProvider::OpenAi, default_route, routes)
        }
    };

    if config.provider != ProviderMode::Auto {
        for (name, route) in &provider_routes {
            ensure_route_supported(route, &default_provider, &default_provider, name)?;
        }
    }

    let provider_kinds = if config.provider == ProviderMode::Auto {
        available_provider_kinds_for_routes(config, &provider_routes, &default_provider)
    } else if provider_api_key_configured(config, &default_provider) {
        vec![default_provider.clone()]
    } else {
        Vec::new()
    };

    for (route_name, route) in &provider_routes {
        let has_available_provider = route.candidates().iter().any(|candidate| {
            let provider = candidate.provider.as_ref().unwrap_or(&default_provider);
            provider_kinds.iter().any(|available| available == provider)
        });
        if !has_available_provider {
            return Err(LlmError::config(format!(
                "model route `{route_name}` has no available provider; configure an API key for at least one provider referenced by this route"
            )));
        }
    }

    if provider_kinds.is_empty() {
        return Err(LlmError::config(
            "no LLM provider is available for configured model routes; configure an API key for at least one referenced provider",
        ));
    }

    Ok(ProviderBuildPlan {
        default_route,
        provider_routes,
        provider_kinds,
    })
}

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
        && candidates[0].provider.as_ref() != Some(&ModelProvider::DeepSeek)
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

/// 校验 route 中显式引用的自定义 provider 都已在配置文件中声明。
///
/// API key 缺失仍由 auto 模式的可用性预检处理；这里仅拦截拼写错误或漏写
/// `[providers.*]` 的配置，避免错误 provider 前缀被静默跳过。
pub(crate) fn ensure_custom_providers_declared(
    routes: &[(String, ModelRoute)],
    configured_providers: &[ModelProvider],
) -> Result<(), LlmError> {
    for (route_name, route) in routes {
        for candidate in route.candidates() {
            let Some(provider @ ModelProvider::Custom(name)) = candidate.provider.as_ref() else {
                continue;
            };
            if !configured_providers
                .iter()
                .any(|configured| configured == provider)
            {
                return Err(LlmError::config(format!(
                    "{route_name} candidate `{}` references custom provider `{name}`, but providers.{name} is not configured",
                    candidate.to_request_model()
                )));
            }
        }
    }
    Ok(())
}

/// 收集所有 named route 实际引用到的 provider，按固定顺序去重。
///
/// 顺序固定为 OpenAI -> DeepSeek -> BigModel -> Gemini，保证 `build_provider` 构造的
/// provider 列表与原实现一致。
pub(crate) fn provider_kinds_for_routes(
    routes: &[(String, ModelRoute)],
    default_provider: &ModelProvider,
    configured_providers: &[ModelProvider],
) -> Vec<ModelProvider> {
    let mut providers = vec![
        ModelProvider::OpenAi,
        ModelProvider::DeepSeek,
        ModelProvider::BigModel,
        ModelProvider::Gemini,
    ];
    for provider in configured_providers {
        if !providers.iter().any(|existing| existing == provider) {
            providers.push(provider.clone());
        }
    }
    providers
        .into_iter()
        .filter(|provider| {
            routes
                .iter()
                .any(|(_, route)| route_uses_provider(route, provider, default_provider))
        })
        .collect()
}

/// 收集 auto 模式下具备 API key、可以初始化的 provider。
///
/// 候选链允许写多个 provider 做 fallback；缺少某个 provider 的 API key 时，
/// 运行时候选链会跳过缺少凭证的 provider，并继续尝试后续可用候选。
pub(crate) fn available_provider_kinds_for_routes(
    config: &LlmConfig,
    routes: &[(String, ModelRoute)],
    default_provider: &ModelProvider,
) -> Vec<ModelProvider> {
    let configured_providers = config
        .openai_compatible_providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    provider_kinds_for_routes(routes, default_provider, &configured_providers)
        .into_iter()
        .filter(|provider| provider_api_key_configured(config, provider))
        .collect()
}

pub(crate) fn provider_api_key_configured(config: &LlmConfig, provider: &ModelProvider) -> bool {
    match provider {
        ModelProvider::OpenAi => config.openai_api_key.as_deref(),
        ModelProvider::DeepSeek => config.deepseek_api_key.as_deref(),
        ModelProvider::BigModel => config.bigmodel_api_key.as_deref(),
        ModelProvider::Gemini => config.gemini_api_key.as_deref(),
        ModelProvider::Custom(_) => config
            .openai_compatible_providers
            .iter()
            .find(|entry| &entry.id == provider)
            .and_then(|entry| entry.api_key.as_deref()),
    }
    .is_some_and(|value| !value.trim().is_empty())
}

/// 单 provider 模式下校验某条 route 的所有候选都落在该 provider 上。
///
/// 候选未显式声明 provider 时使用 `default_provider` 兜底，行为与原实现一致。
pub(crate) fn ensure_route_supported(
    route: &ModelRoute,
    supported: &ModelProvider,
    default_provider: &ModelProvider,
    name: &str,
) -> Result<(), LlmError> {
    for candidate in route.candidates() {
        let provider = candidate.provider.as_ref().unwrap_or(default_provider);
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
    provider: &ModelProvider,
    default_provider: &ModelProvider,
) -> bool {
    route
        .candidates()
        .iter()
        .any(|candidate| candidate.provider.as_ref().unwrap_or(default_provider) == provider)
}
