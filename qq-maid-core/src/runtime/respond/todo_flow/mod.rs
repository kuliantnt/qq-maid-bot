//! 待办（Todo）的指令处理和待确认操作流程。
//! 负责解析 `/todo` 系列子命令（list/all/add/done/undo/edit/delete/search）、
//! 调用 LLM 解析自然语言待办内容、以及处理新增/完成/编辑/删除
//! 待操作的待确认交互（确认、取消、修改草稿、多候选选择）。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    error::LlmError,
    runtime::{
        pending::{PendingOperation, PendingTodoAction},
        session::{SessionMeta, SessionRecord, now_iso_cn, valid_last_visible_todo_query},
        todo::{
            TodoItem, TodoItemDraft, TodoOwner, TodoStatus, TodoStore, enrich_draft_time_from_text,
        },
    },
    util::time_context::request_time_context,
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{CommandBody, empty_respond_request, todo_error},
    llm_service::{ChatService, LlmChatService},
};

use train_todo::{TrainTodoAddOutcome, looks_like_train_todo_add};

mod command;
mod completed_query;
mod draft;
mod format;
mod pending;
mod target;
mod train_todo;

pub(super) use command::parse_todo_command;
use completed_query::{parse_completed_todo_time_query, valid_last_completed_todo_bulk_query};
use draft::{
    TodoEditPatch, apply_todo_edit_patch, enrich_todo_edit_patch_time_from_text,
    parse_todo_draft_json, parse_todo_edit_patch_json,
};
use format::*;
use target::{
    TodoTarget, is_cancelled_todo_cleanup_target, is_completed_todo_cleanup_target,
    parse_todo_edit_argument, parse_todo_number_list, remember_todo_query,
    resolve_todo_numbers_from_snapshot, resolve_todo_target, todo_target_label,
};

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
        let initiator_user_id = meta.user_id.clone();
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
                let items = self.todo_store.list_all(&owner).map_err(todo_error)?;
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
            "todo_add" => {
                session.last_todo_query = None;
                let argument = command.argument.trim();
                if argument.is_empty() {
                    (
                        CommandBody::plain("用法：/todo add 待办内容"),
                        "todo_add".to_owned(),
                        false,
                    )
                } else {
                    // 先用本地保守规则判断是否明显像火车行程，避免普通 Todo
                    // 稳定多一次 train_add 模型请求和 12306 校验链路。
                    if looks_like_train_todo_add(argument) {
                        match self.try_handle_train_todo_add(argument, &owner).await? {
                            TrainTodoAddOutcome::Handled { reply, pending } => {
                                if let Some(pending_add) = pending {
                                    session.pending_operation = Some(PendingOperation::TodoAdd {
                                        initiator_user_id: initiator_user_id.clone(),
                                        owner_key: pending_add.owner_key,
                                        draft: pending_add.draft,
                                        allow_revision: false,
                                        created_at: pending_add.created_at,
                                    });
                                }
                                (reply, "todo_train_add".to_owned(), false)
                            }
                            // 启发式只负责“是否值得尝试 train_add”；
                            // 只要 LLM 没有明确识别为火车行程，就回退普通 Todo，
                            // 避免误杀带编号、日期的日常待办。
                            TrainTodoAddOutcome::NotTrain => {
                                match self.parse_todo_draft(argument, None).await? {
                                    Ok(draft) => {
                                        session.pending_operation =
                                            Some(PendingOperation::TodoAdd {
                                                initiator_user_id: initiator_user_id.clone(),
                                                owner_key: owner.key.clone(),
                                                draft: draft.clone(),
                                                allow_revision: true,
                                                created_at: now_iso_cn(),
                                            });
                                        (
                                            format_todo_add_confirm(&draft),
                                            "todo_add".to_owned(),
                                            false,
                                        )
                                    }
                                    Err(message) => {
                                        (CommandBody::plain(message), "todo_add".to_owned(), false)
                                    }
                                }
                            }
                        }
                    } else {
                        match self.parse_todo_draft(argument, None).await? {
                            Ok(draft) => {
                                session.pending_operation = Some(PendingOperation::TodoAdd {
                                    initiator_user_id: initiator_user_id.clone(),
                                    owner_key: owner.key.clone(),
                                    draft: draft.clone(),
                                    allow_revision: true,
                                    created_at: now_iso_cn(),
                                });
                                (
                                    format_todo_add_confirm(&draft),
                                    "todo_add".to_owned(),
                                    false,
                                )
                            }
                            Err(message) => {
                                (CommandBody::plain(message), "todo_add".to_owned(), false)
                            }
                        }
                    }
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
                    let (reply, command_name) =
                        self.complete_todo_list_numbers(session, &owner, argument)?;
                    (reply, command_name, false)
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
                    let (reply, command_name) =
                        self.restore_todo_list_numbers(session, &owner, argument)?;
                    (reply, command_name, false)
                }
            }
            "todo_delete" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    if let Some(query) = valid_last_completed_todo_bulk_query(session, &owner) {
                        let (reply, command_name) = self.prepare_todo_bulk_delete_from_ids(
                            session,
                            &owner,
                            initiator_user_id.clone(),
                            query.result_ids,
                            query.condition,
                            TodoStatus::Completed,
                        )?;
                        (reply, command_name, false)
                    } else {
                        (
                            format_todo_delete_usage_reply(),
                            "todo_delete".to_owned(),
                            false,
                        )
                    }
                } else if is_completed_todo_cleanup_target(argument) {
                    let items = self.todo_store.list_completed(&owner).map_err(todo_error)?;
                    remember_todo_query(
                        session,
                        &owner,
                        "completed-list",
                        "全部已完成待办",
                        &items,
                    );
                    let (reply, command_name) = self.prepare_todo_bulk_delete_from_items(
                        session,
                        &owner,
                        initiator_user_id.clone(),
                        items,
                        "全部已完成待办".to_owned(),
                        TodoStatus::Completed,
                    )?;
                    (reply, command_name, false)
                } else if is_cancelled_todo_cleanup_target(argument) {
                    let items = self.todo_store.list_cancelled(&owner).map_err(todo_error)?;
                    remember_todo_query(
                        session,
                        &owner,
                        "cancelled-list",
                        "全部已取消待办",
                        &items,
                    );
                    let (reply, command_name) = self.prepare_todo_bulk_delete_from_items(
                        session,
                        &owner,
                        initiator_user_id.clone(),
                        items,
                        "全部已取消待办".to_owned(),
                        TodoStatus::Cancelled,
                    )?;
                    (reply, command_name, false)
                } else if let Some(completed_query) = parse_completed_todo_time_query(argument) {
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
                    let (reply, command_name) = self.prepare_todo_bulk_delete_from_items(
                        session,
                        &owner,
                        initiator_user_id.clone(),
                        items,
                        completed_query.source_condition,
                        TodoStatus::Completed,
                    )?;
                    (reply, command_name, false)
                } else {
                    let (reply, command_name) = self.prepare_todo_match_operation(
                        session,
                        &owner,
                        initiator_user_id.clone(),
                        PendingTodoAction::Delete,
                        argument,
                    )?;
                    (reply, command_name, false)
                }
            }
            "todo_edit" => {
                let argument = command.argument.trim();
                let Some((target, edit_text)) = parse_todo_edit_argument(argument) else {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        format_todo_edit_usage_reply(),
                        "todo_edit",
                    )?));
                };
                let target = resolve_todo_target(session, &owner, &target, true);
                let target_label = todo_target_label(&target);
                let candidates = match &target {
                    TodoTarget::ListIndex { id, .. } => self.match_pending_todo_id(&owner, id)?,
                    TodoTarget::MissingListIndex {
                        index,
                        source_label,
                        source_command,
                    } => {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            format_todo_missing_snapshot_index_reply(
                                *index,
                                source_label,
                                source_command,
                            ),
                            "todo_edit",
                        )?));
                    }
                    TodoTarget::ListUnavailable {
                        source_label,
                        source_command,
                    } => {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            format_todo_snapshot_unavailable_reply(source_label, source_command),
                            "todo_edit",
                        )?));
                    }
                    TodoTarget::Query(query) => self
                        .todo_store
                        .match_pending(&owner, query)
                        .map_err(todo_error)?,
                };
                match candidates.as_slice() {
                    [] => (
                        format_todo_no_match_reply(&target_label),
                        "todo_edit".to_owned(),
                        false,
                    ),
                    [item] => match self.parse_todo_edit_draft(&edit_text, item).await? {
                        Ok(draft) => {
                            session.pending_operation = Some(PendingOperation::TodoEdit {
                                initiator_user_id: initiator_user_id.clone(),
                                owner_key: owner.key.clone(),
                                before: item.clone(),
                                draft: draft.clone(),
                                created_at: now_iso_cn(),
                            });
                            (
                                format_todo_edit_confirm(item, &draft),
                                "todo_edit".to_owned(),
                                false,
                            )
                        }
                        Err(message) => {
                            (CommandBody::plain(message), "todo_edit".to_owned(), false)
                        }
                    },
                    _ => {
                        let candidates = candidates.into_iter().take(5).collect::<Vec<_>>();
                        session.pending_operation = Some(PendingOperation::TodoSelectCandidate {
                            initiator_user_id: initiator_user_id.clone(),
                            owner_key: owner.key.clone(),
                            action: PendingTodoAction::Edit,
                            candidates: candidates.clone(),
                            edit_text: Some(edit_text),
                            created_at: now_iso_cn(),
                        });
                        (
                            format_todo_candidate_selection(&PendingTodoAction::Edit, &candidates),
                            "todo_select".to_owned(),
                            false,
                        )
                    }
                }
            }
            _ => {
                session.last_todo_query = None;
                (
                    CommandBody::plain("用法：/todo [list|all|add|done|undo|edit|delete|search]"),
                    command.action,
                    false,
                )
            }
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
                let items = self.todo_store.list_all(owner).map_err(todo_error)?;
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

    /// 准备待办匹配操作：根据用户输入解析目标，匹配待办，设置待确认状态。
    fn prepare_todo_match_operation(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        initiator_user_id: Option<String>,
        action: PendingTodoAction,
        target: &str,
    ) -> Result<(CommandBody, String), LlmError> {
        let command = match &action {
            PendingTodoAction::Done => "todo_done",
            PendingTodoAction::Edit => "todo_edit",
            PendingTodoAction::Delete => "todo_delete",
        }
        .to_owned();
        let target = resolve_todo_target(session, owner, target, true);
        let target_label = todo_target_label(&target);
        let candidates = match &target {
            TodoTarget::ListIndex { id, .. } if action == PendingTodoAction::Delete => {
                self.match_todo_id_for_delete(owner, id)?
            }
            TodoTarget::ListIndex { id, .. } => self.match_pending_todo_id(owner, id)?,
            TodoTarget::MissingListIndex {
                index,
                source_label,
                source_command,
            } => {
                return Ok((
                    format_todo_missing_snapshot_index_reply(*index, source_label, source_command),
                    command,
                ));
            }
            TodoTarget::ListUnavailable {
                source_label,
                source_command,
            } => {
                return Ok((
                    format_todo_snapshot_unavailable_reply(source_label, source_command),
                    command,
                ));
            }
            TodoTarget::Query(query) => self
                .todo_store
                .match_pending(owner, query)
                .map_err(todo_error)?,
        };
        match candidates.as_slice() {
            [] => Ok((format_todo_no_match_reply(&target_label), command)),
            [item] => match &action {
                PendingTodoAction::Done => {
                    session.pending_operation = Some(PendingOperation::TodoDone {
                        initiator_user_id: initiator_user_id.clone(),
                        owner_key: owner.key.clone(),
                        item: item.clone(),
                        created_at: now_iso_cn(),
                    });
                    Ok((format_todo_done_confirm(item), command))
                }
                PendingTodoAction::Delete => self.prepare_todo_delete_operation(
                    session,
                    owner,
                    initiator_user_id.clone(),
                    item,
                    todo_delete_source_condition(&target),
                ),
                PendingTodoAction::Edit => unreachable!("edit prepares candidates separately"),
            },
            _ => {
                let candidates = candidates.into_iter().take(5).collect::<Vec<_>>();
                session.pending_operation = Some(PendingOperation::TodoSelectCandidate {
                    initiator_user_id,
                    owner_key: owner.key.clone(),
                    action: action.clone(),
                    candidates: candidates.clone(),
                    edit_text: None,
                    created_at: now_iso_cn(),
                });
                Ok((
                    format_todo_candidate_selection(&action, &candidates),
                    command,
                ))
            }
        }
    }

    /// 准备单条删除待确认操作。
    /// 已取消待办确认后会物理删除记录，必须走带状态的批量删除 pending，
    /// 避免复用普通软删除文案让用户误以为只是标记为已取消。
    fn prepare_todo_delete_operation(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        initiator_user_id: Option<String>,
        item: &TodoItem,
        source_condition: String,
    ) -> Result<(CommandBody, String), LlmError> {
        if matches!(item.status, TodoStatus::Completed | TodoStatus::Cancelled) {
            return self.prepare_todo_bulk_delete_from_items(
                session,
                owner,
                initiator_user_id,
                vec![item.clone()],
                source_condition,
                item.status.clone(),
            );
        }

        session.pending_operation = Some(PendingOperation::TodoDelete {
            initiator_user_id,
            owner_key: owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        });
        Ok((format_todo_delete_confirm(item), "todo_delete".to_owned()))
    }

    /// 从当前待办列表中精确匹配指定内部 ID 的待办项。
    /// 这里不接受用户原始输入，调用方必须先通过最近列表快照完成编号映射。
    fn match_pending_todo_id(
        &self,
        owner: &TodoOwner,
        id: &str,
    ) -> Result<Vec<TodoItem>, LlmError> {
        if id.trim().is_empty() {
            return Ok(Vec::new());
        }
        let items = self.todo_store.list_pending(owner).map_err(todo_error)?;
        Ok(items.into_iter().filter(|item| item.id == id).collect())
    }

    /// 删除命令已经通过最近列表快照拿到内部 ID，这里按 owner/scope 再查一次当前项。
    /// 与编辑/完成不同，删除允许从 `/todo all` 指向未完成、已完成或已取消项；
    /// 确认阶段会根据保存的内部 ID 和当前状态执行对应的安全删除语义。
    fn match_todo_id_for_delete(
        &self,
        owner: &TodoOwner,
        id: &str,
    ) -> Result<Vec<TodoItem>, LlmError> {
        Ok(self
            .todo_store
            .get_by_id(owner, id)
            .map_err(todo_error)?
            .into_iter()
            .collect())
    }

    /// 按最近一次 `/todo` 列表中的可见编号批量完成待办。
    fn complete_todo_list_numbers(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        argument: &str,
    ) -> Result<(CommandBody, String), LlmError> {
        let numbers = match parse_todo_number_list(argument) {
            Ok(numbers) => numbers,
            Err(message) => return Ok((CommandBody::plain(message), "todo_done".to_owned())),
        };
        let Some(query) = valid_last_visible_todo_query(session, &owner.key) else {
            return Ok((
                CommandBody::plain("请先发送 /todo 查看待办列表。"),
                "todo_done".to_owned(),
            ));
        };
        let resolved = resolve_todo_numbers_from_snapshot(&query, &numbers);
        let item_ids = resolved
            .matched
            .iter()
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        let outcome = self
            .todo_store
            .complete_by_ids(owner, &item_ids)
            .map_err(todo_error)?;
        let mut completed_by_id = outcome
            .completed
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let mut completed_items = Vec::new();
        let mut missing_numbers = resolved.missing;
        for (number, id) in resolved.matched {
            if let Some(item) = completed_by_id.remove(&id) {
                completed_items.push((number, item));
            } else {
                missing_numbers.push(number);
            }
        }
        if !completed_items.is_empty() {
            // 成功变更状态后不继续复用旧快照，避免用户后续编号指向已变化的列表。
            session.last_todo_query = None;
            let completed = completed_items
                .iter()
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            session.update_last_todo_action_from_items(&owner.key, "completed", &completed);
        }
        Ok((
            format_todo_numbered_item_operation_result(
                "已完成待办",
                &completed_items,
                "未找到匹配的未完成待办",
                &missing_numbers,
            ),
            "todo_done".to_owned(),
        ))
    }

    /// 按最近一次 `/todo done` 列表中的可见编号批量恢复已完成待办。
    fn restore_todo_list_numbers(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        argument: &str,
    ) -> Result<(CommandBody, String), LlmError> {
        let numbers = match parse_todo_number_list(argument) {
            Ok(numbers) => numbers,
            Err(message) => return Ok((CommandBody::plain(message), "todo_undo".to_owned())),
        };
        let Some(query) = valid_last_visible_todo_query(session, &owner.key) else {
            return Ok((
                CommandBody::plain("请先发送 /todo done 查看已完成待办。"),
                "todo_undo".to_owned(),
            ));
        };
        let resolved = resolve_todo_numbers_from_snapshot(&query, &numbers);
        let item_ids = resolved
            .matched
            .iter()
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        let outcome = self
            .todo_store
            .restore_completed_by_ids(owner, &item_ids)
            .map_err(todo_error)?;
        let mut restored_by_id = outcome
            .restored
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let mut restored_items = Vec::new();
        let mut missing_numbers = resolved.missing;
        for (number, id) in resolved.matched {
            if let Some(item) = restored_by_id.remove(&id) {
                restored_items.push((number, item));
            } else {
                missing_numbers.push(number);
            }
        }
        if !restored_items.is_empty() {
            // 恢复成功后清空 completed 快照，避免继续用旧完成列表编号操作。
            session.last_todo_query = None;
            let restored = restored_items
                .iter()
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            session.update_last_todo_action_from_items(&owner.key, "restored", &restored);
        }
        Ok((
            format_todo_numbered_item_operation_result(
                "已恢复待办",
                &restored_items,
                "未找到匹配的已完成待办",
                &missing_numbers,
            ),
            "todo_undo".to_owned(),
        ))
    }

    /// 根据 ID 列表准备批量删除已完成待办的待确认操作。
    fn prepare_todo_bulk_delete_from_ids(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        initiator_user_id: Option<String>,
        item_ids: Vec<String>,
        source_condition: String,
        status: TodoStatus,
    ) -> Result<(CommandBody, String), LlmError> {
        let items = self
            .todo_store
            .list_by_ids_with_status(owner, &item_ids, status.clone())
            .map_err(todo_error)?;
        self.prepare_todo_bulk_delete_from_items(
            session,
            owner,
            initiator_user_id,
            items,
            source_condition,
            status,
        )
    }

    /// 根据 TodoItem 列表准备批量删除的待确认操作。
    fn prepare_todo_bulk_delete_from_items(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        initiator_user_id: Option<String>,
        items: Vec<TodoItem>,
        source_condition: String,
        status: TodoStatus,
    ) -> Result<(CommandBody, String), LlmError> {
        if items.is_empty() {
            return Ok((
                CommandBody::plain(match status {
                    TodoStatus::Cancelled => "当前没有已取消待办需要删除。",
                    _ => "没有可删除的已完成待办。",
                }),
                "todo_delete".to_owned(),
            ));
        }
        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let summary = format_todo_bulk_delete_summary_for_status(&items, &status);
        session.pending_operation = Some(PendingOperation::TodoBulkDelete {
            initiator_user_id,
            owner_key: owner.key.clone(),
            item_ids,
            matched_count: items.len(),
            status: status.clone(),
            summary: summary.clone(),
            source_condition: source_condition.clone(),
            created_at: now_iso_cn(),
        });
        Ok((
            format_todo_bulk_delete_confirm_for_status(
                items.len(),
                status,
                &source_condition,
                &summary,
            ),
            "todo_delete".to_owned(),
        ))
    }

    /// 调用 LLM 解析用户输入的待办文本，返回结构化的待办草稿。
    async fn parse_todo_draft(
        &self,
        user_text: &str,
        existing: Option<&TodoItem>,
    ) -> Result<Result<TodoItemDraft, String>, LlmError> {
        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .respond(RespondRequest {
                model: self.todo_model.clone(),
                purpose: RespondPurpose::TodoParse,
                user_text: user_text.to_owned(),
                session: existing
                    .map(serde_json::to_value)
                    .transpose()
                    .unwrap_or(None)
                    .unwrap_or(Value::Null),
                metadata: HashMap::from([
                    ("purpose".to_owned(), "todo_parse".to_owned()),
                    (
                        "todo_operation".to_owned(),
                        if existing.is_some() { "edit" } else { "add" }.to_owned(),
                    ),
                ]),
                ..empty_respond_request()
            })
            .await?;

        let mut draft = match parse_todo_draft_json(&output.reply, user_text, existing) {
            Ok(draft) => draft,
            Err(message) => return Ok(Err(message)),
        };
        let time_ctx = request_time_context();
        enrich_draft_time_from_text(&mut draft, user_text, &time_ctx);
        Ok(Ok(draft))
    }

    /// 调用 LLM 解析编辑待办的增量补丁，保留现有字段不变。
    async fn parse_todo_edit_draft(
        &self,
        user_text: &str,
        existing: &TodoItem,
    ) -> Result<Result<TodoItemDraft, String>, LlmError> {
        match self.parse_todo_edit_patch(user_text, existing).await? {
            Ok(patch) if patch.has_changes() => {
                let base = TodoItemDraft::from_item(existing, user_text);
                Ok(Ok(apply_todo_edit_patch(base, patch, user_text)))
            }
            Ok(_) => self.parse_todo_draft(user_text, Some(existing)).await,
            Err(message) => Ok(Err(message)),
        }
    }

    /// 调用 LLM 解析用户编辑意图，生成字段级别的增量补丁。
    async fn parse_todo_edit_patch(
        &self,
        user_text: &str,
        existing: &TodoItem,
    ) -> Result<Result<TodoEditPatch, String>, LlmError> {
        let service = LlmChatService::new(self.provider.clone());
        let mut current = existing.clone();
        current.raw_text = None;
        let output = service
            .respond(RespondRequest {
                model: self.todo_model.clone(),
                purpose: RespondPurpose::TodoParse,
                user_text: user_text.to_owned(),
                session: serde_json::to_value(&current).unwrap_or(Value::Null),
                metadata: HashMap::from([
                    ("purpose".to_owned(), "todo_parse".to_owned()),
                    ("todo_operation".to_owned(), "edit_patch".to_owned()),
                ]),
                ..empty_respond_request()
            })
            .await?;

        let mut patch = match parse_todo_edit_patch_json(&output.reply) {
            Ok(patch) => patch,
            Err(message) => return Ok(Err(message)),
        };
        enrich_todo_edit_patch_time_from_text(&mut patch, user_text);
        Ok(Ok(patch))
    }

    async fn revise_todo_add_draft_with_llm(
        &self,
        current_draft: &TodoItemDraft,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Result<TodoItemDraft, String>, LlmError> {
        self.revise_todo_draft_with_llm("add_revise", None, current_draft, user_text, session)
            .await
    }

    async fn revise_todo_edit_draft_with_llm(
        &self,
        original: &TodoItem,
        current_draft: &TodoItemDraft,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Result<TodoItemDraft, String>, LlmError> {
        self.revise_todo_draft_with_llm(
            "edit_revise",
            Some(original),
            current_draft,
            user_text,
            session,
        )
        .await
    }

    async fn revise_todo_draft_with_llm(
        &self,
        operation: &str,
        original: Option<&TodoItem>,
        current_draft: &TodoItemDraft,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Result<TodoItemDraft, String>, LlmError> {
        let service = LlmChatService::new(self.provider.clone());
        let request_body = json!({
            "operation": operation,
            "original": original,
            "current_draft": current_draft,
            "user_input": user_text.trim(),
        });
        let output = service
            .respond(RespondRequest {
                session_id: session.session_id.clone(),
                model: self.todo_model.clone(),
                purpose: RespondPurpose::TodoParse,
                user_text: user_text.to_owned(),
                session: request_body,
                metadata: HashMap::from([
                    ("purpose".to_owned(), "todo_parse".to_owned()),
                    ("todo_operation".to_owned(), operation.to_owned()),
                ]),
                ..empty_respond_request()
            })
            .await?;

        let fallback_item = match original {
            Some(before) => todo_item_from_draft(before, current_draft),
            None => todo_item_from_revision_draft(current_draft),
        };
        let mut draft = match parse_todo_draft_json(&output.reply, user_text, Some(&fallback_item))
        {
            Ok(draft) => draft,
            Err(message) => return Ok(Err(message)),
        };
        let time_ctx = request_time_context();
        enrich_draft_time_from_text(&mut draft, user_text, &time_ctx);
        Ok(Ok(draft))
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

fn todo_delete_source_condition(target: &TodoTarget) -> String {
    match target {
        TodoTarget::ListIndex {
            source_condition, ..
        } => source_condition.clone(),
        _ => todo_target_label(target),
    }
}

fn todo_item_from_revision_draft(draft: &TodoItemDraft) -> TodoItem {
    TodoItem {
        id: "pending".to_owned(),
        user_id: None,
        scope_key: "pending".to_owned(),
        title: draft.title.clone(),
        detail: draft.detail.clone(),
        raw_text: draft.raw_text.clone(),
        due_date: draft.due_date.clone(),
        due_at: draft.due_at.clone(),
        time_precision: draft.time_precision.clone(),
        status: crate::runtime::todo::TodoStatus::Pending,
        created_at: now_iso_cn(),
        updated_at: now_iso_cn(),
        completed_at: None,
        cancelled_at: None,
    }
}

fn todo_item_from_draft(before: &TodoItem, draft: &TodoItemDraft) -> TodoItem {
    let mut item = before.clone();
    item.title = draft.title.clone();
    item.detail = draft.detail.clone();
    item.raw_text = draft.raw_text.clone();
    item.due_date = draft.due_date.clone();
    item.due_at = draft.due_at.clone();
    item.time_precision = draft.time_precision.clone();
    item
}
