//! 群成员角色判断 helper。
//!
//! 运行时业务只依赖平台归一化后的角色字符串；QQ / OneBot 等平台字段解析
//! 仍由上层接入侧负责，这里只维护 owner/admin 这类业务权限判断。

/// 判断群成员角色是否具备群管理权限。
pub fn is_group_owner_or_admin(role: &str) -> bool {
    matches!(role.trim().to_ascii_lowercase().as_str(), "owner" | "admin")
}

/// 基于通用请求字段判断当前操作是否允许执行群管理类写操作。
pub fn group_management_allowed(
    group_id: Option<&str>,
    scope_key: &str,
    group_member_role: Option<&str>,
) -> bool {
    !is_group_request(group_id, scope_key) || group_member_role.is_some_and(is_group_owner_or_admin)
}

fn is_group_request(group_id: Option<&str>, scope_key: &str) -> bool {
    group_id.is_some_and(|value| !value.trim().is_empty()) || scope_key.starts_with("group:")
}
