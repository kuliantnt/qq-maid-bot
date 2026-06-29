//! QQ gateway 运行域。负责 WebSocket 主循环、事件分发、去重、诊断与回发编排。

mod aggregator;
mod c2c;
mod cache;
pub mod dedupe;
mod dispatcher;
pub mod event;
mod group;
mod group_filter;
pub mod logging;
mod outbound;
pub mod ping;
mod protocol;
pub mod push;
mod stream;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use aggregator::MessageAggregator;
use anyhow::Context;
use dispatcher::MessageDispatcher;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use c2c::handle_c2c_message;
pub(crate) use cache::{BotOutboundCache, ReplyCache, resolve_signals};
use dedupe::MessageDedupe;
use group::handle_group_message;
use group_filter::GroupCooldowns;
use ping::GatewayRuntimeStatus;
use protocol::ResumeState;
use push::GatewayPushSink;

use crate::{
    api::QqApiClient, auth::AccessTokenManager, config::AppConfig, respond::RespondClient,
};

const DEDUPE_TTL: Duration = Duration::from_secs(10 * 60);

/// QQ 网关主循环：初始化所有共享组件后，反复获取网关地址并建立 WebSocket 连接。
/// 连接断开或失败后会等待 `RECONNECT_DELAY` 后重连，从而保证长期在线。
pub async fn run(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    let http_client = reqwest::Client::new();
    let auth = AccessTokenManager::new(
        http_client.clone(),
        config.app_id.clone(),
        config.app_secret.clone(),
        config.token_refresh_margin,
    );
    let api = QqApiClient::new(http_client.clone(), config.api_base.clone(), auth.clone());
    // 消息去重器，用于防止短时间内重复处理同一条 C2C 消息
    let dedupe = Arc::new(MessageDedupe::new(DEDUPE_TTL));
    // 运行时状态，记录网关连接、收发消息等统计信息，供 /ping 等命令使用
    let runtime = GatewayRuntimeStatus::new();
    let group_outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    // 主动推送已经进程内化；Core 通过 PushSink 进入这里，仍由 Gateway 负责 QQ 发送。
    push_sink.bind(api.clone(), runtime.clone(), group_outbound_cache.clone());
    // reply 只需要一个极简 HashMap 缓存，不引入额外抽象层或持久化。
    let reply_cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
    let group_cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
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
        reply_cache.clone(),
        group_outbound_cache.clone(),
        group_cooldowns.clone(),
        runtime.clone(),
        dispatcher_shutdown,
    );
    let dispatcher_handle = dispatcher.handle();
    let aggregator = MessageAggregator::new(
        config.clone(),
        respond.clone(),
        dispatcher_handle,
        dedupe.clone(),
        reply_cache.clone(),
        aggregator_shutdown,
    );
    let aggregator_handle = aggregator.handle();

    loop {
        if shutdown_token.is_cancelled() {
            break;
        }
        info!(api_base = %config.api_base, "fetching QQ gateway url");
        // 每次重连前重新获取网关地址，避免 IP/调度发生变化后仍连旧地址
        let gateway_url = match tokio::select! {
            _ = shutdown_token.cancelled() => break,
            result = protocol::fetch_gateway_url(&http_client, &config, &auth) => result,
        } {
            Ok(url) => {
                info!("fetched QQ gateway url");
                url
            }
            Err(err) => {
                warn!(error = %err, "failed to fetch QQ gateway url");
                return Err(err).context("fetch QQ gateway url");
            }
        };

        match protocol::run_gateway_once(
            &gateway_url,
            &config,
            &auth,
            &runtime,
            &mut resume,
            aggregator_handle.clone(),
            shutdown_token.clone(),
        )
        .await
        {
            // 正常关闭不算错误，但需要重连
            Ok(()) => warn!("QQ gateway connection closed; reconnecting"),
            // 异常断开也要重连
            Err(err) => warn!(error = %err, "QQ gateway connection failed; reconnecting"),
        }

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
