//! LLM 调用指标采集模块。
//!
//! 提供指标记录器 MetricsRecorder，用于测量 LLM API 调用的
//! 首包时间（TTFE）、首 Token 时间（TTFT）和总延迟。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// LLM 调用的一次完整指标记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmMetrics {
    pub provider: String,
    pub model: String,
    pub stream: bool,
    pub ttfe_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub total_latency_ms: u64,
}

/// 指标记录器，测量 LLM 调用的延迟指标。
///
/// 记录起始时间，并在调用过程中标记首个 SSE 事件和首个 Token 的时间点，
/// 最终计算出 TTFE、TTFT 和总延迟。
#[derive(Debug)]
pub struct MetricsRecorder {
    start: Instant,
    first_event_at: Option<Instant>,
    first_token_at: Option<Instant>,
}

impl MetricsRecorder {
    /// 开始计时，返回新的记录器。
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
            first_event_at: None,
            first_token_at: None,
        }
    }

    /// 标记收到第一个 SSE 事件（用于计算 TTFE）。
    pub fn mark_event(&mut self) {
        if self.first_event_at.is_none() {
            self.first_event_at = Some(Instant::now());
        }
    }

    /// 标记收到第一个 Token（用于计算 TTFT）。
    pub fn mark_token(&mut self) {
        if self.first_token_at.is_none() {
            self.first_token_at = Some(Instant::now());
        }
    }

    /// 完成记录，返回最终指标（适用于成功调用）。
    pub fn finish(self, provider: &str, model: &str, stream: bool) -> LlmMetrics {
        let total = self.start.elapsed();
        LlmMetrics {
            provider: provider.to_owned(),
            model: model.to_owned(),
            stream,
            ttfe_ms: self
                .first_event_at
                .map(|instant| duration_ms(instant.duration_since(self.start))),
            ttft_ms: self
                .first_token_at
                .map(|instant| duration_ms(instant.duration_since(self.start))),
            total_latency_ms: duration_ms(total),
        }
    }

    /// 完成记录（调用失败时使用），返回已积累的指标。
    pub fn fail(&self, provider: &str, model: &str, stream: bool) -> LlmMetrics {
        LlmMetrics {
            provider: provider.to_owned(),
            model: model.to_owned(),
            stream,
            ttfe_ms: self
                .first_event_at
                .map(|instant| duration_ms(instant.duration_since(self.start))),
            ttft_ms: self
                .first_token_at
                .map(|instant| duration_ms(instant.duration_since(self.start))),
            total_latency_ms: duration_ms(self.start.elapsed()),
        }
    }
}

/// 将 Duration 转换为毫秒数。
pub fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_ms_converts_duration() {
        assert_eq!(duration_ms(Duration::from_millis(123)), 123);
    }

    #[test]
    fn fail_metrics_include_provider_and_model() {
        let recorder = MetricsRecorder::start();
        let metrics = recorder.fail("openai", "gpt-5-mini", true);
        assert_eq!(metrics.provider, "openai");
        assert_eq!(metrics.model, "gpt-5-mini");
        assert!(metrics.stream);
    }
}
