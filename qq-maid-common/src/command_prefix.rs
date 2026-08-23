//! 跨 Gateway/Core 共用的聊天命令前缀值对象。
//!
//! 这里只处理单字符前缀、消息开头边界和程序生成文案；不理解任何具体命令名或业务权限。

use std::{error::Error, fmt};

use crate::text::is_invisible_format;

pub const DEFAULT_COMMAND_PREFIX: char = '/';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandPrefix(char);

impl Default for CommandPrefix {
    fn default() -> Self {
        Self(DEFAULT_COMMAND_PREFIX)
    }
}

impl CommandPrefix {
    /// 严格解析一个可见、非空白、非控制字符；不 trim，避免空白配置被静默接受。
    pub fn parse(value: &str) -> Result<Self, CommandPrefixError> {
        let mut chars = value.chars();
        let Some(prefix) = chars.next() else {
            return Err(CommandPrefixError);
        };
        if chars.next().is_some()
            || prefix.is_whitespace()
            || prefix.is_control()
            || is_invisible_format(prefix)
        {
            return Err(CommandPrefixError);
        }
        Ok(Self(prefix))
    }

    pub const fn as_char(self) -> char {
        self.0
    }

    pub fn as_str(self) -> String {
        self.0.to_string()
    }

    /// 判断当前用户消息是否以完整的配置前缀开头。
    ///
    /// 重复前缀（如 `##ping`）不属于候选，避免被下游误解析成合法命令。
    pub fn is_candidate(self, text: &str) -> bool {
        self.normalize(text).is_some()
    }

    /// 兼容默认 `/` 前缀下的点号命令入口。
    ///
    /// 点号只作为命令前缀归一化为 `/`，不在这里维护具体命令白名单；命令是否注册、
    /// 参数如何解释统一交给 Core。配置了其他前缀时不额外放开点号，保持自定义前缀的边界。
    pub fn normalize_with_dot_compat(self, text: &str) -> Option<String> {
        self.normalize(text).or_else(|| self.normalize_dot(text))
    }

    /// 判断消息是否是配置前缀命令或默认模式下的点号命令。
    pub fn is_candidate_with_dot_compat(self, text: &str) -> bool {
        self.normalize_with_dot_compat(text).is_some()
    }

    /// 把配置前缀规范化为 Core 现有解析器使用的 `/`，只改消息开头的一个字符。
    pub fn normalize(self, text: &str) -> Option<String> {
        let text = text.trim();
        let remainder = text.strip_prefix(self.0)?;
        if remainder.is_empty() || remainder.starts_with(self.0) {
            return None;
        }
        Some(format!("/{remainder}"))
    }

    /// 将程序生成文案里的 canonical `/命令` 标记渲染为当前前缀。
    ///
    /// 仅替换位于文本边界、且看起来像已知命令或中文命令的 `/`，不会改 URL 或
    /// `/home/...` 这类绝对路径。
    pub fn render(self, text: &str) -> String {
        if self.0 == DEFAULT_COMMAND_PREFIX {
            return text.to_owned();
        }
        let chars = text.chars().collect::<Vec<_>>();
        let mut rendered = String::with_capacity(text.len());
        for (index, character) in chars.iter().copied().enumerate() {
            if character == DEFAULT_COMMAND_PREFIX
                && (index == 0 || is_command_boundary(chars[index - 1]))
                && looks_like_command(&chars[index + 1..])
            {
                rendered.push(self.0);
            } else {
                rendered.push(character);
            }
        }
        rendered
    }

    fn normalize_dot(self, text: &str) -> Option<String> {
        if self.0 != DEFAULT_COMMAND_PREFIX {
            return None;
        }
        let text = text.trim();
        let remainder = text.strip_prefix('.')?;
        if remainder.is_empty() || remainder.starts_with('.') {
            return None;
        }
        Some(format!("/{remainder}"))
    }
}

impl fmt::Display for CommandPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandPrefixError;

impl fmt::Display for CommandPrefixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("command prefix must be exactly one visible non-whitespace character")
    }
}

impl Error for CommandPrefixError {}

fn is_command_boundary(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '`' | '"' | '\'' | '(' | '[' | '{' | '（' | '【' | '“' | '‘' | '：' | ':' | '，'
        )
}

fn looks_like_command(remainder: &[char]) -> bool {
    let Some(first) = remainder.first().copied() else {
        return false;
    };
    if is_cjk(first) {
        return true;
    }
    let action = remainder
        .iter()
        .take_while(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        action.as_str(),
        "help"
            | "ping"
            | "new"
            | "rename"
            | "resume"
            | "list"
            | "clear"
            | "state"
            | "compact"
            | "memory"
            | "zy"
            | "todo"
            | "rss"
            | "search"
            | "train"
            | "weather"
            | "rader"
            | "radar"
            | "roll"
            | "r"
            | "rd"
            | "rap"
            | "rab"
            | "nn"
            | "set"
            | "unset"
            | "ops"
    )
}

fn is_cjk(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_single_visible_character() {
        for value in ["/", "#", "*"] {
            assert_eq!(CommandPrefix::parse(value).unwrap().as_str(), value);
        }
        for value in ["", " ", "\n", "ab", "##", "\u{200b}", "\u{feff}"] {
            assert_eq!(CommandPrefix::parse(value), Err(CommandPrefixError));
        }
    }

    #[test]
    fn normalizes_only_a_single_prefix_at_message_start() {
        let prefix = CommandPrefix::parse("#").unwrap();
        assert_eq!(
            prefix.normalize(" #todo list ").as_deref(),
            Some("/todo list")
        );
        assert_eq!(prefix.normalize("你好 #todo"), None);
        assert_eq!(prefix.normalize("##todo"), None);
        assert_eq!(prefix.normalize("/todo"), None);
    }

    #[test]
    fn normalizes_default_dot_commands_before_core_dispatch() {
        let prefix = CommandPrefix::default();
        assert_eq!(
            prefix.normalize_with_dot_compat(".r2d6xxx").as_deref(),
            Some("/r2d6xxx")
        );
        assert!(prefix.is_candidate_with_dot_compat(".rd优势"));
        assert_eq!(
            prefix.normalize_with_dot_compat(".r测试").as_deref(),
            Some("/r测试")
        );
        assert!(prefix.is_candidate_with_dot_compat(".rap 原因"));
        assert_eq!(
            prefix.normalize_with_dot_compat(".nn emmm").as_deref(),
            Some("/nn emmm")
        );
        assert_eq!(
            prefix.normalize_with_dot_compat(".nn").as_deref(),
            Some("/nn")
        );
        assert_eq!(
            prefix.normalize_with_dot_compat(".help").as_deref(),
            Some("/help")
        );
        assert_eq!(
            prefix.normalize_with_dot_compat(".rename").as_deref(),
            Some("/rename")
        );
        assert_eq!(prefix.normalize_with_dot_compat("."), None);
        assert_eq!(prefix.normalize_with_dot_compat("..help"), None);
        assert_eq!(
            CommandPrefix::parse("#")
                .unwrap()
                .normalize_with_dot_compat(".r2d6"),
            None
        );
    }

    #[test]
    fn renders_generated_commands_without_changing_urls() {
        let prefix = CommandPrefix::parse("*").unwrap();
        assert_eq!(
            prefix.render("发送 `/rss add https://example.com/feed` 或 /天气 杭州"),
            "发送 `*rss add https://example.com/feed` 或 *天气 杭州"
        );
        assert_eq!(
            prefix.render("文件位于 /home/maid/app.db"),
            "文件位于 /home/maid/app.db"
        );
    }
}
