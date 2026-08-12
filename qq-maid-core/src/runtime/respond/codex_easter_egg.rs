//! Codex 风格 Slash 彩蛋。
//!
//! 这里只消费显式白名单并生成确定性短回复，不注册成正式业务命令，也不读取会话、
//! pending 或任何持久化状态。白名单命中必须由 command dispatcher 放在正式命令之后、
//! unknown/suppressed 兜底之前处理。

use qq_maid_common::text::sanitize_visible_text;

use super::{RespondRequest, RespondResponse, common::command_response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexEasterEgg {
    Static(&'static str),
    Review,
}

struct StaticCodexCommand {
    name: &'static str,
    reply: &'static str,
}

/// 静态彩蛋的命令名和确定性回复必须在同一张表中维护；需要请求上下文的 `/review`
/// 使用单独策略，避免把动态内容塞进静态映射。
const STATIC_CODEX_COMMANDS: &[StaticCodexCommand] = &[
    StaticCodexCommand {
        name: "approve",
        reply: "批准了，但没完全批准。",
    },
    StaticCodexCommand {
        name: "approvals",
        reply: "批准了，但没完全批准。",
    },
    StaticCodexCommand {
        name: "cloud",
        reply: "云端已就位。天气不负责。",
    },
    StaticCodexCommand {
        name: "cloud-environment",
        reply: "云环境已选好。大概是晴天。",
    },
    StaticCodexCommand {
        name: "diff",
        reply: "差异存在，眼神坚定。",
    },
    StaticCodexCommand {
        name: "fast",
        reply: "快模式已开启。先别问有多快。",
    },
    StaticCodexCommand {
        name: "feedback",
        reply: "反馈收到，情绪稳定。",
    },
    StaticCodexCommand {
        name: "fork",
        reply: "分叉成功，平行宇宙自行负责。",
    },
    StaticCodexCommand {
        name: "goal",
        reply: "目标已锁定。方向感稍后补上。",
    },
    StaticCodexCommand {
        name: "ide-context",
        reply: "IDE 上下文若隐若现。",
    },
    StaticCodexCommand {
        name: "init",
        reply: "初始化完成：从想象开始。",
    },
    StaticCodexCommand {
        name: "local",
        reply: "本地模式：离线但不失联。",
    },
    StaticCodexCommand {
        name: "mcp",
        reply: "MCP 已连接到想象力服务器。",
    },
    StaticCodexCommand {
        name: "memories",
        reply: "记忆功能记得自己没开。",
    },
    StaticCodexCommand {
        name: "model",
        reply: "模型选择困难症已启动。",
    },
    StaticCodexCommand {
        name: "pet",
        reply: "电子宠物正在假装工作。",
    },
    StaticCodexCommand {
        name: "personality",
        reply: "人格加载中，请勿拍打。",
    },
    StaticCodexCommand {
        name: "plan",
        reply: "计划很完整，变化也很完整。",
    },
    StaticCodexCommand {
        name: "project",
        reply: "项目已选中，需求仍在移动。",
    },
    StaticCodexCommand {
        name: "reasoning",
        reply: "推理强度：想得挺美。",
    },
    StaticCodexCommand {
        name: "side",
        reply: "支线聊天已开启，主线假装没看见。",
    },
    StaticCodexCommand {
        name: "status",
        reply: "状态：还能继续写。大概。",
    },
    StaticCodexCommand {
        name: "task",
        reply: "任务已创建：继续保持忙碌。",
    },
    StaticCodexCommand {
        name: "worktree",
        reply: "工作树长出来了，先别浇水。",
    },
];

impl CodexEasterEgg {
    fn parse(text: &str) -> Option<Self> {
        let command = text.trim().strip_prefix('/')?.trim_start();
        let action = command.split_whitespace().next()?;
        if action.eq_ignore_ascii_case("review") {
            return Some(Self::Review);
        }

        STATIC_CODEX_COMMANDS
            .iter()
            .find(|command| action.eq_ignore_ascii_case(command.name))
            .map(|command| Self::Static(command.reply))
    }

    fn reply(self, req: &RespondRequest) -> String {
        match self {
            Self::Static(reply) => reply.to_owned(),
            Self::Review => review_reply(req),
        }
    }
}

pub(super) fn try_respond(text: &str, req: &RespondRequest) -> Option<RespondResponse> {
    let command = CodexEasterEgg::parse(text)?;
    Some(command_response(
        command.reply(req),
        None,
        Some("codex_easter_egg"),
    ))
}

fn review_reply(req: &RespondRequest) -> String {
    match review_target(req) {
        Some(target) => format!("审判官 {target} 已就位：LGTM（大概）"),
        None => "LGTM（大概）".to_owned(),
    }
}

/// 只展示平台已提供的昵称或原始 @ 文本；稳定用户 ID 不得回退成展示内容。
fn review_target(req: &RespondRequest) -> Option<String> {
    req.message_context
        .as_ref()?
        .mentions
        .iter()
        .filter(|mention| !mention.is_self)
        .find_map(|mention| {
            [
                mention.target.display_name.as_deref(),
                mention.raw_text.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find_map(normalize_review_target)
        })
}

fn normalize_review_target(value: &str) -> Option<String> {
    let value = sanitize_visible_text(value);
    let value = value.trim().trim_start_matches('@').trim();
    if value.is_empty() {
        return None;
    }
    let display_name = value.chars().take(24).collect::<String>();
    (!display_name.is_empty()).then(|| format!("@{display_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_codex_whitelist_is_case_insensitive_and_accepts_arguments() {
        for command in [
            "approve",
            "approvals",
            "cloud",
            "cloud-environment",
            "diff",
            "fast",
            "feedback",
            "fork",
            "goal",
            "ide-context",
            "init",
            "local",
            "mcp",
            "memories",
            "model",
            "pet",
            "personality",
            "plan",
            "project",
            "reasoning",
            "review",
            "side",
            "status",
            "task",
            "worktree",
        ] {
            assert!(
                CodexEasterEgg::parse(&format!("/{command} 参数")).is_some(),
                "{command} should be whitelisted"
            );
        }
        assert_eq!(
            CodexEasterEgg::parse("/REVIEW @用户"),
            Some(CodexEasterEgg::Review)
        );
    }

    #[test]
    fn registered_and_unknown_commands_are_not_claimed() {
        for command in ["/new", "/clear", "/compact", "/help", "/unknown"] {
            assert!(
                CodexEasterEgg::parse(command).is_none(),
                "{command} must keep its existing route"
            );
        }
    }

    #[test]
    fn review_target_removes_invisible_formats_before_truncating() {
        assert_eq!(
            normalize_review_target("安\u{202e}全\u{200b}昵称\u{2066}"),
            Some("@安全昵称".to_owned())
        );
        assert_eq!(
            normalize_review_target("\u{200b}@安全昵称"),
            Some("@安全昵称".to_owned())
        );

        let padded = format!("{}abcdefghijklmnopqrstuvwx", "\u{200b}".repeat(24));
        assert_eq!(
            normalize_review_target(&padded),
            Some("@abcdefghijklmnopqrstuvwx".to_owned())
        );
    }
}
