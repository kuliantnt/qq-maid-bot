//! `/todo group` 群管理命令编排。

use crate::{
    error::LlmError,
    identity::group_raw_target_from_scope_key,
    runtime::{
        group_role::is_group_owner_or_admin,
        respond::{
            RespondRequest, RespondResponse, RustRespondService,
            common::{CommandBody, GROUP_ADMIN_REQUIRED_REPLY, todo_error, truncate_chars},
        },
        session::{SessionMeta, SessionRecord, SessionTurnActor},
        tools::todo::group_admin::{
            GroupTodoAdminContext, GroupTodoSelectionError, format_group_todo_deleted_reply,
            format_group_todo_list_reply, format_group_todo_usage_reply, remember_group_todo_query,
            resolve_group_todo_number, visible_group_todo_items,
        },
    },
};

enum GroupTodoCommand {
    List,
    Delete(usize),
    Invalid,
}

impl RustRespondService {
    pub(super) fn handle_group_todo_command(
        &self,
        req: &RespondRequest,
        meta: &SessionMeta,
        interaction_session: &mut SessionRecord,
        conversation_session: &SessionRecord,
        user_text: &str,
        argument: &str,
    ) -> Result<RespondResponse, LlmError> {
        let Some(scope_key) = authoritative_group_scope(req, meta) else {
            return self.append_pending_response(
                interaction_session,
                user_text,
                CommandBody::plain("群 Todo 管理只支持当前群聊。"),
                "todo_group_scope_required",
            );
        };
        if !req
            .group_member_role
            .as_deref()
            .is_some_and(is_group_owner_or_admin)
        {
            return self.append_pending_response(
                interaction_session,
                user_text,
                CommandBody::plain(GROUP_ADMIN_REQUIRED_REPLY),
                "group_admin_required",
            );
        }
        let Some(actor_id) = meta.user_id.as_deref().filter(|id| !id.trim().is_empty()) else {
            return self.append_pending_response(
                interaction_session,
                user_text,
                CommandBody::plain("缺少稳定的操作者身份，无法使用群 Todo 管理。"),
                "todo_group_actor_required",
            );
        };
        let Some(context) = GroupTodoAdminContext::new(
            actor_id,
            &meta.platform,
            meta.account_id.as_deref(),
            scope_key,
        ) else {
            return self.append_pending_response(
                interaction_session,
                user_text,
                CommandBody::plain("当前群作用域无效，无法使用群 Todo 管理。"),
                "todo_group_scope_required",
            );
        };

        match parse_group_todo_command(argument) {
            GroupTodoCommand::List => self.list_group_todos(
                req,
                meta,
                interaction_session,
                conversation_session,
                user_text,
                &context,
            ),
            GroupTodoCommand::Delete(number) => {
                self.delete_group_todo(interaction_session, user_text, scope_key, &context, number)
            }
            GroupTodoCommand::Invalid => self.append_pending_response(
                interaction_session,
                user_text,
                format_group_todo_usage_reply(),
                "todo_group_usage",
            ),
        }
    }

    fn list_group_todos(
        &self,
        req: &RespondRequest,
        meta: &SessionMeta,
        interaction_session: &mut SessionRecord,
        conversation_session: &SessionRecord,
        user_text: &str,
        context: &GroupTodoAdminContext,
    ) -> Result<RespondResponse, LlmError> {
        let items = self
            .task_store
            .list_pending_for_group_scope(&meta.scope_key)
            .map_err(todo_error)?;
        let visible = visible_group_todo_items(&items);
        let creator_names = visible
            .iter()
            .map(|item| self.group_todo_creator_name(req, meta, conversation_session, item))
            .collect::<Vec<_>>();
        remember_group_todo_query(interaction_session, context, visible);
        self.append_pending_response(
            interaction_session,
            user_text,
            format_group_todo_list_reply(visible, &creator_names, items.len()),
            "todo_group_list",
        )
    }

    fn delete_group_todo(
        &self,
        interaction_session: &mut SessionRecord,
        user_text: &str,
        scope_key: &str,
        context: &GroupTodoAdminContext,
        number: usize,
    ) -> Result<RespondResponse, LlmError> {
        let item_id = match resolve_group_todo_number(interaction_session, context, number) {
            Ok(item_id) => item_id,
            Err(error) => {
                let text = match error {
                    GroupTodoSelectionError::InvalidNumber => {
                        "编号无效，请先重新发送 /todo group 查看当前列表。"
                    }
                    GroupTodoSelectionError::SnapshotUnavailable => {
                        "群 Todo 列表已过期或不属于当前群，请重新发送 /todo group。"
                    }
                };
                return self.append_pending_response(
                    interaction_session,
                    user_text,
                    CommandBody::plain(text),
                    "todo_group_snapshot_required",
                );
            }
        };
        let Some(item) = self
            .task_store
            .delete_pending_for_group_scope_and_cancel_notification(scope_key, &item_id)
            .map_err(todo_error)?
        else {
            interaction_session.last_todo_query = None;
            return self.append_pending_response(
                interaction_session,
                user_text,
                CommandBody::plain(
                    "目标 Todo 已变化或不属于当前群，没有执行删除。请重新查看列表。",
                ),
                "todo_group_delete_stale",
            );
        };
        interaction_session.last_todo_query = None;
        self.append_pending_response(
            interaction_session,
            user_text,
            format_group_todo_deleted_reply(&item),
            "todo_group_delete",
        )
    }

    fn group_todo_creator_name(
        &self,
        req: &RespondRequest,
        meta: &SessionMeta,
        conversation_session: &SessionRecord,
        item: &crate::runtime::tools::todo::TodoItem,
    ) -> String {
        let Some(user_id) = item.user_id.as_deref().filter(|id| !id.trim().is_empty()) else {
            return "群成员".to_owned();
        };
        if let Ok(Some(name)) = self.display_name_store().get(&meta.scope_key, user_id)
            && let Some(name) = clean_creator_name(&name)
        {
            return name;
        }
        if req.user_id.as_deref() == Some(user_id)
            && let Some(name) = req
                .message_context
                .as_ref()
                .and_then(|context| context.actor.as_ref())
                .and_then(|actor| actor.display_name.as_deref())
                .and_then(clean_creator_name)
        {
            return name;
        }
        creator_name_from_history(conversation_session, &meta.scope_key, user_id)
            .unwrap_or_else(|| "群成员".to_owned())
    }
}

fn authoritative_group_scope<'a>(req: &RespondRequest, meta: &'a SessionMeta) -> Option<&'a str> {
    let group_id = req.group_id.as_deref()?.trim();
    let scope_group_id = group_raw_target_from_scope_key(&meta.scope_key)?;
    (!group_id.is_empty() && scope_group_id == group_id).then_some(meta.scope_key.as_str())
}

fn parse_group_todo_command(argument: &str) -> GroupTodoCommand {
    let mut parts = argument.split_whitespace();
    let Some(action) = parts.next() else {
        return GroupTodoCommand::List;
    };
    if !matches!(
        action.to_ascii_lowercase().as_str(),
        "delete" | "del" | "删除"
    ) {
        return GroupTodoCommand::Invalid;
    }
    let Some(number) = parts.next().and_then(|value| value.parse::<usize>().ok()) else {
        return GroupTodoCommand::Invalid;
    };
    if number == 0 || parts.next().is_some() {
        return GroupTodoCommand::Invalid;
    }
    GroupTodoCommand::Delete(number)
}

fn creator_name_from_history(
    session: &SessionRecord,
    scope_key: &str,
    user_id: &str,
) -> Option<String> {
    let actor_ref = SessionTurnActor::actor_ref_for_user(scope_key, user_id)?;
    session.history.iter().rev().find_map(|message| {
        let actor = message.turn_actor.as_ref()?;
        (actor.actor_ref.as_deref() == Some(actor_ref.as_str()))
            .then_some(actor.display_name.as_deref())
            .flatten()
            .and_then(clean_creator_name)
    })
}

fn clean_creator_name(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then(|| truncate_chars(&normalized, 32))
}
