use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::{event_stream::C2cStreamSender, types::C2cStreamingPhase};
use crate::{api::C2cReplyTarget, gateway::event::C2cMessage};
use qq_maid_core::service::{CoreOutputPolicy, CoreResponseStatus};

/// QQ C2C 流式发送的节流间隔（毫秒）。
///
/// 避免每个 LLM delta 都请求一次 QQ API，减少接口压力。
pub(crate) const STREAM_THROTTLE_MS: u64 = 500;

pub(super) fn stream_flush_wait(
    phase: &C2cStreamingPhase,
    has_pending_update: bool,
    last_send_at: Instant,
) -> Option<Duration> {
    // delta 事件可能连续到达且一直让 receiver 就绪；若只在取到新事件时检查节流，
    // 生成结束前没有下一事件就会把累计全文留到 complete。这里给活跃流设置可取消的
    // 读取超时，让未提交的全文按周期刷新，同时所有请求仍在同一个事件循环中串行执行。
    if !matches!(phase, C2cStreamingPhase::Active(_)) || !has_pending_update {
        return None;
    }
    Some(Duration::from_millis(STREAM_THROTTLE_MS).saturating_sub(last_send_at.elapsed()))
}

pub(super) fn should_send_progress_status(
    enabled: bool,
    policy: CoreOutputPolicy,
    attempted: bool,
) -> bool {
    enabled
        && !attempted
        && matches!(
            policy,
            CoreOutputPolicy::ProgressThenComplete | CoreOutputPolicy::ProgressThenStream
        )
}

pub(super) async fn send_progress_status<S: C2cStreamSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    status: &CoreResponseStatus,
    masked_user: &str,
    masked_reply_msg_id: &str,
    stream_state: &str,
) {
    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    // Status 是系统短文案，独立普通文本发送；失败只记日志，不影响 Tool Loop 和最终回复。
    match sender.send_text(&target, &status.text).await {
        Ok(_) => {
            debug!(
                user = %masked_user,
                reply_msg_id = %masked_reply_msg_id,
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                stream_state,
                status_chars = status.text.chars().count(),
                "C2C 进度状态已发送"
            );
        }
        Err(err) => {
            warn!(
                user = %masked_user,
                reply_msg_id = %masked_reply_msg_id,
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                stream_state,
                error = %err.log_summary(),
                "C2C 进度状态发送失败，将继续发送最终回复"
            );
        }
    }
}
