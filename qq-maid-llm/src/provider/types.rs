//! LLM 提供商通用数据类型定义。
//!
//! 包含聊天请求/响应的核心结构体、角色枚举、模型标识解析等。

use std::collections::HashMap;

use qq_maid_common::input_part::{MessageInputPart, TextSource};
use serde::{Deserialize, Serialize};

use crate::{context_budget::ContextBudgetConfig, error::LlmError};

/// LLM 提供商枚举。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModelProvider {
    /// OpenAI（含兼容 API）。
    OpenAi,
    /// DeepSeek。
    DeepSeek,
    /// 智谱 BigModel。
    BigModel,
    /// Google Gemini。
    Gemini,
    /// 配置文件声明的 OpenAI-compatible provider。
    Custom(String),
}

impl ModelProvider {
    pub fn as_str(&self) -> &str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::BigModel => "bigmodel",
            Self::Gemini => "gemini",
            Self::Custom(name) => name,
        }
    }

    pub fn parse_prefix(value: &str) -> Result<Self, LlmError> {
        let value = value.trim().to_ascii_lowercase();
        match value.as_str() {
            "openai" => Ok(Self::OpenAi),
            "deepseek" => Ok(Self::DeepSeek),
            "bigmodel" | "zhipu" | "glm" => Ok(Self::BigModel),
            "gemini" | "google" => Ok(Self::Gemini),
            other if is_valid_provider_name(other) => Ok(Self::Custom(other.to_owned())),
            other => Err(LlmError::new(
                "bad_request",
                format!("invalid model provider prefix `{other}`"),
                "request",
            )),
        }
    }
}

fn is_valid_provider_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

/// 模型标识，包含可选的提供商前缀和模型名称。
///
/// 支持 `"openai:gpt-5-mini"`、`"deepseek:deepseek-chat"`、`"bigmodel:glm-5.2"`、
/// `"gemini:gemini-2.5-flash"` 或单纯的 `"gpt-5-mini"` 格式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelId {
    /// 解析出的提供商（如 `"openai:"` 前缀），无前缀则为 `None`。
    pub provider: Option<ModelProvider>,
    /// 模型名称（去除前缀后的部分）。
    pub name: String,
}

/// 按优先级排列的模型候选链。
///
/// 配置层统一把 `LLM_MODEL=a,b` 解析成该结构，provider 层只消费结构化候选，
/// 避免在不同业务 flow 中重复处理逗号分隔字符串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    candidates: Vec<ModelId>,
}

/// 通用推理强度档位。
///
/// Core 只传递结构化策略；具体 provider 负责判断是否支持并映射到协议参数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// 低推理预算，适合快速回复。
    Low,
    /// 中等推理预算，默认用于常规私聊。
    Medium,
    /// 高推理预算，适合复杂任务。
    High,
}

/// 聊天消息角色。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    /// 系统指令。
    System,
    /// 用户消息。
    User,
    /// 助手回复。
    Assistant,
}

/// 单条聊天消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    /// 消息角色（system / user / assistant）。
    pub role: ChatRole,
    /// 消息文本内容。
    pub content: String,
    /// 有序多模态内容块。为空时按 `content` 兼容为单个文本块。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_parts: Vec<MessageInputPart>,
}

/// LLM 聊天请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatRequest {
    /// 会话标识，用于关联上下文和会话存储。
    pub session_id: String,
    /// 内部可用 `openai:gpt-...` / `deepseek:deepseek-chat` 指定模型归属。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 消息列表，按时间顺序排列。
    pub messages: Vec<ChatMessage>,
    /// 可选上下文预算，由应用装配层注入；LLM crate 不读取 AGENT_* 环境变量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<ContextBudgetConfig>,
    /// 请求级输出预算覆盖；为空时使用 provider 初始化时的默认预算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 请求级推理强度；provider 不支持时必须明确忽略或降级，不能在业务层伪装生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 附加元数据（透传，可用于日志追踪等）。
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl ChatRequest {
    pub fn has_non_text_parts(&self) -> bool {
        self.messages.iter().any(ChatMessage::has_non_text_parts)
    }
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// 令牌用量统计。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// 输入（请求）令牌数。
    pub input_tokens: Option<u64>,
    /// 来自上游 prompt cache 的输入令牌数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// 输出（回复）令牌数。
    pub output_tokens: Option<u64>,
    /// 总计令牌数。
    pub total_tokens: Option<u64>,
}

impl ModelId {
    /// 解析模型标识字符串。
    ///
    /// 支持格式：
    /// - `"openai:gpt-5-mini"` → provider: OpenAi, name: "gpt-5-mini"
    /// - `"deepseek:deepseek-chat"` → provider: DeepSeek, name: "deepseek-chat"
    /// - `"bigmodel:glm-5.2"` → provider: BigModel, name: "glm-5.2"
    /// - `"gemini:gemini-2.5-flash"` → provider: Gemini, name: "gemini-2.5-flash"
    /// - `"gpt-5-mini"` → provider: None, name: "gpt-5-mini"
    ///
    /// `stage` 参数用于错误上下文标记（如 `"request"` / `"config"`）。
    pub fn parse(value: &str, stage: &'static str) -> Result<Self, LlmError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "model must not be empty",
                stage,
            ));
        }

        let Some((provider, model)) = value.split_once(':') else {
            return Ok(Self {
                provider: None,
                name: value.to_owned(),
            });
        };
        let provider = ModelProvider::parse_prefix(provider)
            .map_err(|err| LlmError::new(err.code, err.message, stage))?;
        let model = model.trim();
        if model.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "provider-prefixed model must include a model name",
                stage,
            ));
        }
        Ok(Self {
            provider: Some(provider),
            name: model.to_owned(),
        })
    }

    /// 解析配置中的模型标识（阶段固定为 "config"），失败时返回配置错误。
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        Self::parse(value, "config")
            .map_err(|err| LlmError::config(format!("invalid {name}: {}", err.message)))
    }

    /// 还原成可传给 provider 的模型请求字符串。
    pub fn to_request_model(&self) -> String {
        match self.provider.as_ref() {
            Some(provider) => format!("{}:{}", provider.as_str(), self.name),
            None => self.name.clone(),
        }
    }
}

impl ModelRoute {
    /// 解析请求或内部调用中的模型候选链。
    pub fn parse(value: &str, stage: &'static str) -> Result<Self, LlmError> {
        let raw = value.trim();
        if raw.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "model candidate list must not be empty",
                stage,
            ));
        }

        let mut candidates = Vec::new();
        for (index, item) in raw.split(',').enumerate() {
            let item = item.trim();
            if item.is_empty() {
                return Err(LlmError::new(
                    "bad_request",
                    format!("model candidate #{index} must not be empty"),
                    stage,
                ));
            }
            candidates.push(ModelId::parse(item, stage)?);
        }

        Ok(Self { candidates })
    }

    /// 解析环境配置中的模型候选链，错误消息包含配置项名称。
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        Self::parse(value, "config")
            .map_err(|err| LlmError::config(format!("invalid {name}: {}", err.message)))
    }

    pub fn from_candidates(candidates: Vec<ModelId>) -> Result<Self, LlmError> {
        if candidates.is_empty() {
            return Err(LlmError::config(
                "model route must contain at least one candidate",
            ));
        }
        Ok(Self { candidates })
    }

    pub fn candidates(&self) -> &[ModelId] {
        &self.candidates
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// 构造入口会保证候选链非空；保留该方法用于和公共集合 API 保持一致。
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn first(&self) -> &ModelId {
        &self.candidates[0]
    }

    pub fn display(&self) -> String {
        self.candidates
            .iter()
            .map(ModelId::to_request_model)
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl ChatMessage {
    /// 创建一条系统消息。
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            content_parts: Vec::new(),
        }
    }

    /// 创建一条用户消息。
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            content_parts: Vec::new(),
        }
    }

    /// 创建一条带有有序内容块的用户消息。
    pub fn user_with_parts(
        content: impl Into<String>,
        content_parts: Vec<MessageInputPart>,
    ) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            content_parts,
        }
    }

    /// 返回 provider 应发送的内容块；旧纯文本消息自动兼容为 text part。
    pub fn effective_content_parts(&self) -> Vec<MessageInputPart> {
        if !self.content_parts.is_empty() {
            // Gateway/Core 都应先移除不可信引用文字；Provider 边界再做一次兜底，避免
            // 任一调用方误把污染标记序列化到外部模型请求。
            return self
                .content_parts
                .iter()
                .filter(|part| {
                    !matches!(
                        part,
                        MessageInputPart::Text {
                            source: Some(TextSource::QuoteContaminated),
                            ..
                        }
                    )
                })
                .cloned()
                .collect();
        }
        let text = self.content.trim();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![MessageInputPart::text(self.content.clone())]
        }
    }

    pub fn has_non_text_parts(&self) -> bool {
        self.content_parts.iter().any(MessageInputPart::is_non_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_roundtrips_json() {
        let req = ChatRequest {
            session_id: "group:g1".to_owned(),
            model: None,
            messages: vec![ChatMessage::user("你好")],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: HashMap::from([("platform".to_owned(), "qq".to_owned())]),
        };

        let encoded = serde_json::to_string(&req).unwrap();
        let decoded: ChatRequest = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, req);
        assert!(
            serde_json::from_str::<serde_json::Value>(&encoded)
                .unwrap()
                .get("model")
                .is_none()
        );
    }

    #[test]
    fn model_id_parses_provider_prefix() {
        assert_eq!(
            ModelId::parse("openai:gpt-5-mini", "request").unwrap(),
            ModelId {
                provider: Some(ModelProvider::OpenAi),
                name: "gpt-5-mini".to_owned()
            }
        );
        assert_eq!(
            ModelId::parse("deepseek:deepseek-chat", "request").unwrap(),
            ModelId {
                provider: Some(ModelProvider::DeepSeek),
                name: "deepseek-chat".to_owned()
            }
        );
        assert_eq!(
            ModelId::parse("bigmodel:glm-5.2", "request").unwrap(),
            ModelId {
                provider: Some(ModelProvider::BigModel),
                name: "glm-5.2".to_owned()
            }
        );
        assert_eq!(
            ModelId::parse("gpt-5-mini", "request").unwrap(),
            ModelId {
                provider: None,
                name: "gpt-5-mini".to_owned()
            }
        );
    }

    #[test]
    fn model_route_parses_ordered_candidates_and_spaces() {
        let route =
            ModelRoute::parse("openai:gpt-5.4-mini, deepseek:deepseek-chat", "request").unwrap();

        assert_eq!(route.len(), 2);
        assert_eq!(
            route.candidates()[0],
            ModelId {
                provider: Some(ModelProvider::OpenAi),
                name: "gpt-5.4-mini".to_owned()
            }
        );
        assert_eq!(
            route.candidates()[1],
            ModelId {
                provider: Some(ModelProvider::DeepSeek),
                name: "deepseek-chat".to_owned()
            }
        );
        assert_eq!(
            route.display(),
            "openai:gpt-5.4-mini,deepseek:deepseek-chat"
        );
    }

    #[test]
    fn model_route_rejects_empty_candidates() {
        let err =
            ModelRoute::parse_config("openai:gpt-5.4-mini,,deepseek:deepseek-chat", "LLM_MODEL")
                .unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("LLM_MODEL"));
        assert!(err.message.contains("candidate #1"));
    }

    #[test]
    fn model_route_rejects_missing_provider_or_model() {
        let err = ModelRoute::parse_config(":gpt-5.4-mini", "LLM_MODEL").unwrap_err();
        assert_eq!(err.code, "config");
        assert!(err.message.contains("invalid model provider prefix"));

        let err = ModelRoute::parse_config("openai:", "LLM_MODEL").unwrap_err();
        assert_eq!(err.code, "config");
        assert!(err.message.contains("must include a model name"));
    }
}
