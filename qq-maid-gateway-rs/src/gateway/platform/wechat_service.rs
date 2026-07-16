//! 微信服务号协议 adapter。
//!
//! 本模块收口微信 URL 验证、XML 文本消息解析、统一入站模型映射和同步 XML 回复渲染。
//! Core 只接收平台无关的 `InboundMessage` / `CoreRequest`，不能看到微信 XML 字段。

use aes::{
    Aes256,
    cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::NoPadding},
};
use base64::{
    Engine as _, alphabet,
    engine::{GeneralPurpose, GeneralPurposeConfig, general_purpose::STANDARD},
};
use cbc::{Decryptor, Encryptor};
use quick_xml::{Reader, events::Event};
use sha1::{Digest, Sha1};
use thiserror::Error;

use qq_maid_common::identity_context::IdentitySource;

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

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum WechatCryptoError {
    #[error("invalid EncodingAESKey")]
    InvalidEncodingAesKey,
    #[error("invalid encrypted message encoding")]
    InvalidCiphertext,
    #[error("invalid encrypted message padding")]
    InvalidPadding,
    #[error("invalid encrypted message layout")]
    InvalidMessageLayout,
    #[error("encrypted message AppID does not match configured AppID")]
    AppIdMismatch,
    #[error("decrypted message is not valid UTF-8")]
    InvalidUtf8,
    #[error("message is too large to encrypt")]
    MessageTooLarge,
    #[error("secure random generation failed")]
    RandomGeneration,
}

/// 微信公众平台安全模式消息加解密器。
///
/// 密钥、Token 与 AppID 只保存在 Gateway 平台边界内；错误信息不得包含这些原值。
pub(crate) struct WechatMessageCrypto {
    token: String,
    app_id: String,
    aes_key: [u8; 32],
}

// 微信官方生成的 43 位 EncodingAESKey 可能包含非零尾随位；官方 SDK 的 Base64
// 解码器会忽略这些位，因此这里只对密钥保持同样兼容行为。
const WECHAT_ENCODING_AES_KEY_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
);

#[derive(Debug, Default)]
struct RawWechatMessage {
    to_user_name: Option<String>,
    from_user_name: Option<String>,
    create_time: Option<String>,
    msg_type: Option<String>,
    content: Option<String>,
    msg_id: Option<String>,
}

#[derive(Debug, Default)]
struct RawEncryptedEnvelope {
    encrypted: Option<String>,
}

pub(crate) fn verify_signature(token: &str, timestamp: &str, nonce: &str, signature: &str) -> bool {
    let actual = sha1_signature(&[token, timestamp, nonce]);
    signature_matches(&actual, signature)
}

impl WechatMessageCrypto {
    pub(crate) fn new(
        token: &str,
        app_id: &str,
        encoding_aes_key: &str,
    ) -> Result<Self, WechatCryptoError> {
        if encoding_aes_key.len() != 43 {
            return Err(WechatCryptoError::InvalidEncodingAesKey);
        }
        let decoded = WECHAT_ENCODING_AES_KEY_BASE64
            .decode(format!("{encoding_aes_key}="))
            .map_err(|_| WechatCryptoError::InvalidEncodingAesKey)?;
        let aes_key: [u8; 32] = decoded
            .try_into()
            .map_err(|_| WechatCryptoError::InvalidEncodingAesKey)?;
        Ok(Self {
            token: token.to_owned(),
            app_id: app_id.to_owned(),
            aes_key,
        })
    }

    pub(crate) fn verify_message_signature(
        &self,
        timestamp: &str,
        nonce: &str,
        encrypted: &str,
        signature: &str,
    ) -> bool {
        signature_matches(
            &self.message_signature(timestamp, nonce, encrypted),
            signature,
        )
    }

    pub(crate) fn message_signature(
        &self,
        timestamp: &str,
        nonce: &str,
        encrypted: &str,
    ) -> String {
        sha1_signature(&[&self.token, timestamp, nonce, encrypted])
    }

    pub(crate) fn decrypt(&self, encrypted: &str) -> Result<String, WechatCryptoError> {
        let ciphertext = STANDARD
            .decode(encrypted.trim())
            .map_err(|_| WechatCryptoError::InvalidCiphertext)?;
        if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
            return Err(WechatCryptoError::InvalidCiphertext);
        }
        let decryptor = Decryptor::<Aes256>::new_from_slices(&self.aes_key, &self.aes_key[..16])
            .map_err(|_| WechatCryptoError::InvalidEncodingAesKey)?;
        let padded = decryptor
            .decrypt_padded_vec::<NoPadding>(&ciphertext)
            .map_err(|_| WechatCryptoError::InvalidCiphertext)?;
        let plaintext = remove_wechat_pkcs7_padding(padded)?;
        if plaintext.len() < 20 {
            return Err(WechatCryptoError::InvalidMessageLayout);
        }
        let message_len = u32::from_be_bytes(
            plaintext[16..20]
                .try_into()
                .map_err(|_| WechatCryptoError::InvalidMessageLayout)?,
        ) as usize;
        let message_end = 20usize
            .checked_add(message_len)
            .filter(|end| *end <= plaintext.len())
            .ok_or(WechatCryptoError::InvalidMessageLayout)?;
        if &plaintext[message_end..] != self.app_id.as_bytes() {
            return Err(WechatCryptoError::AppIdMismatch);
        }
        String::from_utf8(plaintext[20..message_end].to_vec())
            .map_err(|_| WechatCryptoError::InvalidUtf8)
    }

    pub(crate) fn encrypt(&self, message: &str) -> Result<String, WechatCryptoError> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| WechatCryptoError::RandomGeneration)?;
        self.encrypt_with_random(message, random)
    }

    pub(crate) fn encrypt_with_random(
        &self,
        message: &str,
        random: [u8; 16],
    ) -> Result<String, WechatCryptoError> {
        let message_len =
            u32::try_from(message.len()).map_err(|_| WechatCryptoError::MessageTooLarge)?;
        let mut plaintext = Vec::with_capacity(20 + message.len() + self.app_id.len() + 32);
        plaintext.extend_from_slice(&random);
        plaintext.extend_from_slice(&message_len.to_be_bytes());
        plaintext.extend_from_slice(message.as_bytes());
        plaintext.extend_from_slice(self.app_id.as_bytes());
        add_wechat_pkcs7_padding(&mut plaintext);

        let encryptor = Encryptor::<Aes256>::new_from_slices(&self.aes_key, &self.aes_key[..16])
            .map_err(|_| WechatCryptoError::InvalidEncodingAesKey)?;
        let ciphertext = encryptor.encrypt_padded_vec::<NoPadding>(&plaintext);
        Ok(STANDARD.encode(ciphertext))
    }
}

pub(crate) fn random_callback_nonce() -> Result<String, WechatCryptoError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| WechatCryptoError::RandomGeneration)?;
    let mut nonce = String::with_capacity(32);
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut nonce, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(nonce)
}

pub(crate) fn parse_encrypted_message_xml(xml: &str) -> Result<String, WechatXmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut current = None::<String>;
    let mut raw = RawEncryptedEnvelope::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                current = Some(String::from_utf8_lossy(event.name().as_ref()).into_owned());
            }
            Ok(Event::Text(text)) => {
                // quick-xml 0.41 移除了 `BytesText::unescape`；微信回包是 XML 1.0 文本节点，
                // 用 `xml10_content` 恢复实体转义并保持原有行为。
                let value = text
                    .xml10_content()
                    .map_err(|err| WechatXmlError::InvalidXml(err.to_string()))?
                    .into_owned();
                if current.as_deref() == Some("Encrypt") {
                    raw.encrypted = Some(value);
                }
            }
            Ok(Event::CData(text)) => {
                let value = text
                    .decode()
                    .map_err(|err| WechatXmlError::InvalidXml(err.to_string()))?
                    .into_owned();
                if current.as_deref() == Some("Encrypt") {
                    raw.encrypted = Some(value);
                }
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) => break,
            Err(err) => return Err(WechatXmlError::InvalidXml(err.to_string())),
            _ => {}
        }
    }

    required(raw.encrypted, "Encrypt")
}

pub(crate) fn render_encrypted_reply_xml(
    encrypted: &str,
    signature: &str,
    timestamp: &str,
    nonce: &str,
) -> String {
    format!(
        "<xml><Encrypt><![CDATA[{encrypted}]]></Encrypt><MsgSignature><![CDATA[{signature}]]></MsgSignature><TimeStamp>{}</TimeStamp><Nonce><![CDATA[{}]]></Nonce></xml>",
        escape_xml_text(timestamp),
        escape_xml_text(nonce),
    )
}

fn sha1_signature(parts: &[&str]) -> String {
    let mut sorted = parts.to_vec();
    sorted.sort_unstable();
    let digest = Sha1::digest(sorted.concat().as_bytes());
    // digest 0.11 的输出类型未实现 `LowerHex`，这里按字节手写小写十六进制，行为与原 `{digest:x}` 一致。
    let mut sig = String::with_capacity(40);
    for byte in digest.iter() {
        use std::fmt::Write as _;
        write!(&mut sig, "{byte:02x}").expect("writing to String cannot fail");
    }
    sig
}

fn signature_matches(actual: &str, provided: &str) -> bool {
    let provided = provided.trim().as_bytes();
    if actual.len() != provided.len() {
        return false;
    }
    actual
        .bytes()
        .zip(provided.iter().copied())
        .fold(0u8, |difference, (left, right)| {
            difference | (left.to_ascii_lowercase() ^ right.to_ascii_lowercase())
        })
        == 0
}

fn add_wechat_pkcs7_padding(value: &mut Vec<u8>) {
    // 微信规范固定按 32 字节块补位，不使用 AES 默认的 16 字节 PKCS#7 块大小。
    let padding = 32 - value.len() % 32;
    value.resize(value.len() + padding, padding as u8);
}

fn remove_wechat_pkcs7_padding(mut value: Vec<u8>) -> Result<Vec<u8>, WechatCryptoError> {
    let padding = usize::from(*value.last().ok_or(WechatCryptoError::InvalidPadding)?);
    if !(1..=32).contains(&padding)
        || padding > value.len()
        || !value[value.len() - padding..]
            .iter()
            .all(|byte| usize::from(*byte) == padding)
    {
        return Err(WechatCryptoError::InvalidPadding);
    }
    value.truncate(value.len() - padding);
    Ok(value)
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
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: IdentitySource::Event,
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
        visible_entity_snapshot: None,
        mentions: Vec::new(),
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
                    .xml10_content()
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
    fn aes_encryption_matches_wechat_official_sample_vector() {
        let crypto = WechatMessageCrypto::new(
            "",
            "wxb11529c136998cb6",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        )
        .unwrap();

        let encrypted = crypto
            .encrypt_with_random("我是中文abcd123", *b"aaaabbbbccccdddd")
            .unwrap();

        assert_eq!(
            encrypted,
            "jn1L23DB+6ELqJ+6bruv21Y6MD7KeIfP82D6gU39rmkgczbWwt5+3bnyg5K55bgVtVzd832WzZGMhkP72vVOfg=="
        );
        assert_eq!(crypto.decrypt(&encrypted).unwrap(), "我是中文abcd123");
    }

    #[test]
    fn encrypted_url_verification_matches_wechat_official_sample_vector() {
        let crypto = WechatMessageCrypto::new(
            "QDG6eK",
            "wx5823bf96d3bd56c7",
            "jWmYm7qr5nMoAUwZRjGtBxmz3KA1tkAj3ykkR6q2B2C",
        )
        .unwrap();
        let timestamp = "1409659589";
        let nonce = "263014780";
        let echostr = "P9nAzCzyDtyTWESHep1vC5X9xho/qYX3Zpb4yKa9SKld1DsH3Iyt3tP3zNdtp+4RPcs8TgAE7OaBO+FZXvnaqQ==";

        assert!(crypto.verify_message_signature(
            timestamp,
            nonce,
            echostr,
            "5c45ff5e21c57e6ad56bac8758b79b1d9ac89fd3"
        ));
        assert_eq!(crypto.decrypt(echostr).unwrap(), "1616140317555161061");
    }

    #[test]
    fn aes_decryption_rejects_message_for_different_app_id() {
        let sender = WechatMessageCrypto::new(
            "token",
            "wx-app-a",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        )
        .unwrap();
        let receiver = WechatMessageCrypto::new(
            "token",
            "wx-app-b",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        )
        .unwrap();
        let encrypted = sender
            .encrypt_with_random("<xml />", *b"0123456789abcdef")
            .unwrap();

        assert_eq!(
            receiver.decrypt(&encrypted),
            Err(WechatCryptoError::AppIdMismatch)
        );
    }

    #[test]
    fn parses_and_renders_encrypted_xml_envelopes() {
        let encrypted = parse_encrypted_message_xml(
            "<xml><ToUserName><![CDATA[gh]]></ToUserName><Encrypt><![CDATA[cipher+/=]]></Encrypt></xml>",
        )
        .unwrap();
        assert_eq!(encrypted, "cipher+/=");

        let xml = render_encrypted_reply_xml("cipher+/=", "signature", "42", "nonce");
        assert!(xml.contains("<Encrypt><![CDATA[cipher+/=]]></Encrypt>"));
        assert!(xml.contains("<MsgSignature><![CDATA[signature]]></MsgSignature>"));
        assert!(xml.contains("<TimeStamp>42</TimeStamp>"));
        assert!(xml.contains("<Nonce><![CDATA[nonce]]></Nonce>"));
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
