//! 待办（Todo）的查询指令、用户可见编号快照与待确认操作流程。
//! Slash 写入口已移除：`/todo` 系列只保留列表/搜索等查询能力；新增、修改、
//! 完成、恢复、取消和永久删除由 Tool Loop 触发。这里仍处理取消/永久删除确认、
//! 目标澄清，以及旧版 `TodoAdd` pending 兼容。

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord},
        todo::{TodoItem, TodoOwner, TodoStatus, TodoStore},
    },
    storage::session::valid_last_visible_todo_query,
    util::time_context::{parse_single_date_expression, request_time_context},
};

use chrono::NaiveDate;

use super::{
    RespondResponse, RustRespondService,
    common::{CommandBody, todo_error},
};

mod command;
mod completed_query;
mod format;
mod pending;
mod receipt;

pub(super) use command::parse_todo_command;
use completed_query::parse_completed_todo_time_query;
use format::*;
pub(in crate::runtime::respond) use receipt::aggregate_todo_tool_results;

const TODO_QUERY_NOUNS: &[&str] = &["待办", "代办", "任务"];
const TODO_QUERY_LIST_VERBS: &[&str] = &[
    "看一下",
    "看下",
    "看看",
    "查询",
    "查看",
    "列出",
    "有哪些",
    "我的",
];
const TODO_QUERY_ALL_MARKERS: &[&str] = &["全部", "所有", "包含已完成", "包含已取消"];
const TODO_QUERY_PENDING_EXACT: &[&str] = &["我的待办", "待办列表"];
const TODO_QUERY_ALL_EXACT: &[&str] = &[
    "全部待办",
    "所有待办",
    "全部代办",
    "所有代办",
    "全部任务",
    "所有任务",
];
const TODO_QUERY_ALL_FULL_EXACT: &[&str] = &[
    "完整待办",
    "完整代办",
    "完整任务",
    "查看完整待办",
    "查看完整代办",
    "查看完整任务",
    "看完整待办",
    "看完整代办",
    "看完整任务",
    "显示完整待办",
    "显示完整代办",
    "显示完整任务",
];
const TODO_QUERY_COMPLETED_EXACT: &[&str] =
    &["已完成的待办", "看看已完成", "查看已完成", "列出已完成"];
const TODO_QUERY_CANCELLED_EXACT: &[&str] =
    &["已取消的待办", "看看已取消", "查看已取消", "列出已取消"];
const TODO_QUERY_COMPLETED_MARKERS: &[&str] =
    &["已完成", "完成的", "做完", "做完的", "搞定的", "结束的"];
const TODO_QUERY_CANCELLED_MARKERS: &[&str] =
    &["已取消", "取消的", "被取消", "取消列表", "已作废", "作废的"];
const TODO_QUERY_PENDING_MARKERS: &[&str] = &["进行中", "待处理", "待完成"];
const TODO_QUERY_PENDING_NEGATED_COMPLETED_MARKERS: &[&str] = &[
    "未完成",
    "没完成",
    "没有完成",
    "未做完",
    "没做完",
    "没有做完",
    "还没做完",
    "还没有做完",
    "未结束",
    "没结束",
    "没有结束",
    "还没结束",
];
const TODO_QUERY_NEGATED_CANCELLED_MARKERS: &[&str] =
    &["未取消", "没取消", "没有取消", "还没取消", "还没有取消"];
const TODO_QUERY_WRITE_MARKERS: &[&str] = &[
    "新增",
    "添加",
    "增加",
    "创建",
    "记下",
    "加一个",
    "加一条",
    "加到",
    "完成",
    "取消",
    "恢复",
    "删除",
    "修改",
    "更新",
    "改成",
];

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
            .map_err(super::common::session_error)?;
        Ok(super::common::command_response(
            reply,
            Some(session.session_id.clone()),
            Some(command),
        ))
    }

    /// 处理待办指令的主入口。解析 `/todo` 子命令并分派到对应的处理逻辑。
    pub(super) async fn handle_todo_flow(
        &self,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let owner = TodoStore::owner(meta.user_id.as_deref(), &meta.scope_key);
        if is_full_todo_result_request(user_text) {
            let (reply, command_name) = self.full_todo_result_from_last_query(&owner, session)?;
            return Ok(Some(self.append_todo_query_response(
                session,
                user_text,
                reply,
                command_name,
            )?));
        }
        // 自然语言“看看我的待办 / 看看已完成的待办”优先按查询处理，
        // 避免旧的隐式创建/解析链路把纯查询误判成 todo_add -> todo_parse。
        if let Some((reply, command_name)) =
            self.try_handle_natural_todo_query(user_text, &owner, session)?
        {
            return Ok(Some(self.append_todo_query_response(
                session,
                user_text,
                reply,
                command_name,
            )?));
        }
        let Some(command) = parse_todo_command(user_text) else {
            return Ok(None);
        };
        let write_tool_notice = self.todo_write_tool_notice(meta);

        let (reply, command_name, visible_query_shown) = match command.action.as_str() {
            "todo_list" => {
                if let Some(date_query) = parse_todo_due_date_query(&command.argument) {
                    let items = self
                        .todo_store
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
                    let items = self.todo_store.list_pending(&owner).map_err(todo_error)?;
                    remember_todo_query(session, &owner, "list", "", &items, false);
                    (
                        format_todo_list_reply(&items, false),
                        "todo_list".to_owned(),
                        true,
                    )
                }
            }
            "todo_all" => {
                let items = self
                    .todo_store
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
                        .todo_store
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
                        .todo_store
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
                        self.todo_store.list_pending(&owner).map_err(todo_error)?
                    } else {
                        self.todo_store
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
                    let items = self.todo_store.list_completed(&owner).map_err(todo_error)?;
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
                    let items = self.todo_store.list_completed(&owner).map_err(todo_error)?;
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
            "todo_cancelled_list" => {
                let items = self.todo_store.list_cancelled(&owner).map_err(todo_error)?;
                remember_todo_query(
                    session,
                    &owner,
                    "cancelled-list",
                    "已取消列表",
                    &items,
                    false,
                );
                (
                    format_todo_cancelled_list_reply(&items, false),
                    "todo_cancelled_list".to_owned(),
                    true,
                )
            }
            "todo_add" | "todo_edit" | "todo_delete" => {
                // 旧 slash 写入口只给迁移提示，没有真正修改待办；
                // 保留用户刚看见的编号快照，便于随后按提示用“完成第一条待办”等自然语言续指。
                (write_tool_notice.clone(), command.action, false)
            }
            _ => (self.todo_usage_notice(meta), command.action, false),
        };

        let response = if visible_query_shown {
            self.append_todo_query_response(session, user_text, reply, command_name)?
        } else {
            self.append_pending_response(session, user_text, reply, command_name)?
        };
        Ok(Some(response))
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

    fn try_handle_natural_todo_query(
        &self,
        user_text: &str,
        owner: &TodoOwner,
        session: &mut SessionRecord,
    ) -> Result<Option<(CommandBody, String)>, LlmError> {
        let Some(query) = detect_natural_todo_query(user_text) else {
            return Ok(None);
        };
        let result = match query.kind {
            NaturalTodoQueryKind::Pending => {
                let items = self.todo_store.list_pending(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "list", "", &items, query.force_full);
                (
                    format_todo_list_reply(&items, query.force_full),
                    "todo_list".to_owned(),
                )
            }
            NaturalTodoQueryKind::DueDate(date_query) => {
                let items = self
                    .todo_store
                    .list_by_due_date(owner, TodoStatus::Pending, date_query.date)
                    .map_err(todo_error)?;
                remember_todo_query(
                    session,
                    owner,
                    "due-date",
                    date_query.condition.clone(),
                    &items,
                    query.force_full,
                );
                (
                    format_todo_due_date_reply(&items, &date_query.label, query.force_full),
                    "todo_due_date".to_owned(),
                )
            }
            NaturalTodoQueryKind::All => {
                let items = self
                    .todo_store
                    .list_all_for_board(owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, owner, "all", "全部待办", &items, query.force_full);
                (
                    format_todo_all_reply(&items, query.force_full),
                    "todo_all".to_owned(),
                )
            }
            NaturalTodoQueryKind::Completed => {
                let items = self.todo_store.list_completed(owner).map_err(todo_error)?;
                remember_todo_query(
                    session,
                    owner,
                    "completed-list",
                    "已完成列表",
                    &items,
                    query.force_full,
                );
                (
                    format_todo_done_list_reply(&items, query.force_full),
                    "todo_done".to_owned(),
                )
            }
            NaturalTodoQueryKind::Cancelled => {
                let items = self.todo_store.list_cancelled(owner).map_err(todo_error)?;
                remember_todo_query(
                    session,
                    owner,
                    "cancelled-list",
                    "已取消列表",
                    &items,
                    query.force_full,
                );
                (
                    format_todo_cancelled_list_reply(&items, query.force_full),
                    "todo_cancelled_list".to_owned(),
                )
            }
        };
        Ok(Some(result))
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
        match query.query_type.as_str() {
            "list" => {
                let items = self.todo_store.list_pending(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "list", "", &items, true);
                Ok((format_todo_list_reply(&items, true), "todo_list".to_owned()))
            }
            "all" => {
                let items = self
                    .todo_store
                    .list_all_for_board(owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, owner, "all", "全部待办", &items, true);
                Ok((format_todo_all_reply(&items, true), "todo_all".to_owned()))
            }
            "completed-list" => {
                let items = self.todo_store.list_completed(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "completed-list", "已完成列表", &items, true);
                Ok((
                    format_todo_done_list_reply(&items, true),
                    "todo_done".to_owned(),
                ))
            }
            "cancelled-list" => {
                let items = self.todo_store.list_cancelled(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "cancelled-list", "已取消列表", &items, true);
                Ok((
                    format_todo_cancelled_list_reply(&items, true),
                    "todo_cancelled_list".to_owned(),
                ))
            }
            "search" => {
                let condition = query.condition.trim();
                let items = if condition.is_empty() {
                    self.todo_store.list_pending(owner).map_err(todo_error)?
                } else {
                    self.todo_store
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
                    .todo_store
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
                    .todo_store
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum NaturalTodoQueryKind {
    Pending,
    DueDate(TodoDueDateQuery),
    All,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NaturalTodoQuery {
    kind: NaturalTodoQueryKind,
    force_full: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoDueDateQuery {
    label: String,
    condition: String,
    date: NaiveDate,
}

fn detect_natural_todo_query(user_text: &str) -> Option<NaturalTodoQuery> {
    let text = normalize_natural_todo_text(user_text);
    if text.is_empty() {
        return None;
    }
    // slash `/todo ...` 继续走显式命令解析，避免自然语言词表误抢 `/任务 搜 查询` 之类分支。
    if text.starts_with('/') {
        return None;
    }
    let force_full = contains_full_marker(&text);
    if let Some(date_query) = detect_natural_todo_due_date_query(&text) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::DueDate(date_query),
            force_full,
        });
    }
    if TODO_QUERY_PENDING_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Pending,
            force_full,
        });
    }
    if TODO_QUERY_ALL_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::All,
            // “全部待办”表达跨状态范围，不等同于展开全部结果；展开仍由
            // “查看完整结果”基于最近查询快照恢复。
            force_full: false,
        });
    }
    if TODO_QUERY_ALL_FULL_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::All,
            force_full: true,
        });
    }
    if TODO_QUERY_COMPLETED_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Completed,
            force_full,
        });
    }
    if TODO_QUERY_CANCELLED_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Cancelled,
            force_full,
        });
    }
    let mentions_todo = contains_any(&text, TODO_QUERY_NOUNS);
    let asks_list = TODO_QUERY_LIST_VERBS
        .iter()
        .any(|needle| text.contains(needle));
    if !asks_list {
        return None;
    }
    let mentions_list = text.contains("列表") || text.contains("清单");
    let mentions_completed = contains_any(&text, TODO_QUERY_COMPLETED_MARKERS);
    let mentions_cancelled = contains_any(&text, TODO_QUERY_CANCELLED_MARKERS);
    let mentions_pending = contains_any(&text, TODO_QUERY_PENDING_MARKERS);
    let mentions_pending_by_negated_completed =
        contains_any(&text, TODO_QUERY_PENDING_NEGATED_COMPLETED_MARKERS);
    let mentions_negated_cancelled = contains_any(&text, TODO_QUERY_NEGATED_CANCELLED_MARKERS);
    let mentions_state = mentions_completed
        || mentions_cancelled
        || mentions_pending
        || mentions_pending_by_negated_completed
        || mentions_negated_cancelled;
    if !(mentions_todo || mentions_list && mentions_state) {
        return None;
    }
    // 否定状态必须先于肯定子串判断；例如“未完成的待办”包含“完成的”，
    // 但语义是进行中列表，不是已完成列表。
    if mentions_pending_by_negated_completed || mentions_pending {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Pending,
            force_full,
        });
    }
    if mentions_negated_cancelled {
        return None;
    }
    let mentions_all = TODO_QUERY_ALL_MARKERS
        .iter()
        .any(|needle| text.contains(needle));
    // “包含已完成 / 包含已取消”表达的是跨状态总览，不是单独的已完成列表。
    if mentions_all && text.contains("包含") {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::All,
            force_full,
        });
    }
    if mentions_all
        && !text.contains("已完成")
        && !text.contains("已取消")
        && !mentions_cancelled
        && !mentions_completed
    {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::All,
            force_full,
        });
    }
    // 状态判断只在“列表查询语义”确认后执行，避免“取消这个待办”等写操作被抢走。
    if mentions_cancelled {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Cancelled,
            force_full,
        });
    }
    if mentions_completed {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::Completed,
            force_full,
        });
    }
    if mentions_all {
        return Some(NaturalTodoQuery {
            kind: NaturalTodoQueryKind::All,
            force_full,
        });
    }
    Some(NaturalTodoQuery {
        kind: NaturalTodoQueryKind::Pending,
        force_full,
    })
}

fn detect_natural_todo_due_date_query(text: &str) -> Option<TodoDueDateQuery> {
    let date_query = parse_todo_due_date_query(text)?;
    let mentions_pending_by_negated_completed =
        contains_any(text, TODO_QUERY_PENDING_NEGATED_COMPLETED_MARKERS);
    if contains_todo_due_date_write_marker(text, mentions_pending_by_negated_completed) {
        return None;
    }
    let mentions_completed = contains_any(text, TODO_QUERY_COMPLETED_MARKERS);
    let mentions_cancelled = contains_any(text, TODO_QUERY_CANCELLED_MARKERS);
    if mentions_cancelled || mentions_completed && !mentions_pending_by_negated_completed {
        return None;
    }
    let mentions_todo = contains_any(text, TODO_QUERY_NOUNS);
    let asks_list = TODO_QUERY_LIST_VERBS
        .iter()
        .any(|needle| text.contains(needle));
    let asks_daily_work = [
        "要做什么",
        "要做啥",
        "有什么事",
        "有哪些事",
        "有什么安排",
        "有哪些安排",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    if mentions_todo
        || asks_list && (text.contains("有哪些") || text.contains("有什么"))
        || asks_daily_work
    {
        Some(date_query)
    } else {
        None
    }
}

fn contains_todo_due_date_write_marker(
    text: &str,
    mentions_pending_by_negated_completed: bool,
) -> bool {
    TODO_QUERY_WRITE_MARKERS.iter().any(|needle| {
        // “未完成/没完成”是未完成状态查询，不是完成写操作；其他写词仍拦截，
        // 避免“明天删除待办”等操作句误入确定性日期查询。
        !(*needle == "完成" && mentions_pending_by_negated_completed) && text.contains(needle)
    })
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

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// 自然语言待办查询入口统一做别字归一化，避免“代办/待办”落到不同链路。
pub(super) fn normalize_natural_todo_text(text: &str) -> String {
    text.trim().replace("代办", "待办")
}

/// 判断一段自然语言是否应走确定性的待办查询分支。
///
/// 这里与 Tool Loop 守卫、入站分类共享同一套词表，避免简单查询重新漂回 LLM 自由决策。
pub(super) fn is_natural_todo_query_text(text: &str) -> bool {
    is_full_todo_result_request(text) || detect_natural_todo_query(text).is_some()
}

fn contains_full_marker(text: &str) -> bool {
    text.contains("全部") || text.contains("所有") || text.contains("完整")
}

fn is_full_todo_result_request(text: &str) -> bool {
    matches!(
        normalize_natural_todo_text(text).as_str(),
        "查看完整结果" | "显示完整结果" | "看完整结果" | "完整结果"
    )
}
