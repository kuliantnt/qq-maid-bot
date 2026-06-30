//! 普通私聊 Tool Loop 前置路由。
//!
//! 当前这里只覆盖已经接入 Tool Loop 的 Weather 和 Todo，用来决定本轮普通
//! 私聊是否要走 Complete Tool Loop。它不是通用语义分类器，也不负责 Memory、
//! RSS、MCP、Skills 等未来工具；完整通用 Tool Loop 流式能力留给 #83 处理。

use crate::runtime::session::SessionRecord;

use super::{RespondRequest, chat_flow::todo_guard, todo_flow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolLoopRoute {
    PlainChat,
    CompleteToolLoop,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolRouteContext<'a> {
    pub tool_calling_enabled: bool,
    pub provider_supports_tool_calling: bool,
    pub active_session: Option<&'a SessionRecord>,
}

pub(super) fn route_tool_loop(req: &RespondRequest, ctx: ToolRouteContext<'_>) -> ToolLoopRoute {
    if !ctx.tool_calling_enabled || !ctx.provider_supports_tool_calling {
        return ToolLoopRoute::PlainChat;
    }
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('／')
        || req
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    {
        return ToolLoopRoute::PlainChat;
    }

    if looks_like_weather_tool_request(trimmed) || looks_like_todo_tool_request(trimmed, ctx) {
        ToolLoopRoute::CompleteToolLoop
    } else {
        ToolLoopRoute::PlainChat
    }
}

/// 判断普通私聊是否明显需要天气 Tool。
///
/// 这里只识别当前稳定接入的天气工具请求，不处理“天气 API 怎么设计”这类
/// 元讨论，避免工具链路抢走本该流式输出的普通聊天。
fn looks_like_weather_tool_request(text: &str) -> bool {
    if looks_like_weather_meta_discussion(text) {
        return false;
    }
    contains_any(
        text,
        &[
            "下雨",
            "下雪",
            "带伞",
            "温度",
            "气温",
            "几度",
            "降雨",
            "降雪",
            "空气质量",
            "雾霾",
            "预警",
            "台风",
            "风力",
            "冷不冷",
            "热不热",
            "穿什么",
        ],
    ) || (text.contains("天气")
        && contains_any(text, &["今天", "明天", "后天", "现在", "本周", "周末"]))
}

fn looks_like_weather_meta_discussion(text: &str) -> bool {
    text.contains("天气")
        && contains_any(
            text,
            &[
                "API",
                "api",
                "接口",
                "设计",
                "实现",
                "架构",
                "模块",
                "代码",
                "怎么做",
                "怎么写",
            ],
        )
}

/// 判断普通私聊是否明显需要 Todo Tool。
///
/// 写操作复用 `todo_guard` 的严格判定；查询类只覆盖当前 Tool Loop 已承载的
/// 可见编号/最近对象绑定场景，避免“聊聊待办设计”误入工具链路。
fn looks_like_todo_tool_request(text: &str, ctx: ToolRouteContext<'_>) -> bool {
    let has_last_query = ctx
        .active_session
        .and_then(|session| session.last_todo_query.as_ref())
        .is_some();
    let has_last_action = ctx
        .active_session
        .and_then(|session| session.last_todo_action.as_ref())
        .is_some();
    if todo_guard::requires_todo_tool_with_context(text, has_last_query, has_last_action) {
        return true;
    }
    if todo_flow::is_natural_todo_query_text(text) {
        return true;
    }
    contains_any(
        text,
        &[
            "看看已完成",
            "查看已完成",
            "列出已完成",
            "看看已取消",
            "查看已取消",
            "列出已取消",
            "看看待办",
            "查看待办",
            "列出待办",
            "还有什么待办",
            "有哪些待办",
        ],
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::runtime::{
        session::{LastTodoAction, LastTodoQuery, SessionRecord},
        todo::TodoStatus,
    };

    use super::*;

    fn request(text: &str) -> RespondRequest {
        RespondRequest {
            content: text.to_owned(),
            scope_key: "private:u1".to_owned(),
            user_id: Some("u1".to_owned()),
            platform: "qq_official".to_owned(),
            ..Default::default()
        }
    }

    fn context(active_session: Option<&SessionRecord>) -> ToolRouteContext<'_> {
        ToolRouteContext {
            tool_calling_enabled: true,
            provider_supports_tool_calling: true,
            active_session,
        }
    }

    fn empty_session() -> SessionRecord {
        serde_json::from_value(json!({})).unwrap()
    }

    fn session_with_last_query() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_query = Some(LastTodoQuery {
            owner_key: "private:u1".to_owned(),
            query_type: "list".to_owned(),
            condition: String::new(),
            result_ids: vec!["item-1".to_owned()],
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    fn session_with_last_action() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_action = Some(LastTodoAction {
            owner_key: "private:u1".to_owned(),
            item_id: "item-1".to_owned(),
            title: "示例待办".to_owned(),
            action: "restored".to_owned(),
            resulting_status: TodoStatus::Pending,
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    #[test]
    fn general_chat_keeps_plain_route_even_with_tool_calling_enabled() {
        assert_eq!(
            route_tool_loop(&request("聊聊 Rust 的所有权"), context(None)),
            ToolLoopRoute::PlainChat
        );
    }

    #[test]
    fn weather_route_avoids_meta_discussion() {
        assert_eq!(
            route_tool_loop(&request("杭州明天要带伞吗"), context(None)),
            ToolLoopRoute::CompleteToolLoop
        );
        assert_eq!(
            route_tool_loop(&request("聊聊天气 API 怎么设计"), context(None)),
            ToolLoopRoute::PlainChat
        );
        // 当前阶段只覆盖明确 Weather/Todo 工具意图；泛化外出建议留给 #83 的通用 Tool Loop。
        assert_eq!(
            route_tool_loop(&request("杭州明天适合出门吗"), context(None)),
            ToolLoopRoute::PlainChat
        );
    }

    #[test]
    fn todo_mutations_and_queries_route_to_tool_loop() {
        let with_query = session_with_last_query();
        let with_action = session_with_last_action();
        for (text, session) in [
            ("提醒我明天下午三点开会", None),
            ("完成第 1 个待办", None),
            ("修改第 2 个待办", None),
            ("取消第 2 个任务", None),
            ("永久删除已完成待办第 3 个", None),
            ("恢复第 1 个", Some(&with_query)),
            ("取消它", Some(&with_action)),
            ("看看已完成", None),
        ] {
            assert_eq!(
                route_tool_loop(&request(text), context(session)),
                ToolLoopRoute::CompleteToolLoop,
                "{text}"
            );
        }
    }

    #[test]
    fn disabled_or_group_request_keeps_plain_route() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_tool_loop(&group, context(None)),
            ToolLoopRoute::PlainChat
        );
        assert_eq!(
            route_tool_loop(
                &request("杭州明天要带伞吗"),
                ToolRouteContext {
                    tool_calling_enabled: false,
                    provider_supports_tool_calling: true,
                    active_session: None,
                },
            ),
            ToolLoopRoute::PlainChat
        );
    }
}
