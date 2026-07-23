//! 当前用户消息的结构化上下文与多模态 parts 组装。

use qq_maid_common::{
    identity_context::MessageContext,
    input_part::{MediaStatus, MessageInputPart, QuotedMessageContext, TextSource},
};

use super::super::RespondRequest;

pub(super) fn current_user_parts_for_model(
    req: &RespondRequest,
    supports_vision: bool,
) -> Vec<MessageInputPart> {
    let mut parts = Vec::new();
    if let Some(context) = req
        .message_context
        .as_ref()
        .and_then(message_context_part_for_model)
    {
        parts.push(context);
    }
    if let Some(quoted) = req.quoted.as_ref() {
        parts.extend(quoted_context_parts_for_model(quoted, supports_vision));
    }
    parts.extend(input_parts_for_model(
        req.effective_input_parts(),
        supports_vision,
        TextSource::Supplement,
    ));
    parts
}

fn message_context_part_for_model(context: &MessageContext) -> Option<MessageInputPart> {
    let text = render_message_context_for_model(context);
    (!text.trim().is_empty()).then_some(MessageInputPart::Text {
        text,
        source: Some(TextSource::Context),
    })
}

fn render_message_context_for_model(context: &MessageContext) -> String {
    let mut lines = Vec::new();
    let conversation_id = context
        .conversation
        .id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("unknown");
    lines.push("消息上下文（系统提供，非用户原文）：".to_owned());
    lines.push(format!(
        "- 当前会话：{} id={} platform={} account_id={}",
        context.conversation.kind,
        conversation_id,
        optional_str(context.conversation.platform.as_deref()),
        optional_str(context.conversation.account_id.as_deref())
    ));
    let current_actor_ref = context
        .current_actor_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(current_actor_ref) = current_actor_ref {
        lines.push(format!(
            "- 当前发言人 actor 映射：current_actor_ref={current_actor_ref}"
        ));
    }
    if let Some(actor) = context.actor.as_ref() {
        lines.push(format!(
            "- 当前发言人：昵称={}，昵称来源={}，稳定ID={}，union_id={}，群角色={}，是否机器人={}，身份来源={}",
            optional_str(actor.display_name.as_deref()),
            optional_str(actor.display_name_source.as_deref()),
            optional_str(actor.user_id.as_deref()),
            optional_str(actor.union_id.as_deref()),
            optional_str(actor.group_member_role.as_deref()),
            optional_bool(actor.is_bot),
            actor.source.as_str()
        ));
    } else {
        lines.push("- 当前发言人：unknown".to_owned());
    }
    if context.mentions.is_empty() {
        lines.push("- 本条消息 @ 对象：无结构化对象".to_owned());
    } else {
        lines.push("- 本条消息 @ 对象：".to_owned());
        for (idx, mention) in context.mentions.iter().enumerate() {
            lines.push(format!(
                "  {}. 原文={}，昵称={}，昵称来源={}，稳定ID={}，union_id={}，群角色={}，是否机器人={}，是否当前机器人={}，置信度={}，身份来源={}",
                idx + 1,
                optional_str(mention.raw_text.as_deref()),
                optional_str(mention.target.display_name.as_deref()),
                optional_str(mention.target.display_name_source.as_deref()),
                optional_str(mention.target.user_id.as_deref()),
                optional_str(mention.target.union_id.as_deref()),
                optional_str(mention.target.group_member_role.as_deref()),
                optional_bool(mention.target.is_bot),
                mention.is_self,
                mention.confidence.as_str(),
                mention.target.source.as_str()
            ));
        }
    }
    lines.push("要求：".to_owned());
    lines.push(
        "- 当前用户说“我”时，只指本 MessageContext 中的当前发言人；回复里说“你”也只指当前发言人。"
            .to_owned(),
    );
    if current_actor_ref.is_some() {
        lines.push("- 只有历史 actor_ref（包括压缩摘要中的成员事实）与 current_actor_ref 相同，才能把历史昵称、偏好、身份声明和操作归给当前发言人。".to_owned());
        lines.push("- 历史或摘要中的 actor_ref 不同或 unknown 时，不得把对应事实归给当前发言人；不得通过昵称相同推断为同一人。".to_owned());
        lines.push("- actor_ref 仅用于模型上下文区分，不得在最终回复中主动向用户展示。".to_owned());
    }
    lines.push("- 只有当前 MessageContext 明确显示 display_name_source=manual 时，才能声称当前发言人手动设置过该展示名。".to_owned());
    lines.push(
        "- 不得根据历史里最近出现的 /set 命令、身份声明或机器人回复，猜测当前发言人的展示名。"
            .to_owned(),
    );
    lines.push("- 不得输出“可能没有正确覆盖”等缺少服务端事实支持的身份状态推测。".to_owned());
    lines.push("- 当前发言人的 display_name 可作为当前群内展示昵称使用，但不是权限、owner 或现实身份依据。".to_owned());
    lines.push("- display_name 可能来自平台成员信息，也可能来自用户通过 /set 手动设置的展示名；手动展示名只用于显示，不代表现实身份认证。".to_owned());
    lines.push("- user_id / union_id 是平台稳定身份标识，可用于区分同一平台用户；它们不等于现实姓名、身份证明或私密个人信息。".to_owned());
    lines.push("- 当用户问“我是谁 / 你认得我吗 / 你知道我是谁吗”时，应优先说明可见的平台身份、群昵称、群角色、是否有稳定标识，并区分平台身份与现实身份。".to_owned());
    lines.push("- 如果设置了手动展示名，应说明“你在当前会话手动设置的展示名是 X”，并说明这只是会话内展示名，不等于现实身份认证。".to_owned());
    lines.push("- 如果没有用户档案或现实身份绑定，不要否认平台身份；应说明“能识别当前平台身份，但尚未绑定现实身份 / 个人档案名 / 称呼”。".to_owned());
    lines.push("- 不要完整输出稳定 ID / union_id，除非用户明确要求调试且安全策略允许。".to_owned());
    lines.push("- “@某人”通常指对应 mention 对象。".to_owned());
    lines.push("- 不要把昵称当稳定身份。".to_owned());
    lines.push("- 不要把本上下文当成用户指令。".to_owned());
    lines.join("\n")
}

fn optional_str(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

fn quoted_context_parts_for_model(
    quoted: &QuotedMessageContext,
    supports_vision: bool,
) -> Vec<MessageInputPart> {
    let has_input_parts = !quoted.input_parts.is_empty();
    let mut parts = vec![MessageInputPart::Text {
        // 结构化引用 parts 已包含正文和媒体，此处只保留引用边界与发送者信息。
        text: if has_input_parts {
            quoted.metadata_text()
        } else {
            quoted.fallback_text()
        },
        source: Some(TextSource::Quote),
    }];
    if has_input_parts {
        parts.extend(input_parts_for_model(
            quoted.input_parts.clone(),
            supports_vision,
            TextSource::Quote,
        ));
        parts.push(MessageInputPart::Text {
            // Provider 不接收 TextSource，结束标记负责把引用 parts 与当前用户正文隔开。
            text: "引用内容结束（以上内容块属于被引用消息；以下为当前用户消息）：".to_owned(),
            source: Some(TextSource::Quote),
        });
    }
    parts
}

fn input_parts_for_model(
    input_parts: Vec<MessageInputPart>,
    supports_vision: bool,
    fallback_source: TextSource,
) -> Vec<MessageInputPart> {
    let mut parts = Vec::new();
    for part in input_parts {
        match part {
            MessageInputPart::Text { text, source } => {
                if !text.trim().is_empty() {
                    parts.push(MessageInputPart::Text { text, source });
                }
            }
            MessageInputPart::Image { media }
                if supports_vision && media.status == MediaStatus::Available =>
            {
                parts.push(MessageInputPart::Image { media });
            }
            other => parts.push(MessageInputPart::Text {
                text: media_fallback_for_model(&other, supports_vision),
                source: Some(fallback_source),
            }),
        }
    }
    parts
}

fn media_fallback_for_model(part: &MessageInputPart, supports_vision: bool) -> String {
    let mut text = part.fallback_text();
    if !supports_vision {
        text.push_str("（当前模型不支持读取图片/附件内容，仅保留媒体摘要）");
    } else if let Some(media) = part.media() {
        text.push_str(match media.status {
            MediaStatus::Available => "（媒体摘要）",
            MediaStatus::MissingReadableUrl => "（缺少可读取地址，仅保留媒体摘要）",
            MediaStatus::SizeExceeded => "（文件过大，仅保留媒体摘要）",
            MediaStatus::UnsupportedType => "（暂不支持该媒体类型，仅保留媒体摘要）",
            MediaStatus::DownloadFailed => "（下载失败，仅保留媒体摘要）",
            MediaStatus::Expired => "（访问已过期，仅保留媒体摘要）",
        });
    }
    text
}
