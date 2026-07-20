use super::*;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use qq_maid_common::{
    identity_context::{ConversationKind, IdentitySource},
    input_part::QuotedMessageContext,
    output_part::{AssistantOutput, OutputMedia, OutputPart},
};
use qq_maid_llm::provider::{LlmStreamEvent, ToolCallingProtocol};
use tokio::sync::Notify;

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            PlannedRespond, RespondPlan, RespondRequest, RespondResponse, StatusAudience,
            StatusHint,
        },
        session::SessionMeta,
        tools::todo::{TodoItemDraft, TodoPendingPayload, TodoStore, TodoTimePrecision},
    },
    util::metrics::LlmMetrics,
};

mod support;
use support::*;

struct BlockingWeatherExecutor {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::runtime::tools::weather::WeatherExecutor for BlockingWeatherExecutor {
    async fn weather(
        &self,
        _req: crate::runtime::tools::weather::WeatherRequest,
    ) -> Result<crate::runtime::tools::weather::WeatherOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        Err(LlmError::new(
            "weather_failed",
            "controlled weather result",
            "weather",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "blocking-weather"
    }
}

mod agent;
mod commands;
mod planning;
mod request;
mod response;
mod streaming;
mod web_search;
