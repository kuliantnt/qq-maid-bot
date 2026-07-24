use super::*;
use crate::api::{C2cReplyTarget, GroupReplyTarget, OutboundSender, SendFuture, SendMessageIds};
use crate::markdown::MarkdownPayload;
use crate::render::OutboundMessage;
use std::sync::Mutex;

/// 拼接各段消费的原始文本（去 synthetic fence）应等于原始回复，不丢字不重字不乱序。
fn reconstructed_raw(chunks: &[OutboundChunk]) -> String {
    let mut joined = String::new();
    for chunk in chunks {
        if let Some(md) = &chunk.markdown {
            // 去除 synthetic fence：仅当该段确实补充了 synthetic fence 时才剥离，
            // 避免误删原文里真实的 closing fence。
            let stripped =
                strip_synthetic_fence(&md.content, chunk.synthetic_reopen, chunk.synthetic_close);
            joined.push_str(&stripped);
        } else {
            joined.push_str(&chunk.fallback_text);
        }
    }
    joined
}

fn strip_synthetic_fence(rendered: &str, reopen: bool, close: bool) -> String {
    // 仅当该段确实补充了 synthetic fence（由 synthetic_reopen / synthetic_close 标记）才剥离对应标记，
    // 避免误删原文里真实的 closing fence。
    if !reopen && !close {
        return rendered.to_owned();
    }
    let mut s = rendered.to_owned();
    if reopen && s.starts_with(FENCE_REOPEN) {
        s = s[FENCE_REOPEN.len()..].to_owned();
    }
    if close && s.ends_with(FENCE_CLOSE) {
        s = s[..s.len() - FENCE_CLOSE.len()].to_owned();
    }
    s
}

#[test]
fn markdown_below_limit_produces_single_chunk() {
    let md = "短回复\n普通文本";
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(md, &limits);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_count, 1);
    assert_eq!(chunks[0].rendered_chars, md.chars().count());
    assert_eq!(chunks[0].synthetic_fence_chars, 0);
    assert!(!chunks[0].synthetic_reopen);
    assert!(!chunks[0].synthetic_close);
    assert_eq!(reconstructed_raw(&chunks), md);
}

#[test]
fn plain_text_below_limit_single_chunk() {
    let text = "普通纯文本";
    let chunks = chunk_plain_text(text, 1800);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].markdown.is_none());
}

#[test]
fn c2c_passive_reply_plan_never_exceeds_four_visible_chunks() {
    let source = OutboundMessage::Text {
        text: "甲".repeat(400),
    };
    let limits = ChunkLimits::new(64, 64);
    let (planned, planned_limits, truncated) =
        plan_qq_passive_reply_outbounds(&[source], &limits, QQ_C2C_PASSIVE_REPLY_BUDGET);
    let chunks = planned
        .iter()
        .flat_map(|outbound| chunk_outbound(outbound, &planned_limits))
        .collect::<Vec<_>>();

    assert_eq!(chunks.len(), 4);
    assert!(truncated);
    assert!(
        chunks
            .last()
            .unwrap()
            .fallback_text
            .contains("平台被动回复上限截断")
    );
}

#[test]
fn group_passive_reply_plan_never_exceeds_five_visible_chunks() {
    let source = OutboundMessage::Text {
        text: "乙".repeat(500),
    };
    let limits = ChunkLimits::new(64, 64);
    let (planned, planned_limits, truncated) =
        plan_qq_passive_reply_outbounds(&[source], &limits, QQ_GROUP_PASSIVE_REPLY_BUDGET);
    let chunks = planned
        .iter()
        .flat_map(|outbound| chunk_outbound(outbound, &planned_limits))
        .collect::<Vec<_>>();

    assert_eq!(chunks.len(), 5);
    assert!(truncated);
    assert!(
        chunks
            .last()
            .unwrap()
            .fallback_text
            .contains("平台被动回复上限截断")
    );
}

#[test]
fn over_budget_media_fallback_still_has_visible_truncation_notice() {
    let source = (0..6)
        .map(|index| OutboundMessage::ImagePlaceholder {
            fallback_text: format!("图片 {index}"),
        })
        .collect::<Vec<_>>();
    let limits = ChunkLimits::new(64, 64);
    let (planned, planned_limits, truncated) =
        plan_qq_passive_reply_outbounds(&source, &limits, QQ_C2C_PASSIVE_REPLY_BUDGET);
    let chunks = planned
        .iter()
        .flat_map(|outbound| chunk_outbound(outbound, &planned_limits))
        .collect::<Vec<_>>();

    assert_eq!(chunks.len(), 1);
    assert!(truncated);
    assert!(chunks[0].fallback_text.contains("平台被动回复上限截断"));
}

#[test]
fn plain_text_over_limit_splits_by_unicode_scalar() {
    let text = "甲".repeat(4000);
    let chunks = chunk_plain_text(&text, 1800);
    assert!(chunks.len() >= 3);
    let joined: String = chunks.iter().map(|c| c.fallback_text.as_str()).collect();
    assert_eq!(joined, text);
}

#[test]
fn markdown_paragraph_split_at_blank_line() {
    let para_a = "第一段内容。".repeat(400);
    let para_b = "第二段内容。".repeat(400);
    let md = format!("{para_a}\n\n{para_b}");
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(&md, &limits);
    assert!(chunks.len() >= 2, "should split into multiple chunks");
    assert_eq!(reconstructed_raw(&chunks), md);
}

#[test]
fn chinese_emoji_not_split_unsafe() {
    let md = "😊".repeat(2000);
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(&md, &limits);
    let joined = reconstructed_raw(&chunks);
    assert_eq!(joined, md);
}

#[test]
fn long_code_block_uses_synthetic_fence() {
    let code_inner = "fn add(a: u32) -> u32 { a + 1 }\n".repeat(120);
    let md = format!("```rust\n{code_inner}```");
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(&md, &limits);
    assert!(chunks.len() >= 2);
    let joined = reconstructed_raw(&chunks);
    assert_eq!(joined, md);
    // fallback 不应包含 synthetic fence（` ``` ` 出现次数应等于原文真实 fence 数 = 2）。
    let fences_in_fallback: usize = chunks
        .iter()
        .map(|c| c.fallback_text.matches("```").count())
        .sum();
    // fallback 由 to_chat_text 处理，它会移除所有 fence 行（含真实与 synthetic），
    // 因此 fallback 不应出现 ```，既不含 synthetic fence 也不含真实 fence 标记。
    assert_eq!(fences_in_fallback, 0);
    // synthetic fence 必须计入 rendered 长度。
    let total_synthetic: usize = chunks.iter().map(|c| c.synthetic_fence_chars).sum();
    assert!(total_synthetic > 0);
}

#[test]
fn long_code_block_with_oversized_first_line_keeps_content_in_first_chunk() {
    let long_line = "a".repeat(180);
    let md = format!("```rust\n{long_line}\n```\n");
    let limits = ChunkLimits::new(72, 72);
    let chunks = chunk_markdown(&md, &limits);

    assert!(chunks.len() >= 2);
    assert_eq!(reconstructed_raw(&chunks), md);
    let first_markdown = chunks[0].markdown.as_ref().unwrap().content.as_str();
    assert!(first_markdown.contains('a'));
    assert!(!chunks[0].fallback_text.trim().is_empty());
    assert!(
        chunks
            .iter()
            .all(|chunk| !chunk.fallback_text.trim().is_empty()),
        "code chunks should not fall back to empty text"
    );
}

#[test]
fn markdown_headings_and_links_preserved() {
    let md = format!(
        "# 标题\n\n{}\n\n[OpenAI](https://openai.com)",
        "正文内容很丰富。".repeat(400)
    );
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(&md, &limits);
    assert!(chunks.len() >= 2);
    let joined = reconstructed_raw(&chunks);
    assert_eq!(joined, md);
    // 头部段含 `# 标题`。
    assert!(
        chunks[0]
            .markdown
            .as_ref()
            .unwrap()
            .content
            .contains("# 标题")
    );
}

#[test]
fn no_natural_boundary_long_run() {
    let md = "a".repeat(5000);
    let limits = ChunkLimits::new(1800, 1800);
    let chunks = chunk_markdown(&md, &limits);
    assert!(chunks.len() >= 3);
    let joined = reconstructed_raw(&chunks);
    assert_eq!(joined, md);
}

#[test]
fn chunk_limits_clamped_to_minimum() {
    let limits = ChunkLimits::new(1, 1);
    assert_eq!(limits.markdown_soft_limit, MIN_CHUNK_SOFT_LIMIT);
    assert_eq!(limits.text_soft_limit, MIN_CHUNK_SOFT_LIMIT);
}

#[test]
fn markdown_outbound_chunking_preserves_group_mention_fallback_prefix() {
    let body_markdown = "**正文** ".repeat(80);
    let body_fallback = "正文 ".repeat(80);
    let outbound = OutboundMessage::Markdown {
        markdown: MarkdownPayload::new(format!("<@member-1>\n{body_markdown}")),
        fallback_text: format!("<@member-1>\n{body_fallback}"),
    };
    let chunks = chunk_outbound(&outbound, &ChunkLimits::new(120, 120));

    assert!(chunks.len() >= 2);
    assert!(chunks[0].fallback_text.starts_with("<@member-1>\n"));
    assert!(!chunks[1].fallback_text.contains("<@member-1>"));
}

#[derive(Debug, Default)]
struct RecordingC2cSender {
    markdown_calls: Mutex<Vec<String>>,
    text_calls: Mutex<Vec<String>>,
    fail_on: Mutex<Option<usize>>,
}

impl OutboundSender for RecordingC2cSender {
    fn send_text<'a>(&'a self, _target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.text_calls.lock().unwrap().push(text.to_owned());
            let index = self.text_calls.lock().unwrap().len();
            Ok(SendMessageIds {
                message_id: Some(format!("tid-{index}")),
                ref_index_id: Some(format!("REFIDX_tid_{index}")),
            })
        })
    }
    fn send_markdown<'a>(
        &'a self,
        _target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let mut calls = self.markdown_calls.lock().unwrap();
            let index = calls.len();
            calls.push(markdown.content.clone());
            if let Some(fail_index) = *self.fail_on.lock().unwrap()
                && index == fail_index
            {
                return Err(ApiError::Unsupported("markdown"));
            }
            Ok(SendMessageIds {
                message_id: Some(format!("mid-{index}")),
                ref_index_id: Some(format!("REFIDX_mid_{index}")),
            })
        })
    }
    fn send_image<'a>(
        &'a self,
        _target: &'a C2cReplyTarget,
        _image: &'a crate::media::ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async { Err(ApiError::Unsupported("image")) })
    }
}

fn c2c_target() -> C2cReplyTarget {
    C2cReplyTarget {
        user_openid: "u1".to_owned(),
        msg_id: Some("m1".to_owned()),
    }
}

#[tokio::test]
async fn c2c_chunked_send_dispatches_each_chunk() {
    let sender = RecordingC2cSender::default();
    let md = "长回复内容 ".repeat(400);
    let outbound = OutboundMessage::Markdown {
        markdown: MarkdownPayload::new(md.clone()),
        fallback_text: md.clone(),
    };
    let limits = ChunkLimits::new(800, 800);
    let on_sent_ids = std::sync::Mutex::new(Vec::new());
    let ids = send_c2c_outbound_chunked(&sender, &c2c_target(), &outbound, &limits, |_, id| {
        on_sent_ids.lock().unwrap().push(id.clone());
    })
    .await
    .unwrap();
    let md_calls = sender.markdown_calls.lock().unwrap().len();
    assert!(md_calls >= 2);
    assert_eq!(ids.len(), md_calls);
    assert_eq!(on_sent_ids.lock().unwrap().len(), md_calls);
}

#[tokio::test]
async fn c2c_chunked_markdown_failure_falls_back_that_chunk_only() {
    let sender = RecordingC2cSender {
        fail_on: Mutex::new(Some(1)),
        ..Default::default()
    };
    let md = "长回复内容 ".repeat(400);
    let outbound = OutboundMessage::Markdown {
        markdown: MarkdownPayload::new(md.clone()),
        fallback_text: md.clone(),
    };
    let limits = ChunkLimits::new(800, 800);
    let result = send_c2c_outbound_chunked(&sender, &c2c_target(), &outbound, &limits, |_, _| {})
        .await
        .unwrap();
    // 第二段 markdown 失败 -> 该段 fallback text 发送，整条仍成功。
    assert!(result.len() >= 2);
    let text_calls = sender.text_calls.lock().unwrap().clone();
    // 只有失败的那一段走 fallback。
    assert!(text_calls.iter().any(|c| c.contains("长回复内容")));
}

#[tokio::test]
async fn c2c_partially_sent_returns_error_with_progress() {
    // 让第二段文本也失败（fallback_text 为空避免回退，构造纯文本，第二段 text 失败）。
    struct FailOnText {
        fail: Mutex<usize>,
        count: Mutex<usize>,
    }
    impl OutboundSender for FailOnText {
        fn send_text<'a>(&'a self, _t: &'a C2cReplyTarget, _text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                let mut c = self.count.lock().unwrap();
                let i = *c;
                *c += 1;
                if i == *self.fail.lock().unwrap() {
                    return Err(ApiError::Unsupported("text"));
                }
                Ok(SendMessageIds {
                    message_id: Some(format!("tid-{i}")),
                    ref_index_id: Some(format!("REFIDX_tid_{i}")),
                })
            })
        }
        fn send_markdown<'a>(
            &'a self,
            _t: &'a C2cReplyTarget,
            _m: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("markdown")) })
        }
        fn send_image<'a>(
            &'a self,
            _t: &'a C2cReplyTarget,
            _i: &'a crate::media::ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("image")) })
        }
    }
    let sender = FailOnText {
        fail: Mutex::new(1),
        count: Mutex::new(0),
    };
    let text = "甲".repeat(2500);
    let outbound = OutboundMessage::Text { text };
    let limits = ChunkLimits::new(1800, 1000);
    let err = send_c2c_outbound_chunked(&sender, &c2c_target(), &outbound, &limits, |_, _| {})
        .await
        .unwrap_err();
    match err {
        OutboundSendError::PartiallySent {
            sent_chunks,
            total_chunks,
            failed_chunk_index,
            remaining_chars,
            source: _,
        } => {
            assert_eq!(sent_chunks, 1);
            assert!(total_chunks >= 2);
            assert_eq!(failed_chunk_index, 1);
            assert!(remaining_chars > 0);
        }
        other => panic!("expected PartiallySent, got {other:?}"),
    }
}

#[tokio::test]
async fn c2c_not_sent_when_first_chunk_fails() {
    // 用纯文本多段回复，sender 的 send_text 首段失败 -> NotSent。
    struct FailOnText {
        count: Mutex<usize>,
    }
    impl OutboundSender for FailOnText {
        fn send_text<'a>(&'a self, _t: &'a C2cReplyTarget, _text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                let mut c = self.count.lock().unwrap();
                let i = *c;
                *c += 1;
                if i == 0 {
                    return Err(ApiError::Unsupported("text"));
                }
                Ok(SendMessageIds {
                    message_id: Some(format!("tid-{i}")),
                    ref_index_id: Some(format!("REFIDX_tid_{i}")),
                })
            })
        }
        fn send_markdown<'a>(
            &'a self,
            _t: &'a C2cReplyTarget,
            _m: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("markdown")) })
        }
        fn send_image<'a>(
            &'a self,
            _t: &'a C2cReplyTarget,
            _i: &'a crate::media::ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("image")) })
        }
    }
    let sender = FailOnText {
        count: Mutex::new(0),
    };
    let text = "甲".repeat(2500);
    let outbound = OutboundMessage::Text { text };
    let limits = ChunkLimits::new(1800, 1000);
    let err = send_c2c_outbound_chunked(&sender, &c2c_target(), &outbound, &limits, |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(err, OutboundSendError::NotSent { .. }));
}

#[derive(Debug, Default)]
struct RecordingGroupSender {
    markdown_calls: Mutex<Vec<String>>,
    text_calls: Mutex<Vec<String>>,
}

impl crate::api::GroupOutboundSender for RecordingGroupSender {
    fn send_text<'a>(&'a self, _target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.text_calls.lock().unwrap().push(text.to_owned());
            Ok(SendMessageIds::none())
        })
    }
    fn send_markdown<'a>(
        &'a self,
        _target: &'a GroupReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.markdown_calls
                .lock()
                .unwrap()
                .push(markdown.content.clone());
            Ok(SendMessageIds::none())
        })
    }
}

fn group_target() -> GroupReplyTarget {
    GroupReplyTarget {
        group_openid: "g1".to_owned(),
        msg_id: Some("m1".to_owned()),
    }
}

#[tokio::test]
async fn group_chunked_send_dispatches_each_chunk() {
    let sender = RecordingGroupSender::default();
    let md = "群长回复内容 ".repeat(400);
    let outbound = OutboundMessage::Markdown {
        markdown: MarkdownPayload::new(md.clone()),
        fallback_text: md,
    };
    let limits = ChunkLimits::new(800, 800);
    let mut sent = 0usize;
    send_group_outbound_chunked(&sender, &group_target(), &outbound, &limits, |_, _| {
        sent += 1;
    })
    .await
    .unwrap();
    let md_calls = sender.markdown_calls.lock().unwrap().len();
    assert!(md_calls >= 2);
    assert_eq!(sent, md_calls);
}
