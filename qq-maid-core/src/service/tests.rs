use super::*;
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    config::{
        AppConfig, DEFAULT_BIGMODEL_BASE_URL, DEFAULT_DEEPSEEK_BASE_URL,
        DEFAULT_RSS_SUMMARY_MAX_CHARS, DailyReminderTime, OpenAiApiMode, ProviderMode,
    },
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol, ToolChatRequest,
        status::{UpstreamStatus, observe_provider},
        types::{ChatRequest, ModelRoute, TokenUsage},
    },
    runtime::{
        knowledge::KnowledgeIndex,
        pending::PendingOperation,
        prompt::PromptConfig,
        query::{QueryExecutor, QueryOutcome, QueryRequest},
        respond::{RespondPlan, RespondRequest, RespondResponse},
        rss::{RssFetchConfig, RssFetcher, RssStore},
        session::{SessionMeta, SessionStore},
        todo::{TodoItemDraft, TodoStore, TodoTimePrecision},
        tools::{RadarExecutor, RadarSnapshot, RadarTarget},
        train::{TrainExecutor, TrainSchedule, TrainScheduleRequest},
        weather::{WeatherExecutor, WeatherOutcome, WeatherRequest},
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
    util::metrics::LlmMetrics,
};

#[test]
fn private_conversation_derives_private_scope() {
    let req = CoreRequest {
        text: "hello".to_owned(),
        input_parts: Vec::new(),
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
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
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: None,
            group_member_role: None,
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
    let session_store = state.session_store.clone();
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
    let session_store = state.session_store.clone();
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
    let session_store = state.session_store.clone();
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
    let session_store = state.session_store.clone();
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
    let todo_store = state.todo_store.clone();
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

async fn collect_stream_failure(
    output: Result<CoreRespondOutput, CoreError>,
) -> CoreRespondFailure {
    let CoreRespondOutput::Stream(mut stream) = output.unwrap() else {
        panic!("expected stream output");
    };
    collect_failure_without_text_delta(&mut stream).await
}

async fn collect_failure_without_text_delta(stream: &mut CoreResponseStream) -> CoreRespondFailure {
    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::Failed(failure) => return failure,
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta before failure: {delta}");
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response before failure: {response:?}");
            }
        }
    }
    panic!("stream ended without failure");
}

async fn collect_stream_completed(output: Result<CoreRespondOutput, CoreError>) -> CoreResponse {
    let mut stream = expect_stream(output.unwrap());
    while let Some(event) = stream.recv().await {
        if let CoreResponseEvent::Completed(response) = event {
            return response;
        }
    }
    panic!("stream ended without completed response");
}

async fn collect_completed_without_text_delta(stream: &mut CoreResponseStream) -> CoreResponse {
    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::Completed(response) => return response,
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta before completed response: {delta}");
            }
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    }
    panic!("stream ended without completed response");
}

fn expect_stream(output: CoreRespondOutput) -> CoreResponseStream {
    let CoreRespondOutput::Stream(stream) = output else {
        panic!("expected stream output");
    };
    stream
}

#[derive(Clone)]
enum ProviderBehavior {
    Reply(String),
    Stream(Vec<Result<LlmStreamEvent, LlmError>>),
    Error(LlmError),
    Delayed { reply: String, delay: Duration },
}

#[derive(Clone)]
struct TestProvider {
    behavior: ProviderBehavior,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    calls: Arc<AtomicUsize>,
    tool_calls: Arc<AtomicUsize>,
    tool_protocol: Option<ToolCallingProtocol>,
    stream_enabled: bool,
}

impl TestProvider {
    fn replying(reply: &str) -> Self {
        Self::new(ProviderBehavior::Reply(reply.to_owned()))
    }

    fn failing(error: LlmError) -> Self {
        Self::new(ProviderBehavior::Error(error))
    }

    fn streaming(events: Vec<Result<LlmStreamEvent, LlmError>>) -> Self {
        Self::new(ProviderBehavior::Stream(events)).with_stream_enabled(true)
    }

    fn delayed(reply: &str, delay: Duration) -> Self {
        Self::new(ProviderBehavior::Delayed {
            reply: reply.to_owned(),
            delay,
        })
    }

    fn new(behavior: ProviderBehavior) -> Self {
        Self {
            behavior,
            requests: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            tool_calls: Arc::new(AtomicUsize::new(0)),
            tool_protocol: None,
            stream_enabled: false,
        }
    }

    fn with_stream_enabled(mut self, enabled: bool) -> Self {
        self.stream_enabled = enabled;
        self
    }

    fn with_tool_protocol(mut self, protocol: ToolCallingProtocol) -> Self {
        self.tool_protocol = Some(protocol);
        self
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for TestProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
            ProviderBehavior::Stream(events) => {
                let reply = events
                    .iter()
                    .filter_map(|event| match event {
                        Ok(LlmStreamEvent::TextDelta(delta)) => Some(delta.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                Ok(chat_outcome(&reply))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(reply))
            }
        }
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(Box::pin(futures::stream::iter(vec![
                Ok(LlmStreamEvent::TextDelta(reply.clone())),
                Ok(LlmStreamEvent::Completed {
                    usage: None,
                    finish_reason: None,
                    fallback_used: false,
                }),
            ]))),
            ProviderBehavior::Stream(events) => {
                Ok(Box::pin(futures::stream::iter(events.to_vec())))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                let reply = reply.clone();
                let delay = *delay;
                Ok(Box::pin(futures::stream::unfold(
                    (0_u8, reply, delay),
                    |(state, reply, delay)| async move {
                        if state == 0 {
                            tokio::time::sleep(delay).await;
                            return Some((
                                Ok(LlmStreamEvent::TextDelta(reply)),
                                (1, String::new(), delay),
                            ));
                        }
                        if state == 1 {
                            return Some((
                                Ok(LlmStreamEvent::Completed {
                                    usage: None,
                                    finish_reason: None,
                                    fallback_used: false,
                                }),
                                (2, String::new(), delay),
                            ));
                        }
                        None
                    },
                )))
            }
        }
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.tool_protocol
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.chat);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
            ProviderBehavior::Stream(events) => {
                let reply = events
                    .iter()
                    .filter_map(|event| match event {
                        Ok(LlmStreamEvent::TextDelta(delta)) => Some(delta.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                Ok(chat_outcome(&reply))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(reply))
            }
        }
    }

    fn name(&self) -> &str {
        "test-provider"
    }

    fn model(&self) -> &str {
        "test-model"
    }

    fn stream_enabled(&self) -> bool {
        self.stream_enabled
    }
}

struct EmptyQueryExecutor;

#[async_trait::async_trait]
impl QueryExecutor for EmptyQueryExecutor {
    async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Err(LlmError::provider("query unused", "query"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-query"
    }
}

struct EmptyWeatherExecutor;

#[async_trait::async_trait]
impl WeatherExecutor for EmptyWeatherExecutor {
    async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        Err(LlmError::provider("weather unused", "weather"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-weather"
    }
}

struct EmptyTrainExecutor;

#[async_trait::async_trait]
impl TrainExecutor for EmptyTrainExecutor {
    async fn query_train_schedule(
        &self,
        _req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        Err(LlmError::provider("train unused", "train"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-train"
    }
}

struct EmptyRadarExecutor;

#[async_trait::async_trait]
impl RadarExecutor for EmptyRadarExecutor {
    async fn radar(&self, _target: RadarTarget) -> Result<RadarSnapshot, LlmError> {
        Err(LlmError::provider("radar unused", "radar"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-radar"
    }
}

fn private_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    }
}

fn private_scope() -> &'static str {
    "platform:qq_official:account:app-1:private:u1"
}

fn group_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    }
}

fn wechat_service_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        platform: Platform::WechatService,
        account_id: Some("gh-service".to_owned()),
        actor: CoreActor {
            user_id: Some("openid-u1".to_owned()),
            group_member_role: None,
        },
        conversation: CoreConversation::ServiceAccount {
            account_id: Some("gh_test".to_owned()),
            peer_id: "openid-u1".to_owned(),
        },
    }
}

fn chat_outcome(reply: &str) -> ChatOutcome {
    ChatOutcome {
        reply: reply.to_owned(),
        metrics: LlmMetrics {
            provider: "test-provider".to_owned(),
            model: "test-model".to_owned(),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 1,
        },
        usage: Some(TokenUsage {
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            total_tokens: None,
        }),
        fallback_used: false,
        executed_tools: Vec::new(),
        tool_results: Vec::new(),
    }
}

fn test_state(
    provider: TestProvider,
    request_timeout_seconds: u64,
) -> crate::http::routes::AppState {
    test_state_with_tool_calling(provider, request_timeout_seconds, false)
}

fn test_state_with_tool_calling(
    provider: TestProvider,
    request_timeout_seconds: u64,
    tool_calling_enabled: bool,
) -> crate::http::routes::AppState {
    test_state_with_group_tool_calling(
        provider,
        request_timeout_seconds,
        tool_calling_enabled,
        false,
    )
}

fn test_state_with_group_tool_calling(
    provider: TestProvider,
    request_timeout_seconds: u64,
    tool_calling_enabled: bool,
    tool_calling_group_enabled: bool,
) -> crate::http::routes::AppState {
    let base_dir = std::env::temp_dir().join(format!(
        "qq-maid-core-service-test-{}",
        uuid::Uuid::new_v4()
    ));
    let prompt_dir = base_dir.join("prompts");
    fs::create_dir_all(&prompt_dir).unwrap();
    for file_name in crate::runtime::prompt::PROMPT_FILES {
        fs::write(prompt_dir.join(file_name), format!("{file_name} content")).unwrap();
    }
    let app_db_file = base_dir.join("app.db");
    let database = SqliteDatabase::open(&app_db_file, APP_MIGRATIONS).unwrap();
    let knowledge_dir = base_dir.join("knowledge");
    let knowledge_index =
        KnowledgeIndex::new(KnowledgeStore::new(database.clone()), &knowledge_dir);
    knowledge_index.sync().unwrap();
    let upstream_status = UpstreamStatus::default();

    crate::http::routes::AppState {
        config: AppConfig {
            provider: ProviderMode::OpenAi,
            model: "test-model".to_owned(),
            model_route: ModelRoute::parse_config("test-model", "LLM_MODEL").unwrap(),
            agent_config: crate::config::AgentRuntimeConfig::from_legacy(
                crate::config::LegacyAgentConfig {
                    main_model: "test-model".to_owned(),
                    max_output_tokens: 1200,
                    openai_search_model: "test-search".to_owned(),
                    tool_calling_enabled,
                    group_tool_calling_enabled: tool_calling_group_enabled,
                    tool_calling_max_rounds: 3,
                    group_llm_model: None,
                    private_llm_model: None,
                    group_openai_search_model: None,
                    private_openai_search_model: None,
                },
            )
            .unwrap(),
            title_model: None,
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
            openai_search_model: "test-search".to_owned(),
            openai_api_key: Some("test".to_owned()),
            openai_base_url: None,
            openai_api_mode: OpenAiApiMode::Auto,
            deepseek_api_key: None,
            deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
            deepseek_model: "deepseek-chat".to_owned(),
            bigmodel_api_key: None,
            bigmodel_base_url: DEFAULT_BIGMODEL_BASE_URL.to_owned(),
            bigmodel_model: "glm-5.2".to_owned(),
            stream: false,
            request_timeout_seconds,
            ttft_warn_seconds: 30,
            media_max_bytes: crate::config::DEFAULT_MEDIA_MAX_BYTES,
            max_output_tokens: 1200,
            max_concurrent_responses: 4,
            tool_calling_enabled,
            tool_calling_group_enabled,
            tool_calling_max_rounds: 3,
            context_budget: qq_maid_llm::context_budget::ContextBudgetConfig {
                context_window_chars: crate::config::DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
                output_reserve_chars: crate::config::DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS
                    as usize,
                protected_recent_turns: crate::config::DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS
                    as usize,
            },
            tool_result_max_chars: crate::config::DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT as usize,
            status_display_name: crate::config::DEFAULT_STATUS_DISPLAY_NAME.to_owned(),
            server_host: "127.0.0.1".to_owned(),
            server_port: 8787,
            app_db_file: app_db_file.to_string_lossy().into_owned(),
            rss_enabled: false,
            rss_poll_interval_seconds: 300,
            rss_http_timeout_seconds: 15,
            rss_max_body_bytes: 2 * 1024 * 1024,
            rss_max_push_per_feed: 3,
            rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS,
            rss_seen_retention: 500,
            rss_push_max_failures: 3,
            rss_push_message_type: "markdown".to_owned(),
            todo_daily_reminder_enabled: false,
            todo_daily_reminder_time: DailyReminderTime { hour: 9, minute: 0 },
            rss_allow_private_urls: true,
            prompt_dir: prompt_dir.to_string_lossy().into_owned(),
            prompt_dir_uses_builtin_defaults: false,
            knowledge_dir: knowledge_dir.to_string_lossy().into_owned(),
            qweather_api_key: "test".to_owned(),
            qweather_api_host: "https://api.qweather.com".to_owned(),
            qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
            web_console_enabled: false,
            web_console_allowed_origins: Vec::new(),
        },
        provider: observe_provider(Arc::new(provider), upstream_status.clone()),
        upstream_status,
        query_executor: Arc::new(EmptyQueryExecutor),
        weather_executor: Arc::new(EmptyWeatherExecutor),
        train_executor: Arc::new(EmptyTrainExecutor),
        radar_executor: Arc::new(EmptyRadarExecutor),
        memory_store: crate::runtime::memory::MemoryStore::new(database.clone()),
        session_store: SessionStore::new(database.clone()),
        todo_store: TodoStore::new(database.clone()),
        notification_store: crate::storage::notification::NotificationOutboxStore::new(
            database.clone(),
        ),
        rss_store: RssStore::new(database),
        rss_fetcher: RssFetcher::new(RssFetchConfig {
            allow_private_networks: true,
            ..RssFetchConfig::default()
        })
        .unwrap(),
        knowledge_index,
        prompt_config: PromptConfig::new(prompt_dir),
    }
}
