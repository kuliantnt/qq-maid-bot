//! 待办（Todo）的查询指令、用户可见编号快照与待确认操作流程。
//! Slash 写入口已移除：`/todo` 系列只保留列表/搜索等查询能力；新增、修改、
//! 完成、恢复和删除由 Tool Loop 触发。这里仍处理删除确认、目标澄清，
//! 以及旧版 `TodoAdd` pending 兼容。
//!
//! 普通自然语言待办查询不再在进入模型前做词表短路，统一走 Tool Loop 的
//! `list_todos`；本模块只处理显式 `/todo` 命令、完整结果恢复与 pending 流程。

use qq_maid_common::time_context::{parse_single_date_expression, request_time_context};

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        respond::{
            RespondRequest, RespondResponse, RustRespondService,
            common::{CommandBody, command_response, session_error, todo_error},
        },
        session::{SessionMeta, SessionRecord},
        tools::todo::{
            TodoItem, TodoOwner, TodoQueryStatus, TodoStatus, TodoStore,
            query_filter::parse_todo_list_query, remember_todo_query_snapshot, replay_todo_query,
            todo_visible_entity_snapshot, valid_last_visible_todo_query,
        },
    },
};

use chrono::NaiveDate;

mod command;
mod completed_query;
mod format;
mod group_admin;
mod pending;
mod pending_clarification;
mod pending_lifecycle;
mod receipt;

pub(crate) use command::parse_todo_command;
use completed_query::parse_completed_todo_time_query;
use format::*;
pub(crate) use receipt::aggregate_todo_tool_results;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DailyReminderCommand {
    Enable,
    Disable,
    Status,
}

fn remember_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    items: &[TodoItem],
    force_full: bool,
) {
    let query_type = query_type.into();
    let visible_items = if query_type == "all" {
        visible_todo_all_board_items(items, force_full)
    } else {
        visible_todo_items(items, force_full)
    };
    session.remember_last_todo_query(
        &owner.key,
        query_type,
        condition,
        visible_items.iter().map(|item| item.id.clone()).collect(),
    );
}

impl RustRespondService {
    fn append_todo_query_response(
        &self,
        meta: &SessionMeta,
        session: &mut SessionRecord,
        user_text: &str,
        reply: impl Into<CommandBody>,
        command: impl Into<String>,
    ) -> Result<RespondResponse, LlmError> {
        let reply = reply.into();
        self.session_store
            .append_exchange_with_latest(session, user_text, &reply.text, |latest, current| {
                // 确定性 Todo 查询必须把“用户刚刚看到的列表快照”写入最新 session，
                // 但不能用旧 SessionRecord 覆盖 Tool Loop 或其他路径刚写入的 pending、
                // 最近操作对象、历史等状态。
                latest.state = current.state.clone();
                latest.last_todo_query = current.last_todo_query.clone();
            })
            .map_err(session_error)?;
        let mut response = command_response(reply, Some(session.session_id.clone()), Some(command));
        response.visible_entity_snapshot = todo_visible_entity_snapshot(session, Some(meta));
        Ok(response)
    }

    /// 处理待办指令的主入口。解析 `/todo` 子命令并分派到对应的处理逻辑。
    pub(crate) async fn handle_todo_flow(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
        conversation_session: &SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        // Todo 默认归属当前 actor；即使消息来自群聊，也不能隐式写入群共享 Todo。
        let owner = TodoStore::owner(meta.user_id.as_deref(), &meta.scope_key);
        // “查看完整结果”只恢复最近一次可见列表截断，不承担普通自然语言查询。
        if is_full_todo_result_request(user_text) {
            let (reply, command_name) = self.full_todo_result_from_last_query(&owner, session)?;
            return Ok(Some(self.append_todo_query_response(
                meta,
                session,
                user_text,
                reply,
                command_name,
            )?));
        }
        let Some(command) = parse_todo_command(user_text) else {
            return Ok(None);
        };
        if command.action == "todo_group" {
            return self
                .handle_group_todo_command(
                    req,
                    meta,
                    session,
                    conversation_session,
                    user_text,
                    &command.argument,
                )
                .map(Some);
        }
        let write_tool_notice = self.todo_write_tool_notice(meta);

        let (reply, command_name, visible_query_shown) = match command.action.as_str() {
            "todo_list" => {
                match parse_todo_list_query(&command.argument, &request_time_context()) {
                    Ok(parsed) => {
                        let page = self
                            .task_store
                            .query_todos(&owner, &parsed.query)
                            .map_err(todo_error)?;
                        let query_type = match parsed.query.status {
                            TodoQueryStatus::All => "all",
                            TodoQueryStatus::Completed => "completed-list",
                            TodoQueryStatus::Pending
                                if parsed.query.keyword.is_some()
                                    || parsed.query.recurring.is_some()
                                    || matches!(
                                        parsed.query.time,
                                        Some(
                                            crate::runtime::tools::todo::TodoQueryTimeFilter::Overdue { .. }
                                                | crate::runtime::tools::todo::TodoQueryTimeFilter::NoDueDate
                                        )
                                    ) =>
                            {
                                "search"
                            }
                            TodoQueryStatus::Pending if parsed.query.time.is_some() => "due-date",
                            TodoQueryStatus::Pending => "list",
                        };
                        remember_todo_query_snapshot(
                            session,
                            &owner,
                            query_type,
                            parsed.condition.clone(),
                            &parsed.query,
                            page.items.iter().map(|item| item.id.clone()).collect(),
                        );
                        let title = match parsed.query.status {
                            TodoQueryStatus::Pending => "🚧 进行中",
                            TodoQueryStatus::Completed => "✅ 已完成",
                            TodoQueryStatus::All => "📋 全部待办",
                        };
                        let empty_text = if parsed.condition.is_empty() {
                            match parsed.query.status {
                                TodoQueryStatus::Pending => "暂无未完成待办",
                                TodoQueryStatus::Completed => "暂无已完成待办",
                                TodoQueryStatus::All => "当前没有待办。",
                            }
                        } else {
                            "没有找到匹配的待办。"
                        };
                        (
                            format_todo_query_page_reply(
                                &page,
                                title,
                                empty_text,
                                &parsed.condition,
                            ),
                            "todo_list".to_owned(),
                            true,
                        )
                    }
                    Err(err) => (
                        simple_todo_notice(&format!(
                            "筛选条件无效：{}\n用法：/todo list [今天|明天|本周|逾期|无截止时间] [未完成|已完成|全部] [周期性|一次性] [关键词 文本]",
                            err.message()
                        )),
                        "todo_list_invalid".to_owned(),
                        false,
                    ),
                }
            }
            "todo_all" => {
                let items = self
                    .task_store
                    .list_all_for_board(&owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, &owner, "all", "全部待办", &items, false);
                (
                    format_todo_all_reply(&items, false),
                    "todo_all".to_owned(),
                    true,
                )
            }
            "todo_search" => {
                let query = command.argument.trim();
                if let Some(completed_query) = parse_completed_todo_time_query(query) {
                    let items = self
                        .task_store
                        .list_completed_before(&owner, completed_query.completed_before)
                        .map_err(todo_error)?;
                    session.remember_last_todo_query(
                        &owner.key,
                        "completed-time",
                        completed_query.source_condition.clone(),
                        visible_todo_items(&items, false)
                            .iter()
                            .map(|item| item.id.clone())
                            .collect(),
                    );
                    (
                        format_completed_todo_time_query_reply(
                            &items,
                            &completed_query.source_condition,
                            false,
                        ),
                        "todo_completed_search".to_owned(),
                        true,
                    )
                } else if let Some(date_query) = parse_todo_due_date_query(query) {
                    let items = self
                        .task_store
                        .list_by_due_date(&owner, TodoStatus::Pending, date_query.date)
                        .map_err(todo_error)?;
                    remember_todo_query(
                        session,
                        &owner,
                        "due-date",
                        date_query.condition.clone(),
                        &items,
                        false,
                    );
                    (
                        format_todo_due_date_reply(&items, &date_query.label, false),
                        "todo_due_date".to_owned(),
                        true,
                    )
                } else {
                    session.last_todo_query = None;
                    let items = if query.is_empty() {
                        self.task_store.list_pending(&owner).map_err(todo_error)?
                    } else {
                        self.task_store
                            .search_pending(&owner, query)
                            .map_err(todo_error)?
                    };
                    remember_todo_query(session, &owner, "search", query, &items, false);
                    (
                        format_todo_search_reply(&items, query, false),
                        "todo_search".to_owned(),
                        true,
                    )
                }
            }
            "todo_done" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    let items = self.task_store.list_completed(&owner).map_err(todo_error)?;
                    remember_todo_query(
                        session,
                        &owner,
                        "completed-list",
                        "已完成列表",
                        &items,
                        false,
                    );
                    (
                        format_todo_done_list_reply(&items, false),
                        "todo_done".to_owned(),
                        true,
                    )
                } else {
                    (write_tool_notice.clone(), "todo_done".to_owned(), false)
                }
            }
            "todo_undo" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    let items = self.task_store.list_completed(&owner).map_err(todo_error)?;
                    remember_todo_query(
                        session,
                        &owner,
                        "completed-list",
                        "已完成列表",
                        &items,
                        false,
                    );
                    (
                        format_todo_done_list_reply(&items, false),
                        "todo_undo".to_owned(),
                        true,
                    )
                } else {
                    (write_tool_notice.clone(), "todo_undo".to_owned(), false)
                }
            }
            "todo_add" | "todo_edit" | "todo_delete" => {
                // 旧 slash 写入口只给迁移提示，没有真正修改待办；
                // 保留用户刚看见的编号快照，便于随后按提示用“完成第一条待办”等自然语言续指。
                (write_tool_notice.clone(), command.action, false)
            }
            "todo_daily_reminder" => (
                self.handle_todo_daily_reminder_command(meta, &owner, &command.argument)?,
                "todo_daily_reminder".to_owned(),
                false,
            ),
            _ => (self.todo_usage_notice(meta), command.action, false),
        };

        let response = if visible_query_shown {
            self.append_todo_query_response(meta, session, user_text, reply, command_name)?
        } else {
            self.append_pending_response(session, user_text, reply, command_name)?
        };
        Ok(Some(response))
    }

    fn handle_todo_daily_reminder_command(
        &self,
        meta: &SessionMeta,
        owner: &TodoOwner,
        argument: &str,
    ) -> Result<CommandBody, LlmError> {
        if meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(CommandBody::plain(
                "Todo 每日摘要只支持私聊开启；群聊里的个人 Todo 不会主动推送到群。",
            ));
        }
        match parse_daily_reminder_command(argument) {
            Some(DailyReminderCommand::Enable) => {
                self.task_store
                    .set_daily_reminder_enabled(owner, true)
                    .map_err(todo_error)?;
                Ok(CommandBody::plain(
                    "已开启 Todo 每日摘要。到配置时间后，会推送今天截止和逾期的未完成待办。",
                ))
            }
            Some(DailyReminderCommand::Disable) => {
                self.task_store
                    .set_daily_reminder_enabled(owner, false)
                    .map_err(todo_error)?;
                Ok(CommandBody::plain(
                    "已关闭 Todo 每日摘要。明确设置了提醒时间的单条 Todo 提醒不受影响。",
                ))
            }
            Some(DailyReminderCommand::Status) => {
                let enabled = self
                    .task_store
                    .daily_reminder_enabled(owner)
                    .map_err(todo_error)?;
                let status = if enabled { "开启" } else { "关闭" };
                Ok(CommandBody::plain(format!(
                    "Todo 每日摘要当前为{status}。用法：/todo daily on 或 /todo daily off。"
                )))
            }
            None => Ok(CommandBody::plain(
                "用法：/todo daily on；/todo daily off；/todo daily status。",
            )),
        }
    }

    fn todo_write_tool_notice(&self, meta: &SessionMeta) -> CommandBody {
        let policy = self.todo_notice_policy(meta);
        if matches!(policy.0, ChatScene::Group) && !policy.2 {
            return format_todo_write_private_only_reply();
        }
        if !policy.1 {
            return format_todo_write_tool_disabled_reply();
        }
        format_todo_write_tool_only_reply()
    }

    fn todo_usage_notice(&self, meta: &SessionMeta) -> CommandBody {
        let policy = self.todo_notice_policy(meta);
        if matches!(policy.0, ChatScene::Group) && !policy.2 {
            return CommandBody::plain(
                "用法：/todo [list|all|search|done|undo]；群聊默认只开放待办查询，写操作请私聊发起。",
            );
        }
        if !policy.1 {
            return CommandBody::plain(
                "用法：/todo [list|all|search|done|undo]；当前未启用工具调用，写操作暂不可用。",
            );
        }
        CommandBody::plain("用法：/todo [list|all|search|done|undo]；写操作请直接用自然语言发起。")
    }

    fn todo_notice_policy(&self, meta: &SessionMeta) -> (ChatScene, bool, bool) {
        let scene = if meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            ChatScene::Group
        } else {
            ChatScene::Private
        };
        match self.agent_config.resolve(scene) {
            Ok(policy) => (
                scene,
                policy.tool_calling_enabled,
                policy.group_tool_calling_enabled,
            ),
            Err(err) => {
                tracing::warn!(
                    error_code = %err.code,
                    error_stage = %err.stage,
                    "failed to resolve agent policy for todo notice"
                );
                (scene, false, false)
            }
        }
    }

    fn full_todo_result_from_last_query(
        &self,
        owner: &TodoOwner,
        session: &mut SessionRecord,
    ) -> Result<(CommandBody, String), LlmError> {
        let Some(query) = valid_last_visible_todo_query(session, &owner.key) else {
            return Ok((
                simple_todo_notice("当前没有可恢复的待办查询范围，请先查看待办列表。"),
                "todo_full_result_unavailable".to_owned(),
            ));
        };
        if let Some(replay_query) = replay_todo_query(&query) {
            let page = self
                .task_store
                .query_todos(owner, &replay_query)
                .map_err(todo_error)?;
            remember_todo_query_snapshot(
                session,
                owner,
                query.query_type.clone(),
                query.condition.clone(),
                &replay_query,
                page.items.iter().map(|item| item.id.clone()).collect(),
            );
            let (title, empty_text) = match replay_query.status {
                TodoQueryStatus::Pending => ("🚧 进行中", "没有找到匹配的待办。"),
                TodoQueryStatus::Completed => ("✅ 已完成", "没有找到匹配的待办。"),
                TodoQueryStatus::All => ("📋 全部待办", "当前没有待办。"),
            };
            return Ok((
                format_todo_query_page_reply(&page, title, empty_text, query.condition.trim()),
                "todo_list".to_owned(),
            ));
        }
        match query.query_type.as_str() {
            "list" => {
                let items = self.task_store.list_pending(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "list", "", &items, true);
                Ok((format_todo_list_reply(&items, true), "todo_list".to_owned()))
            }
            "all" => {
                let items = self
                    .task_store
                    .list_all_for_board(owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, owner, "all", "全部待办", &items, true);
                Ok((format_todo_all_reply(&items, true), "todo_all".to_owned()))
            }
            "completed-list" => {
                let items = self.task_store.list_completed(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "completed-list", "已完成列表", &items, true);
                Ok((
                    format_todo_done_list_reply(&items, true),
                    "todo_done".to_owned(),
                ))
            }
            "search" => {
                let condition = query.condition.trim();
                let items = if condition.is_empty() {
                    self.task_store.list_pending(owner).map_err(todo_error)?
                } else {
                    self.task_store
                        .search_pending(owner, condition)
                        .map_err(todo_error)?
                };
                remember_todo_query(session, owner, "search", condition, &items, true);
                Ok((
                    format_todo_search_reply(&items, condition, true),
                    "todo_search".to_owned(),
                ))
            }
            "completed-time" => {
                let Some(completed_query) = parse_completed_todo_time_query(&query.condition)
                else {
                    return Ok((
                        simple_todo_notice("当前待办查询范围已经无法恢复，请重新输入筛选条件。"),
                        "todo_full_result_unavailable".to_owned(),
                    ));
                };
                let items = self
                    .task_store
                    .list_completed_before(owner, completed_query.completed_before)
                    .map_err(todo_error)?;
                remember_todo_query(
                    session,
                    owner,
                    "completed-time",
                    completed_query.source_condition.clone(),
                    &items,
                    true,
                );
                Ok((
                    format_completed_todo_time_query_reply(
                        &items,
                        &completed_query.source_condition,
                        true,
                    ),
                    "todo_completed_search".to_owned(),
                ))
            }
            "due-date" => {
                let Some(date_query) = parse_todo_due_date_query(&query.condition) else {
                    return Ok((
                        simple_todo_notice("当前待办查询范围已经无法恢复，请重新输入日期条件。"),
                        "todo_full_result_unavailable".to_owned(),
                    ));
                };
                let items = self
                    .task_store
                    .list_by_due_date(owner, TodoStatus::Pending, date_query.date)
                    .map_err(todo_error)?;
                remember_todo_query(
                    session,
                    owner,
                    "due-date",
                    date_query.condition.clone(),
                    &items,
                    true,
                );
                Ok((
                    format_todo_due_date_reply(&items, &date_query.label, true),
                    "todo_due_date".to_owned(),
                ))
            }
            _ => Ok((
                simple_todo_notice("当前待办查询范围暂不支持恢复，请重新查看待办列表。"),
                "todo_full_result_unavailable".to_owned(),
            )),
        }
    }
}

fn parse_daily_reminder_command(argument: &str) -> Option<DailyReminderCommand> {
    let normalized = argument.trim().to_ascii_lowercase();
    let compact = normalized.split_whitespace().collect::<String>();
    match compact.as_str() {
        "" | "status" | "状态" | "查看" | "查询" => Some(DailyReminderCommand::Status),
        "on" | "enable" | "enabled" | "open" | "开启" | "打开" | "启用" => {
            Some(DailyReminderCommand::Enable)
        }
        "off" | "disable" | "disabled" | "close" | "关闭" | "关掉" | "停用" => {
            Some(DailyReminderCommand::Disable)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoDueDateQuery {
    label: String,
    condition: String,
    date: NaiveDate,
}

fn parse_todo_due_date_query(text: &str) -> Option<TodoDueDateQuery> {
    let time_ctx = request_time_context();
    let date = parse_single_date_expression(text, &time_ctx)?;
    Some(TodoDueDateQuery {
        label: date.raw.clone(),
        condition: date.date.format("%Y-%m-%d").to_string(),
        date: date.date,
    })
}

/// 折叠列表后的“查看完整结果”入口；普通自然语言待办查询不再做词表短路。
pub(crate) fn is_full_todo_result_request(text: &str) -> bool {
    matches!(
        text.trim().replace("代办", "待办").as_str(),
        "查看完整结果" | "显示完整结果" | "看完整结果" | "完整结果"
    )
}
