//! 模型可调用 Tool 的通用抽象。
//!
//! 这里定义的是可执行能力（Tool），不是未来的 Skill 文件加载层。
//! Skill 后续只应作为说明、元数据和多个 Tool 的组合，不直接承担业务执行。

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
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

/// Tool 执行上下文。
///
/// 该上下文由服务端按当前请求生成，不能来自模型参数；后续 Todo 等有副作用 Tool
/// 必须依赖这里的用户和作用域信息做权限绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    /// 单次 Tool Loop 的服务端任务 ID，用于日志关联和后续审计。
    pub task_id: String,
    /// 当前请求的用户 ID；平台未提供时保持为空，不能让模型补充。
    pub user_id: Option<String>,
    /// 当前请求的会话/业务作用域 ID。
    pub scope_id: String,
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
    /// 执行工具。上下文来自服务端，参数已经由 Tool Loop 按 JSON 解析完成。
    async fn execute(&self, context: ToolContext, arguments: Value)
    -> Result<ToolOutput, LlmError>;
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

    pub async fn execute_json(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: &str,
    ) -> Result<String, LlmError> {
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
        let output = timeout(self.timeout, tool.execute(context.clone(), arguments))
            .await
            .map_err(|_| LlmError::new("timeout", "tool execution timed out", "tool"))??;
        let serialized = serde_json::to_string(&output.value).map_err(|err| {
            LlmError::new(
                "tool_output_error",
                format!("failed to serialize tool `{name}` output: {err}"),
                "tool",
            )
        })?;
        Ok(truncate_tool_output(&serialized, self.output_max_chars))
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

/// 把待回传给模型的 Tool 输出按字符限制包装。
///
/// 旧实现直接按字符截断序列化后的 JSON 字符串，会产生残缺 JSON 导致模型上下文被污染。
/// 这里改为始终保持合法 JSON：未超限时原样返回；超限时用 `{"truncated":true,"original_chars":N,"preview":"..."}`
/// 包装，`preview` 为截断后的原始 JSON 片段，且最终序列化结果的整体字符数仍不超过 `max_chars`。
fn truncate_tool_output(serialized: &str, max_chars: usize) -> String {
    let total_chars = serialized.chars().count();
    if total_chars <= max_chars {
        // 未超限，保持原始 JSON 输出。
        return serialized.to_owned();
    }
    let chars: Vec<char> = serialized.chars().collect();

    // 尝试以长度 k 的 preview 构造包装 JSON，返回序列化后在限制内的结果。
    let try_wrap = |k: usize| -> Option<String> {
        let preview: String = chars[..k].iter().collect();
        let wrapper = json!({
            "truncated": true,
            "original_chars": total_chars,
            "preview": preview,
        });
        let wrapped = serde_json::to_string(&wrapper).ok()?;
        if wrapped.chars().count() <= max_chars {
            Some(wrapped)
        } else {
            None
        }
    };

    // 二分查找最大的 preview 长度，使包装后的整体字符数不超过 max_chars。
    let mut lo = 0usize;
    let mut hi = chars.len().min(max_chars);
    let mut best = try_wrap(lo);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        match try_wrap(mid) {
            Some(wrapped) => {
                lo = mid;
                best = Some(wrapped);
            }
            None => hi = mid.saturating_sub(1),
        }
    }
    best.unwrap_or_else(|| {
        // 极端情况下 max_chars 过小连空 preview 都装不下，返回最简合法 JSON 标记。
        r#"{"truncated":true}"#.to_owned()
    })
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

        async fn execute(
            &self,
            context: ToolContext,
            arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            Ok(ToolOutput::json(json!({
                "arguments": arguments,
                "task_id": context.task_id,
                "user_id": context.user_id,
                "scope_id": context.scope_id,
            })))
        }
    }

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
        }
    }

    #[tokio::test]
    async fn registry_executes_registered_tool() {
        let registry = ToolRegistry::new().register(EchoTool).unwrap();

        let output = registry
            .execute_json(&test_context(), "echo", r#"{"text":"hello"}"#)
            .await
            .unwrap();

        assert_eq!(
            output,
            r#"{"arguments":{"text":"hello"},"scope_id":"private:u1","task_id":"task-1","user_id":"u1"}"#
        );
    }

    #[tokio::test]
    async fn registry_rejects_unknown_tool() {
        let registry = ToolRegistry::new();

        let err = registry
            .execute_json(&test_context(), "missing", "{}")
            .await
            .unwrap_err();

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

    struct BigTool;

    #[async_trait]
    impl Tool for BigTool {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "big".to_owned(),
                description: "returns large output".to_owned(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(
            &self,
            _context: ToolContext,
            _arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            // 构造超过 default 4000 字符的输出。
            let padding = "x".repeat(8192);
            Ok(ToolOutput::json(json!({
                "data": padding,
                "meta": "large output fixture",
            })))
        }
    }

    #[tokio::test]
    async fn registry_truncates_large_tool_output_as_valid_json() {
        let registry = ToolRegistry::new()
            .with_limits(DEFAULT_TOOL_TIMEOUT, DEFAULT_TOOL_OUTPUT_MAX_CHARS)
            .register(BigTool)
            .unwrap();

        let output = registry
            .execute_json(&test_context(), "big", "{}")
            .await
            .unwrap();

        // 整体字符数不超过限制。
        assert!(
            output.chars().count() <= DEFAULT_TOOL_OUTPUT_MAX_CHARS,
            "truncated output must not exceed max chars"
        );

        // 结果必须是合法 JSON，且使用 truncated 包装。
        let parsed: Value = serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(parsed["truncated"], Value::Bool(true));
        assert!(parsed["original_chars"].as_u64().unwrap() > DEFAULT_TOOL_OUTPUT_MAX_CHARS as u64);
        assert!(parsed["preview"].is_string());
    }
}
