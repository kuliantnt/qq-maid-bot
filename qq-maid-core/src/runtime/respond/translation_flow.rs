//! 翻译指令处理流程。
//!
//! 负责解析 `/翻译` 相关指令，并调用共享翻译执行器完成翻译。
//! 不读取普通聊天历史，也不走普通聊天的上下文组装逻辑。

use std::collections::HashMap;

use serde_json::json;

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        session::SessionRecord,
        translation::{TRANSLATION_SOURCE_MAX_LENGTH, TranslationPurpose, TranslationRequest},
    },
};

use super::{
    RespondResponse, RustRespondService,
    common::{command_response, session_error, structured_command_body},
};

// 翻译指令的空参数用法提示
const TRANSLATION_USAGE_REPLY: &str = "用法：/翻译 文本；/翻译日语 文本；/翻译成英语 文本";
// 待翻译内容超长时的提示
const TRANSLATION_TOO_LONG_REPLY: &str = "待翻译内容太长了，请压缩到 3000 字以内再试。";
// 翻译结果为空时的提示
const TRANSLATION_EMPTY_REPLY: &str = "【翻译】没有拿到可用译文，请稍后再试。";
// 翻译功能配置缺失时的提示
const TRANSLATION_CONFIG_ERROR_REPLY: &str = "【翻译】翻译功能还没有配置好，请检查模型配置。";
// 翻译上游超时时的提示
const TRANSLATION_TIMEOUT_REPLY: &str = "【翻译】翻译超时了，请稍后再试。";
// 翻译上游异常时的提示
const TRANSLATION_UPSTREAM_ERROR_REPLY: &str =
    "【翻译】翻译服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";

/// 已解析的翻译指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedTranslationCommand {
    /// 固定动作名，和其他 command flow 保持一致。
    pub action: String,
    /// 目标语言展示名与提示词使用名，例如“英语”“日语”“简体中文”。
    pub target_language: String,
    /// 待翻译正文（已去除首尾空白）。
    pub source_text: String,
    /// 用户输入中用于日志和 session 记录的原始命令。
    pub raw_command: String,
}

struct TranslationResponseArgs {
    session_id: String,
    command: String,
    reply: String,
    target_language: String,
    source_chars: usize,
    error_code: Option<String>,
    error_stage: Option<String>,
    translation_provider: String,
    translation_model: String,
}

/// 从用户文本中解析翻译指令。
///
/// 支持：
/// - `/翻译 文本`
/// - `/翻译日语 文本`
/// - `/翻译 日语 文本`
/// - `/翻译成日语 文本`
pub(super) fn parse_translation_command(text: &str) -> Option<ParsedTranslationCommand> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }

    let command_text = text.trim_start_matches('/').trim();
    let after_command = command_text.strip_prefix("翻译")?;
    let after_command = after_command.trim_start();
    if after_command.is_empty() {
        return Some(ParsedTranslationCommand {
            action: "translation".to_owned(),
            target_language: default_translation_target(""),
            source_text: String::new(),
            raw_command: "翻译".to_owned(),
        });
    }

    if let Some((target_language, source_text)) = parse_explicit_translation_target(after_command) {
        return Some(ParsedTranslationCommand {
            action: "translation".to_owned(),
            target_language,
            source_text,
            raw_command: "翻译".to_owned(),
        });
    }

    if let Some(after_cheng) = after_command.strip_prefix('成') {
        let after_cheng = after_cheng.trim_start();
        if after_cheng.is_empty() {
            return Some(ParsedTranslationCommand {
                action: "translation".to_owned(),
                target_language: default_translation_target(""),
                source_text: String::new(),
                raw_command: "翻译".to_owned(),
            });
        }
        if let Some((target_language, source_text)) = parse_explicit_translation_target(after_cheng)
        {
            return Some(ParsedTranslationCommand {
                action: "translation".to_owned(),
                target_language,
                source_text,
                raw_command: "翻译成".to_owned(),
            });
        }
    }

    Some(ParsedTranslationCommand {
        action: "translation".to_owned(),
        target_language: default_translation_target(after_command),
        source_text: after_command.to_owned(),
        raw_command: "翻译".to_owned(),
    })
}

impl RustRespondService {
    /// 处理翻译指令。
    ///
    /// 通过共享翻译服务完成翻译，不读取普通聊天历史，不注入会话上下文。
    pub(super) async fn handle_translation_command(
        &self,
        command: ParsedTranslationCommand,
        meta: &crate::runtime::session::SessionMeta,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        let source_text = command.source_text.trim();
        if source_text.is_empty() {
            return Ok(command_response(
                TRANSLATION_USAGE_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }

        let source_chars = source_text.chars().count();
        if source_chars > TRANSLATION_SOURCE_MAX_LENGTH {
            return Ok(command_response(
                TRANSLATION_TOO_LONG_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }

        let metadata = HashMap::from([
            ("platform".to_owned(), meta.platform.clone()),
            ("scope_key".to_owned(), meta.scope_key.clone()),
        ]);
        let translation_req = TranslationRequest {
            session_id: session.session_id.clone(),
            source_text: source_text.to_owned(),
            target_language: command.target_language.clone(),
            purpose: TranslationPurpose::Command,
            metadata,
        };
        let scene = if meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            ChatScene::Group
        } else {
            ChatScene::Private
        };
        let translation_model = self.translation_service.model_for_scene(scene)?;
        let translation_model_for_log = self
            .translation_service
            .model_name_for_log(translation_model.as_deref())
            .to_owned();

        let outcome = match self
            .translation_service
            .translate_with_model(translation_req, translation_model)
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    translation_provider = self.translation_service.provider_name(),
                    translation_model = %translation_model_for_log,
                    target_language = %command.target_language,
                    "translation command failed"
                );
                let reply = translation_error_reply(&err);
                self.session_store
                    .append_exchange(session, user_text, &reply)
                    .map_err(session_error)?;
                return Ok(build_translation_response(TranslationResponseArgs {
                    session_id: session.session_id.clone(),
                    command: command.action,
                    reply,
                    target_language: command.target_language,
                    source_chars,
                    error_code: Some(err.code),
                    error_stage: Some(err.stage),
                    translation_provider: self.translation_service.provider_name().to_owned(),
                    translation_model: translation_model_for_log,
                }));
            }
        };

        let reply = format_translation_reply(&command.target_language, &outcome.translated_text);
        self.session_store
            .append_exchange(session, user_text, &reply)
            .map_err(session_error)?;

        Ok(build_translation_response(TranslationResponseArgs {
            session_id: session.session_id.clone(),
            command: command.action,
            reply,
            target_language: command.target_language,
            source_chars,
            error_code: None,
            error_stage: None,
            translation_provider: outcome.provider,
            translation_model: outcome.model,
        }))
    }
}

fn build_translation_response(args: TranslationResponseArgs) -> RespondResponse {
    let mut response = command_response(
        structured_command_body(args.reply),
        Some(args.session_id),
        Some(args.command),
    );
    let mut diagnostics = json!({
        "backend": "rust",
        "session_backend": "rust",
        "used_memory": false,
        "used_search": false,
        "used_translation": true,
        "target_language": args.target_language,
        "source_chars": args.source_chars,
        "translation_provider": args.translation_provider,
        "translation_model": args.translation_model,
    });
    if let Some(code) = args.error_code {
        diagnostics["translation_error_code"] = json!(code);
    }
    if let Some(stage) = args.error_stage {
        diagnostics["translation_error_stage"] = json!(stage);
    }
    response.diagnostics = Some(diagnostics);
    response
}

fn translation_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "config" => TRANSLATION_CONFIG_ERROR_REPLY.to_owned(),
        "timeout" => TRANSLATION_TIMEOUT_REPLY.to_owned(),
        "empty_translation" => TRANSLATION_EMPTY_REPLY.to_owned(),
        _ => TRANSLATION_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

fn format_translation_reply(target_language: &str, translated_text: &str) -> String {
    format!("【翻译·{target_language}】\n\n{}", translated_text.trim())
}

fn parse_explicit_translation_target(text: &str) -> Option<(String, String)> {
    for alias in TRANSLATION_TARGET_ALIASES {
        let Some(rest) = text.strip_prefix(alias.alias) else {
            continue;
        };
        let source_text = trim_translation_separators(rest);
        return Some((alias.label.to_owned(), source_text.to_owned()));
    }
    None
}

fn default_translation_target(_source_text: &str) -> String {
    // 默认目标固定为简体中文；只有用户显式指定语言时才翻译成其他语言。
    "简体中文".to_owned()
}

fn trim_translation_separators(text: &str) -> &str {
    text.trim_start_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, ':' | '：' | '—' | '-' | '·' | '、')
    })
}

struct TranslationAlias {
    alias: &'static str,
    label: &'static str,
}

const TRANSLATION_TARGET_ALIASES: &[TranslationAlias] = &[
    TranslationAlias {
        alias: "繁体中文",
        label: "繁体中文",
    },
    TranslationAlias {
        alias: "简体中文",
        label: "简体中文",
    },
    TranslationAlias {
        alias: "西班牙语",
        label: "西班牙语",
    },
    TranslationAlias {
        alias: "日本语",
        label: "日语",
    },
    TranslationAlias {
        alias: "韩语",
        label: "韩语",
    },
    TranslationAlias {
        alias: "英文",
        label: "英语",
    },
    TranslationAlias {
        alias: "英语",
        label: "英语",
    },
    TranslationAlias {
        alias: "韩文",
        label: "韩语",
    },
    TranslationAlias {
        alias: "日语",
        label: "日语",
    },
    TranslationAlias {
        alias: "日文",
        label: "日语",
    },
    TranslationAlias {
        alias: "法语",
        label: "法语",
    },
    TranslationAlias {
        alias: "德语",
        label: "德语",
    },
    TranslationAlias {
        alias: "俄语",
        label: "俄语",
    },
    TranslationAlias {
        alias: "繁中",
        label: "繁体中文",
    },
    TranslationAlias {
        alias: "简中",
        label: "简体中文",
    },
    TranslationAlias {
        alias: "中文",
        label: "简体中文",
    },
];
