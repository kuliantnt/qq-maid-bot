//! QQ gateway 运行域。负责 WebSocket 主循环、事件分发、去重、诊断与回发编排。

mod aggregator;
mod bot_identity;
mod c2c;
mod cache;
pub mod console;
pub mod dedupe;
mod dispatcher;
pub mod event;
mod group;
mod group_filter;
pub mod logging;
mod media_fetch;
pub(crate) mod outbound;
pub mod ping;
pub(crate) mod platform;
mod protocol;
pub mod push;
mod ref_index;
mod retry;
mod stream;
mod typing;
mod wechat_service;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use aggregator::MessageAggregator;
use anyhow::Context;
use bot_identity::BotIdentity;
use dispatcher::MessageDispatcher;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use c2c::handle_c2c_message;
pub(crate) use cache::BotOutboundCache;
use dedupe::MessageDedupe;
use group::handle_group_message;
use group_filter::GroupCooldowns;
use ping::GatewayRuntimeStatus;
use protocol::ResumeState;
use push::GatewayPushSink;
use ref_index::{SharedRefIndex, ref_index};
use retry::{GatewayFetchBackoff, GatewayFetchOutcome, fetch_gateway_url_with_retry};

use crate::{
    api::QqApiClient,
    auth::AccessTokenManager,
    config::{AppConfig, QqOfficialBindingState},
    respond::RespondClient,
};

const DEDUPE_TTL: Duration = Duration::from_secs(10 * 60);

/// QQ 网关主循环：初始化所有共享组件后，反复获取网关地址并建立 WebSocket 连接。
/// 连接断开或失败后会等待 `RECONNECT_DELAY` 后重连，从而保证长期在线。
pub async fn run(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    runtime: GatewayRuntimeStatus,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    // 微信与 QQ 共用 Core，但生命周期相互独立；QQ 未绑定时也必须先启动微信监听。
    let dedupe = Arc::new(MessageDedupe::new(DEDUPE_TTL));
    let wechat_service_handle = if config.wechat_service.enabled {
        Some(
            wechat_service::spawn_callback_server(
                config.wechat_service.clone(),
                respond.clone(),
                dedupe.clone(),
                runtime.clone(),
                shutdown_token.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let ref_index = ref_index();
    let result = match config.qq_official_binding_state() {
        QqOfficialBindingState::Enabled => {
            run_qq_official(
                config,
                respond,
                push_sink,
                runtime,
                shutdown_token,
                dedupe,
                ref_index,
            )
            .await
        }
        QqOfficialBindingState::Unbound => {
            push_sink.mark_qq_official_unavailable("QQ official channel is not bound");
            info!(
                channel = "qq_official",
                state = "unbound",
                "skipping channel initialization"
            );
            shutdown_token.cancelled().await;
            Ok(())
        }
        QqOfficialBindingState::Disabled => {
            push_sink.mark_qq_official_unavailable("QQ official channel is disabled");
            info!(
                channel = "qq_official",
                state = "disabled",
                "skipping channel initialization"
            );
            shutdown_token.cancelled().await;
            Ok(())
        }
    };
    if let Some(handle) = wechat_service_handle {
        let _ = handle.await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_qq_official(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    runtime: GatewayRuntimeStatus,
    shutdown_token: CancellationToken,
    dedupe: Arc<MessageDedupe>,
    ref_index: SharedRefIndex,
) -> anyhow::Result<()> {
    let (app_id, app_secret) = config
        .enabled_qq_official_credentials()
        .context("QQ official channel enabled without credentials")?;
    let app_id = app_id.to_owned();
    let app_secret = app_secret.to_owned();
    let respond = respond.with_qq_official_account_id(app_id.clone());
    let http_client = reqwest::Client::new();
    let auth = AccessTokenManager::new(
        http_client.clone(),
        app_id.clone(),
        app_secret,
        config.token_refresh_margin,
    );
    let api = QqApiClient::new(http_client.clone(), config.api_base.clone(), auth.clone());
    let group_outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    // 主动推送已经进程内化；Core 通过 PushSink 进入这里，仍由 Gateway 负责 QQ 发送。
    push_sink.bind(
        api.clone(),
        app_id.clone(),
        runtime.clone(),
        group_outbound_cache.clone(),
        ref_index.clone(),
    );
    let group_cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
    let bot_identity = Arc::new(BotIdentity::new(&app_id, &config.bot_mention_ids));
    // 断线续连所需的状态（session_id + seq）
    let mut resume = ResumeState::default();
    // 聚合器必须先 flush 到 Dispatcher，不能让全局 shutdown 同时取消两者。
    // 顶层 run 负责在停止接收新 Gateway 入站后，按 aggregator -> dispatcher 的顺序关闭。
    let dispatcher_shutdown = CancellationToken::new();
    let aggregator_shutdown = CancellationToken::new();
    let dispatcher = MessageDispatcher::new(
        config.clone(),
        auth.clone(),
        respond.clone(),
        api.clone(),
        dedupe.clone(),
        ref_index.clone(),
        group_outbound_cache.clone(),
        group_cooldowns.clone(),
        bot_identity.clone(),
        runtime.clone(),
        dispatcher_shutdown,
    );
    let dispatcher_handle = dispatcher.handle();
    let aggregator = MessageAggregator::new(
        config.clone(),
        app_id,
        respond.clone(),
        dispatcher_handle,
        dedupe.clone(),
        aggregator_shutdown,
    );
    let aggregator_handle = aggregator.handle();
    let mut gateway_fetch_backoff = GatewayFetchBackoff::default();

    loop {
        if shutdown_token.is_cancelled() {
            break;
        }
        info!(api_base = %config.api_base, "fetching QQ gateway url");
        // 每次重连前重新获取网关地址，避免 IP/调度发生变化后仍连旧地址
        let gateway_url = match fetch_gateway_url_with_retry(
            &shutdown_token,
            &mut gateway_fetch_backoff,
            || protocol::fetch_gateway_url(&http_client, &config, &auth),
            || fastrand::i16(-20..=20),
        )
        .await
        {
            Ok(GatewayFetchOutcome::Url(url)) => url,
            Ok(GatewayFetchOutcome::Shutdown) => break,
            Err(error) => return Err(error).context("fetch QQ gateway url"),
        };
        info!("fetched QQ gateway url");

        match protocol::run_gateway_once(
            &gateway_url,
            &config,
            &auth,
            &runtime,
            &mut resume,
            aggregator_handle.clone(),
            bot_identity.clone(),
            shutdown_token.clone(),
        )
        .await
        {
            // 正常关闭不算错误，但需要重连
            Ok(()) => warn!("QQ gateway connection closed; reconnecting"),
            // 异常断开也要重连
            Err(err) => warn!(error = %err, "QQ gateway connection failed; reconnecting"),
        }
        // run_gateway_once 返回即代表当前 WebSocket 生命周期已经结束；后续重连成功时
        // record_gateway_connected 会重新置为 true。
        runtime.record_gateway_disconnected();

        // 等待一段时间再重连，避免频繁重试给服务端带来压力
        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            _ = tokio::time::sleep(protocol::reconnect_delay()) => {}
        }
    }

    aggregator.shutdown().await;
    dispatcher.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use qq_maid_core::{
        runtime::push::{PushIntent, PushSink, PushTarget, PushTargetType},
        service::{
            CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreRequest,
            CoreRespondOutput, CoreService, UpstreamStatusSnapshot,
        },
    };
    use std::collections::HashMap;

    struct NoopCore;

    #[async_trait]
    impl CoreService for NoopCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            unreachable!("unbound QQ startup must not dispatch messages")
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            unreachable!("unbound QQ startup must not classify messages")
        }

        async fn upstream_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        fn health_snapshot(&self) -> CoreHealthSnapshot {
            CoreHealthSnapshot {
                ok: true,
                provider: "test".to_owned(),
                model: "test".to_owned(),
                stream: false,
                upstream: UpstreamStatusSnapshot::default(),
            }
        }
    }

    #[tokio::test]
    async fn unbound_qq_skips_gateway_and_marks_sender_unavailable() {
        let config = AppConfig::from_map(&HashMap::new()).unwrap();
        let push_sink = GatewayPushSink::unbound();
        let push_probe = push_sink.clone();
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        run(
            config,
            RespondClient::new(Arc::new(NoopCore)),
            push_sink,
            GatewayRuntimeStatus::new(),
            shutdown,
        )
        .await
        .unwrap();

        let err = push_probe
            .push(PushIntent {
                target: PushTarget::qq_official(PushTargetType::Private, "user-1"),
                text: "hello".to_owned(),
                fallback_text: None,
                message_type: "text".to_owned(),
                visible_entity_snapshot: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not bound"));
    }

    #[tokio::test]
    async fn wechat_listener_starts_when_qq_is_unbound() {
        let config = AppConfig::from_map(&HashMap::from([
            ("WECHAT_SERVICE_ENABLED".to_owned(), "true".to_owned()),
            ("WECHAT_SERVICE_TOKEN".to_owned(), "wechat-token".to_owned()),
            ("WECHAT_SERVICE_BIND_PORT".to_owned(), "0".to_owned()),
        ]))
        .unwrap();
        let runtime = GatewayRuntimeStatus::new();
        let observed_runtime = runtime.clone();
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(run(
            config,
            RespondClient::new(Arc::new(NoopCore)),
            GatewayPushSink::unbound(),
            runtime,
            shutdown,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            while !observed_runtime.snapshot().wechat_service_listening {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!observed_runtime.snapshot().qq_connected);

        stop.cancel();
        task.await.unwrap().unwrap();
        assert!(!observed_runtime.snapshot().wechat_service_listening);
    }
}
