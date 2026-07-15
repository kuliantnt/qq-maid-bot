//! Memory 写入的确定性用户回执。

use super::MemoryKind;

pub(crate) fn memory_kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Personal => "个人记忆",
        MemoryKind::GroupProfile => "当前群画像",
        MemoryKind::Group => "当前群公共记忆",
        MemoryKind::LegacyUnassigned => "未归属旧记忆",
    }
}

pub(crate) fn format_memory_saved_reply(kind: MemoryKind, content: &str) -> String {
    format!(
        "🧠 已记住\n范围：{}\n内容：{}",
        memory_kind_label(kind),
        content.trim()
    )
}

pub(crate) fn memory_write_error_reply(code: &str) -> &'static str {
    match code {
        "memory_sensitive_rejected" => {
            "这段内容包含或疑似包含密码、Token、密钥、身份证件等敏感信息，未保存。"
        }
        "memory_actor_missing" => "当前请求缺少稳定用户身份，不能保存长期记忆。",
        "group_admin_required" | "forbidden" => {
            "当前群公共记忆只能由群主或管理员保存，本次未写入。"
        }
        "profile_opted_out" => {
            "你已停止当前群保存群内画像，本次未写入。可使用 `/memory profile enable` 重新授权。"
        }
        "memory_scope_unsupported" => "当前会话范围不支持保存这类长期记忆。",
        "memory_pending_conflict" => "当前还有一项待确认操作，请先处理后再保存记忆。",
        _ => "长期记忆写入失败，没有保存。请稍后重试。",
    }
}
