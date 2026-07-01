//! 普通消息安全分段模块。
//!
//! 把完整回复按 Markdown 安全边界拆成可独立发送的多段，并保证每段 Markdown 与
//! 纯文本 fallback 一一对应。底层只消费原始文本范围，不耦合 `OutboundMessage`，
//! 为后续普通聊天与 C2C 流式轮转复用同一套边界与原文消费语义。
//!
//! 关键约束（见 Issue #124）：
//! - Markdown 与 fallback 必须按同一原文段成对生成，禁止分别独立切割；
//! - synthetic fence 用于跨段代码块，计入实际发送长度但不推进原文消费位置；
//! - fallback 不得包含 synthetic fence；
//! - 各段消费的原文连续拼接应等于原始回复，不丢字、不重字、不乱序；
//! - 中文、Emoji、组合字符不会被切坏，按 Unicode scalar 安全切分。

use thiserror::Error;
use tracing::{debug, info, warn};

use crate::api::{
    ApiError, C2cReplyTarget, GroupOutboundSender, GroupReplyTarget, OutboundSender, SendResult,
};
use crate::logging::mask_openid;
use crate::markdown::MarkdownPayload;
use crate::render::OutboundMessage;

/// 复用 Core 的 Markdown 剥离能力，按段为同一原文生成纯文本 fallback。
/// Gateway 不另起一套 strip 实现，避免 fallback 语义与 Core 漂移。
use qq_maid_core::runtime::respond::markdown_strip::strip_markdown_for_chat;

/// 跨段代码块的 synthetic fence。
///
/// - 闭合 fence `FENCE_CLOSE` 补在本段末尾换行后；
/// - 开启 fence `FENCE_REOPEN` 补在下一段开头换行前；
/// - 二者都不推进原文消费位置，但计入实际发送长度。
const FENCE_REOPEN: &str = "```\n";
const FENCE_CLOSE: &str = "\n```";

/// 普通回复分段软限制配置。
///
/// 这是 Gateway 侧保守的软上限，并非 QQ 平台已确认的硬限制；真实平台限制仍需后续真机验证。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLimits {
    /// Markdown 通道（实际渲染并计入 QQ 限制的字符数）的软上限。
    pub markdown_soft_limit: usize,
    /// 纯文本通道的软上限。
    pub text_soft_limit: usize,
}

/// 默认软限制。与历史 Core `MAX_REPLY_LENGTH` 保持一致起步，便于平滑迁移。
pub const DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT: usize = 1800;
pub const DEFAULT_TEXT_CHUNK_SOFT_LIMIT: usize = 1800;
/// 软限制允许的下限；低于此值没有实际分段意义，且无法容纳 synthetic fence。
const MIN_CHUNK_SOFT_LIMIT: usize = 64;

impl ChunkLimits {
    pub fn new(markdown_soft_limit: usize, text_soft_limit: usize) -> Self {
        Self {
            markdown_soft_limit: markdown_soft_limit.max(MIN_CHUNK_SOFT_LIMIT),
            text_soft_limit: text_soft_limit.max(MIN_CHUNK_SOFT_LIMIT),
        }
    }

    pub fn defaults() -> Self {
        Self::new(
            DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
            DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
        )
    }
}

/// 一段可独立发送的回复内容。
///
/// `markdown` 与 `fallback_text` 由同一段原文成对生成：先确定本段消费的原文范围，
/// 再为该范围分别生成 rendered Markdown 和纯文本 fallback。`markdown` 为 `None`
/// 表示这条回复本身没有 Markdown 通道（例如 `enable_markdown=false`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundChunk {
    /// 实际发送的 rendered Markdown（含 synthetic fence）。纯文本回复时为 `None`。
    pub markdown: Option<MarkdownPayload>,
    /// 本段对应的纯文本 fallback；由同一段原文剥除 Markdown 得到，不含 synthetic fence。
    pub fallback_text: String,
    /// 本段从原始回复消费的字符数（按 Unicode scalar 计），不含 synthetic fence。
    pub consumed_original_chars: usize,
    /// 本段自动补充的 synthetic fence 字符数；计入实际发送长度但不推进原文消费。
    pub synthetic_fence_chars: usize,
    /// 本段是否补了 synthetic reopening fence（下一段开头 ` ```\n `）。
    pub synthetic_reopen: bool,
    /// 本段是否补了 synthetic closing fence（本段末尾 `\n``` `）。
    pub synthetic_close: bool,
    /// 实际渲染并计入 QQ 限制的字符数：Markdown 通道为 rendered Markdown 长度，
    /// 纯文本通道为 fallback_text 长度。
    pub rendered_chars: usize,
    pub chunk_index: usize,
    pub chunk_count: usize,
}

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

// ===========================================================================
// 底层安全分段 primitive：只消费原始 Markdown 文本，产出原文范围段。
// 不依赖 OutboundMessage，后续 C2C 流式轮转可复用同一套边界与消费语义。
// ===========================================================================

/// 原始 Markdown 的一个消费范围段。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawSegment {
    /// 本段消费的原文（不含 synthetic fence），拼接后等于原始回复。
    raw: String,
    /// 本段开始时代码块是否处于打开状态（渲染时补 synthetic reopening fence）。
    reopen: bool,
    /// 本段结束时代码块是否仍处于打开状态（渲染时补 synthetic closing fence）。
    close: bool,
    chunk_index: usize,
    chunk_count: usize,
}

/// 一行带换行的文本单元（保留尾随 `\n`，拼接后等于原文）。
struct LineUnit {
    text: String,
    char_len: usize,
    is_fence: bool,
    is_blank: bool,
}

// 列表边界判定保留为语义参考；当前分段优先级实际使用空行边界与代码块边界。
// 后续如需按列表边界拆分，可在此处复用该判定。
#[allow(dead_code)]
fn is_list_item_line(trimmed: &str) -> bool {
    // 列表边界判定与 markdown_strip 保持一致：无序 `- * +` 或有序 `1.`/`1)` 后接空白。
    if let Some(rest) = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        .or_else(|| trimmed.strip_prefix('+'))
    {
        return rest.starts_with(char::is_whitespace);
    }
    let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let rest = &trimmed[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'));
    rest.is_some_and(|r| r.starts_with(char::is_whitespace))
}

fn build_units(raw: &str) -> Vec<LineUnit> {
    raw.split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim();
            LineUnit {
                char_len: line.chars().count(),
                is_fence: trimmed.starts_with("```"),
                is_blank: trimmed.is_empty(),
                text: line.to_owned(),
            }
        })
        .collect()
}

/// rendered Markdown 长度 = 原文段长度 + synthetic fence 长度。
fn rendered_len(reopen: bool, raw_chars: usize, close: bool) -> usize {
    let fences = (if reopen {
        FENCE_REOPEN.chars().count()
    } else {
        0
    }) + (if close {
        FENCE_CLOSE.chars().count()
    } else {
        0
    });
    raw_chars + fences
}

/// 把完整原始 Markdown 按软限制切成多个原文范围段。
///
/// 分段优先级：
/// 1. 完整代码块边界（段末 fence 关闭，避免 synthetic）；
/// 2. 段落空行 / 列表边界 / 普通换行；
/// 3. 单行本身超限时退回句末边界，再退回 Unicode scalar 安全切分。
fn chunk_markdown_raw(raw: &str, limit: usize) -> Vec<RawSegment> {
    if raw.chars().count() <= limit {
        return vec![RawSegment {
            raw: raw.to_owned(),
            reopen: false,
            close: false,
            chunk_index: 0,
            chunk_count: 1,
        }];
    }

    let units = build_units(raw);
    let mut segments: Vec<RawSegment> = Vec::new();
    let mut in_fence = false;
    let mut idx = 0;

    while idx < units.len() {
        let reopen = in_fence;

        // 贪心收集行单元，直到加入下一行会让 rendered 长度超出软限制。
        let mut acc_chars = 0usize;
        let mut cur_fence = in_fence;
        let mut j = idx;
        loop {
            if j >= units.len() {
                break;
            }
            let unit = &units[j];
            let new_acc = acc_chars + unit.char_len;
            let new_fence = if unit.is_fence { !cur_fence } else { cur_fence };
            // rendered 超过软限制即停止收集；首行就超限时保持 j == idx，
            // 由下方的行内切分路径处理，避免把整条超限行强行放进一个段。
            let rendered = rendered_len(reopen, new_acc, new_fence);
            if rendered > limit {
                break;
            }
            acc_chars = new_acc;
            cur_fence = new_fence;
            j += 1;
        }

        if j == idx {
            // 首行本身就超过软限制：行内切分该行，逐块产出后推进到下一行。
            // `split_inline_unit` 总会把整行按字符消费完，不跨越 fence 行，
            // 因此 fence 状态在行内切分后保持不变，无需调整 in_fence。
            let unit = &units[idx];
            let inline_pieces = split_inline_unit(unit, in_fence, limit, reopen);
            for piece in inline_pieces.pieces {
                segments.push(RawSegment {
                    raw: piece.raw,
                    reopen: piece.reopen,
                    close: piece.close,
                    chunk_index: 0,
                    chunk_count: 0,
                });
            }
            idx += 1;
            continue;
        }

        // [idx..j) 已贪心收集；选择最佳断点。
        let chosen_end = choose_break_end(&units, idx, j, reopen, limit, in_fence);

        // 收集 [idx..=chosen_end] 为本段原文。
        let raw_part: String = units[idx..=chosen_end]
            .iter()
            .map(|u| u.text.as_str())
            .collect();
        let close = {
            let mut fence = in_fence;
            for unit in &units[idx..=chosen_end] {
                if unit.is_fence {
                    fence = !fence;
                }
            }
            fence
        };

        // 避免在闭合 fence 前断开导致下一段开头出现“空代码块”：
        // 若本段以 fence 打开收尾、且紧随其后就是闭合 fence 行，则把闭合 fence 并入本段。
        let (raw_part, close, chosen_end) =
            absorb_trailing_close_fence(raw_part, close, &units, chosen_end, j);

        segments.push(RawSegment {
            raw: raw_part,
            reopen,
            close,
            chunk_index: 0,
            chunk_count: 0,
        });
        in_fence = close;
        idx = chosen_end + 1;
    }

    let count = segments.len();
    for (i, seg) in segments.iter_mut().enumerate() {
        seg.chunk_index = i;
        seg.chunk_count = count;
    }
    segments
}

/// 在 [idx..j) 已收集的行单元中选择断点位置。
///
/// 优先级（见 Issue #124）：
/// 1. 末尾处于代码块内部时，回退到最近的 “fence 关闭” 断点，避免 synthetic；
/// 2. 段落空行边界（保留空行作为段尾，避免拆开一个段落的中间）；
/// 3. 普通换行 boundary（末尾收尾）。
fn choose_break_end(
    units: &[LineUnit],
    idx: usize,
    j: usize,
    reopen: bool,
    limit: usize,
    start_fence: bool,
) -> usize {
    let end = j - 1;
    let end_fence_open = {
        let mut f = start_fence;
        for unit in &units[idx..=end] {
            if unit.is_fence {
                f = !f;
            }
        }
        f
    };

    if !end_fence_open {
        // 不在代码块内：优先在段落空行边界收尾，避免切开一个段落中间。
        for k in (idx..=end).rev() {
            if units[k].is_blank {
                return k;
            }
        }
        return end;
    }

    // 末尾处于代码块内部：向前找最近的“fence 关闭”断点，使本段以 fence 关闭收尾。
    let mut fence = start_fence;
    let mut acc = 0usize;
    let mut last_closed_break: Option<usize> = None;
    for (k, unit) in units.iter().enumerate().take(j).skip(idx) {
        acc += unit.char_len;
        if unit.is_fence {
            fence = !fence;
        }
        let rendered = rendered_len(reopen, acc, fence);
        if rendered > limit {
            break;
        }
        if !fence {
            last_closed_break = Some(k);
        }
    }
    last_closed_break.unwrap_or(end)
}

/// 若本段以打开状态收尾、且紧随其后就是闭合 fence 行，则把闭合 fence 并入本段。
///
/// 这样可避免下一段开头出现 “synthetic reopening + 真实 closing fence” 形成的空代码块。
fn absorb_trailing_close_fence(
    mut raw_part: String,
    mut close: bool,
    units: &[LineUnit],
    chosen_end: usize,
    j: usize,
) -> (String, bool, usize) {
    if close && j < units.len() && units[j].text.trim() == "```" {
        raw_part.push_str(&units[j].text);
        close = false;
        return (raw_part, close, j);
    }
    (raw_part, close, chosen_end)
}

/// 行内切分单行的产物。
///
/// `split_inline_unit` 总会把整行按字符逐块消费完，因此这里只暴露 pieces。
struct InlineSplit {
    pieces: Vec<RawSegment>,
}

/// 单行超限时在行内切分：代码块内按 Unicode scalar 安全切；普通文本先按句末再按 scalar 切。
///
/// 每块自带 reopen/close：在 fence 内的块两端补 synthetic fence；普通块无需补。
fn split_inline_unit(unit: &LineUnit, in_fence: bool, limit: usize, _reopen: bool) -> InlineSplit {
    let content = unit.text.as_str();
    let chars: Vec<char> = content.chars().collect();
    let fence_cost = if in_fence {
        FENCE_REOPEN.chars().count() + FENCE_CLOSE.chars().count()
    } else {
        0
    };
    // 单块可承载的原文字符数；至少为 1 保证前进，绝不死循环。
    let block_budget = limit.saturating_sub(fence_cost).max(1);

    let mut pieces = Vec::new();
    let mut pos = 0usize;
    while pos < chars.len() {
        let remaining = chars.len() - pos;
        let take = if in_fence {
            // 代码块内不按句末切，避免误判代码中的标点。
            block_budget.min(remaining)
        } else {
            // 普通文本优先在 [pos..pos+block_budget] 内找句末。
            match split_at_sentence_end(&chars[pos..], block_budget) {
                Some(take) => take,
                None => block_budget.min(remaining),
            }
        };
        let block: String = chars[pos..pos + take].iter().collect();
        pieces.push(RawSegment {
            raw: block,
            // 在 fence 内的块每块都补 synthetic reopening/closing fence；
            // 普通块不补。reopen/close 由 `render_markdown_segment` 决定是否插入。
            reopen: in_fence,
            close: in_fence,
            chunk_index: 0,
            chunk_count: 0,
        });
        pos += take;
    }

    // 行内切分对整行逐块消费，循环在 pos >= chars.len() 时结束，因此整行被全部切完。
    // 这保证不丢字、不重字：各块 raw 拼接等于本行 unit 原文。
    debug_assert_eq!(
        pieces.iter().map(|p| p.raw.chars().count()).sum::<usize>(),
        chars.len(),
        "inline split must consume whole unit"
    );
    InlineSplit { pieces }
}

/// 句末边界切分：返回满足限制的句末位置（字符数），找不到返回 None。
fn split_at_sentence_end(chars: &[char], limit: usize) -> Option<usize> {
    let mut acc = 0usize;
    let mut candidate: Option<usize> = None;
    for (i, &ch) in chars.iter().enumerate() {
        acc += 1;
        if acc > limit {
            break;
        }
        // 句末标点（中英文）后即视为候选断点。
        if "。！？.!?".contains(ch) {
            candidate = Some(i + 1);
        }
    }
    candidate
}

// ===========================================================================
// OutboundMessage -> OutboundChunk 转换。
// ===========================================================================

/// 把 `OutboundMessage` 切成可独立发送的分段。
///
/// - `Markdown` 通道按 Markdown 软限制切，每段 fallback 由同一段原文剥除得到；
/// - `Text` 通道按文本软限制切，没有 Markdown 通道；
/// - `Image` 不属于文本分段范畴，返回单段 fallback（实际发送由现有图片链路处理）。
pub fn chunk_outbound(message: &OutboundMessage, limits: &ChunkLimits) -> Vec<OutboundChunk> {
    match message {
        OutboundMessage::Text { text } => chunk_plain_text(text, limits.text_soft_limit),
        OutboundMessage::Markdown {
            markdown,
            fallback_text: _,
        } => chunk_markdown(&markdown.content, limits),
        // Image 不做文本分段；发送编排走单段图片链路，fallback 用作纯文本占位。
        OutboundMessage::Image { fallback_text, .. } => {
            chunk_plain_text(fallback_text, limits.text_soft_limit)
        }
    }
}

fn chunk_plain_text(text: &str, limit: usize) -> Vec<OutboundChunk> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return vec![OutboundChunk {
            markdown: None,
            fallback_text: text.to_owned(),
            consumed_original_chars: chars.len(),
            synthetic_fence_chars: 0,
            synthetic_reopen: false,
            synthetic_close: false,
            rendered_chars: chars.len(),
            chunk_index: 0,
            chunk_count: 1,
        }];
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + limit).min(chars.len());
        let part: String = chars[start..end].iter().collect();
        chunks.push(part);
        start = end;
    }

    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let rendered_chars = part.chars().count();
            OutboundChunk {
                markdown: None,
                fallback_text: part,
                consumed_original_chars: rendered_chars,
                synthetic_fence_chars: 0,
                synthetic_reopen: false,
                synthetic_close: false,
                rendered_chars,
                chunk_index: index,
                chunk_count: count,
            }
        })
        .collect()
}

fn chunk_markdown(markdown: &str, limits: &ChunkLimits) -> Vec<OutboundChunk> {
    let segments = chunk_markdown_raw(markdown, limits.markdown_soft_limit);
    let count = segments.len();
    segments
        .into_iter()
        .enumerate()
        .map(|(index, seg)| {
            let rendered_markdown = render_markdown_segment(&seg);
            let rendered_chars = rendered_markdown.chars().count();
            let synthetic_fence_chars = rendered_chars - seg.raw.chars().count();
            // fallback 由同一段 rendered Markdown 剥除得到；
            // 因为每段补齐了 synthetic fence，strip 时 fence 行会被移除，不会出现在 fallback 中。
            let fallback_text = strip_markdown_for_chat(&rendered_markdown);
            OutboundChunk {
                markdown: Some(MarkdownPayload::new(rendered_markdown)),
                fallback_text,
                consumed_original_chars: seg.raw.chars().count(),
                synthetic_fence_chars,
                synthetic_reopen: seg.reopen,
                synthetic_close: seg.close,
                rendered_chars,
                chunk_index: index,
                chunk_count: count,
            }
        })
        .collect()
}

fn render_markdown_segment(seg: &RawSegment) -> String {
    let mut out = String::new();
    if seg.reopen {
        out.push_str(FENCE_REOPEN);
    }
    out.push_str(&seg.raw);
    if seg.close {
        out.push_str(FENCE_CLOSE);
    }
    out
}

// ===========================================================================
// 出站发送编排：逐段发送，按段 fallback，部分送达返回 PartiallySent。
// ===========================================================================

/// 发送一段：先尝试 Markdown，失败再 fallback **当前段** 纯文本。
///
/// 语义与单段 `send_outbound_with_fallback` 一致，但作用在单个分段上：失败只回退
/// 当前段，已成功发送的前段不重发。
async fn send_chunk_c2c<S: OutboundSender + ?Sized>(
    sender: &S,
    target: &C2cReplyTarget,
    chunk: &OutboundChunk,
) -> (SendResult, bool) {
    if let Some(markdown) = &chunk.markdown {
        match sender.send_markdown(target, markdown).await {
            Ok(id) => return (Ok(id), false),
            Err(err) if !chunk.fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "chunk markdown send failed; falling back to text per chunk"
                );
                let fallback = sender.send_text(target, &chunk.fallback_text).await;
                let used_fallback = fallback.is_ok();
                return (fallback, used_fallback);
            }
            Err(err) => return (Err(err), false),
        }
    }
    let result = sender.send_text(target, &chunk.fallback_text).await;
    (result, false)
}

async fn send_chunk_group<S: GroupOutboundSender + ?Sized>(
    sender: &S,
    target: &GroupReplyTarget,
    chunk: &OutboundChunk,
) -> (SendResult, bool) {
    if let Some(markdown) = &chunk.markdown {
        match sender.send_markdown(target, markdown).await {
            Ok(id) => return (Ok(id), false),
            Err(err) if !chunk.fallback_text.trim().is_empty() => {
                warn!(
                    group = %mask_openid(&target.group_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "group chunk markdown send failed; falling back to text per chunk"
                );
                let fallback = sender.send_text(target, &chunk.fallback_text).await;
                let used_fallback = fallback.is_ok();
                return (fallback, used_fallback);
            }
            Err(err) => return (Err(err), false),
        }
    }
    let result = sender.send_text(target, &chunk.fallback_text).await;
    (result, false)
}

fn remaining_chars(chunks: &[OutboundChunk], from_index: usize) -> usize {
    chunks[from_index..]
        .iter()
        .map(|c| c.consumed_original_chars)
        .sum()
}

/// C2C 普通回复分段发送。
///
/// 逐段发送，每段成功后才发送下一段；任一段失败立即停止并返回 `OutboundSendError`。
/// `on_sent` 仅在分段成功时回调一次（带该段序号与 QQ 返回的 message id），调用方据此
/// 写入 `BotOutboundCache`；失败段不回调。返回值为各段成功后收集到的 message id 列表。
pub async fn send_c2c_outbound_chunked<S, F>(
    sender: &S,
    target: &C2cReplyTarget,
    message: &OutboundMessage,
    limits: &ChunkLimits,
    mut on_sent: F,
) -> Result<Vec<Option<String>>, OutboundSendError>
where
    S: OutboundSender + ?Sized,
    F: FnMut(usize, &Option<String>),
{
    let chunks = chunk_outbound(message, limits);
    let total = chunks.len();
    let masked_user = mask_openid(&target.user_openid);
    info!(
        user = %masked_user,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        kind = outbound_kind(message),
        "preparing chunked C2C outbound"
    );

    let mut sent_ids = Vec::with_capacity(total);
    for (index, chunk) in chunks.iter().enumerate() {
        debug!(
            user = %masked_user,
            source_message_id = target.msg_id.as_deref().unwrap_or(""),
            chunk_index = chunk.chunk_index,
            chunk_count = chunk.chunk_count,
            sent_chars = chunk.rendered_chars,
            remaining_chars = remaining_chars(&chunks, index),
            message_type = message_type_name(chunk),
            "sending C2C chunk"
        );
        match send_chunk_c2c(sender, target, chunk).await {
            (Ok(id), fallback_used) => {
                info!(
                    user = %masked_user,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chars = chunk.rendered_chars,
                    remaining_chars = remaining_chars(&chunks, index + 1),
                    message_type = message_type_name(chunk),
                    fallback_used,
                    "C2C chunk sent"
                );
                on_sent(chunk.chunk_index, &id);
                sent_ids.push(id);
            }
            (Err(err), _) => {
                warn!(
                    user = %masked_user,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "C2C chunk send failed; aborting remaining chunks"
                );
                return Err(make_send_error(
                    err,
                    index,
                    total,
                    remaining_chars(&chunks, index),
                ));
            }
        }
    }
    Ok(sent_ids)
}

/// 群普通回复分段发送。语义同 C2C 版本，区别仅在 `GroupOutboundSender` 与群 target。
pub async fn send_group_outbound_chunked<S, F>(
    sender: &S,
    target: &GroupReplyTarget,
    message: &OutboundMessage,
    limits: &ChunkLimits,
    mut on_sent: F,
) -> Result<Vec<Option<String>>, OutboundSendError>
where
    S: GroupOutboundSender + ?Sized,
    F: FnMut(usize, &Option<String>),
{
    let chunks = chunk_outbound(message, limits);
    let total = chunks.len();
    let masked_group = mask_openid(&target.group_openid);
    info!(
        group = %masked_group,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        kind = outbound_kind(message),
        "preparing chunked group outbound"
    );

    let mut sent_ids = Vec::with_capacity(total);
    for (index, chunk) in chunks.iter().enumerate() {
        debug!(
            group = %masked_group,
            source_message_id = target.msg_id.as_deref().unwrap_or(""),
            chunk_index = chunk.chunk_index,
            chunk_count = chunk.chunk_count,
            sent_chars = chunk.rendered_chars,
            remaining_chars = remaining_chars(&chunks, index),
            message_type = message_type_name(chunk),
            "sending group chunk"
        );
        match send_chunk_group(sender, target, chunk).await {
            (Ok(id), fallback_used) => {
                info!(
                    group = %masked_group,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chars = chunk.rendered_chars,
                    remaining_chars = remaining_chars(&chunks, index + 1),
                    message_type = message_type_name(chunk),
                    fallback_used,
                    "group chunk sent"
                );
                on_sent(chunk.chunk_index, &id);
                sent_ids.push(id);
            }
            (Err(err), _) => {
                warn!(
                    group = %masked_group,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "group chunk send failed; aborting remaining chunks"
                );
                return Err(make_send_error(
                    err,
                    index,
                    total,
                    remaining_chars(&chunks, index),
                ));
            }
        }
    }
    Ok(sent_ids)
}

fn make_send_error(
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

fn outbound_kind(message: &OutboundMessage) -> &'static str {
    match message {
        OutboundMessage::Text { .. } => "text",
        OutboundMessage::Markdown { .. } => "markdown",
        OutboundMessage::Image { .. } => "image",
    }
}

fn message_type_name(chunk: &OutboundChunk) -> &'static str {
    if chunk.markdown.is_some() {
        "markdown"
    } else {
        "text"
    }
}

#[cfg(test)]
mod tests;
