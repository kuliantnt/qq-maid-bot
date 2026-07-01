//! 待办（Todo）的查询指令、用户可见编号快照与待确认操作流程。
//! Slash 写入口已移除：`/todo` 系列只保留列表/搜索等查询能力；新增、修改、
//! 完成、恢复、取消和永久删除由 Tool Loop 触发。这里仍处理取消/永久删除确认、
//! 目标澄清，以及旧版 `TodoAdd` pending 兼容。

use crate::{
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord},
        todo::{TodoItem, TodoOwner, TodoStore},
    },
};

use super::{
    RespondResponse, RustRespondService,
    common::{CommandBody, todo_error},
};

mod command;
mod completed_query;
mod format;
mod pending;

pub(super) use command::parse_todo_command;
use completed_query::parse_completed_todo_time_query;
use format::*;

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
const TODO_QUERY_COMPLETED_EXACT: &[&str] =
    &["已完成的待办", "看看已完成", "查看已完成", "列出已完成"];
const TODO_QUERY_CANCELLED_EXACT: &[&str] =
    &["已取消的待办", "看看已取消", "查看已取消", "列出已取消"];

fn remember_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    items: &[TodoItem],
) {
    session.remember_last_todo_query(
        &owner.key,
        query_type,
        condition,
        items.iter().map(|item| item.id.clone()).collect(),
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

        let (reply, command_name, visible_query_shown) = match command.action.as_str() {
            "todo_list" => {
                let items = self.todo_store.list_pending(&owner).map_err(todo_error)?;
                remember_todo_query(session, &owner, "list", "", &items);
                (format_todo_list_reply(&items), "todo_list".to_owned(), true)
            }
            "todo_all" => {
                let items = self
                    .todo_store
                    .list_all_for_board(&owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, &owner, "all", "全部待办", &items);
                (format_todo_all_reply(&items), "todo_all".to_owned(), true)
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
                        items.iter().map(|item| item.id.clone()).collect(),
                    );
                    (
                        format_completed_todo_time_query_reply(
                            &items,
                            &completed_query.source_condition,
                        ),
                        "todo_completed_search".to_owned(),
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
                    remember_todo_query(session, &owner, "search", query, &items);
                    (
                        format_todo_search_reply(&items, query),
                        "todo_search".to_owned(),
                        true,
                    )
                }
            }
            "todo_done" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    let items = self.todo_store.list_completed(&owner).map_err(todo_error)?;
                    remember_todo_query(session, &owner, "completed-list", "已完成列表", &items);
                    (
                        format_todo_done_list_reply(&items),
                        "todo_done".to_owned(),
                        true,
                    )
                } else {
                    (
                        format_todo_write_tool_only_reply(),
                        "todo_done".to_owned(),
                        false,
                    )
                }
            }
            "todo_undo" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    let items = self.todo_store.list_completed(&owner).map_err(todo_error)?;
                    remember_todo_query(session, &owner, "completed-list", "已完成列表", &items);
                    (
                        format_todo_done_list_reply(&items),
                        "todo_undo".to_owned(),
                        true,
                    )
                } else {
                    (
                        format_todo_write_tool_only_reply(),
                        "todo_undo".to_owned(),
                        false,
                    )
                }
            }
            "todo_cancelled_list" => {
                let items = self.todo_store.list_cancelled(&owner).map_err(todo_error)?;
                remember_todo_query(session, &owner, "cancelled-list", "已取消列表", &items);
                (
                    format_todo_cancelled_list_reply(&items),
                    "todo_cancelled_list".to_owned(),
                    true,
                )
            }
            "todo_add" | "todo_edit" | "todo_delete" => {
                // 旧 slash 写入口只给迁移提示，没有真正修改待办；
                // 保留用户刚看见的编号快照，便于随后按提示用“完成第一条待办”等自然语言续指。
                (format_todo_write_tool_only_reply(), command.action, false)
            }
            _ => (
                CommandBody::plain(
                    "用法：/todo [list|all|search|done|undo]；写操作请直接用自然语言发起。",
                ),
                command.action,
                false,
            ),
        };

        let response = if visible_query_shown {
            self.append_todo_query_response(session, user_text, reply, command_name)?
        } else {
            self.append_pending_response(session, user_text, reply, command_name)?
        };
        Ok(Some(response))
    }

    fn try_handle_natural_todo_query(
        &self,
        user_text: &str,
        owner: &TodoOwner,
        session: &mut SessionRecord,
    ) -> Result<Option<(CommandBody, String)>, LlmError> {
        let Some(query_kind) = detect_natural_todo_query_kind(user_text) else {
            return Ok(None);
        };
        let result = match query_kind {
            NaturalTodoQueryKind::Pending => {
                let items = self.todo_store.list_pending(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "list", "", &items);
                (format_todo_list_reply(&items), "todo_list".to_owned())
            }
            NaturalTodoQueryKind::All => {
                let items = self
                    .todo_store
                    .list_all_for_board(owner)
                    .map_err(todo_error)?;
                remember_todo_query(session, owner, "all", "全部待办", &items);
                (format_todo_all_reply(&items), "todo_all".to_owned())
            }
            NaturalTodoQueryKind::Completed => {
                let items = self.todo_store.list_completed(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "completed-list", "已完成列表", &items);
                (format_todo_done_list_reply(&items), "todo_done".to_owned())
            }
            NaturalTodoQueryKind::Cancelled => {
                let items = self.todo_store.list_cancelled(owner).map_err(todo_error)?;
                remember_todo_query(session, owner, "cancelled-list", "已取消列表", &items);
                (
                    format_todo_cancelled_list_reply(&items),
                    "todo_cancelled_list".to_owned(),
                )
            }
        };
        Ok(Some(result))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NaturalTodoQueryKind {
    Pending,
    All,
    Completed,
    Cancelled,
}

fn detect_natural_todo_query_kind(user_text: &str) -> Option<NaturalTodoQueryKind> {
    let text = normalize_natural_todo_text(user_text);
    if text.is_empty() {
        return None;
    }
    // slash `/todo ...` 继续走显式命令解析，避免自然语言词表误抢 `/任务 搜 查询` 之类分支。
    if text.starts_with('/') {
        return None;
    }
    if TODO_QUERY_PENDING_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQueryKind::Pending);
    }
    if TODO_QUERY_COMPLETED_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQueryKind::Completed);
    }
    if TODO_QUERY_CANCELLED_EXACT.contains(&text.as_str()) {
        return Some(NaturalTodoQueryKind::Cancelled);
    }
    let mentions_todo = TODO_QUERY_NOUNS.iter().any(|needle| text.contains(needle));
    let asks_list = TODO_QUERY_LIST_VERBS
        .iter()
        .any(|needle| text.contains(needle));
    if !mentions_todo || !asks_list {
        return None;
    }
    if TODO_QUERY_ALL_MARKERS
        .iter()
        .any(|needle| text.contains(needle))
    {
        return Some(NaturalTodoQueryKind::All);
    }
    if text.contains("已取消") {
        return Some(NaturalTodoQueryKind::Cancelled);
    }
    if text.contains("已完成") {
        return Some(NaturalTodoQueryKind::Completed);
    }
    Some(NaturalTodoQueryKind::Pending)
}

/// 自然语言待办查询入口统一做别字归一化，避免“代办/待办”落到不同链路。
pub(super) fn normalize_natural_todo_text(text: &str) -> String {
    text.trim().replace("代办", "待办")
}

/// 判断一段自然语言是否应走确定性的待办查询分支。
///
/// 这里与 Tool Loop 守卫、入站分类共享同一套词表，避免简单查询重新漂回 LLM 自由决策。
pub(super) fn is_natural_todo_query_text(text: &str) -> bool {
    detect_natural_todo_query_kind(text).is_some()
}
