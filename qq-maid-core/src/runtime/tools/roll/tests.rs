//! Roll domain 执行与命令兼容回归测试。

use std::sync::{Arc, Mutex};

use crate::{error::LlmError, util::metrics::LlmMetrics};
use async_trait::async_trait;
use qq_maid_llm::provider::{
    ChatOutcome, LlmProvider,
    types::{ChatRequest, TokenUsage},
};

use super::*;

mod dm_success;
mod fallback;
mod local;
mod parsing;

#[derive(Clone)]
struct MockProvider {
    result: Result<String, LlmError>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    delay: Duration,
}

impl MockProvider {
    fn replying(reply: &str) -> Self {
        Self {
            result: Ok(reply.to_owned()),
            requests: Arc::new(Mutex::new(Vec::new())),
            events: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::ZERO,
        }
    }

    fn failing(error: LlmError) -> Self {
        Self {
            result: Err(error),
            requests: Arc::new(Mutex::new(Vec::new())),
            events: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::ZERO,
        }
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.events.lock().unwrap().push("model");
        self.requests.lock().unwrap().push(req.clone());
        tokio::time::sleep(self.delay).await;
        let reply = self.result.clone()?;
        Ok(ChatOutcome {
            reply,
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
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
            agent: Default::default(),
        })
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

fn fortune_json() -> &'static str {
    r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"今晚适合出门","failure_meaning":"今晚适合宅家"}"#
}

fn dice_expression(input: &str) -> DiceExpression {
    match dice::parse_expression(input) {
        DiceExpressionParse::Parsed(expression) => expression,
        other => panic!("expected dice expression {input}, got {other:?}"),
    }
}
