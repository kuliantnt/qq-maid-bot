//! 自定义 Provider 元数据的严格解析与协议字段校验。

use super::*;

pub(super) fn default_auth_header() -> String {
    "Authorization".to_owned()
}

pub(super) fn default_auth_scheme() -> Option<String> {
    Some("Bearer".to_owned())
}

pub(super) fn provider_from_file(
    name: &str,
    provider: ProviderFile,
) -> Result<AgentProviderConfig, LlmError> {
    if provider.display_name.as_deref().is_some_and(|name| {
        name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control)
    }) {
        return Err(LlmError::config("Provider 显示名称必须为 1～80 个可见字符"));
    }
    let id = ModelProvider::parse_prefix(name)
        .map_err(|err| LlmError::config(format!("invalid providers.{name}: {}", err.message)))?;
    if !matches!(id, ModelProvider::Custom(_)) {
        return Err(LlmError::config(format!(
            "providers.{name} cannot override built-in provider `{}`",
            id.as_str()
        )));
    }
    let base_url = provider.base_url.trim();
    if base_url.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.base_url must not be empty"
        )));
    }
    let parsed_base_url = url::Url::parse(base_url)
        .map_err(|_| LlmError::config(format!("providers.{name}.base_url must be a valid URL")))?;
    if !matches!(parsed_base_url.scheme(), "http" | "https")
        || parsed_base_url.host_str().is_none()
        || !parsed_base_url.username().is_empty()
        || parsed_base_url.password().is_some()
        || parsed_base_url.query().is_some()
        || parsed_base_url.fragment().is_some()
    {
        return Err(LlmError::config(format!(
            "providers.{name}.base_url must be an HTTP(S) URL without credentials, query, or fragment"
        )));
    }
    let api_key_env = provider.api_key_env.trim();
    if api_key_env.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.api_key_env must not be empty"
        )));
    }
    let mut env_chars = api_key_env.chars();
    if !env_chars
        .next()
        .is_some_and(|value| value == '_' || value.is_ascii_alphabetic())
        || !env_chars.all(|value| value == '_' || value.is_ascii_alphanumeric())
    {
        return Err(LlmError::config(format!(
            "providers.{name}.api_key_env must be a valid environment variable name"
        )));
    }
    let auth_header = provider.auth_header.trim();
    if auth_header.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.auth_header must not be empty"
        )));
    }
    if reqwest::header::HeaderName::from_bytes(auth_header.as_bytes()).is_err() {
        return Err(LlmError::config(format!(
            "providers.{name}.auth_header is invalid"
        )));
    }
    if let Some(scheme) = provider.auth_scheme.as_deref().map(str::trim)
        && !scheme.is_empty()
        && !scheme.bytes().all(is_http_token_byte)
    {
        return Err(LlmError::config(format!(
            "providers.{name}.auth_scheme must be a valid HTTP authentication scheme"
        )));
    }
    if let Some(seconds) = provider.request_timeout_seconds {
        validate_positive("request_timeout_seconds", seconds as usize)?;
    }
    let chat_fallback = match provider.kind {
        AgentProviderKind::OpenAiCompatible => {
            if provider.chat_fallback.is_some() {
                return Err(LlmError::config(format!(
                    "providers.{name}.chat_fallback is only valid for openai_responses"
                )));
            }
            false
        }
        AgentProviderKind::OpenAiResponses => {
            // Tool Loop 目前只能沿用 Responses 协议；禁止只在普通聊天中生效的降级，
            // 避免同一公开配置在不同调用链上表达不同语义。
            if provider.chat_fallback == Some(true) {
                return Err(LlmError::config(format!(
                    "providers.{name}.chat_fallback=true is not supported; omit it or set false"
                )));
            }
            false
        }
    };
    Ok(AgentProviderConfig {
        enabled: provider.enabled,
        id,
        kind: provider.kind,
        base_url: base_url.to_owned(),
        api_key_env: api_key_env.to_owned(),
        auth_header: auth_header.to_owned(),
        auth_scheme: provider
            .auth_scheme
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        request_timeout_seconds: provider.request_timeout_seconds,
        chat_fallback,
    })
}

/// 检查全部保存路线，包括当前场景未使用的路线；避免留下等待启用才暴露的悬空引用。
pub(super) fn validate_connection_references(
    document: &AgentConfigDocument,
) -> Result<(), LlmError> {
    // 旧手工 TOML key 保持原样，只在引用查找时按模型前缀规则规范化。
    let mut providers = HashMap::new();
    for (name, provider) in &document.providers {
        let id = ModelProvider::parse_prefix(name).map_err(|err| {
            LlmError::config(format!("invalid providers.{name}: {}", err.message))
        })?;
        // 大小写别名不能覆盖另一项的 enabled 状态，否则停用引用可能被漏检。
        if providers.insert(id.clone(), provider).is_some() {
            return Err(LlmError::config(format!(
                "duplicate provider `{}`",
                id.as_str()
            )));
        }
    }
    let mut invalid = Vec::new();
    let mut check = |value: &str, location: String| -> Result<(), LlmError> {
        let model = ModelId::parse_config(value, &location)?;
        if let Some(ModelProvider::Custom(id)) = model.provider {
            match providers.get(&ModelProvider::Custom(id.clone())) {
                Some(provider) if provider.enabled => {}
                Some(_) => invalid.push(format!("{location}: provider `{id}` is disabled")),
                None => invalid.push(format!("{location}: providers.{id} is not configured")),
            }
        }
        Ok(())
    };
    for (name, route) in &document.model_routes {
        // 沿用现有候选链解析，兼容单个配置字符串内的逗号分隔候选。
        let route = ModelRoute::parse_config(&route.candidates.join(","), name)?;
        for (index, model) in route.candidates().iter().enumerate() {
            check(
                &model.to_request_model(),
                format!("model_routes.{name}.candidates[{index}]"),
            )?;
        }
    }
    for (name, route) in &document.tools.web_search.routes {
        check(&route.model, format!("tools.web_search.routes.{name}"))?;
    }
    invalid.sort();
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(LlmError::config(invalid.join("; ")))
    }
}

fn is_http_token_byte(value: u8) -> bool {
    value.is_ascii_alphanumeric()
        || matches!(
            value,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
