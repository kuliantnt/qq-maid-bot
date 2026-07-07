//! `/set` / `/unset` 用户偏好设置指令处理（#326）。
//!
//! `/set` 是通用的用户偏好设置入口，避免每新增一个设置项就要新增一个独立命令。
//! 当前唯一支持的设置项是“手动展示名 / 昵称”：在平台成员信息接口不可用时，
//! 让用户主动声明“我在这个会话里叫什么”。展示名只用于显示和帮助 LLM 理解上下文，
//! 不参与权限判断、owner 或稳定身份认证。
//!
//! 查看和设置走 `/set`，清除走 `/unset`，语义分离更直观。
//!
//! 新增设置项时：
//! 1. 在 [`SettingKind`] 增加变体并补全 [`SETTING_KEYS`] 别名分组；
//! 2. 在 `set` / `view` / `unset` 三个分发函数里补上对应分支；
//! 3. 不需要改动命令路由或 `/help` 总入口。

use crate::{
    error::LlmError,
    runtime::{command::ParsedCommand, session::SessionRecord},
};

use super::{
    RespondResponse, RustRespondService,
    command_render::CommandRender,
    common::{command_response, session_error},
};

/// 手动展示名最小与最大 Unicode 字符数。
const DISPLAY_NAME_MIN_CHARS: usize = 1;
const DISPLAY_NAME_MAX_CHARS: usize = 32;

/// `/set` 无参数时的用法提示。
const SET_USAGE_REPLY: &str = "用法：\n- /set 昵称 脸脸：设置当前会话里的展示名\n- /set 昵称：查看当前展示名\n- /unset 昵称：清除展示名";

/// `/unset` 无参数时的用法提示。
const UNSET_USAGE_REPLY: &str = "用法：/unset 昵称\n清除当前会话里你设置的展示名。";

/// 已支持的设置项。
///
/// 当前只有手动展示名；后续新增偏好项时在此扩展，并补全对应别名分组与处理分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKind {
    /// 手动展示名 / 昵称，按稳定身份绑定在当前会话空间。
    DisplayName,
}

/// 各设置项接受的命令别名分组。
///
/// 每组至少保留一个中文别名和一个英文 key，符合 issue 命令入口收敛要求。
/// 顺序即帮助与匹配优先级；新增项追加在末尾即可。
const SETTING_KEYS: &[(SettingKind, &[&str])] = &[(
    SettingKind::DisplayName,
    &["昵称", "nickname", "display_name"],
)];

impl RustRespondService {
    /// 处理 `/set` 指令。
    ///
    /// 未匹配到已知设置项时返回用法提示，不静默成功，也不让模型文案代替执行结果。
    pub(super) async fn handle_set_command(
        &self,
        command: ParsedCommand,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        let body = render_set_reply(self, &command.argument, session)?;
        self.session_store
            .append_exchange(session, user_text, &body.text)
            .map_err(session_error)?;
        Ok(command_response(
            body,
            Some(session.session_id.clone()),
            Some("set"),
        ))
    }

    /// 处理 `/unset` 指令。
    pub(super) async fn handle_unset_command(
        &self,
        command: ParsedCommand,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        let body = render_unset_reply(self, &command.argument, session)?;
        self.session_store
            .append_exchange(session, user_text, &body.text)
            .map_err(session_error)?;
        Ok(command_response(
            body,
            Some(session.session_id.clone()),
            Some("unset"),
        ))
    }
}

/// 解析 `/set` 的参数为 (设置项, 剩余 value)。
///
/// value 缺失表示查看当前值。未识别的 key 返回 `None`，由上层回退到用法提示。
fn parse_set_argument(argument: &str) -> Option<(SettingKind, &str)> {
    let mut parts = argument.splitn(2, char::is_whitespace);
    let key = parts.next()?.trim().to_ascii_lowercase();
    let value = parts.next().unwrap_or("").trim();
    resolve_setting_kind(&key).map(|kind| (kind, value))
}

/// 解析 `/unset` 的参数为设置项。
fn parse_unset_argument(argument: &str) -> Option<SettingKind> {
    resolve_setting_kind(argument.trim().to_ascii_lowercase().as_str())
}

/// 按别名解析设置项。
fn resolve_setting_kind(key: &str) -> Option<SettingKind> {
    SETTING_KEYS
        .iter()
        .find(|(_, aliases)| aliases.contains(&key))
        .map(|(kind, _)| *kind)
}

fn render_set_reply(
    service: &RustRespondService,
    argument: &str,
    session: &SessionRecord,
) -> Result<super::common::CommandBody, LlmError> {
    let argument = argument.trim();
    if argument.is_empty() {
        return Ok(usage_body(SET_USAGE_REPLY));
    }
    let Some((kind, value)) = parse_set_argument(argument) else {
        return Ok(usage_body(SET_USAGE_REPLY));
    };

    // value 为空 = 查看当前值；非空 = 设置新值。
    if value.is_empty() {
        view_setting_body(service, kind, session)
    } else {
        apply_setting_body(service, kind, value, session)
    }
}

fn render_unset_reply(
    service: &RustRespondService,
    argument: &str,
    session: &SessionRecord,
) -> Result<super::common::CommandBody, LlmError> {
    let argument = argument.trim();
    if argument.is_empty() {
        return Ok(usage_body(UNSET_USAGE_REPLY));
    }
    let Some(kind) = parse_unset_argument(argument) else {
        return Ok(usage_body(UNSET_USAGE_REPLY));
    };
    unset_setting_body(service, kind, session)
}

/// 取当前会话对应的稳定 user_id。
///
/// 展示名等按稳定身份绑定的设置项在缺少 user_id 时不允许写入，并返回明确错误。
fn session_user_id(session: &SessionRecord) -> &str {
    session
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
}

fn view_setting_body(
    service: &RustRespondService,
    kind: SettingKind,
    session: &SessionRecord,
) -> Result<super::common::CommandBody, LlmError> {
    match kind {
        SettingKind::DisplayName => Ok(view_display_name_body(service, session)),
    }
}

fn apply_setting_body(
    service: &RustRespondService,
    kind: SettingKind,
    value: &str,
    session: &SessionRecord,
) -> Result<super::common::CommandBody, LlmError> {
    match kind {
        SettingKind::DisplayName => {
            // 校验失败 / 写入失败都如实返回，不伪造成功。
            match validate_display_name(value) {
                Ok(name) => match service.display_name_store.set(
                    &session.scope_key,
                    session_user_id(session),
                    &name,
                ) {
                    Ok(()) => Ok(set_success_body(&name, session)),
                    Err(err) => Ok(set_failure_body(&err)),
                },
                Err(reason) => Ok(invalid_display_name_body(reason)),
            }
        }
    }
}

fn unset_setting_body(
    service: &RustRespondService,
    kind: SettingKind,
    session: &SessionRecord,
) -> Result<super::common::CommandBody, LlmError> {
    match kind {
        SettingKind::DisplayName => match service
            .display_name_store
            .unset(&session.scope_key, session_user_id(session))
        {
            Ok(true) => Ok(unset_success_body(session)),
            Ok(false) => Ok(no_display_name_body()),
            Err(err) => Ok(set_failure_body(&err)),
        },
    }
}

/// 校验展示名：非空、不含换行、长度在 1~32 个 Unicode 字符之间。
fn validate_display_name(value: &str) -> Result<String, &'static str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("展示名不能为空。");
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err("展示名不能包含换行。");
    }
    let count = trimmed.chars().count();
    if count < DISPLAY_NAME_MIN_CHARS {
        return Err("展示名太短了。");
    }
    if count > DISPLAY_NAME_MAX_CHARS {
        return Err("展示名太长了，请控制在 32 个字符以内。");
    }
    Ok(trimmed.to_owned())
}

fn set_success_body(name: &str, session: &SessionRecord) -> super::common::CommandBody {
    let mut render = CommandRender::new();
    render.title("✅ 展示名已设置");
    render.blank();
    render.bullet(&format!("当前展示名：{name}"));
    render.bullet(&format!("作用域：{}", scope_label(session)));
    render.blank();
    render.paragraph(
        "该展示名只用于显示和帮助小女仆理解上下文，不代表现实身份认证，也不影响权限判断。",
    );
    render.paragraph("清除请发送 /unset 昵称。");
    render.build()
}

fn set_failure_body(
    err: &crate::runtime::display_name::DisplayNameError,
) -> super::common::CommandBody {
    let mut render = CommandRender::new();
    render.title("⚠️ 展示名设置失败");
    render.blank();
    render.paragraph(err.message());
    render.build()
}

fn invalid_display_name_body(reason: &str) -> super::common::CommandBody {
    let mut render = CommandRender::new();
    render.title("⚠️ 展示名无效");
    render.blank();
    render.paragraph(reason);
    render.build()
}

fn view_display_name_body(
    service: &RustRespondService,
    session: &SessionRecord,
) -> super::common::CommandBody {
    match service
        .display_name_store
        .get(&session.scope_key, session_user_id(session))
    {
        Ok(Some(name)) => {
            let mut render = CommandRender::new();
            render.title("🏷 当前展示名");
            render.blank();
            render.bullet(&format!("展示名：{name}"));
            render.bullet(&format!("作用域：{}", scope_label(session)));
            render.blank();
            render.paragraph("该展示名只用于显示和帮助小女仆理解上下文，不代表现实身份认证。");
            render.build()
        }
        Ok(None) => {
            let mut render = CommandRender::new();
            render.title("🏷 当前展示名");
            render.blank();
            render.paragraph("你还没有设置展示名。");
            render.blank();
            render.paragraph(
                "发送 /set 昵称 脸脸 即可设置；该展示名只用于显示，不代表现实身份认证。",
            );
            render.build()
        }
        Err(err) => set_failure_body(&err),
    }
}

fn unset_success_body(session: &SessionRecord) -> super::common::CommandBody {
    let mut render = CommandRender::new();
    render.title("🗑 展示名已清除");
    render.blank();
    render.bullet(&format!("作用域：{}", scope_label(session)));
    render.blank();
    render.paragraph("展示名已回退到平台昵称或 unknown。需要时可以再次 /set 昵称 设置。");
    render.build()
}

fn no_display_name_body() -> super::common::CommandBody {
    let mut render = CommandRender::new();
    render.title("🏷 当前展示名");
    render.blank();
    render.paragraph("你还没有设置展示名，无需清除。");
    render.build()
}

fn usage_body(reply: &str) -> super::common::CommandBody {
    super::common::CommandBody::plain(reply.to_owned())
}

/// 给用户展示的作用域标签，避免直接暴露内部 scope_key 全文。
fn scope_label(session: &SessionRecord) -> String {
    if session.scope_key.contains(":group:") {
        "当前群聊".to_owned()
    } else if session.scope_key.contains(":private:") {
        "当前私聊".to_owned()
    } else {
        "当前会话".to_owned()
    }
}

/// 尝试从用户文本中解析 `/set` 指令。
pub(super) fn parse_set_command(text: &str) -> Option<ParsedCommand> {
    let command = crate::runtime::command::parse_slash_command(text)?;
    (command.action == "set").then_some(command)
}

/// 尝试从用户文本中解析 `/unset` 指令。
pub(super) fn parse_unset_command(text: &str) -> Option<ParsedCommand> {
    let command = crate::runtime::command::parse_slash_command(text)?;
    (command.action == "unset").then_some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_argument_recognizes_aliases() {
        assert_eq!(
            parse_set_argument("昵称 脸脸"),
            Some((SettingKind::DisplayName, "脸脸"))
        );
        assert_eq!(
            parse_set_argument("nickname face"),
            Some((SettingKind::DisplayName, "face"))
        );
        assert_eq!(
            parse_set_argument("display_name  脸脸"),
            Some((SettingKind::DisplayName, "脸脸"))
        );
        // 仅 key 视为查看
        assert_eq!(
            parse_set_argument("昵称"),
            Some((SettingKind::DisplayName, ""))
        );
        // 未知 key
        assert_eq!(parse_set_argument("xxx yyy"), None);
        assert_eq!(parse_set_argument(""), None);
    }

    #[test]
    fn parse_unset_argument_recognizes_aliases() {
        assert_eq!(parse_unset_argument("昵称"), Some(SettingKind::DisplayName));
        assert_eq!(
            parse_unset_argument("nickname"),
            Some(SettingKind::DisplayName)
        );
        assert_eq!(
            parse_unset_argument("display_name"),
            Some(SettingKind::DisplayName)
        );
        assert_eq!(parse_unset_argument("xxx"), None);
        assert_eq!(parse_unset_argument(""), None);
    }

    #[test]
    fn validate_display_name_rules() {
        assert_eq!(validate_display_name("脸脸").unwrap(), "脸脸");
        assert!(validate_display_name("").is_err());
        assert!(validate_display_name("   ").is_err());
        assert!(validate_display_name("脸\n脸").is_err());
        assert!(validate_display_name(&"a".repeat(33)).is_err());
        // 32 个字符仍合法
        assert!(validate_display_name(&"a".repeat(32)).is_ok());
    }
}
