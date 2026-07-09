//! 运行时快照新鲜度 helper。
//!
//! 这里只处理通用的 RFC3339 + TTL 时间判断；具体业务域的可见性、owner
//! 校验和过期清理规则放在对应 `runtime::tools::<domain>` 模块内。

use chrono::{DateTime, Duration};
use qq_maid_common::time_context;

/// 判断一条快照记录是否仍在有效期内（created_at 为 RFC3339，TTL 单位为秒）。
pub fn query_is_fresh(created_at: &str, ttl_seconds: i64) -> bool {
    let Ok(created_at) = DateTime::parse_from_rfc3339(created_at.trim()) else {
        return false;
    };
    let Ok(now) = DateTime::parse_from_rfc3339(&time_context::now_iso_cn()) else {
        return false;
    };
    let age = now.signed_duration_since(created_at.with_timezone(&time_context::shanghai_offset()));
    age >= Duration::zero() && age.num_seconds() <= ttl_seconds
}
