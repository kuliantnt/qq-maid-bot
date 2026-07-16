//! 模型可调用 Tool 的通用抽象。
//!
//! 这里定义的是可执行能力（Tool），不是未来的 Skill 文件加载层。
//! Skill 后续只应作为说明、元数据和多个 Tool 的组合，不直接承担业务执行。

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
#[cfg(test)]
use qq_maid_common::identity_context::ConversationKind;
use qq_maid_common::identity_context::{ExecutionActorContext, ExecutionConversationContext};
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio::time::timeout;

use crate::error::LlmError;

/// Tool 执行结果最大字符数，避免把上游大响应直接灌回模型上下文。
pub const DEFAULT_TOOL_OUTPUT_MAX_CHARS: usize = 4000;
/// 合法生产配置的最小 Tool 输出字符数，至少能表达 `{"truncated":true}`。
pub const MIN_TOOL_OUTPUT_MAX_CHARS: usize = r#"{"truncated":true}"#.len();
/// 单个 Tool 默认超时时间。
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(15);

/// Tool 执行超时策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTimeoutPolicy {
    /// 沿用 ToolRegistry 的统一绝对超时。
    RegistryDefault,
    /// 工具内部自行维护超时边界，注册表不再套第二层绝对超时。
    ToolManaged,
}

/// Tool 对外部状态的影响类型。
///
/// 默认按有副作用处理，只有明确只读的查询工具才应覆盖为 [`ReadOnly`](Self::ReadOnly)，
/// 避免候选回退或取消清理阶段重复执行写操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    ReadOnly,
    SideEffecting,
}

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
    /// 当前操作者身份，只能由服务端请求上下文生成。
    pub actor: ExecutionActorContext,
    /// 当前会话与作用域，只能由服务端请求上下文生成。
    pub conversation: ExecutionConversationContext,
    /// 当前工具调用的稳定标识；由上游 Tool Loop 生成，用于幂等去重与审计关联。
    pub tool_call_id: Option<String>,
    /// 只读工具本次允许执行到的最晚时刻；由 Agent Runtime 从请求 deadline
    /// 扣除最终回答预留后注入，模型参数不能覆盖。非 Agent 兼容入口为 None。
    pub execution_deadline: Option<Instant>,
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

/// 预处理后的工具调用依赖关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallDependency {
    /// 与同轮其他工具调用无显式依赖，当前仍按模型输出顺序执行。
    ///
    /// 同轮调用可能共享 session、用户可见编号或外部副作用；在工具元数据
    /// 能明确表达读写集与并行安全性前，不根据 `None` 盲目并行。
    None,
    /// 依赖前一个工具调用成功；若前一项失败，本项应跳过。
    PreviousCallSuccess,
}

/// Tool 在真正执行前返回的预处理结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPreparation {
    /// 预处理后的参数。可用于把用户侧编号绑定成稳定内部 ID。
    pub arguments: Value,
    /// 与同轮其他工具调用的依赖信息。
    pub dependency: ToolCallDependency,
}

impl ToolPreparation {
    pub fn ready(arguments: Value) -> Self {
        Self {
            arguments,
            dependency: ToolCallDependency::None,
        }
    }

    pub fn with_dependency(mut self, dependency: ToolCallDependency) -> Self {
        self.dependency = dependency;
        self
    }
}

/// 可执行 Tool。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 返回工具元数据。
    fn metadata(&self) -> ToolMetadata;
    /// 返回工具执行超时策略。
    fn timeout_policy(&self) -> ToolTimeoutPolicy {
        ToolTimeoutPolicy::RegistryDefault
    }
    /// 返回工具是否可能修改外部状态；默认保守视为有副作用。
    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffecting
    }
    /// 返回同一请求内只读调用的去重键；写入类工具默认不参与自动去重。
    fn deduplication_key(&self, arguments: &Value) -> Option<String> {
        (self.effect() == ToolEffect::ReadOnly)
            .then(|| serde_json::to_string(arguments).ok())
            .flatten()
    }
    /// 执行前的本地预处理。
    ///
    /// 默认直接沿用模型参数；有状态工具可在这里把用户可见编号预绑定成稳定内部 ID，
    /// 避免同轮前序写操作改变后续编号语义。
    fn prepare(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolPreparation, LlmError> {
        Ok(ToolPreparation::ready(arguments))
    }
    /// 执行工具。上下文来自服务端，参数已经由 Tool Loop 按 JSON 解析完成。
    async fn execute(&self, context: ToolContext, arguments: Value)
    -> Result<ToolOutput, LlmError>;
}

/// 动态 Tool 指针。
pub type DynTool = Arc<dyn Tool>;

/// 已完成参数校验/预处理的工具调用。
#[derive(Clone)]
pub struct PreparedToolCall {
    tool: DynTool,
    /// 工具名，仅用于日志、测试和结果聚合。
    pub name: String,
    /// 当前调用上下文。
    pub context: ToolContext,
    /// 预处理后的参数。
    pub arguments: Value,
    /// 工具的读写语义，用于取消、候选回退和重复调用控制。
    pub effect: ToolEffect,
    /// 同一 Agent 请求内的只读调用去重键。
    pub deduplication_key: Option<String>,
    /// 与同轮前一项调用的依赖关系。
    pub dependency: ToolCallDependency,
}

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

    /// 用同名工具替换已注册实例，用于受限 Tool Loop 的请求级工具覆盖。
    ///
    /// 典型场景：澄清恢复需要为原 Todo 工具注入本次候选作用域，但又不污染主注册表
    /// 与全局状态时，先 [`subset`](Self::subset) 出受限白名单，再用本方法把对应工具
    /// 替换成携带请求级状态的实例。名字未注册时报错，避免静默新增工具绕过白名单。
    pub fn replace(&mut self, tool: DynTool) -> Result<(), LlmError> {
        let metadata = tool.metadata();
        validate_tool_name(&metadata.name)?;
        let tools = Arc::make_mut(&mut self.tools);
        if !tools.contains_key(&metadata.name) {
            return Err(LlmError::config(format!(
                "cannot replace unregistered tool `{}`",
                metadata.name
            )));
        }
        tools.insert(metadata.name, tool);
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 构造一个只包含指定工具的受限注册表，复用原工具实例与限额。
    ///
    /// 用于澄清恢复等受限 Tool Loop：只开放原任务所需工具，禁止模型切换到其他有
    /// 副作用工具。返回的注册表克隆同一份 `Arc<dyn Tool>`，因此与主注册表共享同一
    /// 份 store / session 依赖，执行语义完全一致。任一指定工具不存在则报错，避免
    /// 静默开放空注册表导致受限 Loop 无法执行原任务。
    pub fn subset(&self, names: &[&str]) -> Result<ToolRegistry, LlmError> {
        let mut restricted = ToolRegistry::new().with_limits(self.timeout, self.output_max_chars);
        for name in names {
            let Some(tool) = self.tools.get(*name).cloned() else {
                return Err(LlmError::config(format!(
                    "tool `{name}` not found for restricted subset"
                )));
            };
            restricted.insert(tool)?;
        }
        Ok(restricted)
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
        let prepared = self.prepare_json(context, name, arguments)?;
        self.execute_prepared(prepared).await
    }

    pub fn prepare_json(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: &str,
    ) -> Result<PreparedToolCall, LlmError> {
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
        let preparation = tool.prepare(context, arguments)?;
        let effect = tool.effect();
        let deduplication_key = tool.deduplication_key(&preparation.arguments);
        Ok(PreparedToolCall {
            effect,
            deduplication_key,
            tool,
            name: name.to_owned(),
            context: context.clone(),
            arguments: preparation.arguments,
            dependency: preparation.dependency,
        })
    }

    pub async fn execute_prepared(&self, prepared: PreparedToolCall) -> Result<String, LlmError> {
        let execution = prepared
            .tool
            .execute(prepared.context.clone(), prepared.arguments);
        let output = match prepared.tool.timeout_policy() {
            ToolTimeoutPolicy::RegistryDefault => timeout(self.timeout, execution)
                .await
                .map_err(|_| LlmError::new("timeout", "tool execution timed out", "tool"))??,
            ToolTimeoutPolicy::ToolManaged => execution.await?,
        };
        let serialized = serde_json::to_string(&output.value).map_err(|err| {
            LlmError::new(
                "tool_output_error",
                format!("failed to serialize tool `{}` output: {err}", prepared.name),
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
        // 合法生产配置不会低于 MIN_TOOL_OUTPUT_MAX_CHARS，正常应至少返回
        // {"truncated":true}。下面只兜住内部测试或未来误用 ToolRegistry::with_limits
        // 传入极小值的场景，避免 panic 或残缺 JSON。
        if max_chars >= MIN_TOOL_OUTPUT_MAX_CHARS {
            r#"{"truncated":true}"#.to_owned()
        } else {
            match max_chars {
                0 => String::new(),
                1 => "0".to_owned(),
                _ => "{}".to_owned(),
            }
        }
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
                "user_id": context.actor.user_id,
                "scope_id": context.conversation.scope_id,
            })))
        }
    }

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            actor: ExecutionActorContext {
                user_id: Some("u1".to_owned()),
                group_member_role: None,
            },
            conversation: ExecutionConversationContext {
                platform: "test".to_owned(),
                account_id: None,
                kind: ConversationKind::Private,
                target_id: Some("u1".to_owned()),
                scope_id: "private:u1".to_owned(),
                interaction_scope_id: "private:u1".to_owned(),
            },
            tool_call_id: None,
            execution_deadline: None,
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

    #[tokio::test]
    async fn registry_tiny_tool_output_limit_still_returns_valid_json() {
        let registry = ToolRegistry::new()
            .with_limits(DEFAULT_TOOL_TIMEOUT, 2)
            .register(BigTool)
            .unwrap();

        let output = registry
            .execute_json(&test_context(), "big", "{}")
            .await
            .unwrap();

        assert!(output.chars().count() <= 2);
        serde_json::from_str::<Value>(&output).expect("tiny output must remain valid JSON");
    }

    #[tokio::test]
    async fn registry_minimum_tool_output_limit_keeps_truncated_marker() {
        let registry = ToolRegistry::new()
            .with_limits(DEFAULT_TOOL_TIMEOUT, MIN_TOOL_OUTPUT_MAX_CHARS)
            .register(BigTool)
            .unwrap();

        let output = registry
            .execute_json(&test_context(), "big", "{}")
            .await
            .unwrap();

        assert!(output.chars().count() <= MIN_TOOL_OUTPUT_MAX_CHARS);
        let parsed: Value = serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(parsed["truncated"], Value::Bool(true));
    }

    struct TaggedEcho {
        name: String,
        tag: &'static str,
    }

    #[async_trait]
    impl Tool for TaggedEcho {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: self.name.clone(),
                description: "tagged echo".to_owned(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(
            &self,
            _context: ToolContext,
            _arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            Ok(ToolOutput::json(json!({"tag": self.tag})))
        }
    }

    #[tokio::test]
    async fn registry_subset_keeps_whitelist_and_limits() {
        let registry = ToolRegistry::new()
            .with_limits(DEFAULT_TOOL_TIMEOUT, 123)
            .register(EchoTool)
            .unwrap()
            .register(TaggedEcho {
                name: "alpha".to_owned(),
                tag: "a",
            })
            .unwrap()
            .register(TaggedEcho {
                name: "beta".to_owned(),
                tag: "b",
            })
            .unwrap();

        let restricted = registry.subset(&["alpha", "beta"]).unwrap();
        assert_eq!(restricted.output_max_chars, 123);
        assert_eq!(restricted.metadata().len(), 2);
        // 白名单外的工具不可执行。
        let err = restricted
            .execute_json(&test_context(), "echo", "{}")
            .await
            .unwrap_err();
        assert_eq!(err.code, "tool_not_found");
        // 请求未注册的名字报错。
        let err = match registry.subset(&["missing"]) {
            Ok(_) => panic!("subset of unknown tool should fail"),
            Err(err) => err,
        };
        assert_eq!(err.code, "config");
    }

    #[tokio::test]
    async fn registry_replace_swaps_tool_instance_for_request_scoped_override() {
        let mut registry = ToolRegistry::new()
            .register(TaggedEcho {
                name: "alpha".to_owned(),
                tag: "original",
            })
            .unwrap();

        let original = registry
            .execute_json(&test_context(), "alpha", "{}")
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&original).unwrap()["tag"],
            "original"
        );

        registry
            .replace(Arc::new(TaggedEcho {
                name: "alpha".to_owned(),
                tag: "overridden",
            }) as DynTool)
            .unwrap();
        let overridden = registry
            .execute_json(&test_context(), "alpha", "{}")
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&overridden).unwrap()["tag"],
            "overridden"
        );

        // 名字未注册时报错，避免静默新增工具绕过白名单。
        let err = match registry.replace(Arc::new(TaggedEcho {
            name: "ghost".to_owned(),
            tag: "x",
        }) as DynTool)
        {
            Ok(_) => panic!("replace of unregistered tool should fail"),
            Err(err) => err,
        };
        assert_eq!(err.code, "config");
    }
}
