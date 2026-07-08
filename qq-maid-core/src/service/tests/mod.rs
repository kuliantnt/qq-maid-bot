use super::*;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use qq_maid_common::{identity_context::IdentitySource, input_part::QuotedMessageContext};

use crate::{
    error::LlmError,
    provider::{LlmStreamEvent, ToolCallingProtocol},
    runtime::{
        pending::PendingOperation,
        respond::{RespondPlan, RespondRequest, RespondResponse, StatusAudience, StatusHint},
        session::SessionMeta,
        todo::{TodoItemDraft, TodoStore, TodoTimePrecision},
    },
    util::metrics::LlmMetrics,
};

mod support;
use support::*;

#[test]
fn private_conversation_derives_private_scope() {
    let req = CoreRequest {
        text: "hello".to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();

    assert_eq!(
        respond.scope_key,
        "platform:qq_official:account:app-1:private:u1"
    );
    assert_eq!(respond.platform, "qq_official");
    assert_eq!(respond.user_id.as_deref(), Some("u1"));
    assert_eq!(respond.group_id, None);
}

#[test]
fn group_conversation_derives_group_scope_without_member_split() {
    let req = CoreRequest {
        text: "/todo".to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: None,
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();

    assert_eq!(
        respond.scope_key,
        "platform:qq_official:account:app-1:group:g1"
    );
    assert_eq!(respond.platform, "qq_official");
    assert_eq!(respond.user_id, None);
    assert_eq!(respond.group_id.as_deref(), Some("g1"));
}

#[test]
fn message_context_is_derived_from_core_request_authoritative_fields() {
    // #319 收敛：message_context 由 CoreRequest 权威字段派生，Gateway 不再单独构造。
    use qq_maid_common::identity_context::{
        IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext,
    };

    let req = CoreRequest {
        text: "hi".to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: vec![MentionIdentity {
            raw_text: None,
            target: MessageActorContext {
                user_id: Some("member-2".to_owned()),
                source: IdentitySource::MemberApi,
                ..Default::default()
            },
            is_self: false,
            confidence: MentionConfidence::Event,
        }],
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("sender-1".to_owned()),
            union_id: Some("union-1".to_owned()),
            display_name: Some("昵称".to_owned()),
            group_member_role: Some(CoreGroupMemberRole::Admin),
            is_bot: false,
            identity_source: IdentitySource::MemberApi,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();
    let context = respond
        .message_context
        .as_ref()
        .expect("message_context should be derived");

    // actor 字段从 CoreActor 派生。
    let actor = context.actor.as_ref().expect("actor present");
    assert_eq!(actor.user_id.as_deref(), Some("sender-1"));
    assert_eq!(actor.union_id.as_deref(), Some("union-1"));
    assert_eq!(actor.display_name.as_deref(), Some("昵称"));
    assert_eq!(actor.group_member_role.as_deref(), Some("admin"));
    assert_eq!(actor.is_bot, Some(false));
    assert_eq!(actor.source, IdentitySource::MemberApi);

    // mentions 透传。
    assert_eq!(context.mentions.len(), 1);
    assert_eq!(
        context.mentions[0].target.user_id.as_deref(),
        Some("member-2")
    );

    // conversation 从 CoreConversation 派生。
    assert_eq!(context.conversation.kind, "group");
    assert_eq!(context.conversation.id.as_deref(), Some("g1"));
    assert_eq!(
        context.conversation.platform.as_deref(),
        Some("qq_official")
    );
    assert_eq!(context.conversation.account_id.as_deref(), Some("app-1"));
}

#[test]
fn safe_error_message_redacts_secret_like_detail() {
    let err = LlmError::http(
        "OpenAI chat returned HTTP 400: key sk-test-secret and bearer abc.def.ghi rejected",
    );

    let message = safe_error_message(&err);

    assert!(message.contains("HTTP 400"));
    assert!(!message.contains("sk-test-secret"));
    assert!(!message.contains("abc.def.ghi"));
}

#[test]
fn core_response_keeps_public_fields_from_respond_response() {
    let response = CoreResponse::from(RespondResponse {
        ok: true,
        text: Some("text".to_owned()),
        markdown: Some("**text**".to_owned()),
        handled: Some(true),
        session_id: Some("session-1".to_owned()),
        command: Some("chat".to_owned()),
        diagnostics: Some(serde_json::json!({"k":"v"})),
        visible_entity_snapshot: None,
        metrics: LlmMetrics {
            provider: "test".to_owned(),
            model: "test".to_owned(),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 1,
        },
        usage: None,
        error: None,
    });

    // Core→Gateway 正文只通过结构化 output 表达，旧 text/markdown 字段已删除。
    assert_eq!(response.text_content(), Some("text"));
    assert_eq!(response.markdown_content(), Some("**text**"));
    let output = response.output.as_ref().expect("assistant output");
    assert_eq!(output.text_fallback, "text");
    assert_eq!(output.markdown.as_deref(), Some("**text**"));
    assert_eq!(
        output.parts,
        vec![OutputPart::Markdown {
            markdown: "**text**".to_owned()
        }]
    );
    assert_eq!(response.handled, Some(true));
    assert_eq!(response.session_id.as_deref(), Some("session-1"));
    assert_eq!(response.command.as_deref(), Some("chat"));
    assert_eq!(response.diagnostics.unwrap()["k"], "v");
}

#[test]
fn assistant_output_text_builds_plain_fallback_part() {
    let output = AssistantOutput::text("hello");

    assert_eq!(output.text_fallback, "hello");
    assert_eq!(output.markdown, None);
    assert_eq!(
        output.parts,
        vec![OutputPart::Text {
            text: "hello".to_owned()
        }]
    );
}

#[test]
fn core_response_with_output_sets_structured_output() {
    let response = CoreResponse {
        output: None,
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    }
    .with_output(AssistantOutput::markdown("fallback", "# title"));

    assert_eq!(response.text_content(), Some("fallback"));
    assert_eq!(response.markdown_content(), Some("# title"));
    assert_eq!(
        response.output.as_ref().map(|output| output.parts.clone()),
        Some(vec![OutputPart::Markdown {
            markdown: "# title".to_owned()
        }])
    );
}

#[test]
fn text_content_and_markdown_content_read_structured_output() {
    // 正文访问器只读取结构化 output，旧 text/markdown 兼容字段已删除。
    let response = CoreResponse {
        output: Some(AssistantOutput::markdown(
            "structured fallback",
            "# structured",
        )),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), Some("structured fallback"));
    assert_eq!(response.markdown_content(), Some("# structured"));
}

#[test]
fn markdown_content_is_none_when_output_only_has_text() {
    // output 仅含纯文本 part（markdown=None）时，markdown_content 返回 None。
    let response = CoreResponse {
        output: Some(AssistantOutput::text("plain")),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), Some("plain"));
    assert_eq!(response.markdown_content(), None);
}

#[test]
fn text_content_returns_none_when_output_absent() {
    let response = CoreResponse {
        output: None,
        handled: Some(false),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };

    assert_eq!(response.text_content(), None);
    assert_eq!(response.markdown_content(), None);
}

#[test]
fn core_plan_routes_general_private_chat_to_streaming_when_tools_available() {
    let provider =
        TestProvider::replying("普通回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("hello").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[test]
fn core_plan_routes_ambiguous_private_chat_to_streaming_when_tools_available() {
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
            RespondPlan::StreamingChat,
            "{input}"
        );
    }
}

#[test]
fn core_plan_routes_private_weather_message_to_complete_tool_loop_when_tools_available() {
    let provider =
        TestProvider::replying("工具回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("杭州明天要带伞吗").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::CompleteToolLoop
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
            RespondPlan::CompleteToolLoop,
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
            RespondPlan::CompleteToolLoop,
            "{input}"
        );
    }
}

#[test]
fn core_plan_keeps_weak_todo_reference_plain_without_recent_context() {
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
            RespondPlan::StreamingChat,
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
    session.pending_operation = Some(PendingOperation::TodoAdd {
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
        },
        allow_revision: true,
        created_at: "2026-06-30T00:00:00+08:00".to_owned(),
    });
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
        RespondPlan::CompleteToolLoop
    );
}

#[test]
fn core_plan_routes_group_search_intent_to_web_search_when_bot_mentioned() {
    // 群聊 @机器人 + 搜索意图走显式 WebSearch，即使群聊 Tool Loop 关闭也不依赖该开关。
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, false, false);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request_with_self_mention("联网查询下今日 ai 新闻").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::WebSearch
    );
}

#[test]
fn core_plan_keeps_group_pasted_text_processing_as_chat_when_bot_mentioned() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, false, false);
    let service = CoreHandle::new(state).respond_service();
    let input = "\
Codex 执行报告：
- 检查 WebSearch 路由
- 查询工具返回：查询内容太长
- tool_route 命中 search 关键词
帮我整理成 issue";
    let req: RespondRequest = group_request_with_self_mention(input).into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[test]
fn core_plan_does_not_route_group_search_intent_to_web_search_without_bot_mention() {
    // 群聊未 @机器人时，仅出现“联网查询 / 搜索”等词不应自动触发 WebSearch；
    // 保留原有路由（这里 Tool Loop 关闭，回落 StreamingChat）。
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, false, false);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("联网查询下今日 ai 新闻").into();

    let plan = service.plan_core_respond(&req).unwrap();
    assert_ne!(plan, RespondPlan::WebSearch);
}

#[test]
fn core_plan_keeps_group_plain_chat_streaming_even_when_group_switch_enabled() {
    let provider =
        TestProvider::replying("群聊回复").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider, 5, true, true);
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = group_request("写一段长文本测试流式").into();

    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::StreamingChat
    );
}

#[tokio::test]
async fn upstream_check_calls_provider_without_creating_session() {
    let provider = TestProvider::replying("OK");
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    service.upstream_check().await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].session_id, "diagnostic:upstream_check");
    assert_eq!(
        requests[0].metadata.get("purpose").map(String::as_str),
        Some("upstream_check")
    );
    // `/ping check` 只验证 provider 连通性，不能创建业务会话或写聊天历史。
    let sessions = session_store
        .list_for_scope("diagnostic:upstream_check", None)
        .unwrap();
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn provider_error_is_returned_as_stream_failure() {
    let state = test_state(
        TestProvider::failing(LlmError::provider("boom", "provider")),
        5,
    );
    let service = CoreHandle::new(state);

    let failure = collect_stream_failure(service.respond(private_request("hello")).await).await;

    assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
    assert!(failure.retryable);
}

#[tokio::test]
async fn stream_response_is_not_cut_by_request_total_timeout() {
    let state = test_state(TestProvider::delayed("late", Duration::from_millis(80)), 0);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("late"));
}

#[tokio::test]
async fn chat_stream_forwards_text_delta_and_completed_from_same_stream() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("你".to_owned())),
        Ok(LlmStreamEvent::TextDelta("好".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ]);
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) =
        service.respond(private_request("hello")).await.unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::DirectStream);

    assert_eq!(
        stream.recv().await,
        Some(CoreResponseEvent::TextDelta("你".to_owned()))
    );
    assert_eq!(
        stream.recv().await,
        Some(CoreResponseEvent::TextDelta("好".to_owned()))
    );
    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("你好"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let sessions = session_store.list_for_scope(private_scope(), None).unwrap();
    assert_eq!(sessions[0].history.last().unwrap().content, "你好");
}

#[tokio::test]
async fn stream_disabled_chat_completes_without_synthetic_delta() {
    let provider = TestProvider::replying("非流完整回复");
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) =
        service.respond(private_request("hello")).await.unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("非流完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
}

#[test]
fn output_policy_names_are_consistent_across_stream_plans() {
    let cases = [
        (
            RespondPlan::StreamingChat,
            true,
            CoreOutputPolicy::DirectStream,
            "direct_stream",
        ),
        (
            RespondPlan::StreamingChat,
            false,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::CommandEvent,
            true,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::WebSearch,
            true,
            CoreOutputPolicy::DirectStream,
            "direct_stream",
        ),
        (
            RespondPlan::WebSearch,
            false,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::CompleteToolLoop,
            true,
            CoreOutputPolicy::ProgressThenStream,
            "progress_then_stream",
        ),
        (
            RespondPlan::CompleteToolLoop,
            false,
            CoreOutputPolicy::ProgressThenComplete,
            "progress_then_complete",
        ),
    ];

    for (plan, provider_stream_enabled, expected_policy, expected_name) in cases {
        let policy = output_policy_for_stream(plan, provider_stream_enabled);
        assert_eq!(policy, expected_policy);
        assert_eq!(policy.as_str(), expected_name);
    }
}

#[test]
fn core_plan_routes_help_to_command_event_only() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();

    for input in ["/help", "/help rss", "/帮助"] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::CommandEvent,
            "{input}"
        );
    }

    let req: RespondRequest = private_request("/天气 杭州").into();
    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::Immediate
    );
}

#[tokio::test]
async fn core_help_command_is_wrapped_as_response_events() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);
    let mut stream = expect_stream(service.respond(private_request("/help")).await.unwrap());

    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandStarted);

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command finished status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandFinished);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed help response");
    };
    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.starts_with("女仆长助手"))
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_command_event_failure_does_not_send_finished_or_completed() {
    let provider = TestProvider::failing(LlmError::provider("compact unavailable", "provider"))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
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
    session_store
        .append_exchange(&mut session, "上一轮用户消息", "上一轮助手回复")
        .unwrap();
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("/compact").into();
    let mut stream = start_core_response_stream(
        service,
        req,
        RespondPlan::CommandEvent,
        CoreOutputPolicy::CompleteThenSend,
        false,
        Duration::from_secs(5),
        ProgressStatusConfig {
            hint: StatusHint::model(),
            audience: StatusAudience::Private,
            display_name: "小女仆".to_owned(),
        },
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandStarted);

    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(status) => {
                assert_ne!(status.kind, CoreResponseStatusKind::CommandFinished);
            }
            CoreResponseEvent::Failed(failure) => {
                assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
                assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
                return;
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response after command failure: {response:?}");
            }
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta in command event failure path: {delta}");
            }
        }
    }
    panic!("stream ended without failure");
}

#[tokio::test]
async fn wechat_service_chat_completes_without_direct_stream() {
    let provider = TestProvider::replying("微信完整回复").with_stream_enabled(true);
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) = service
        .respond(wechat_service_request("hello"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("微信完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
}

#[tokio::test]
async fn core_web_search_private_intent_uses_query_when_provider_stream_disabled() {
    let provider = TestProvider::replying("普通聊天不应调用");
    let query_executor = MockQueryExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("联网查询下今日 ai 新闻"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(
        response
            .markdown_content()
            .is_some_and(|text| text.starts_with("【联网查询】"))
    );
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("web answer: 联网查询下今日 ai 新闻"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "联网查询下今日 ai 新闻");
    assert_eq!(
        requests[0].raw_question.as_deref(),
        Some("/查 联网查询下今日 ai 新闻")
    );
}

#[tokio::test]
async fn core_web_search_wechat_sync_path_uses_query() {
    let provider = TestProvider::replying("普通聊天不应调用").with_stream_enabled(true);
    let query_executor = MockQueryExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(wechat_service_request("联网查询下今日 ai 新闻"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(
        response
            .markdown_content()
            .is_some_and(|text| text.starts_with("【联网查询】"))
    );
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("web answer: 联网查询下今日 ai 新闻"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "联网查询下今日 ai 新闻");
}

#[tokio::test]
async fn core_web_search_query_raw_question_includes_quoted_context() {
    let provider = TestProvider::replying("普通聊天不应调用");
    let query_executor = MockQueryExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);
    let mut request = private_request("联网查询下这件事的最新进展");
    request.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        text_summary: Some("Rust 1.90 发布候选版已经开放测试".to_owned()),
        ..QuotedMessageContext::default()
    });

    let mut stream = expect_stream(service.respond(request).await.unwrap());
    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "联网查询下这件事的最新进展");
    let raw_question = requests[0].raw_question.as_deref().unwrap();
    assert!(raw_question.contains("/查 联网查询下这件事的最新进展"));
    assert!(raw_question.contains("引用消息上下文"));
    assert!(raw_question.contains("引用文本：Rust 1.90 发布候选版已经开放测试"));
}

#[tokio::test]
async fn core_private_weather_chat_with_tool_capability_completes_without_synthetic_delta() {
    let provider = TestProvider::replying("工具完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );
    assert_eq!(
        stream.output_policy(),
        CoreOutputPolicy::ProgressThenComplete
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::ToolLoopStarted);
    assert_eq!(status.text, "小女仆正在查天气…");

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool call started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::ToolCallStarted);
    assert_eq!(status.text, "小女仆正在查天气…");

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool call finished status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::ToolCallFinished);
    assert_eq!(status.text, "小女仆正在确认结果…");

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.text_content(), Some("工具完整回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_private_tool_status_uses_configured_display_name() {
    let provider = TestProvider::replying("工具完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let mut state = test_state_with_tool_calling(provider.clone(), 5, true);
    state.config.status_display_name = "助手".to_owned();
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };

    assert_eq!(status.kind, CoreResponseStatusKind::ToolLoopStarted);
    assert_eq!(status.text, "助手正在查天气…");
}

#[tokio::test]
async fn core_group_tool_status_uses_short_hint() {
    let provider = TestProvider::replying("群聊工具回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider.clone(), 5, true, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(group_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };

    assert_eq!(status.kind, CoreResponseStatusKind::ToolLoopStarted);
    assert_eq!(status.text, "正在查…");
}

#[tokio::test]
async fn core_tool_loop_streams_only_final_answer_after_tool_status() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("最终".to_owned())),
        Ok(LlmStreamEvent::TextDelta("回答".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ])
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("帮我新增待办"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::ProgressThenStream);

    let mut status_kinds = Vec::new();
    let mut deltas = Vec::new();
    let response = loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before completed response");
        };
        match event {
            CoreResponseEvent::Status(status) => status_kinds.push(status.kind),
            CoreResponseEvent::Completed(response) => break response,
            CoreResponseEvent::TextDelta(delta) => deltas.push(delta),
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    };

    assert_eq!(deltas, vec!["最终".to_owned(), "回答".to_owned()]);
    assert_eq!(response.text_content(), Some("最终回答"));
    assert!(status_kinds.contains(&CoreResponseStatusKind::ToolLoopStarted));
    assert!(status_kinds.contains(&CoreResponseStatusKind::ToolLoopFinalizing));
    assert!(
        status_kinds
            .iter()
            .position(|kind| *kind == CoreResponseStatusKind::ToolLoopFinalizing)
            .is_some_and(|index| index > 0)
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_loop_stream_failure_after_delta_does_not_complete_or_replay() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("部分回答".to_owned())),
        Err(LlmError::provider(
            "stream failed after visible delta",
            "stream_after_delta",
        )),
    ])
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("帮我查一下天气"))
            .await
            .unwrap(),
    );

    let mut saw_delta = false;
    loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before failure");
        };
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::TextDelta(delta) => {
                assert_eq!(delta, "部分回答");
                saw_delta = true;
            }
            CoreResponseEvent::Failed(failure) => {
                assert!(saw_delta);
                assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
                break;
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response after partial delta: {response:?}");
            }
        }
    }
}

#[tokio::test]
async fn core_tool_loop_failure_is_reported_as_stream_failure_without_delta() {
    let provider = TestProvider::failing(LlmError::new(
        "tool_loop_limit",
        "tool loop exceeded maximum rounds",
        "tool_loop",
    ))
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Stream(mut stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };
    let failure = collect_failure_without_text_delta(&mut stream).await;

    assert_eq!(failure.kind, CoreFailureKind::Internal);
    assert!(!failure.retryable);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_loop_stream_preserves_request_timeout_for_background_complete_path() {
    let provider = TestProvider::delayed("迟到回复", Duration::from_millis(80))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 0, true);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };

    let failure = collect_failure_without_text_delta(&mut stream).await;

    assert_eq!(failure.kind, CoreFailureKind::LlmTimeout);
    assert!(failure.retryable);
}

#[tokio::test]
async fn core_private_simple_todo_queries_use_deterministic_path() {
    let provider = TestProvider::replying("不应调用模型")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let owner = TodoStore::owner(Some("u1"), private_scope());
    let todo_store = state.stores.todo_store.clone();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待查看项目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let service = CoreHandle::new(state);

    let mut responses = Vec::new();
    for input in ["看一下待办", "看一下代办"] {
        let output = service.respond(private_request(input)).await.unwrap();
        let CoreRespondOutput::Complete(response) = output else {
            panic!("expected complete output for deterministic todo query");
        };
        let response = *response;
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(response.text_content().unwrap().contains("待查看项目"));
        responses.push(response.text_content().map(str::to_owned));
    }

    assert_eq!(responses[0], responses[1]);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_private_general_chat_with_tool_capability_uses_streaming_chat() {
    let provider = TestProvider::replying("普通完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("晚上好")).await).await;

    assert_eq!(response.text_content(), Some("普通完整回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_group_chat_keeps_stream_path_even_when_tool_capable() {
    let provider = TestProvider::replying("群聊普通回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response =
        collect_stream_completed(service.respond(group_request("群里问天气")).await).await;

    assert_eq!(response.text_content(), Some("群聊普通回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_slash_command_does_not_enter_tool_loop() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let output = service
        .respond(private_request("/天气 杭州"))
        .await
        .unwrap();

    assert_eq!(output.output_policy(), CoreOutputPolicy::CompleteThenSend);
    assert!(matches!(output, CoreRespondOutput::Complete(_)));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_calling_disabled_keeps_plain_stream_path() {
    let provider = TestProvider::replying("普通流式回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, false);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("普通流式回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_unsupported_provider_capability_keeps_plain_stream_path() {
    let provider = TestProvider::replying("未适配 provider 普通回复");
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("未适配 provider 普通回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
