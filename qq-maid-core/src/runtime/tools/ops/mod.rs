//! 配置驱动的白名单运维命令与内置 Codex 长任务。
//!
//! 普通命令和 Codex 都只执行部署者预先声明的固定程序。用户输入只能成为通过本地
//! 规则校验后的独立 argv；程序路径、工作目录、profile 和 sandbox 不可由消息覆盖，
//! 所有结果只写入统一 Notification Outbox，不经过 Shell。

mod codex_log;
mod config;
mod execute;
mod receipt;
mod registry;
mod storage;

use std::time::Instant;

use qq_maid_common::identity_context::{ConversationKind, IdentitySource};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    runtime::push::{PushTarget, PushTargetType},
    storage::{
        notification::{NotificationOutboxStore, NotificationUpsert},
        session::now_iso_cn,
    },
};

pub use config::{OPS_CONFIG_FILE_ENV, OpsCodexConfig, OpsConfig};
pub use execute::{OpsExecutionResult, OpsExecutionStatus};
pub(crate) use registry::OpsTaskRegistry;
pub use storage::{OPS_EXECUTION_SCHEMA_V1, OpsExecutionStore};

use execute::{execute, execute_codex};
use receipt::{RenderedOpsPart, render_result};
use registry::{CancelOutcome, ManagedTaskStatus, NewManagedTask, RegisterOutcome, TaskScope};
use storage::ClaimOutcome;

const OPS_SOURCE_TYPE: &str = "ops";
const OPS_NOTIFICATION_KIND: &str = "ops_result";
const OPS_MAX_ATTEMPTS: u32 = 5;
const MAX_TASK_ID_ATTEMPTS: usize = 32;
#[cfg(not(test))]
const CODEX_LOG_DIRECTORY: &str = "logs/ops";

fn codex_log_directory() -> std::path::PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("qq-maid-ops-test-logs-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        std::path::PathBuf::from(CODEX_LOG_DIRECTORY)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedOpsCommand {
    pub name: Option<String>,
    pub args: Vec<String>,
    /// 只有内置 `codex` 使用：保留命令名后的完整文本，不按空白拆分。
    pub trailing_text: Option<String>,
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
    /// Gateway 从可信平台事件映射的稳定消息 ID；不从正文或昵称推断。
    pub inbound_id: Option<String>,
}

#[derive(Clone)]
pub struct OpsService {
    config: OpsConfig,
    notification_store: NotificationOutboxStore,
    execution_store: OpsExecutionStore,
    task_registry: OpsTaskRegistry,
}

/// 只识别边界完整的 `/ops`。Codex 保留完整尾部文本；取消别名统一归一化为 cancel。
pub fn parse_ops_command(text: &str) -> Option<ParsedOpsCommand> {
    let trimmed = text.trim();
    let remainder = trimmed.strip_prefix("/ops")?;
    if !remainder.is_empty() && !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let remainder = remainder.trim_start();
    if remainder.is_empty() {
        return Some(ParsedOpsCommand {
            name: None,
            args: Vec::new(),
            trailing_text: None,
        });
    }
    let name_end = remainder
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(remainder.len());
    let raw_name = &remainder[..name_end];
    let tail = remainder[name_end..].trim_start();
    let name = match raw_name {
        "stop" | "kill" | "close" => "cancel",
        other => other,
    };
    if name == "codex" {
        return Some(ParsedOpsCommand {
            name: Some(name.to_owned()),
            args: Vec::new(),
            trailing_text: Some(tail.to_owned()),
        });
    }
    Some(ParsedOpsCommand {
        name: Some(name.to_owned()),
        args: tail.split_whitespace().map(ToOwned::to_owned).collect(),
        trailing_text: None,
    })
}

impl OpsService {
    pub fn new(config: OpsConfig, notification_store: NotificationOutboxStore) -> Self {
        let execution_store = OpsExecutionStore::new(notification_store.database());
        Self::new_with_runtime(
            config,
            notification_store,
            execution_store,
            OpsTaskRegistry::default(),
        )
    }

    pub(crate) fn new_with_runtime(
        config: OpsConfig,
        notification_store: NotificationOutboxStore,
        execution_store: OpsExecutionStore,
        task_registry: OpsTaskRegistry,
    ) -> Self {
        Self {
            config,
            notification_store,
            execution_store,
            task_registry,
        }
    }

    /// 权限先于所有内置命令；执行类命令还必须在 spawn 前持久化领取稳定入站键。
    pub fn accept(&self, command: ParsedOpsCommand, context: OpsRequestContext) -> String {
        let authorization = match self.authorize(&context) {
            Ok(authorization) => authorization,
            Err(reply) => return reply,
        };
        let Some(name) = command.name.as_deref() else {
            return self.help_reply();
        };
        match name {
            "codex" => self.accept_codex(command, context, authorization),
            "list" => self.list_tasks(&command, &context, &authorization),
            "cancel" => self.cancel_task(&command, &context, &authorization),
            _ => self.accept_configured(command, context, authorization),
        }
    }

    fn accept_codex(
        &self,
        command: ParsedOpsCommand,
        context: OpsRequestContext,
        authorization: AuthorizedTarget,
    ) -> String {
        if !self.config.codex.enabled {
            return "Codex 运维任务未启用。".to_owned();
        }
        let prompt = command.trailing_text.unwrap_or_default();
        if prompt.is_empty() {
            return "用法：/ops codex <任务描述>".to_owned();
        }
        if prompt == "-"
            || prompt.len() > self.config.codex.max_prompt_bytes
            || prompt.chars().any(|character| {
                character == '\0' || (character.is_control() && !matches!(character, '\n' | '\t'))
            })
        {
            return format!(
                "Codex 任务描述不合法或超过 {} 字节。",
                self.config.codex.max_prompt_bytes
            );
        }
        let inbound_key = match inbound_key(&context, &authorization) {
            Ok(key) => key,
            Err(reply) => return reply,
        };
        match self.execution_store.get_by_inbound_key(&inbound_key) {
            Ok(Some(existing)) => {
                return duplicate_reply(&existing.task_id, &existing.command_name);
            }
            Ok(None) => {}
            Err(error) => return storage_failure_reply(&error),
        }

        let scope = task_scope(&context, &authorization);
        for _ in 0..MAX_TASK_ID_ATTEMPTS {
            let task_id = new_task_id();
            let registration = self.task_registry.register_codex(
                NewManagedTask {
                    task_id: task_id.clone(),
                    inbound_key: inbound_key.clone(),
                    command_type: "codex".to_owned(),
                    command_name: "codex".to_owned(),
                    scope: scope.clone(),
                    cancellable: self.config.codex.cancellable,
                },
                self.config.codex.max_concurrent_tasks,
            );
            let (cancellation, process_id) = match registration {
                RegisterOutcome::Registered {
                    cancellation,
                    process_id,
                } => (cancellation, process_id),
                RegisterOutcome::Existing(existing) => {
                    return duplicate_reply(&existing, "codex");
                }
                RegisterOutcome::AtCapacity => {
                    return "当前已有 Codex 任务正在运行，请等待完成或使用 /ops cancel <任务ID> 取消。"
                        .to_owned();
                }
                RegisterOutcome::TaskIdCollision => continue,
            };
            match self.execution_store.claim(&inbound_key, &task_id, "codex") {
                Ok(ClaimOutcome::Claimed) => {
                    return self.spawn_codex(
                        task_id,
                        prompt,
                        context,
                        authorization,
                        cancellation,
                        process_id,
                    );
                }
                Ok(ClaimOutcome::Existing(existing)) => {
                    self.task_registry.remove_unstarted(&task_id);
                    return duplicate_reply(&existing.task_id, &existing.command_name);
                }
                Ok(ClaimOutcome::TaskIdCollision) => {
                    self.task_registry.remove_unstarted(&task_id);
                }
                Err(error) => {
                    self.task_registry.remove_unstarted(&task_id);
                    return storage_failure_reply(&error);
                }
            }
        }
        "暂时无法生成唯一任务 ID，请稍后重试。".to_owned()
    }

    fn spawn_codex(
        &self,
        task_id: String,
        prompt: String,
        context: OpsRequestContext,
        authorization: AuthorizedTarget,
        cancellation: tokio::sync::watch::Receiver<bool>,
        process_id: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
    ) -> String {
        let target = push_target(&context, &authorization);
        let config = self.config.codex.clone();
        let notification_store = self.notification_store.clone();
        let execution_store = self.execution_store.clone();
        let registry = self.task_registry.clone();
        let background_task_id = task_id.clone();
        info!(ops_command = "codex", conversation_type = %authorization.target_type.as_str(), "ops command accepted");
        tokio::spawn(async move {
            let started_at = Instant::now();
            let mut result = execute_codex(&config, &prompt, cancellation, process_id).await;
            if let Err(error) = codex_log::prepare_for_delivery(
                &codex_log_directory(),
                &background_task_id,
                &prompt,
                &mut result,
            ) {
                warn!(
                    ops_command = "codex",
                    error_code = error.code(),
                    "ops codex error log write failed"
                );
            }
            let managed_status = managed_status(result.status);
            registry.finish(&background_task_id, managed_status);
            if let Err(error) =
                execution_store.mark_status(&background_task_id, result.status.as_str())
            {
                warn!(
                    ops_command = "codex",
                    error_code = error.code(),
                    "ops execution status update failed"
                );
            }
            enqueue_result(
                &notification_store,
                &background_task_id,
                target,
                &result,
                Some(&background_task_id),
                started_at,
            );
        });

        let mut reply = format!("Codex 任务已受理\n任务 ID：{task_id}");
        if self.config.codex.cancellable {
            reply.push_str(&format!("\n取消：/ops cancel {task_id}"));
        }
        reply
    }

    fn accept_configured(
        &self,
        command: ParsedOpsCommand,
        context: OpsRequestContext,
        authorization: AuthorizedTarget,
    ) -> String {
        let name = command.name.expect("configured command has a name");
        let Some(config) = self.config.commands.get(&name).cloned() else {
            return format!("未知运维命令：{name}。发送 /ops 查看可用命令。");
        };
        if let Err(reason) = config.validate_args(&command.args) {
            return format!("运维命令 {name} 的参数不合法：{reason}");
        }
        let inbound_key = match inbound_key(&context, &authorization) {
            Ok(key) => key,
            Err(reply) => return reply,
        };
        let execution_id = match self.claim_configured(&inbound_key, &name) {
            Ok(Some(id)) => id,
            Ok(None) => return "该入站请求已受理，不会重复执行运维命令。".to_owned(),
            Err(reply) => return reply,
        };
        let target = push_target(&context, &authorization);
        let notification_store = self.notification_store.clone();
        let execution_store = self.execution_store.clone();
        let background_execution_id = execution_id.clone();
        let task_name = name.clone();
        let args = command.args;
        info!(ops_command = %task_name, conversation_type = authorization.target_type.as_str(), "ops command accepted");
        tokio::spawn(async move {
            let started_at = Instant::now();
            let result = execute(&task_name, &config, &args).await;
            if let Err(error) =
                execution_store.mark_status(&background_execution_id, result.status.as_str())
            {
                warn!(ops_command = %task_name, error_code = error.code(), "ops execution status update failed");
            }
            enqueue_result(
                &notification_store,
                &background_execution_id,
                target,
                &result,
                None,
                started_at,
            );
        });
        format!("运维任务 {name} 已受理，完成后会通知你。")
    }

    fn claim_configured(&self, inbound_key: &str, name: &str) -> Result<Option<String>, String> {
        for _ in 0..MAX_TASK_ID_ATTEMPTS {
            let execution_id = Uuid::new_v4().to_string();
            match self.execution_store.claim(inbound_key, &execution_id, name) {
                Ok(ClaimOutcome::Claimed) => return Ok(Some(execution_id)),
                Ok(ClaimOutcome::Existing(_)) => return Ok(None),
                Ok(ClaimOutcome::TaskIdCollision) => {}
                Err(error) => return Err(storage_failure_reply(&error)),
            }
        }
        Err("暂时无法领取运维执行，请稍后重试。".to_owned())
    }

    fn list_tasks(
        &self,
        command: &ParsedOpsCommand,
        context: &OpsRequestContext,
        authorization: &AuthorizedTarget,
    ) -> String {
        if !command.args.is_empty() {
            return "用法：/ops list".to_owned();
        }
        let tasks = self.task_registry.list(&task_scope(context, authorization));
        if tasks.is_empty() {
            return "当前没有运行中的 Codex 运维任务。".to_owned();
        }
        let mut lines = vec!["运行中的运维任务：".to_owned()];
        for (index, task) in tasks.iter().enumerate() {
            lines.extend([
                String::new(),
                format!("{}. {}", index + 1, task.task_id),
                format!("类型：{}", task.command_type),
                format!("命令：{}", task.command_name),
                format!("状态：{}", task.status.label()),
                format!("运行时间：{}", format_running_time(task.elapsed)),
            ]);
            if task.cancellable {
                lines.push(format!("取消：/ops cancel {}", task.task_id));
            }
        }
        lines.join("\n")
    }

    fn cancel_task(
        &self,
        command: &ParsedOpsCommand,
        context: &OpsRequestContext,
        authorization: &AuthorizedTarget,
    ) -> String {
        if command.args.len() != 1 {
            return "用法：/ops cancel <任务ID>".to_owned();
        }
        let task_id = &command.args[0];
        match self
            .task_registry
            .cancel(task_id, &task_scope(context, authorization))
        {
            CancelOutcome::Cancelling => format!("正在取消任务 {task_id}。"),
            CancelOutcome::Finished(status) => {
                format!(
                    "任务 {task_id} 已结束，当前状态：{}，不能取消。",
                    status.label()
                )
            }
            CancelOutcome::NotCancellable => format!("任务 {task_id} 未开放远程取消。"),
            CancelOutcome::NotFound => format!("未找到可由当前会话管理的任务 {task_id}。"),
        }
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
        let mut lines = vec![
            "用法：".to_owned(),
            "/ops cancel <任务ID>".to_owned(),
            "/ops list".to_owned(),
            "/ops codex <任务描述>".to_owned(),
        ];
        if !commands.is_empty() {
            lines.push("/ops <command> [args...]".to_owned());
            lines.push(format!("配置命令：{commands}"));
        }
        if !self.config.codex.enabled {
            lines.push("Codex 长任务当前未启用。".to_owned());
        }
        lines.join("\n")
    }
}

#[derive(Clone)]
struct AuthorizedTarget {
    target_type: PushTargetType,
    target_id: String,
}

fn push_target(context: &OpsRequestContext, authorization: &AuthorizedTarget) -> PushTarget {
    PushTarget::new(
        context.platform.clone(),
        context.account_id.clone(),
        authorization.target_type,
        authorization.target_id.clone(),
    )
}

fn task_scope(context: &OpsRequestContext, authorization: &AuthorizedTarget) -> TaskScope {
    TaskScope {
        platform: context.platform.trim().to_owned(),
        account_id: clean(context.account_id.as_deref()).map(ToOwned::to_owned),
        target_type: authorization.target_type.as_str().to_owned(),
        target_id: authorization.target_id.clone(),
        private_actor_hash: (authorization.target_type == PushTargetType::Private)
            .then(|| actor_hash(context))
            .flatten(),
    }
}

fn actor_hash(context: &OpsRequestContext) -> Option<String> {
    let user_id = clean(context.user_id.as_deref())?;
    let mut hasher = Sha256::new();
    hasher.update(context.platform.trim().as_bytes());
    hasher.update([0]);
    hasher.update(
        context
            .account_id
            .as_deref()
            .unwrap_or("-")
            .trim()
            .as_bytes(),
    );
    hasher.update([0]);
    hasher.update(user_id.as_bytes());
    Some(digest_hex(hasher.finalize()))
}

fn inbound_key(
    context: &OpsRequestContext,
    authorization: &AuthorizedTarget,
) -> Result<String, String> {
    let Some(inbound_id) = clean(context.inbound_id.as_deref()) else {
        return Err("当前平台事件缺少可信消息 ID，为避免重复执行已拒绝受理。".to_owned());
    };
    if inbound_id.len() > 512 || inbound_id.chars().any(char::is_control) {
        return Err("当前平台消息 ID 不符合安全要求，已拒绝受理。".to_owned());
    }
    let mut hasher = Sha256::new();
    hasher.update(context.platform.trim().as_bytes());
    hasher.update([0]);
    hasher.update(
        context
            .account_id
            .as_deref()
            .unwrap_or("-")
            .trim()
            .as_bytes(),
    );
    hasher.update([0]);
    hasher.update(authorization.target_type.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(authorization.target_id.as_bytes());
    hasher.update([0]);
    hasher.update(inbound_id.as_bytes());
    Ok(format!("ops-inbound:{}", digest_hex(hasher.finalize())))
}

fn digest_hex(digest: impl IntoIterator<Item = u8>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn new_task_id() -> String {
    let value = Uuid::new_v4().simple().to_string();
    format!("ops-{}", &value[..8])
}

fn duplicate_reply(task_id: &str, command_name: &str) -> String {
    if command_name == "codex" {
        format!("该入站请求已受理，不会重复执行。\n任务 ID：{task_id}")
    } else {
        "该入站请求已受理，不会重复执行运维命令。".to_owned()
    }
}

fn storage_failure_reply(error: &storage::OpsStorageError) -> String {
    warn!(error_code = error.code(), "ops execution claim failed");
    "当前无法可靠领取运维执行，为避免重复运行已拒绝受理。".to_owned()
}

fn enqueue_result(
    store: &NotificationOutboxStore,
    execution_id: &str,
    target: PushTarget,
    result: &OpsExecutionResult,
    visible_task_id: Option<&str>,
    started_at: Instant,
) {
    let parts = render_result(result, visible_task_id);
    let request = NotificationUpsert {
        source_type: OPS_SOURCE_TYPE.to_owned(),
        source_id: execution_id.to_owned(),
        dedupe_key: format!("ops:{execution_id}:result"),
        target,
        channel: "push".to_owned(),
        kind: OPS_NOTIFICATION_KIND.to_owned(),
        payload: json!({"parts": notification_parts(&parts)}),
        scheduled_at: now_iso_cn(),
        max_attempts: OPS_MAX_ATTEMPTS,
        reactivate_cancelled: false,
    };
    match store.upsert(request) {
        Ok(_) => info!(
            ops_command = %result.command,
            execution_status = result.status.as_str(),
            exit_code = result.exit_code,
            elapsed_ms = started_at.elapsed().as_millis(),
            stdout_truncated = result.stdout_truncated,
            stderr_truncated = result.stderr_truncated,
            notification_parts = parts.len(),
            "ops result notification queued"
        ),
        Err(error) => warn!(
            ops_command = %result.command,
            execution_status = result.status.as_str(),
            error_code = error.code(),
            "ops result notification enqueue failed"
        ),
    }
}

fn notification_parts(parts: &[RenderedOpsPart]) -> Vec<serde_json::Value> {
    parts
        .iter()
        .map(|part| {
            json!({
                "message_type": "markdown",
                "text": part.markdown,
                "fallback_text": part.text,
            })
        })
        .collect()
}

fn managed_status(status: OpsExecutionStatus) -> ManagedTaskStatus {
    match status {
        OpsExecutionStatus::Succeeded => ManagedTaskStatus::Succeeded,
        OpsExecutionStatus::Failed => ManagedTaskStatus::Failed,
        OpsExecutionStatus::TimedOut => ManagedTaskStatus::TimedOut,
        OpsExecutionStatus::Cancelled => ManagedTaskStatus::Cancelled,
        OpsExecutionStatus::SpawnFailed => ManagedTaskStatus::SpawnFailed,
    }
}

fn format_running_time(duration: std::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if minutes == 0 {
        format!("{seconds} 秒")
    } else {
        format!("{minutes} 分 {seconds} 秒")
    }
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
