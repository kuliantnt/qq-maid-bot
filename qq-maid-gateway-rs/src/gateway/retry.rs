//! QQ Gateway 地址获取阶段的错误退避与可取消重试。

use std::{future::Future, time::Duration};

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::protocol::FetchGatewayUrlError;

const BACKOFF_SECONDS: [u64; 4] = [5, 10, 30, 60];
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(super) struct GatewayFetchBackoff {
    consecutive_failures: u32,
}

impl GatewayFetchBackoff {
    pub(super) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub(super) fn record_failure(&mut self, jitter_percent: i16) -> Duration {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let index = self
            .consecutive_failures
            .saturating_sub(1)
            .min((BACKOFF_SECONDS.len() - 1) as u32) as usize;
        let base_millis = Duration::from_secs(BACKOFF_SECONDS[index]).as_millis() as u64;
        let factor = (100_i64 + i64::from(jitter_percent.clamp(-20, 20))) as u64;
        // 任务约束中的 60 秒是实际等待上限；因此 jitter 后再次封顶，第四档落在 48～60 秒。
        Duration::from_millis(base_millis.saturating_mul(factor) / 100).min(MAX_BACKOFF)
    }

    pub(super) fn reset(&mut self) -> u32 {
        let previous = self.consecutive_failures;
        self.consecutive_failures = 0;
        previous
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GatewayFetchOutcome {
    Url(String),
    Shutdown,
}

/// 将远端瞬时故障留在 Gateway 生命周期内处理，同时让 shutdown 能打断请求或退避等待。
pub(super) async fn fetch_gateway_url_with_retry<F, Fut, J>(
    shutdown_token: &CancellationToken,
    backoff: &mut GatewayFetchBackoff,
    mut fetch: F,
    mut jitter_percent: J,
) -> Result<GatewayFetchOutcome, FetchGatewayUrlError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<String, FetchGatewayUrlError>>,
    J: FnMut() -> i16,
{
    loop {
        let result = tokio::select! {
            _ = shutdown_token.cancelled() => return Ok(GatewayFetchOutcome::Shutdown),
            result = fetch() => result,
        };

        match result {
            Ok(url) => {
                let recovered_failures = backoff.reset();
                if recovered_failures > 0 {
                    info!(
                        recovered_failures,
                        "QQ gateway url fetch recovered after transient failures"
                    );
                }
                return Ok(GatewayFetchOutcome::Url(url));
            }
            Err(error) if error.is_retryable() => {
                let delay = backoff.record_failure(jitter_percent());
                warn!(
                    error = %error,
                    consecutive_failures = backoff.consecutive_failures(),
                    retry_after_ms = delay.as_millis(),
                    retryable = true,
                    "failed to fetch QQ gateway url; retrying"
                );
                tokio::select! {
                    _ = shutdown_token.cancelled() => return Ok(GatewayFetchOutcome::Shutdown),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(error) => {
                warn!(
                    error = %error,
                    consecutive_failures = backoff.consecutive_failures().saturating_add(1),
                    retryable = false,
                    "failed to fetch QQ gateway url; not retrying"
                );
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use reqwest::StatusCode;

    use super::*;

    #[test]
    fn backoff_sequence_is_capped_and_reset_after_success() {
        let mut backoff = GatewayFetchBackoff::default();
        let delays = (0..6)
            .map(|_| backoff.record_failure(0))
            .collect::<Vec<_>>();

        assert_eq!(delays, [5, 10, 30, 60, 60, 60].map(Duration::from_secs));
        assert_eq!(backoff.record_failure(20), Duration::from_secs(60));
        assert_eq!(backoff.reset(), 7);
        assert_eq!(backoff.consecutive_failures(), 0);
        assert_eq!(backoff.record_failure(0), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn gateway_500_is_retried_then_success_resets_backoff() {
        let shutdown = CancellationToken::new();
        let mut results = VecDeque::from([
            Err(FetchGatewayUrlError::Status {
                status: StatusCode::INTERNAL_SERVER_ERROR,
            }),
            Ok("wss://gateway.example.test".to_owned()),
        ]);
        let handle = tokio::spawn(async move {
            let mut backoff = GatewayFetchBackoff::default();
            let outcome = fetch_gateway_url_with_retry(
                &shutdown,
                &mut backoff,
                || std::future::ready(results.pop_front().expect("test result")),
                || 0,
            )
            .await;
            (outcome, backoff)
        });

        tokio::time::advance(Duration::from_secs(5)).await;
        let (outcome, backoff) = handle.await.unwrap();
        assert_eq!(
            outcome.unwrap(),
            GatewayFetchOutcome::Url("wss://gateway.example.test".to_owned())
        );
        assert_eq!(backoff.consecutive_failures(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn gateway_network_error_is_retried_then_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let request_error = qq_maid_common::http_client::client()
            .get(format!("http://{address}/gateway"))
            .send()
            .await
            .unwrap_err();
        let shutdown = CancellationToken::new();
        let mut results = VecDeque::from([
            Err(FetchGatewayUrlError::Request(request_error)),
            Ok("wss://gateway.example.test".to_owned()),
        ]);
        let handle = tokio::spawn(async move {
            let mut backoff = GatewayFetchBackoff::default();
            fetch_gateway_url_with_retry(
                &shutdown,
                &mut backoff,
                || std::future::ready(results.pop_front().expect("test result")),
                || 0,
            )
            .await
        });

        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(
            handle.await.unwrap().unwrap(),
            GatewayFetchOutcome::Url(_)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_interrupts_backoff_without_another_fetch() {
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        let fetches = Arc::new(AtomicUsize::new(0));
        let fetch_count = fetches.clone();
        let handle = tokio::spawn(async move {
            let mut backoff = GatewayFetchBackoff::default();
            fetch_gateway_url_with_retry(
                &shutdown,
                &mut backoff,
                || {
                    fetch_count.fetch_add(1, Ordering::SeqCst);
                    std::future::ready(Err(FetchGatewayUrlError::Status {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                    }))
                },
                || 0,
            )
            .await
        });
        tokio::task::yield_now().await;

        cancel.cancel();

        assert_eq!(
            handle.await.unwrap().unwrap(),
            GatewayFetchOutcome::Shutdown
        );
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn ordinary_4xx_returns_without_retrying() {
        let shutdown = CancellationToken::new();
        let fetches = Arc::new(AtomicUsize::new(0));
        let fetch_count = fetches.clone();
        let mut backoff = GatewayFetchBackoff::default();

        let error = fetch_gateway_url_with_retry(
            &shutdown,
            &mut backoff,
            || {
                fetch_count.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(FetchGatewayUrlError::Status {
                    status: StatusCode::UNAUTHORIZED,
                }))
            },
            || 0,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            FetchGatewayUrlError::Status {
                status: StatusCode::UNAUTHORIZED
            }
        ));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }
}
