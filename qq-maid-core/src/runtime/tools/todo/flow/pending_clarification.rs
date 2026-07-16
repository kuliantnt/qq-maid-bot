//! TodoClarify 的受限控制工具、提示构造与候选执行上下文。

use std::sync::Arc;

use async_trait::async_trait;
use qq_maid_llm::{
    provider::{ChatOutcome, types::ChatMessage},
    tool::{Tool, ToolContext, ToolMetadata, ToolOutput},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    error::LlmError,
    runtime::{
        session::SessionRecord,
        tools::todo::{PendingTodoClarification, TodoOwner},
    },
};

const CLARIFICATION_CONTROL_TOOL_NAME: &str = "clarification_control";

pub(super) struct ClarificationControlTool;

#[async_trait]
impl Tool for ClarificationControlTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CLARIFICATION_CONTROL_TOOL_NAME.to_owned(),
            description: "澄清恢复控制工具。仅用于表示仍需追问或放弃当前澄清，不操作 Todo 数据。"
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["ask_again", "abandon"],
                        "description": "ask_again=信息仍不足，需要继续追问；abandon=用户放弃或明显不是在回答当前澄清。"
                    },
                    "question": {
                        "type": ["string", "null"],
                        "description": "action=ask_again 时给用户的最小澄清问题；其他情况传 null。"
                    }
                },
                "required": ["action", "question"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::new("bad_tool_arguments", "action is required", "tool"))?;
        let question = arguments
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        match action {
            "ask_again" => Ok(ToolOutput::json(json!({
                "ok": true,
                "action": "ask_again",
                "question": question.unwrap_or_else(|| "请再具体说明要操作哪条待办。".to_owned()),
            }))),
            "abandon" => Ok(ToolOutput::json(json!({
                "ok": true,
                "action": "abandon",
                "question": Value::Null,
            }))),
            _ => Err(LlmError::new(
                "bad_tool_arguments",
                "action must be ask_again or abandon",
                "tool",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClarificationControlAction {
    AskAgain(String),
    Abandon,
}

pub(super) fn clarification_control_action(
    outcome: &ChatOutcome,
) -> Option<ClarificationControlAction> {
    outcome
        .agent
        .tool_results
        .iter()
        .rev()
        .find(|result| result.name == CLARIFICATION_CONTROL_TOOL_NAME)
        .and_then(
            |result| match result.output.get("action").and_then(Value::as_str) {
                Some("ask_again") => Some(ClarificationControlAction::AskAgain(
                    result
                        .output
                        .get("question")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("请再具体说明要操作哪条待办。")
                        .to_owned(),
                )),
                Some("abandon") => Some(ClarificationControlAction::Abandon),
                _ => None,
            },
        )
}

pub(super) fn candidate_scope(
    request: &PendingTodoClarification,
) -> Result<Arc<[String]>, LlmError> {
    if request.candidates.is_empty() {
        return Err(LlmError::new(
            "todo_clarification_scope_empty",
            "todo clarification candidates are empty",
            "todo_pending",
        ));
    }
    let ids = request
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    Ok(Arc::from(ids.into_boxed_slice()))
}

pub(super) fn build_todo_clarification_messages(
    user_text: &str,
    request: &PendingTodoClarification,
) -> Vec<ChatMessage> {
    let candidates = request
        .candidates
        .iter()
        .map(|candidate| format!("{}. {}", candidate.display_number, candidate.title))
        .collect::<Vec<_>>()
        .join("\n");
    let original_arguments =
        serde_json::to_string_pretty(&request.arguments).unwrap_or_else(|_| "{}".to_owned());
    let system = format!(
        "你正在恢复一个待办工具澄清任务。\n\n\
职责边界：\n\
- 只能恢复原工具 `{tool_name}`，不得改成其他 Todo 操作。\n\
- 当前候选编号只在本次澄清中有效，必须从候选 1..N 里选择；不要使用数据库内部 ID。\n\
- 候选标题里的数字（例如“6 号”“买 2 个”）不是候选编号。\n\
- 如果能唯一确定目标，请调用原工具 `{tool_name}`，用候选展示编号作为 number/numbers，并保留或补全原始参数里的其他业务字段。\n\
- 如果仍无法唯一确定，请调用 `{control_tool}`，action=ask_again，并给出最小澄清问题。\n\
- 如果用户明确放弃或明显不是在回答当前澄清，请调用 `{control_tool}`，action=abandon。\n\
- 不要编造成功结果；工具结果才是真实执行状态。\n\n\
原工具：{tool_name}\n原始参数 JSON：\n{original_arguments}\n\n上一次澄清问题：\n{question}\n\n当前候选：\n{candidates}",
        tool_name = request.tool_name,
        control_tool = CLARIFICATION_CONTROL_TOOL_NAME,
        question = request.question,
    );
    vec![
        ChatMessage::system(system),
        ChatMessage::user(user_text.trim().to_owned()),
    ]
}

pub(super) fn clarification_tool_context(
    session: &SessionRecord,
    owner: &TodoOwner,
) -> ToolContext {
    let kind = if session
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        qq_maid_common::identity_context::ConversationKind::Group
    } else {
        qq_maid_common::identity_context::ConversationKind::Private
    };
    ToolContext {
        task_id: format!("todo-clarify:{}", Uuid::new_v4()),
        actor: qq_maid_common::identity_context::ExecutionActorContext {
            user_id: owner.user_id.clone(),
            group_member_role: None,
        },
        conversation: qq_maid_common::identity_context::ExecutionConversationContext {
            platform: session.platform.clone(),
            // 历史 SessionRecord 未保存 account_id；澄清恢复沿用已绑定 owner/scope，
            // 不通过解析 scope 猜测账号。新入站 ToolContext 会携带完整 account_id。
            account_id: None,
            kind,
            target_id: session.group_id.clone().or_else(|| owner.user_id.clone()),
            scope_id: owner.scope_key.clone(),
            interaction_scope_id: session.scope_key.clone(),
        },
        tool_call_id: Some(format!("clarify-{}", session.session_id)),
        execution_deadline: None,
    }
}

pub(super) fn is_clarification_abandon_text(text: &str) -> bool {
    let compact = text
        .trim()
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '，' | ','
                        | '。'
                        | '.'
                        | '！'
                        | '!'
                        | '？'
                        | '?'
                        | '、'
                        | ';'
                        | '；'
                        | ':'
                        | '：'
                )
        })
        .collect::<String>()
        .trim_end_matches(['了', '吧', '啊', '呀', '呢'])
        .to_owned();
    matches!(
        compact.as_str(),
        "取消" | "放弃" | "算了" | "不用" | "不要" | "撤销"
    )
}
