//! 跨工具可复用的 PreparedAction 与 pending 回复分类基础设施。

mod model;
mod reply;

pub use model::*;
pub use reply::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prepared_action() -> PreparedAction {
        PreparedAction::new(
            PreparedActionMetadata {
                domain: "todo".to_owned(),
                action_kind: "todo_bulk_delete".to_owned(),
                initiator_user_id: Some("u1".to_owned()),
                owner_key: Some("owner:u1".to_owned()),
                scope_key: "group:g1:actor:u1".to_owned(),
                created_at: "2026-07-15T10:00:00+08:00".to_owned(),
                expires_at: "2026-07-15T10:10:00+08:00".to_owned(),
            },
            json!({"summary": "删除 2 条"}),
            json!({"kind": "todo_bulk_delete", "item_ids": ["a", "b"]}),
        )
    }

    #[test]
    fn prepared_action_round_trips_with_lifecycle_metadata() {
        let original = prepared_action();
        let value = serde_json::to_value(&original).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["state"], "waiting_confirmation");
        assert_eq!(value["revision"], 1);
        assert_eq!(value["expires_at"], "2026-07-15T10:10:00+08:00");
        assert_eq!(value["scope_key"], "group:g1:actor:u1");

        let restored: PreparedAction = serde_json::from_value(value).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn legacy_pending_without_new_fields_deserializes() {
        let pending: PreparedAction = serde_json::from_value(json!({
            "kind": "todo_add",
            "owner_key": "u1",
            "draft": {"title": "旧事项"},
            "created_at": "2026-06-27T12:00:00+08:00"
        }))
        .unwrap();

        assert!(pending.is_legacy());
        assert_eq!(pending.domain(), "todo");
        assert_eq!(pending.kind(), "todo_add");
        assert_eq!(pending.owner_key(), Some("u1"));
        assert_eq!(pending.initiator_user_id(), None);
        assert_eq!(pending.revision(), 1);
    }

    #[test]
    fn execution_rejects_expired_cross_user_scope_owner_and_revision() {
        let base = PreparedActionExecutionContext {
            initiator_user_id: Some("u1"),
            owner_key: Some("owner:u1"),
            scope_key: "group:g1:actor:u1",
            expected_revision: 1,
            now: "2026-07-15T10:05:00+08:00",
        };
        let action = prepared_action();
        assert_eq!(action.validate_for_execution(&base), Ok(()));

        let mut context = base;
        context.initiator_user_id = Some("u2");
        assert_eq!(
            action.validate_for_execution(&context),
            Err(PreparedActionValidationError::InitiatorMismatch)
        );
        context = base;
        context.owner_key = Some("owner:u2");
        assert_eq!(
            action.validate_for_execution(&context),
            Err(PreparedActionValidationError::OwnerMismatch)
        );
        context = base;
        context.scope_key = "group:g2:actor:u1";
        assert_eq!(
            action.validate_for_execution(&context),
            Err(PreparedActionValidationError::ScopeMismatch)
        );
        context = base;
        context.expected_revision = 0;
        assert_eq!(
            action.validate_for_execution(&context),
            Err(PreparedActionValidationError::RevisionMismatch)
        );
        context = base;
        context.now = "2026-07-15T10:10:00+08:00";
        assert_eq!(
            action.validate_for_execution(&context),
            Err(PreparedActionValidationError::Expired)
        );
    }

    #[test]
    fn revised_action_invalidates_old_revision_and_supports_failed_state() {
        let mut action = prepared_action();
        let revision = action
            .revise(
                json!({"kind": "todo_bulk_delete", "item_ids": ["a"]}),
                json!({"summary": "删除 1 条"}),
                "2026-07-15T10:12:00+08:00",
            )
            .unwrap();
        assert_eq!(revision, 2);

        let old_context = PreparedActionExecutionContext {
            initiator_user_id: Some("u1"),
            owner_key: Some("owner:u1"),
            scope_key: "group:g1:actor:u1",
            expected_revision: 1,
            now: "2026-07-15T10:05:00+08:00",
        };
        assert_eq!(
            action.begin_execution(&old_context),
            Err(PreparedActionValidationError::RevisionMismatch)
        );

        let current_context = PreparedActionExecutionContext {
            expected_revision: 2,
            ..old_context
        };
        action.begin_execution(&current_context).unwrap();
        action.mark_failed(2).unwrap();
        assert_eq!(action.state(), PreparedActionState::Failed);
        assert_eq!(
            action.begin_execution(&current_context),
            Err(PreparedActionValidationError::InvalidState)
        );
    }

    #[test]
    fn expiry_helper_uses_explicit_timestamp() {
        assert_eq!(
            expires_at_after("2026-07-15T10:00:00+08:00", 600).as_deref(),
            Some("2026-07-15T10:10:00+08:00")
        );
        assert!(expires_at_after("invalid", 600).is_none());
        assert!(!qq_maid_common::time_context::now_iso_cn().is_empty());
    }
}
