//! Codex 风格 Slash 彩蛋。
//!
//! 这里只消费显式白名单并生成确定性短回复，不注册成正式业务命令，也不读取会话、
//! pending 或任何持久化状态。白名单命中必须由 command dispatcher 放在正式命令之后、
//! unknown/suppressed 兜底之前处理。

use super::{RespondRequest, RespondResponse, common::command_response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexEasterEgg {
    Approve,
    Approvals,
    Cloud,
    CloudEnvironment,
    Diff,
    Fast,
    Feedback,
    Fork,
    Goal,
    IdeContext,
    Init,
    Local,
    Mcp,
    Memories,
    Model,
    Pet,
    Personality,
    Plan,
    Project,
    Reasoning,
    Review,
    Side,
    Status,
    Task,
    Worktree,
}

impl CodexEasterEgg {
    fn parse(text: &str) -> Option<Self> {
        let command = text.trim().strip_prefix('/')?.trim_start();
        let action = command.split_whitespace().next()?.to_ascii_lowercase();
        Some(match action.as_str() {
            "approve" => Self::Approve,
            "approvals" => Self::Approvals,
            "cloud" => Self::Cloud,
            "cloud-environment" => Self::CloudEnvironment,
            "diff" => Self::Diff,
            "fast" => Self::Fast,
            "feedback" => Self::Feedback,
            "fork" => Self::Fork,
            "goal" => Self::Goal,
            "ide-context" => Self::IdeContext,
            "init" => Self::Init,
            "local" => Self::Local,
            "mcp" => Self::Mcp,
            "memories" => Self::Memories,
            "model" => Self::Model,
            "pet" => Self::Pet,
            "personality" => Self::Personality,
            "plan" => Self::Plan,
            "project" => Self::Project,
            "reasoning" => Self::Reasoning,
            "review" => Self::Review,
            "side" => Self::Side,
            "status" => Self::Status,
            "task" => Self::Task,
            "worktree" => Self::Worktree,
            _ => return None,
        })
    }

    fn reply(self, req: &RespondRequest) -> String {
        match self {
            Self::Approve | Self::Approvals => "批准了，但没完全批准。".to_owned(),
            Self::Cloud => "云端已就位。天气不负责。".to_owned(),
            Self::CloudEnvironment => "云环境已选好。大概是晴天。".to_owned(),
            Self::Diff => "差异存在，眼神坚定。".to_owned(),
            Self::Fast => "快模式已开启。先别问有多快。".to_owned(),
            Self::Feedback => "反馈收到，情绪稳定。".to_owned(),
            Self::Fork => "分叉成功，平行宇宙自行负责。".to_owned(),
            Self::Goal => "目标已锁定。方向感稍后补上。".to_owned(),
            Self::IdeContext => "IDE 上下文若隐若现。".to_owned(),
            Self::Init => "初始化完成：从想象开始。".to_owned(),
            Self::Local => "本地模式：离线但不失联。".to_owned(),
            Self::Mcp => "MCP 已连接到想象力服务器。".to_owned(),
            Self::Memories => "记忆功能记得自己没开。".to_owned(),
            Self::Model => "模型选择困难症已启动。".to_owned(),
            Self::Pet => "电子宠物正在假装工作。".to_owned(),
            Self::Personality => "人格加载中，请勿拍打。".to_owned(),
            Self::Plan => "计划很完整，变化也很完整。".to_owned(),
            Self::Project => "项目已选中，需求仍在移动。".to_owned(),
            Self::Reasoning => "推理强度：想得挺美。".to_owned(),
            Self::Review => review_reply(req),
            Self::Side => "支线聊天已开启，主线假装没看见。".to_owned(),
            Self::Status => "状态：还能继续写。大概。".to_owned(),
            Self::Task => "任务已创建：继续保持忙碌。".to_owned(),
            Self::Worktree => "工作树长出来了，先别浇水。".to_owned(),
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
            mention
                .target
                .display_name
                .as_deref()
                .or(mention.raw_text.as_deref())
                .and_then(normalize_review_target)
        })
}

fn normalize_review_target(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('@').trim();
    if value.is_empty() {
        return None;
    }
    let display_name = value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(24)
        .collect::<String>();
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
}
