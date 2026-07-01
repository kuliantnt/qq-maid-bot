//! Dispatcher 拒绝通知 worker。
//!
//! 单独的 task 从 reject channel 读取容量拒绝通知，并按目标类型（C2c / Group）
//! 通过 outbound API 发送“稍后再试”提示。这里只负责发送，不改变拒绝策略：
//! 真正的拒绝判定（队列满、worker slot 耗尽）仍在 `actor` 中完成。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

// outbound / logging / ping 都是 gateway 内 pub(pub(crate)/pub(super)) 项，
// dispatcher 作为 gateway 的后代可以直接引用。
use super::super::{
    logging::{mask_identifier, mask_scope_key},
    outbound::{send_c2c_text_with_status, send_group_text_with_status},
    ping::GatewayRuntimeStatus,
};
use super::types::{RejectNotification, RejectTarget};
use crate::api::QqApiClient;

pub(super) async fn run_reject_worker(
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    mut reject_rx: mpsc::Receiver<RejectNotification>,
    shutdown_token: CancellationToken,
) {
    loop {
        let notification = tokio::select! {
            _ = shutdown_token.cancelled() => break,
            item = reject_rx.recv() => item,
        };
        let Some(notification) = notification else {
            break;
        };
        let masked_target = match &notification.target {
            RejectTarget::C2c { user_openid, .. } => mask_identifier(user_openid),
            RejectTarget::Group { group_openid, .. } => mask_identifier(group_openid),
        };
        let result = match notification.target {
            RejectTarget::C2c {
                user_openid,
                message_id,
            } => {
                send_c2c_text_with_status(
                    &api,
                    &runtime,
                    &user_openid,
                    Some(&message_id),
                    &notification.message,
                )
                .await
            }
            RejectTarget::Group {
                group_openid,
                message_id,
            } => {
                send_group_text_with_status(
                    &api,
                    &runtime,
                    &group_openid,
                    Some(&message_id),
                    &notification.message,
                )
                .await
            }
        };
        if let Err(error) = result {
            warn!(
                scope_key = %mask_scope_key(&notification.scope_key),
                target = %masked_target,
                error = %error.log_summary(),
                "dispatcher reject notification send failed"
            );
        }
    }
}
