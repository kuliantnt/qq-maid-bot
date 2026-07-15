use super::*;

#[test]
fn core_plan_routes_general_private_chat_to_agent_when_tools_available() {
    let provider =
        TestProvider::replying("普通回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("hello").into();

    let planned = service.plan_core_respond(&req).unwrap();
    assert_eq!(planned, RespondPlan::AgentRuntime);
    assert_eq!(planned.status_hint(), StatusHint::model());
}

#[test]
fn core_plan_routes_ambiguous_private_chat_to_agent_when_tools_available() {
    let provider =
        TestProvider::replying("普通回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();

    for input in [
        "安排一下",
        "能不能给我发一条，三行的信息",
        "刚刚没看到，再来一条",
        "帮我写个文案",
        "解释一下这个问题",
        "我好烦，陪我聊会",
    ] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::AgentRuntime,
            "{input}"
        );
    }
}

#[test]
fn core_plan_routes_private_weather_message_to_agent_runtime_when_tools_available() {
    let provider =
        TestProvider::replying("工具回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("杭州明天要带伞吗").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::AgentRuntime
    );
    assert_ne!(
        service.plan_core_respond(&req).unwrap().status_hint(),
        StatusHint::model()
    );
}

#[test]
fn core_plan_routes_simple_todo_queries_to_immediate() {
    let provider =
        TestProvider::replying("工具回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();

    for input in ["看一下待办", "看一下代办", "看看已完成"] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::Immediate,
            "{input}"
        );
    }
}

#[test]
fn core_plan_routes_private_todo_like_messages_to_agent_tool_loop() {
    let provider =
        TestProvider::replying("工具回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    for input in [
        "提醒我明天下午三点开会",
        "明天别忘了",
        "周五别忘了开会",
        "月底提醒我续费",
        "下个月初提醒我看账单",
        "完成第一条",
        "恢复第 1 个",
        "取消它",
    ] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::AgentRuntime,
            "{input}"
        );
    }
}

#[test]
fn core_plan_routes_todo_context_reference_after_recent_list_to_tool_loop() {
    let provider =
        TestProvider::replying("工具回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let session_store = state.stores.session_store.clone();
    let meta = SessionMeta::new(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    let mut session = session_store.get_or_create_active(&meta).unwrap();
    // 等同用户刚通过 /todo 或自然语言列表看到了可见编号，路由只消费 fresh session 快照信号。
    let owner = TodoStore::owner(Some("u1"), private_scope());
    session.remember_last_todo_query(&owner.key, "list", "进行中列表", vec!["todo-1".to_owned()]);
    session_store.save(&mut session).unwrap();

    let service = CoreHandle::new(state).respond_service();
    for input in ["处理第一项", "这个改一下", "都删除了吧"] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::AgentRuntime,
            "{input}"
        );
    }
}

#[test]
fn core_plan_routes_weak_todo_reference_to_agent_without_recent_context() {
    let provider =
        TestProvider::replying("聊天回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();

    for input in [
        "这个改一下",
        "都删除了吧",
        "帮我写个文案",
        "解释一下这个问题",
        "刚刚没看到，再来一条",
    ] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::AgentRuntime,
            "{input}"
        );
    }
}

#[test]
fn core_plan_keeps_pending_confirmation_immediate() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let session_store = state.stores.session_store.clone();
    let meta = SessionMeta::new(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    let mut session = session_store.get_or_create_active(&meta).unwrap();
    let owner = TodoStore::owner(Some("u1"), private_scope());
    session.pending_operation = Some(
        TodoPendingPayload::TodoAdd {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key,
            draft: TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: None,
                raw_text: Some("检查日志".to_owned()),
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
            allow_revision: true,
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        }
        .into_prepared_action(&session.scope_key),
    );
    session_store.save(&mut session).unwrap();

    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("确认").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::Immediate
    );
}

#[test]
fn core_plan_keeps_group_chat_streaming_even_when_tool_capable() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("杭州明天要带伞吗").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[test]
fn core_plan_routes_group_chat_to_tool_loop_when_group_switch_enabled() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, true, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("杭州明天要带伞吗").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::AgentRuntime
    );
}

#[test]
fn core_plan_keeps_group_natural_search_on_chat_route_when_agent_disabled() {
    // 自然语言搜索不再绕过群聊 Agent 开关；关闭时保持普通 StreamingChat。
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, false, false);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("联网查询下今日 ai 新闻").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[test]
fn core_plan_keeps_group_pasted_text_processing_as_chat() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, false, false);
    let service = CoreHandle::new(state).respond_service();
    let input = "\
Codex 执行报告：
- 检查 WebSearch 路由
- 查询工具返回：查询内容太长
- agent_route 命中 search 关键词
帮我整理成 issue";
    let req: RespondRequest = group_request(input).into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[test]
fn core_plan_routes_group_standard_chat_to_agent_when_group_switch_enabled() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, true, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("写一段长文本测试流式").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::AgentRuntime
    );
}
