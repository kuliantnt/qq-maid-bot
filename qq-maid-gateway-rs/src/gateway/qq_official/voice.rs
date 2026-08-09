//! QQ 官方最终回复的语音投递尝试。
//!
//! 本模块只消费 Core 的结构化 delivery hint，并通过 `AssistantOutput::speakable_text`
//! 获取唯一朗读正文。任一阶段失败都返回文字回退信号，不修改原始回复，也不在这里
//! 发送第二条消息。

use qq_maid_core::service::{CoreDeliveryHint, CoreResponse};
use tracing::{debug, warn};

use crate::{
    api::{
        ApiError, C2cReplyTarget, GroupOutboundSender, GroupReplyTarget, OutboundSender,
        SendMessageIds,
    },
    tts::TtsProvider,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VoiceFallbackStage {
    NotRequested,
    EmptyAfterCleaning,
    Tts,
    Upload,
    Send,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VoiceDeliveryAttempt {
    Delivered(SendMessageIds),
    UseText(VoiceFallbackStage),
}

pub(super) async fn try_c2c_voice_delivery<S: OutboundSender + ?Sized>(
    provider: Option<&dyn TtsProvider>,
    sender: &S,
    target: &C2cReplyTarget,
    response: &CoreResponse,
) -> VoiceDeliveryAttempt {
    let Some(audio_url) = synthesize_if_requested(provider, response).await else {
        return fallback_before_send(response);
    };
    match sender.send_voice_url(target, &audio_url).await {
        Ok(sent_ids) => {
            debug!(voice_delivery = "c2c", "QQ 语音最终回复已发送");
            VoiceDeliveryAttempt::Delivered(sent_ids)
        }
        Err(error) => fallback_from_api_error(error),
    }
}

pub(super) async fn try_group_voice_delivery<S: GroupOutboundSender + ?Sized>(
    provider: Option<&dyn TtsProvider>,
    sender: &S,
    target: &GroupReplyTarget,
    response: &CoreResponse,
) -> VoiceDeliveryAttempt {
    let Some(audio_url) = synthesize_if_requested(provider, response).await else {
        return fallback_before_send(response);
    };
    match sender.send_voice_url(target, &audio_url).await {
        Ok(sent_ids) => {
            debug!(voice_delivery = "group", "QQ 语音最终回复已发送");
            VoiceDeliveryAttempt::Delivered(sent_ids)
        }
        Err(error) => fallback_from_api_error(error),
    }
}

async fn synthesize_if_requested(
    provider: Option<&dyn TtsProvider>,
    response: &CoreResponse,
) -> Option<String> {
    if response.delivery_hint != Some(CoreDeliveryHint::Voice) {
        return None;
    }
    let Some(text) = response
        .output
        .as_ref()
        .and_then(|output| output.speakable_text())
    else {
        warn!(
            voice_fallback_stage = "empty_after_cleaning",
            "语音回复没有可朗读文本，将发送原始文本"
        );
        return None;
    };
    let Some(provider) = provider else {
        warn!(
            voice_fallback_stage = "provider_unavailable",
            "语音 Provider 不可用，将发送原始文本"
        );
        return None;
    };
    match provider.synthesize(&text).await {
        Ok(audio_url) => Some(audio_url),
        Err(error) => {
            warn!(
                voice_fallback_stage = "tts",
                tts_error = error.code(),
                "语音合成失败，将发送原始文本"
            );
            None
        }
    }
}

fn fallback_before_send(response: &CoreResponse) -> VoiceDeliveryAttempt {
    if response.delivery_hint != Some(CoreDeliveryHint::Voice) {
        VoiceDeliveryAttempt::UseText(VoiceFallbackStage::NotRequested)
    } else if response
        .output
        .as_ref()
        .and_then(|output| output.speakable_text())
        .is_none()
    {
        VoiceDeliveryAttempt::UseText(VoiceFallbackStage::EmptyAfterCleaning)
    } else {
        VoiceDeliveryAttempt::UseText(VoiceFallbackStage::Tts)
    }
}

fn fallback_from_api_error(error: ApiError) -> VoiceDeliveryAttempt {
    let stage = match error {
        ApiError::VoiceUpload(_) => VoiceFallbackStage::Upload,
        ApiError::VoiceSend(_) => VoiceFallbackStage::Send,
        _ => VoiceFallbackStage::Send,
    };
    warn!(
        voice_fallback_stage = match stage {
            VoiceFallbackStage::Upload => "qq_upload",
            _ => "qq_send",
        },
        error = %error.log_summary(),
        "QQ 语音发送失败，将发送原始文本"
    );
    VoiceDeliveryAttempt::UseText(stage)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use qq_maid_common::output_part::AssistantOutput;

    use super::*;
    use crate::{
        api::{GroupReplyTarget, SendFuture},
        markdown::MarkdownPayload,
        tts::{TtsFuture, TtsProvider},
    };

    struct StaticProvider;

    impl TtsProvider for StaticProvider {
        fn synthesize<'a>(&'a self, text: &'a str) -> TtsFuture<'a> {
            Box::pin(async move {
                assert_eq!(text, "群聊朗读正文");
                Ok("https://audio.example.test/group.wav".to_owned())
            })
        }
    }

    #[derive(Default)]
    struct GroupVoiceSender {
        urls: Mutex<Vec<String>>,
    }

    impl GroupOutboundSender for GroupVoiceSender {
        fn send_text<'a>(
            &'a self,
            _target: &'a GroupReplyTarget,
            _text: &'a str,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("text")) })
        }

        fn send_markdown<'a>(
            &'a self,
            _target: &'a GroupReplyTarget,
            _markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async { Err(ApiError::Unsupported("markdown")) })
        }

        fn send_voice_url<'a>(
            &'a self,
            _target: &'a GroupReplyTarget,
            audio_url: &'a str,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.urls.lock().unwrap().push(audio_url.to_owned());
                Ok(SendMessageIds::message_id("group-voice-id"))
            })
        }
    }

    #[tokio::test]
    async fn group_voice_delivery_uses_same_structured_hint_and_common_text_entry() {
        let sender = GroupVoiceSender::default();
        let response = CoreResponse {
            output: Some(AssistantOutput::markdown(
                "群聊文字 fallback",
                "# 群聊朗读正文",
            )),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
            delivery_hint: Some(CoreDeliveryHint::Voice),
        };

        let attempt = try_group_voice_delivery(
            Some(&StaticProvider),
            &sender,
            &GroupReplyTarget {
                group_openid: "group-1".to_owned(),
                msg_id: Some("message-1".to_owned()),
            },
            &response,
        )
        .await;

        assert!(matches!(attempt, VoiceDeliveryAttempt::Delivered(_)));
        assert_eq!(
            sender.urls.lock().unwrap().as_slice(),
            ["https://audio.example.test/group.wav"]
        );
    }
}
