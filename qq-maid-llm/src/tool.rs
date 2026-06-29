//! 模型可调用 Tool 的通用抽象。
//!
//! 这里定义的是可执行能力（Tool），不是未来的 Skill 文件加载层。
//! Skill 后续只应作为说明、元数据和多个 Tool 的组合，不直接承担业务执行。

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;
use tokio::time::timeout;

use crate::error::LlmError;

/// Tool 执行结果最大字符数，避免把上游大响应直接灌回模型上下文。
pub const DEFAULT_TOOL_OUTPUT_MAX_CHARS: usize = 4000;
/// 单个 Tool 默认超时时间。
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Tool 元数据，直接映射到 OpenAI Responses function tool schema。
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    /// 模型可见的工具名。
    pub name: String,
    /// 模型可见的工具说明。
    pub description: String,
    /// JSON Schema 参数定义。
    pub parameters: Value,
}

/// Tool 执行输出。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// 回传给模型的 JSON 数据。
    pub value: Value,
}

impl ToolOutput {
    pub fn json(value: Value) -> Self {
        Self { value }
    }
}

/// 可执行 Tool。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 返回工具元数据。
    fn metadata(&self) -> ToolMetadata;
    /// 执行工具。参数已经由 Tool Loop 按 JSON 解析完成。
    async fn execute(&self, arguments: Value) -> Result<ToolOutput, LlmError>;
}

/// 动态 Tool 指针。
pub type DynTool = Arc<dyn Tool>;

/// Tool 注册表，只允许模型调用显式注册的工具。
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, DynTool>>,
    timeout: Duration,
    output_max_chars: usize,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(HashMap::new()),
            timeout: DEFAULT_TOOL_TIMEOUT,
            output_max_chars: DEFAULT_TOOL_OUTPUT_MAX_CHARS,
        }
    }

    pub fn with_limits(mut self, timeout: Duration, output_max_chars: usize) -> Self {
        self.timeout = timeout;
        self.output_max_chars = output_max_chars;
        self
    }

    pub fn register<T>(mut self, tool: T) -> Result<Self, LlmError>
    where
        T: Tool + 'static,
    {
        self.insert(Arc::new(tool))?;
        Ok(self)
    }

    pub fn insert(&mut self, tool: DynTool) -> Result<(), LlmError> {
        let metadata = tool.metadata();
        validate_tool_name(&metadata.name)?;
        let tools = Arc::make_mut(&mut self.tools);
        if tools.contains_key(&metadata.name) {
            return Err(LlmError::config(format!(
                "duplicate tool `{}`",
                metadata.name
            )));
        }
        tools.insert(metadata.name, tool);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn metadata(&self) -> Vec<ToolMetadata> {
        let mut items = self
            .tools
            .values()
            .map(|tool| tool.metadata())
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.name.cmp(&right.name));
        items
    }

    pub async fn execute_json(&self, name: &str, arguments: &str) -> Result<String, LlmError> {
        let Some(tool) = self.tools.get(name).cloned() else {
            return Err(LlmError::new(
                "tool_not_found",
                format!("unregistered tool `{name}`"),
                "tool",
            ));
        };
        let arguments = serde_json::from_str::<Value>(arguments).map_err(|err| {
            LlmError::new(
                "bad_tool_arguments",
                format!("invalid JSON arguments for tool `{name}`: {err}"),
                "tool",
            )
        })?;
        let output = timeout(self.timeout, tool.execute(arguments))
            .await
            .map_err(|_| LlmError::new("timeout", "tool execution timed out", "tool"))??;
        let serialized = serde_json::to_string(&output.value).map_err(|err| {
            LlmError::new(
                "tool_output_error",
                format!("failed to serialize tool `{name}` output: {err}"),
                "tool",
            )
        })?;
        Ok(truncate_chars(&serialized, self.output_max_chars))
    }
}

fn validate_tool_name(name: &str) -> Result<(), LlmError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(LlmError::config(format!("invalid tool name `{name}`")))
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let keep = max_chars.saturating_sub(32);
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push_str("...[tool output truncated]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "echo".to_owned(),
                description: "echo arguments".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"],
                    "additionalProperties": false
                }),
            }
        }

        async fn execute(&self, arguments: Value) -> Result<ToolOutput, LlmError> {
            Ok(ToolOutput::json(json!({"arguments": arguments})))
        }
    }

    #[tokio::test]
    async fn registry_executes_registered_tool() {
        let registry = ToolRegistry::new().register(EchoTool).unwrap();

        let output = registry
            .execute_json("echo", r#"{"text":"hello"}"#)
            .await
            .unwrap();

        assert_eq!(output, r#"{"arguments":{"text":"hello"}}"#);
    }

    #[tokio::test]
    async fn registry_rejects_unknown_tool() {
        let registry = ToolRegistry::new();

        let err = registry.execute_json("missing", "{}").await.unwrap_err();

        assert_eq!(err.code, "tool_not_found");
        assert_eq!(err.stage, "tool");
    }

    #[test]
    fn registry_rejects_duplicate_tool_name() {
        let result = ToolRegistry::new()
            .register(EchoTool)
            .unwrap()
            .register(EchoTool);
        let err = match result {
            Ok(_) => panic!("duplicate tool name should be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
    }
}
