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
pub mod onebot11;
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
use anyhow::{Context, anyhow, bail};
use bot_identity::BotIdentity;
use dispatcher::MessageDispatcher;
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::task::JoinHandle;
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
type ChannelTask = JoinHandle<anyhow::Result<()>>;

const QQ_OFFICIAL_CHANNEL: &str = "QQ official gateway";
const WECHAT_SERVICE_CHANNEL: &str = "wechat service callback";
const ONEBOT11_CHANNEL: &str = "OneBot 11 reverse WebSocket";

/// QQ 网关主循环：初始化所有共享组件后，反复获取网关地址并建立 WebSocket 连接。
/// 连接断开或失败后会等待 `RECONNECT_DELAY` 后重连，从而保证长期在线。
pub async fn run(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    runtime: GatewayRuntimeStatus,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    // 微信与 QQ 共用 Core；监听器只创建一次，之后统一交给渠道监督器管理生命周期。
    let dedupe = Arc::new(MessageDedupe::new(DEDUPE_TTL));
    // QQ 官方 REFIDX 与 OneBot message_id 共用同一进程内索引实现，但 key 始终包含
    // platform/account/conversation，平台语义不会相互污染。
    let ref_index = ref_index();
    let mut wechat_service_handle = if config.wechat_service.enabled {
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
    let onebot11_handle = if config.onebot11.enabled {
        match onebot11::spawn_reverse_websocket_server_with_ref_index(
            config.clone(),
            respond.clone(),
            dedupe.clone(),
            ref_index.clone(),
            runtime.clone(),
            shutdown_token.clone(),
        )
        .await
        {
            Ok(server) => {
                // sender 与反向 WebSocket 复用同一个 connection context；离线、替换和断线
                // 都由该上下文返回真实错误，不能另建连接或伪造推送成功。
                push_sink.bind_onebot11(
                    onebot11::OneBotSender::new(server.connection.clone()),
                    ref_index.clone(),
                );
                Some(server.task)
            }
            Err(error) => {
                // 多入口按顺序 bind；后启动的入口失败时必须回收已启动监听器，
                // 避免 run 返回错误后仍留下不可监督的后台任务。
                shutdown_token.cancel();
                if let Some(wechat_service) = wechat_service_handle.take() {
                    match wechat_service.await {
                        Ok(Ok(())) => {}
                        Ok(Err(cleanup_error)) => warn!(
                            error = %cleanup_error,
                            "wechat service cleanup after OneBot startup failure returned an error"
                        ),
                        Err(join_error) => warn!(
                            error = %join_error,
                            "wechat service cleanup after OneBot startup failure failed"
                        ),
                    }
                }
                return Err(error);
            }
        }
    } else {
        push_sink.mark_onebot11_unavailable("OneBot 11 channel is disabled");
        None
    };
    let qq_official_handle = match config.qq_official_binding_state() {
        QqOfficialBindingState::Enabled => Some(tokio::spawn(run_qq_official(
            config,
            respond,
            push_sink,
            runtime,
            shutdown_token.clone(),
            dedupe,
            ref_index,
        ))),
        QqOfficialBindingState::Unbound => {
            push_sink.mark_qq_official_unavailable("QQ official channel is not bound");
            info!(
                channel = "qq_official",
                state = "unbound",
                "skipping channel initialization"
            );
            None
        }
        QqOfficialBindingState::Disabled => {
            push_sink.mark_qq_official_unavailable("QQ official channel is disabled");
            info!(
                channel = "qq_official",
                state = "disabled",
                "skipping channel initialization"
            );
            None
        }
    };

    supervise_all_channels(
        qq_official_handle,
        wechat_service_handle,
        onebot11_handle,
        shutdown_token,
    )
    .await
}

/// 同时监督所有已启用入口。渠道在全局取消前结束属于故障；故障会先取消共享令牌，
/// 再等待其他渠道完成必要清理，并优先返回最先触发退出的原始错误。
async fn supervise_all_channels(
    qq_official: Option<ChannelTask>,
    wechat_service: Option<ChannelTask>,
    onebot11: Option<ChannelTask>,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    let mut configured = Vec::new();
    if let Some(task) = qq_official {
        configured.push((QQ_OFFICIAL_CHANNEL, task));
    }
    if let Some(task) = wechat_service {
        configured.push((WECHAT_SERVICE_CHANNEL, task));
    }
    if let Some(task) = onebot11 {
        configured.push((ONEBOT11_CHANNEL, task));
    }
    if configured.is_empty() {
        bail!(
            "no enabled gateway channel configured; enable QQ official, wechat service, or OneBot 11"
        );
    }

    let channels = FuturesUnordered::new();
    for (name, task) in configured {
        channels.push(async move { (name, task.await) });
    }
    tokio::pin!(channels);
    let first_exit = tokio::select! {
        _ = shutdown_token.cancelled() => None,
        result = channels.next() => result,
    };
    let mut outcome = if let Some((name, result)) = first_exit {
        let shutdown_requested = shutdown_token.is_cancelled();
        if !shutdown_requested {
            shutdown_token.cancel();
        }
        channel_task_result(name, result, shutdown_requested)
    } else {
        Ok(())
    };
    while let Some((name, result)) = channels.next().await {
        outcome = finish_channel_results(outcome, channel_task_result(name, result, true));
    }
    outcome
}

#[cfg(test)]
async fn supervise_channels(
    qq_official: Option<ChannelTask>,
    wechat_service: Option<ChannelTask>,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    supervise_all_channels(qq_official, wechat_service, None, shutdown_token).await
}
fn channel_task_result(
    channel_name: &'static str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
    shutdown_requested: bool,
) -> anyhow::Result<()> {
    match result {
        Ok(Ok(())) if shutdown_requested => Ok(()),
        Ok(Ok(())) => Err(anyhow!("{channel_name} exited unexpectedly")),
        // 渠道内部错误保持为首要错误返回，统一入口仍可看到原始错误链。
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow!("{channel_name} task join failed: {error}")),
    }
}

fn finish_channel_results(
    primary: anyhow::Result<()>,
    secondary: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (primary, secondary) {
        (Err(primary), Err(secondary)) => {
            warn!(error = %secondary, "secondary channel failed while gateway was stopping");
            Err(primary)
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
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

    let result = loop {
        if shutdown_token.is_cancelled() {
            break Ok(());
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
            Ok(GatewayFetchOutcome::Shutdown) => break Ok(()),
            Err(error) => break Err(error).context("fetch QQ gateway url"),
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
            _ = shutdown_token.cancelled() => break Ok(()),
            _ = tokio::time::sleep(protocol::reconnect_delay()) => {}
        }
    };

    aggregator.shutdown().await;
    dispatcher.shutdown().await;
    result
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
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicBool, Ordering},
    };

    struct NoopCore;

    fn task_waiting_for_shutdown(
        shutdown: CancellationToken,
        cleaned_up: Arc<AtomicBool>,
    ) -> ChannelTask {
        tokio::spawn(async move {
            shutdown.cancelled().await;
            cleaned_up.store(true, Ordering::SeqCst);
            Ok(())
        })
    }

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

        let error = run(
            config,
            RespondClient::new(Arc::new(NoopCore)),
            push_sink,
            GatewayRuntimeStatus::new(),
            shutdown,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no enabled gateway channel"));

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
    async fn qq_failure_cancels_wechat_and_returns_original_error() {
        let shutdown = CancellationToken::new();
        let wechat_cleaned_up = Arc::new(AtomicBool::new(false));
        let qq_official = tokio::spawn(async { Err(anyhow!("fatal QQ gateway error")) });
        let wechat_service =
            task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&wechat_cleaned_up));

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            supervise_channels(Some(qq_official), Some(wechat_service), shutdown.clone()),
        )
        .await
        .expect("channel supervision must not deadlock")
        .unwrap_err();

        assert_eq!(error.to_string(), "fatal QQ gateway error");
        assert!(shutdown.is_cancelled());
        assert!(wechat_cleaned_up.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn wechat_only_internal_error_is_returned() {
        let shutdown = CancellationToken::new();
        let wechat_service = tokio::spawn(async { Err(anyhow!("wechat serve failed")) });

        let error = supervise_channels(None, Some(wechat_service), shutdown.clone())
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "wechat serve failed");
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn wechat_join_error_is_not_ignored() {
        let shutdown = CancellationToken::new();
        let wechat_service = tokio::spawn(async {
            panic!("wechat task panic");
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });

        let error = supervise_channels(None, Some(wechat_service), shutdown.clone())
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("wechat service callback task join failed")
        );
        assert!(error.to_string().contains("wechat task panic"));
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn wechat_clean_exit_before_shutdown_is_an_error() {
        let shutdown = CancellationToken::new();
        let wechat_service = tokio::spawn(async { Ok(()) });

        let error = supervise_channels(None, Some(wechat_service), shutdown.clone())
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "wechat service callback exited unexpectedly"
        );
        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn global_shutdown_cleans_up_both_channels_without_error() {
        let shutdown = CancellationToken::new();
        let qq_cleaned_up = Arc::new(AtomicBool::new(false));
        let wechat_cleaned_up = Arc::new(AtomicBool::new(false));
        let qq_official = task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&qq_cleaned_up));
        let wechat_service =
            task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&wechat_cleaned_up));

        shutdown.cancel();
        tokio::time::timeout(
            Duration::from_secs(2),
            supervise_channels(Some(qq_official), Some(wechat_service), shutdown),
        )
        .await
        .expect("cancelled channels must finish promptly")
        .unwrap();

        assert!(qq_cleaned_up.load(Ordering::SeqCst));
        assert!(wechat_cleaned_up.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn global_shutdown_cleans_up_three_channels_without_error() {
        let shutdown = CancellationToken::new();
        let qq_cleaned_up = Arc::new(AtomicBool::new(false));
        let wechat_cleaned_up = Arc::new(AtomicBool::new(false));
        let onebot_cleaned_up = Arc::new(AtomicBool::new(false));
        let qq_official = task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&qq_cleaned_up));
        let wechat_service =
            task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&wechat_cleaned_up));
        let onebot11 = task_waiting_for_shutdown(shutdown.clone(), Arc::clone(&onebot_cleaned_up));

        shutdown.cancel();
        tokio::time::timeout(
            Duration::from_secs(2),
            supervise_all_channels(
                Some(qq_official),
                Some(wechat_service),
                Some(onebot11),
                shutdown,
            ),
        )
        .await
        .expect("cancelled channels must finish promptly")
        .unwrap();

        assert!(qq_cleaned_up.load(Ordering::SeqCst));
        assert!(wechat_cleaned_up.load(Ordering::SeqCst));
        assert!(onebot_cleaned_up.load(Ordering::SeqCst));
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

    #[tokio::test]
    async fn onebot_listener_starts_when_qq_is_unbound() {
        let mut config = AppConfig::from_map(&HashMap::from([(
            "QQ_BOT_ENABLED".to_owned(),
            "false".to_owned(),
        )]))
        .unwrap();
        config.onebot11 = crate::config::OneBot11Config {
            enabled: true,
            bind_port: 0,
            access_token: Some("test-token".to_owned()),
            ..crate::config::OneBot11Config::default()
        };
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
            while !observed_runtime.snapshot().onebot_listening {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!observed_runtime.snapshot().qq_connected);

        stop.cancel();
        task.await.unwrap().unwrap();
        assert!(!observed_runtime.snapshot().onebot_listening);
    }

    #[tokio::test]
    async fn onebot_bind_failure_cleans_up_started_wechat_listener() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let mut config = AppConfig::from_map(&HashMap::from([
            ("QQ_BOT_ENABLED".to_owned(), "false".to_owned()),
            ("WECHAT_SERVICE_ENABLED".to_owned(), "true".to_owned()),
            ("WECHAT_SERVICE_TOKEN".to_owned(), "wechat-token".to_owned()),
            ("WECHAT_SERVICE_BIND_PORT".to_owned(), "0".to_owned()),
        ]))
        .unwrap();
        config.onebot11 = crate::config::OneBot11Config {
            enabled: true,
            bind_port: occupied_port,
            access_token: Some("test-token".to_owned()),
            ..crate::config::OneBot11Config::default()
        };
        let runtime = GatewayRuntimeStatus::new();
        let observed_runtime = runtime.clone();

        let error = run(
            config,
            RespondClient::new(Arc::new(NoopCore)),
            GatewayPushSink::unbound(),
            runtime,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("bind OneBot 11"));
        assert!(!observed_runtime.snapshot().wechat_service_listening);
        assert!(!observed_runtime.snapshot().onebot_listening);
    }
}
