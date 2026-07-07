use super::*;
use std::{sync::atomic::Ordering, time::Duration};

use qq_maid_common::identity_context::IdentitySource;

use crate::{
    error::LlmError,
    provider::{LlmStreamEvent, ToolCallingProtocol},
    runtime::{
        pending::PendingOperation,
        respond::{RespondPlan, RespondRequest, RespondResponse},
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
        tools_visible_snapshot: None,
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
        tools_visible_snapshot: None,
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
        tools_visible_snapshot: None,
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
        tools_visible_snapshot: None,
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

    assert_eq!(response.text.as_deref(), Some("text"));
    assert_eq!(response.markdown.as_deref(), Some("**text**"));
    assert_eq!(response.handled, Some(true));
    assert_eq!(response.session_id.as_deref(), Some("session-1"));
    assert_eq!(response.command.as_deref(), Some("chat"));
    assert_eq!(response.diagnostics.unwrap()["k"], "v");
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

    assert_eq!(response.text.as_deref(), Some("late"));
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

    assert_eq!(response.text.as_deref(), Some("你好"));
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

    assert_eq!(response.text.as_deref(), Some("非流完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
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

    assert_eq!(response.text.as_deref(), Some("微信完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
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

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.text.as_deref(), Some("工具完整回复"));
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
async fn core_tool_loop_completes_only_after_final_answer_is_trusted() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("候选草稿".to_owned())),
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
    assert_eq!(
        stream.output_policy(),
        CoreOutputPolicy::ProgressThenComplete
    );

    let mut status_kinds = Vec::new();
    let response = loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before completed response");
        };
        match event {
            CoreResponseEvent::Status(status) => status_kinds.push(status.kind),
            CoreResponseEvent::Completed(response) => break response,
            CoreResponseEvent::TextDelta(delta) => {
                panic!("tool loop must not expose text delta before final answer: {delta}");
            }
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    };

    assert_eq!(response.text.as_deref(), Some("候选草稿"));
    assert!(status_kinds.contains(&CoreResponseStatusKind::ToolLoopStarted));
    assert!(status_kinds.contains(&CoreResponseStatusKind::ToolLoopFinalizing));
    // Tool Loop 事件流来自完整 Tool Loop 的最终结果，不消费 provider token 流，
    // 因而不会提前外发任何模型中间文本或工具过程。
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
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
        assert!(response.text.as_deref().unwrap().contains("待查看项目"));
        responses.push(response.text);
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

    assert_eq!(response.text.as_deref(), Some("普通完整回复"));
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

    assert_eq!(response.text.as_deref(), Some("群聊普通回复"));
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

    assert_eq!(response.text.as_deref(), Some("普通流式回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_unsupported_provider_capability_keeps_plain_stream_path() {
    let provider = TestProvider::replying("未适配 provider 普通回复");
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text.as_deref(), Some("未适配 provider 普通回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
