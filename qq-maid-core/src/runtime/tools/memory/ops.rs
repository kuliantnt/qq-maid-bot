//! Memory 领域操作与授权不变量。

use super::storage::{
    CreateScopedMemoryRequest, MemoryError, MemoryKind, MemoryQuery, MemoryRecord, MemoryScopeType,
    MemoryStore, MemoryTarget, MemoryVisibility, PersistMemoryRequest, ReplaceScopedStorageRequest,
    ScopedMemoryQuery, UpdateMemoryRequest,
};

use super::types::{
    MemoryActor, MemoryMutationResult, MemoryWriteResult, ProfilePreferenceResult,
    ReplaceScopedMemoryRequest, SaveMemoryRequest,
};

/// Memory 领域门面。Respond、后续 Tool 与 WebUI 应统一通过这里执行管理操作。
#[derive(Debug, Clone)]
pub struct MemoryOperations {
    store: MemoryStore,
}

impl MemoryOperations {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    /// 供需要在高成本草稿生成前做授权门禁的编排层调用；权限规则仍只维护在领域层。
    pub(crate) fn ensure_scoped_write_allowed(
        &self,
        actor: &MemoryActor,
        scope_type: MemoryScopeType,
        scope_id: &str,
    ) -> Result<(), MemoryError> {
        authorize_legacy_scope(scope_type, scope_id, actor, true)
    }

    /// 兼容当前 respond flow 的创建入口，同时补齐严格 personal/group 授权。
    pub fn create_scoped(
        &self,
        actor: &MemoryActor,
        mut req: CreateScopedMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        authorize_legacy_scope(req.scope_type, &req.scope_id, actor, true)?;
        req.created_by_user_id = actor.personal_scope_id.clone();
        self.store.create_scoped(req)
    }

    pub fn list_scoped(
        &self,
        actor: &MemoryActor,
        query: ScopedMemoryQuery,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        authorize_legacy_scope(query.scope_type, &query.scope_id, actor, false)?;
        self.store.list_scoped(query)
    }

    pub fn get_scoped(
        &self,
        actor: &MemoryActor,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id: &str,
    ) -> Result<MemoryRecord, MemoryError> {
        authorize_legacy_scope(scope_type, scope_id, actor, false)?;
        self.store.get_scoped(scope_type, scope_id, id)
    }

    pub fn update_scoped(
        &self,
        actor: &MemoryActor,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id: &str,
        req: UpdateMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        authorize_legacy_scope(scope_type, scope_id, actor, true)?;
        self.store.update_scoped(scope_type, scope_id, id, req)
    }

    pub fn replace_scoped(
        &self,
        req: ReplaceScopedMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        authorize_legacy_scope(req.scope_type, &req.scope_id, &req.actor, true)?;
        self.store.replace_scoped(ReplaceScopedStorageRequest {
            scope_type: req.scope_type,
            scope_id: req.scope_id,
            id_or_prefix: req.id_or_prefix,
            created_by_user_id: req.actor.personal_scope_id,
            user_id: req.user_id,
            group_id: req.group_id,
            content: req.content,
            source_text: req.source_text,
            memory_type: req.memory_type,
            scope: req.scope,
        })
    }

    pub fn delete_scoped(
        &self,
        actor: &MemoryActor,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id: &str,
    ) -> Result<String, MemoryError> {
        authorize_legacy_scope(scope_type, scope_id, actor, true)?;
        self.store.delete_scoped(scope_type, scope_id, id)
    }

    pub fn list(
        &self,
        actor: &MemoryActor,
        query: MemoryQuery,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        authorize_target(&query.target, actor, false)?;
        self.store.list_v3(query)
    }

    /// 兼容当前聊天读取；storage 会固定只读 active personal/group，不含群画像。
    pub(crate) fn list_accessible_for_context(
        &self,
        personal_scope_id: Option<&str>,
        group_scope_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        self.store
            .list_accessible_for_context(personal_scope_id, group_scope_id, limit)
    }

    /// 同一 target、关系主体和 attribute_key 内才会归档冲突记录。
    pub fn save(&self, req: SaveMemoryRequest) -> Result<MemoryWriteResult, MemoryError> {
        authorize_target(&req.target, &req.actor, true)?;
        validate_visibility(&req.target, req.visibility)?;
        let persisted = self.store.persist_v3(persist_request(&req));
        persisted.map(|result| MemoryWriteResult {
            memory: result.record,
            archived_ids: result.archived_ids,
        })
    }

    pub fn replace(
        &self,
        id: &str,
        req: SaveMemoryRequest,
    ) -> Result<MemoryWriteResult, MemoryError> {
        authorize_target(&req.target, &req.actor, true)?;
        validate_visibility(&req.target, req.visibility)?;
        self.store
            .replace_v3(&req.target, id, persist_request(&req))
            .map(|result| MemoryWriteResult {
                memory: result.record,
                archived_ids: result.archived_ids,
            })
    }

    pub fn archive(
        &self,
        actor: &MemoryActor,
        target: &MemoryTarget,
        id: &str,
    ) -> Result<MemoryMutationResult, MemoryError> {
        authorize_target(target, actor, true)?;
        self.store
            .archive_v3(target, id)
            .map(|id| MemoryMutationResult::from_ids(vec![id]))
    }

    pub fn delete(
        &self,
        actor: &MemoryActor,
        target: &MemoryTarget,
        id: &str,
    ) -> Result<MemoryMutationResult, MemoryError> {
        authorize_target(target, actor, true)?;
        self.store
            .delete_v3(target, id)
            .map(|id| MemoryMutationResult::from_ids(vec![id]))
    }

    pub fn clear(
        &self,
        actor: &MemoryActor,
        target: &MemoryTarget,
    ) -> Result<MemoryMutationResult, MemoryError> {
        authorize_target(target, actor, true)?;
        self.store
            .clear_v3(target)
            .map(MemoryMutationResult::from_ids)
    }

    /// 用户只能为自己在当前群的画像设置 opt-in/opt-out；管理员身份不扩大权限。
    pub fn set_group_profile_enabled(
        &self,
        actor: &MemoryActor,
        target: &MemoryTarget,
        enabled: bool,
    ) -> Result<ProfilePreferenceResult, MemoryError> {
        authorize_target(target, actor, true)?;
        if target.memory_kind() != MemoryKind::GroupProfile {
            return Err(MemoryError::bad_request(
                "profile preference requires a group profile target",
            ));
        }
        self.store
            .set_group_profile_enabled(target, enabled)
            .map(|archived_ids| ProfilePreferenceResult {
                enabled,
                archived_ids,
            })
    }
}

fn authorize_legacy_scope(
    scope_type: MemoryScopeType,
    scope_id: &str,
    actor: &MemoryActor,
    write: bool,
) -> Result<(), MemoryError> {
    let allowed = match scope_type {
        MemoryScopeType::Personal => actor.personal_scope_id == scope_id,
        MemoryScopeType::Group => {
            actor.group_scope_id.as_deref() == Some(scope_id)
                && (!write || actor.can_manage_group_memory)
        }
        MemoryScopeType::LegacyUnassigned => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(MemoryError::forbidden(
            "memory is not manageable in current scope",
        ))
    }
}

fn authorize_target(
    target: &MemoryTarget,
    actor: &MemoryActor,
    write: bool,
) -> Result<(), MemoryError> {
    let in_personal_scope = target.scope_id() == actor.personal_scope_id;
    let in_current_group = actor.group_scope_id.as_deref() == Some(target.scope_id());
    let allowed = match target.memory_kind() {
        MemoryKind::Personal => {
            target.scope_type() == MemoryScopeType::Personal
                && in_personal_scope
                && target.subject_id().is_none()
        }
        MemoryKind::GroupProfile => {
            target.scope_type() == MemoryScopeType::Group
                && in_current_group
                && target.subject_id() == Some(actor.personal_scope_id.as_str())
        }
        MemoryKind::Group => {
            target.scope_type() == MemoryScopeType::Group
                && in_current_group
                && (!write || actor.can_manage_group_memory)
        }
        MemoryKind::LegacyUnassigned => false,
    };
    if allowed {
        Ok(())
    } else {
        // 权限错误不查询目标记录，避免泄露 ID 是否存在。
        Err(MemoryError::forbidden(
            "memory is not manageable in current scope",
        ))
    }
}

fn validate_visibility(
    target: &MemoryTarget,
    visibility: MemoryVisibility,
) -> Result<(), MemoryError> {
    let valid = match target.memory_kind() {
        MemoryKind::Personal => matches!(
            visibility,
            MemoryVisibility::Private | MemoryVisibility::ContextOnly | MemoryVisibility::Public
        ),
        MemoryKind::GroupProfile | MemoryKind::Group => matches!(
            visibility,
            MemoryVisibility::GroupMembers | MemoryVisibility::Public
        ),
        MemoryKind::LegacyUnassigned => false,
    };
    if valid {
        Ok(())
    } else {
        Err(MemoryError::bad_request(
            "visibility is not valid for memory target",
        ))
    }
}

fn persist_request(req: &SaveMemoryRequest) -> PersistMemoryRequest {
    PersistMemoryRequest {
        target: req.target.clone(),
        created_by_user_id: req.actor.personal_scope_id.clone(),
        content: req.content.clone(),
        source_text: req.source_text.clone(),
        category: req.category,
        legacy_scope: req.legacy_scope.clone(),
        visibility: req.visibility,
        source_type: req.source_type,
        source_ref: req.source_ref.clone(),
        confirmed_at: req.confirmed_at.clone(),
        pinned: req.pinned,
        attribute_key: req.attribute_key.clone(),
        relation_subject_id: req.relation_subject_id.clone(),
        relation_object_id: req.relation_object_id.clone(),
    }
}
