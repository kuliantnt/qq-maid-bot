//! 长期记忆权限校验 helper。
//!
//! 群记忆编辑/删除/替换只允许群主或管理员操作。权限失败统一返回 forbidden，
//! 且不在错误信息里暴露记录归属，避免越权探测。

use super::{MemoryActor, MemoryError, MemoryRecord, MemoryScopeType};

/// 校验当前操作者是否可修改/删除/替换目标记忆。
///
/// 群记忆管理依赖上游请求带来的群成员角色；storage 不解析平台字段，只使用
/// `MemoryActor::can_manage_group_memory` 这个已归一化的业务权限。
pub(super) fn ensure_can_modify(
    record: &MemoryRecord,
    actor: &MemoryActor,
) -> Result<(), MemoryError> {
    if record.scope_type == MemoryScopeType::Group.as_str() && !actor.can_manage_group_memory {
        return Err(MemoryError::forbidden(
            "memory is not editable in this scope",
        ));
    }
    Ok(())
}
