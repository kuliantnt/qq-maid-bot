use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use qq_maid_llm::{
    provider::{
        AgentRunDiagnostics, AgentStopReason, ChatOutcome, LlmProvider, ToolCallingProtocol,
        ToolChatRequest, ToolExecutionResult,
        types::{ChatRequest, ChatRole, TokenUsage},
    },
    web_search::{WebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource},
};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::super::{
    RespondExecutors, RespondRequest, RespondServiceOptions, RespondStores, RustRespondService,
    common::empty_respond_request,
};
use crate::{
    config::DEFAULT_RSS_SUMMARY_MAX_CHARS,
    error::LlmError,
    runtime::{
        knowledge::KnowledgeIndex,
        memory::{CreateScopedMemoryRequest, MemoryScopeType, MemoryStore},
        pending::PendingOperation,
        prompt::PromptConfig,
        session::{LastTodoQuery, SessionMeta, SessionStore},
        tools::rss::{RssFetchConfig, RssFetcher, RssStore},
        tools::{
            ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexRadarSummary,
            RadarExecutor, RadarSnapshot, RadarTarget,
            todo::{
                TodoItem, TodoItemDraft, TodoOwner, TodoPendingOperation, TodoStatus, TodoStore,
                TodoTimePrecision,
            },
        },
        tools::{
            train::{TrainExecutor, TrainSchedule, TrainScheduleRequest, TrainStop},
            weather::{
                AirQualitySummary, CurrentWeather, DailyWeather, WeatherAlert, WeatherExecutor,
                WeatherLifeIndex, WeatherLocation, WeatherOutcome, WeatherRequest,
                WeatherSupplement,
            },
        },
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
    util::metrics::LlmMetrics,
};
use qq_maid_common::time_context::request_time_context;

mod executors;
mod fixtures;
mod mock_provider;
mod mock_replies;
mod service;

pub(crate) use executors::*;
pub(crate) use fixtures::*;
pub(crate) use mock_provider::*;
pub(crate) use mock_replies::*;
pub(crate) use service::*;
