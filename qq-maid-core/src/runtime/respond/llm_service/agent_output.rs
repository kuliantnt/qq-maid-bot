//! Agent 最终回复的结构化展示契约。
//!
//! 普通模型正文保持原样；只有进入 Tool Loop 且模型明确返回约定 envelope 时，
//! 才从正文中提取“实际展示了哪些 Tool 结果”的元数据。业务域随后仍需用真实
//! Tool Result 验证这些 call id，不能把模型声明直接当作执行成功。

use qq_maid_llm::agent_loop::AgentRunDiagnostics;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentDisplayContract {
    /// 模型最终正文实际展示的 Tool 结果对应的 provider call id。
    pub(crate) published_tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedAgentReply {
    pub(crate) reply: String,
    pub(crate) display_contract: AgentDisplayContract,
}

pub(crate) fn parse_agent_reply(
    raw_reply: String,
    agent: &AgentRunDiagnostics,
) -> ParsedAgentReply {
    if !is_tool_turn(agent) {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    }

    let Some(value) = serde_json::from_str::<Value>(&raw_reply).ok() else {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    };
    let Some(object) = value.as_object() else {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    };
    let Some(reply) = object.get("reply").and_then(Value::as_str) else {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    };
    let Some(call_ids) = object
        .get("published_tool_call_ids")
        .and_then(Value::as_array)
    else {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    };
    if call_ids.iter().any(|value| value.as_str().is_none()) {
        return ParsedAgentReply {
            reply: raw_reply,
            display_contract: AgentDisplayContract::default(),
        };
    }

    let mut published_tool_call_ids = Vec::new();
    for call_id in call_ids.iter().filter_map(Value::as_str) {
        let call_id = call_id.trim();
        if !call_id.is_empty() && !published_tool_call_ids.iter().any(|item| item == call_id) {
            published_tool_call_ids.push(call_id.to_owned());
        }
    }

    ParsedAgentReply {
        reply: reply.trim().to_owned(),
        display_contract: AgentDisplayContract {
            published_tool_call_ids,
        },
    }
}

fn is_tool_turn(agent: &AgentRunDiagnostics) -> bool {
    agent.tool_execution_attempted
        || !agent.emitted_tools.is_empty()
        || !agent.tool_results.is_empty()
        || !agent.tool_attempts.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_agent() -> AgentRunDiagnostics {
        AgentRunDiagnostics {
            tool_execution_attempted: true,
            ..AgentRunDiagnostics::default()
        }
    }

    #[test]
    fn plain_reply_is_not_interpreted_as_contract() {
        let parsed = parse_agent_reply("{\"reply\":\"普通 JSON\"}".to_owned(), &tool_agent());

        assert_eq!(parsed.reply, "{\"reply\":\"普通 JSON\"}");
        assert!(parsed.display_contract.published_tool_call_ids.is_empty());
    }

    #[test]
    fn valid_contract_extracts_reply_and_deduplicates_call_ids() {
        let parsed = parse_agent_reply(
            r#"{"reply":"已展示列表","published_tool_call_ids":[" call-1 ","call-1","call-2"]}"#
                .to_owned(),
            &tool_agent(),
        );

        assert_eq!(parsed.reply, "已展示列表");
        assert_eq!(
            parsed.display_contract.published_tool_call_ids,
            vec!["call-1", "call-2"]
        );
    }

    #[test]
    fn malformed_contract_remains_user_text() {
        let raw = r#"{"reply":"列表","published_tool_call_ids":[1]}"#;
        let parsed = parse_agent_reply(raw.to_owned(), &tool_agent());

        assert_eq!(parsed.reply, raw);
        assert!(parsed.display_contract.published_tool_call_ids.is_empty());
    }

    #[test]
    fn ordinary_chat_never_interprets_contract() {
        let raw = r#"{"reply":"普通聊天","published_tool_call_ids":["call-1"]}"#;
        let parsed = parse_agent_reply(raw.to_owned(), &AgentRunDiagnostics::default());

        assert_eq!(parsed.reply, raw);
        assert!(parsed.display_contract.published_tool_call_ids.is_empty());
    }
}
