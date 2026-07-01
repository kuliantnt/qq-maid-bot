//! 长期记忆权限校验 helper。
//!
//! 群记忆编辑/删除只允许创建者操作；历史群记忆缺 `created_by_user_id` 时
//! 可读可注入但不可被普通成员管理。权限失败统一返回 forbidden，且不在错误
//! 信息里暴露记录归属，避免越权探测。

use super::{MemoryActor, MemoryError, MemoryRecord, MemoryScopeType};

/// 校验当前操作者是否可修改/删除目标记忆。
///
/// 群记忆第一版没有管理员识别能力，只允许创建者修改/删除；
/// `created_by_user_id` 为空的历史群记忆可读可注入，但不可被普通成员管理。
pub(super) fn ensure_can_modify(
    record: &MemoryRecord,
    actor: &MemoryActor,
) -> Result<(), MemoryError> {
    if record.scope_type == MemoryScopeType::Group.as_str()
        && record.created_by_user_id.as_deref() != Some(actor.user_id.as_str())
    {
        return Err(MemoryError::forbidden(
            "memory is not editable in this scope",
        ));
    }
    Ok(())
}
