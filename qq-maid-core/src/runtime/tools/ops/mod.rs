//! 配置驱动的白名单运维命令。
//!
//! 本模块只执行部署者预先声明的固定程序，并把结果快照写入统一 Notification
//! Outbox。用户输入只能成为通过本地规则校验后的独立 argv，不能覆盖程序路径，
//! 也不会经过 Shell 解释。

mod config;
mod execute;
mod receipt;

use std::time::Instant;

use qq_maid_common::identity_context::{ConversationKind, IdentitySource};
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    runtime::push::{PushTarget, PushTargetType},
    storage::{
        notification::{NotificationOutboxStore, NotificationUpsert},
        session::now_iso_cn,
    },
};

pub use config::{OPS_CONFIG_FILE_ENV, OpsConfig};
pub use execute::{OpsExecutionResult, OpsExecutionStatus};

use execute::execute;
use receipt::render_result;

const OPS_SOURCE_TYPE: &str = "ops";
const OPS_NOTIFICATION_KIND: &str = "ops_result";
const OPS_MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOpsCommand {
    pub name: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OpsRequestContext {
    pub conversation_kind: ConversationKind,
    pub conversation_id: Option<String>,
    pub user_id: Option<String>,
    pub user_identity_source: Option<IdentitySource>,
    pub group_member_role: Option<String>,
    pub platform: String,
    pub account_id: Option<String>,
}

#[derive(Clone)]
pub struct OpsService {
    config: OpsConfig,
    notification_store: NotificationOutboxStore,
}

/// 只识别边界完整的 `/ops`，避免 `/opsx` 等普通文本被错误截获。
pub fn parse_ops_command(text: &str) -> Option<ParsedOpsCommand> {
    let trimmed = text.trim();
    let remainder = trimmed.strip_prefix("/ops")?;
    if !remainder.is_empty() && !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let mut parts = remainder.split_whitespace();
    Some(ParsedOpsCommand {
        name: parts.next().map(ToOwned::to_owned),
        args: parts.map(ToOwned::to_owned).collect(),
    })
}

impl OpsService {
    pub fn new(config: OpsConfig, notification_store: NotificationOutboxStore) -> Self {
        Self {
            config,
            notification_store,
        }
    }

    /// 完成权限与参数校验后立即启动后台任务，并返回确定性受理或拒绝文案。
    pub fn accept(&self, command: ParsedOpsCommand, context: OpsRequestContext) -> String {
        let authorization = match self.authorize(&context) {
            Ok(authorization) => authorization,
            Err(reply) => return reply,
        };
        let Some(name) = command.name else {
            return self.help_reply();
        };
        let Some(config) = self.config.commands.get(&name).cloned() else {
            return format!("未知运维命令：{name}。发送 /ops 查看可用命令。");
        };
        if let Err(reason) = config.validate_args(&command.args) {
            return format!("运维命令 {name} 的参数不合法：{reason}");
        }

        let execution_id = Uuid::new_v4().to_string();
        let target = PushTarget::new(
            context.platform,
            context.account_id,
            authorization.target_type,
            authorization.target_id,
        );
        let store = self.notification_store.clone();
        let task_name = name.clone();
        let args = command.args;
        info!(
            ops_command = %task_name,
            conversation_type = authorization.target_type.as_str(),
            "ops command accepted"
        );
        tokio::spawn(async move {
            let started_at = Instant::now();
            let result = execute(&task_name, &config, &args).await;
            let rendered = render_result(&result);
            let request = NotificationUpsert {
                source_type: OPS_SOURCE_TYPE.to_owned(),
                source_id: execution_id.clone(),
                dedupe_key: format!("ops:{execution_id}:result"),
                target,
                channel: "push".to_owned(),
                kind: OPS_NOTIFICATION_KIND.to_owned(),
                payload: json!({
                    "message_type": "markdown",
                    "text": rendered.markdown,
                    "fallback_text": rendered.text,
                }),
                scheduled_at: now_iso_cn(),
                max_attempts: OPS_MAX_ATTEMPTS,
                reactivate_cancelled: false,
            };
            match store.upsert(request) {
                Ok(_) => info!(
                    ops_command = %task_name,
                    execution_status = result.status.as_str(),
                    exit_code = result.exit_code,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    stdout_truncated = result.stdout_truncated,
                    stderr_truncated = result.stderr_truncated,
                    "ops result notification queued"
                ),
                Err(err) => warn!(
                    ops_command = %task_name,
                    execution_status = result.status.as_str(),
                    error_code = err.code(),
                    "ops result notification enqueue failed"
                ),
            }
        });

        format!("运维任务 {name} 已受理，完成后会通知你。")
    }

    fn authorize(&self, context: &OpsRequestContext) -> Result<AuthorizedTarget, String> {
        if !self.config.enabled {
            return Err("运维命令未启用。".to_owned());
        }
        match context.conversation_kind {
            ConversationKind::Private => {
                if !self.config.private.enabled {
                    return Err("当前未开放私聊运维命令。".to_owned());
                }
                let Some(user_id) = clean(context.user_id.as_deref()) else {
                    return Err("无法确认当前用户身份，已拒绝执行。".to_owned());
                };
                if !matches!(
                    context.user_identity_source,
                    Some(IdentitySource::Event | IdentitySource::MemberApi | IdentitySource::Cache)
                ) {
                    return Err("当前用户身份来源不可信，已拒绝执行。".to_owned());
                }
                if !self
                    .config
                    .private
                    .allowed_user_ids
                    .iter()
                    .any(|allowed| allowed == user_id)
                {
                    return Err("你没有执行运维命令的权限。".to_owned());
                }
                let Some(target_id) = clean(context.conversation_id.as_deref()) else {
                    return Err("当前私聊没有可用的结果推送目标。".to_owned());
                };
                Ok(AuthorizedTarget {
                    target_type: PushTargetType::Private,
                    target_id: target_id.to_owned(),
                })
            }
            ConversationKind::Group => {
                if !self.config.group.enabled {
                    return Err("当前未开放群聊运维命令。".to_owned());
                }
                let Some(group_id) = clean(context.conversation_id.as_deref()) else {
                    return Err("无法确认当前群聊，已拒绝执行。".to_owned());
                };
                if !self
                    .config
                    .group
                    .allowed_group_ids
                    .iter()
                    .any(|allowed| allowed == group_id)
                {
                    return Err("当前群聊没有执行运维命令的权限。".to_owned());
                }
                if !matches!(
                    context.group_member_role.as_deref(),
                    Some("owner" | "admin")
                ) {
                    return Err("运维命令只允许当前群的群主或管理员执行。".to_owned());
                }
                Ok(AuthorizedTarget {
                    target_type: PushTargetType::Group,
                    target_id: group_id.to_owned(),
                })
            }
            ConversationKind::Channel
            | ConversationKind::ServiceAccount
            | ConversationKind::Unknown => Err("当前会话类型不支持运维命令。".to_owned()),
        }
    }

    fn help_reply(&self) -> String {
        let commands = self
            .config
            .commands
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("、");
        if commands.is_empty() {
            "尚未配置可用的运维命令。".to_owned()
        } else {
            format!("用法：/ops <command> [args...]\n可用命令：{commands}")
        }
    }
}

struct AuthorizedTarget {
    target_type: PushTargetType,
    target_id: String,
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
