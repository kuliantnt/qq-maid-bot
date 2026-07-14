//! 普通消息分段发送的错误与诊断分类。
//!
//! 本模块只描述发送进度和日志分类，不参与分段边界计算或实际发送，避免错误模型
//! 与较长的 Markdown 分段算法继续混在同一文件中。

use thiserror::Error;

use crate::{api::ApiError, render::OutboundMessage};

use super::OutboundChunk;

/// 出站多段发送编排错误。
///
/// 底层 `ApiError` 继续只表示单次 QQ API / 鉴权 / HTTP / 状态码 / unsupported 错误；
/// 多段发送的进度语义由本类型承载，避免在 `ApiError` 里重复 `SingleApi` 分支。
#[derive(Debug, Error)]
pub enum OutboundSendError {
    /// 一个分段都没成功发出（首段即失败）。
    #[error("qq outbound send failed before any chunk was sent")]
    NotSent {
        #[source]
        source: ApiError,
    },
    /// 已成功发送部分分段，后续分段失败；已成功前段不重发。
    #[error(
        "qq outbound send partially failed: {sent_chunks}/{total_chunks} chunks sent, \
         failed at chunk {failed_chunk_index}, {remaining_chars} chars remaining"
    )]
    PartiallySent {
        #[source]
        source: ApiError,
        sent_chunks: usize,
        total_chunks: usize,
        failed_chunk_index: usize,
        remaining_chars: usize,
    },
}

impl OutboundSendError {
    /// 是否已有分段成功发送，调用方可据此决定是否记录部分送达。
    pub fn has_partial_progress(&self) -> bool {
        matches!(self, Self::PartiallySent { .. })
    }
}

pub(super) fn make_send_error(
    source: ApiError,
    index: usize,
    total: usize,
    remaining: usize,
) -> OutboundSendError {
    if index == 0 {
        OutboundSendError::NotSent { source }
    } else {
        OutboundSendError::PartiallySent {
            source,
            sent_chunks: index,
            total_chunks: total,
            failed_chunk_index: index,
            remaining_chars: remaining,
        }
    }
}

pub(super) fn outbound_kind(message: &OutboundMessage) -> &'static str {
    match message {
        OutboundMessage::Text { .. } => "text",
        OutboundMessage::Markdown { .. } => "markdown",
        OutboundMessage::Image { .. } => "image",
        OutboundMessage::ImagePlaceholder { .. } => "image_placeholder",
        OutboundMessage::AttachmentPlaceholder { .. } => "attachment_placeholder",
    }
}

pub(super) fn message_type_name(chunk: &OutboundChunk) -> &'static str {
    if chunk.markdown.is_some() {
        "markdown"
    } else {
        "text"
    }
}
