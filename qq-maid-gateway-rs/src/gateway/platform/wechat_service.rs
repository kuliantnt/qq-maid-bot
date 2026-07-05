//! 微信服务号协议 adapter。
//!
//! 本模块收口微信 URL 验证、XML 文本消息解析、统一入站模型映射和同步 XML 回复渲染。
//! Core 只接收平台无关的 `InboundMessage` / `CoreRequest`，不能看到微信 XML 字段。

use quick_xml::{Reader, events::Event};
use sha1::{Digest, Sha1};
use thiserror::Error;

use crate::render::OutboundMessage;

use super::model::{Actor, ConversationTarget, InboundMessage, Platform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WechatTextMessage {
    pub(crate) to_user_name: String,
    pub(crate) from_user_name: String,
    pub(crate) create_time: Option<String>,
    pub(crate) content: String,
    pub(crate) msg_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WechatInboundMessage {
    Text(WechatTextMessage),
    Unsupported { msg_type: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum WechatXmlError {
    #[error("invalid wechat xml: {0}")]
    InvalidXml(String),
    #[error("missing required wechat xml field: {0}")]
    MissingField(&'static str),
}

#[derive(Debug, Default)]
struct RawWechatMessage {
    to_user_name: Option<String>,
    from_user_name: Option<String>,
    create_time: Option<String>,
    msg_type: Option<String>,
    content: Option<String>,
    msg_id: Option<String>,
}

pub(crate) fn verify_signature(token: &str, timestamp: &str, nonce: &str, signature: &str) -> bool {
    let mut parts = [token, timestamp, nonce];
    parts.sort_unstable();
    let digest = Sha1::digest(parts.concat().as_bytes());
    let actual = format!("{digest:x}");
    actual.eq_ignore_ascii_case(signature.trim())
}

pub(crate) fn parse_message_xml(xml: &str) -> Result<WechatInboundMessage, WechatXmlError> {
    let raw = parse_raw_xml(xml)?;
    let msg_type = required(raw.msg_type, "MsgType")?;
    if msg_type != "text" {
        return Ok(WechatInboundMessage::Unsupported { msg_type });
    }
    Ok(WechatInboundMessage::Text(WechatTextMessage {
        to_user_name: required(raw.to_user_name, "ToUserName")?,
        from_user_name: required(raw.from_user_name, "FromUserName")?,
        create_time: raw.create_time,
        content: raw.content.unwrap_or_default(),
        msg_id: required(raw.msg_id, "MsgId")?,
    }))
}

pub(crate) fn inbound_from_text_message(message: &WechatTextMessage) -> InboundMessage {
    InboundMessage {
        platform: Platform::WechatService,
        // 使用 ToUserName 作为 account_id：它来自微信回调原文，可区分同一进程未来承载的多个服务号。
        account_id: Some(message.to_user_name.clone()),
        conversation: ConversationTarget::ServiceAccount {
            target_id: message.from_user_name.clone(),
        },
        actor: Actor {
            sender_id: Some(message.from_user_name.clone()),
            display_name: None,
            group_member_role: None,
            is_bot: false,
        },
        message_id: message.msg_id.clone(),
        current_msg_idx: None,
        timestamp: message.create_time.clone(),
        text: message.content.clone(),
        input_parts: if message.content.trim().is_empty() {
            Vec::new()
        } else {
            vec![qq_maid_common::input_part::MessageInputPart::text(
                message.content.clone(),
            )]
        },
        attachments: Vec::new(),
        quoted: None,
        mentioned_bot: false,
    }
}

pub(crate) fn render_text_reply_xml(
    inbound: &WechatTextMessage,
    outbound: &OutboundMessage,
    now_unix_seconds: i64,
) -> String {
    render_text_reply_xml_from_text(inbound, outbound.fallback_text(), now_unix_seconds)
}

pub(crate) fn render_text_reply_xml_from_text(
    inbound: &WechatTextMessage,
    text: &str,
    now_unix_seconds: i64,
) -> String {
    format!(
        "<xml><ToUserName>{}</ToUserName><FromUserName>{}</FromUserName><CreateTime>{}</CreateTime><MsgType>text</MsgType><Content>{}</Content></xml>",
        escape_xml_text(&inbound.from_user_name),
        escape_xml_text(&inbound.to_user_name),
        now_unix_seconds,
        escape_xml_text(text)
    )
}

fn parse_raw_xml(xml: &str) -> Result<RawWechatMessage, WechatXmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut current = None::<String>;
    let mut raw = RawWechatMessage::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                current = Some(String::from_utf8_lossy(event.name().as_ref()).into_owned());
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .unescape()
                    .map_err(|err| WechatXmlError::InvalidXml(err.to_string()))?
                    .into_owned();
                assign_field(&mut raw, current.as_deref(), value);
            }
            Ok(Event::CData(text)) => {
                let value = text
                    .decode()
                    .map_err(|err| WechatXmlError::InvalidXml(err.to_string()))?
                    .into_owned();
                assign_field(&mut raw, current.as_deref(), value);
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) => break,
            Err(err) => return Err(WechatXmlError::InvalidXml(err.to_string())),
            _ => {}
        }
    }

    Ok(raw)
}

fn assign_field(raw: &mut RawWechatMessage, field: Option<&str>, value: String) {
    match field {
        Some("ToUserName") => raw.to_user_name = Some(value),
        Some("FromUserName") => raw.from_user_name = Some(value),
        Some("CreateTime") => raw.create_time = Some(value),
        Some("MsgType") => raw.msg_type = Some(value),
        Some("Content") => raw.content = Some(value),
        Some("MsgId") => raw.msg_id = Some(value),
        _ => {}
    }
}

fn required(value: Option<String>, field: &'static str) -> Result<String, WechatXmlError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or(WechatXmlError::MissingField(field))
}

fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{markdown::MarkdownPayload, render::OutboundMessage};
    use qq_maid_core::service::{CoreConversation, Platform as CorePlatform};

    const TEXT_XML: &str = r#"<xml>
<ToUserName><![CDATA[gh_service]]></ToUserName>
<FromUserName><![CDATA[user_openid]]></FromUserName>
<CreateTime>1460537339</CreateTime>
<MsgType><![CDATA[text]]></MsgType>
<Content><![CDATA[你好 <bot> & bye]]></Content>
<MsgId>1234567890123456</MsgId>
</xml>"#;

    #[test]
    fn verifies_wechat_signature() {
        assert!(verify_signature(
            "token",
            "timestamp",
            "nonce",
            "6db4861c77e0633e0105672fcd41c9fc2766e26e"
        ));
        assert!(verify_signature(
            "weixin",
            "timestamp",
            "nonce",
            "877a7f05557e3052fa30b9bf4a65046c933cbb79"
        ));
        assert!(!verify_signature("token", "timestamp", "nonce", "bad"));
    }

    #[test]
    fn parses_text_xml() {
        let parsed = parse_message_xml(TEXT_XML).unwrap();
        let WechatInboundMessage::Text(message) = parsed else {
            panic!("expected text message");
        };

        assert_eq!(message.to_user_name, "gh_service");
        assert_eq!(message.from_user_name, "user_openid");
        assert_eq!(message.create_time.as_deref(), Some("1460537339"));
        assert_eq!(message.content, "你好 <bot> & bye");
        assert_eq!(message.msg_id, "1234567890123456");
    }

    #[test]
    fn parses_unsupported_message_type_without_panic() {
        let parsed = parse_message_xml(
            "<xml><ToUserName>gh</ToUserName><FromUserName>u</FromUserName><MsgType>image</MsgType></xml>",
        )
        .unwrap();

        assert_eq!(
            parsed,
            WechatInboundMessage::Unsupported {
                msg_type: "image".to_owned()
            }
        );
    }

    #[test]
    fn text_message_maps_to_unified_inbound() {
        let WechatInboundMessage::Text(message) = parse_message_xml(TEXT_XML).unwrap() else {
            panic!("expected text message");
        };
        let inbound = inbound_from_text_message(&message);

        assert_eq!(inbound.platform, Platform::WechatService);
        assert_eq!(inbound.account_id.as_deref(), Some("gh_service"));
        assert_eq!(
            inbound.conversation,
            ConversationTarget::ServiceAccount {
                target_id: "user_openid".to_owned()
            }
        );
        assert_eq!(inbound.actor.sender_id.as_deref(), Some("user_openid"));
        assert_eq!(inbound.message_id, "1234567890123456");
        assert_eq!(inbound.timestamp.as_deref(), Some("1460537339"));
        assert_eq!(inbound.text, "你好 <bot> & bye");
        assert!(inbound.attachments.is_empty());
        assert!(inbound.quoted.is_none());
        assert!(!inbound.mentioned_bot);
    }

    #[test]
    fn text_message_maps_to_wechat_core_request() {
        let WechatInboundMessage::Text(message) = parse_message_xml(TEXT_XML).unwrap() else {
            panic!("expected text message");
        };
        let inbound = inbound_from_text_message(&message);
        let request = super::super::to_core_request(&inbound, inbound.text.clone()).unwrap();

        assert_eq!(request.platform, CorePlatform::WechatService);
        assert_eq!(
            request.conversation,
            CoreConversation::ServiceAccount {
                account_id: Some("gh_service".to_owned()),
                peer_id: "user_openid".to_owned(),
            }
        );
        assert_eq!(
            super::super::core_scope_key(&inbound).unwrap(),
            "platform:wechat_service:account:gh_service:private:user_openid"
        );
    }

    #[test]
    fn renders_sync_text_reply_with_escaped_content_and_reversed_users() {
        let inbound = WechatTextMessage {
            to_user_name: "gh_service".to_owned(),
            from_user_name: "user_openid".to_owned(),
            create_time: Some("1".to_owned()),
            content: "hi".to_owned(),
            msg_id: "m1".to_owned(),
        };
        let xml = render_text_reply_xml_from_text(&inbound, r#"a < b & "c" 'd'"#, 42);

        assert!(xml.contains("<ToUserName>user_openid</ToUserName>"));
        assert!(xml.contains("<FromUserName>gh_service</FromUserName>"));
        assert!(xml.contains("<CreateTime>42</CreateTime>"));
        assert!(xml.contains("<Content>a &lt; b &amp; &quot;c&quot; &apos;d&apos;</Content>"));
    }

    #[test]
    fn markdown_outbound_degrades_to_fallback_text_for_wechat_xml() {
        let inbound = WechatTextMessage {
            to_user_name: "gh".to_owned(),
            from_user_name: "u".to_owned(),
            create_time: None,
            content: String::new(),
            msg_id: "m".to_owned(),
        };
        let outbound = OutboundMessage::Markdown {
            markdown: MarkdownPayload::new("**hello**"),
            fallback_text: "hello".to_owned(),
        };

        let xml = render_text_reply_xml(&inbound, &outbound, 1);
        assert!(xml.contains("<Content>hello</Content>"));
        assert!(!xml.contains("**hello**"));
    }
}
