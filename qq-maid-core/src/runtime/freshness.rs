//! 运行时快照新鲜度 helper。
//!
//! 这里只处理通用的 RFC3339 + TTL 时间判断；具体业务域的可见性、owner
//! 校验和过期清理规则放在对应 `runtime::tools::<domain>` 模块内。

use chrono::{DateTime, Duration};
use qq_maid_common::time_context;

/// 持久化快照使用墙上时钟；允许系统校时造成的小幅回拨，但不接受明显的未来时间。
const MAX_SNAPSHOT_CLOCK_SKEW_SECONDS: i64 = 5;

/// 判断一条快照记录是否仍在有效期内（created_at 为 RFC3339，TTL 单位为秒）。
pub fn query_is_fresh(created_at: &str, ttl_seconds: i64) -> bool {
    query_is_fresh_at(created_at, ttl_seconds, &time_context::now_iso_cn())
}

fn query_is_fresh_at(created_at: &str, ttl_seconds: i64, now: &str) -> bool {
    if ttl_seconds < 0 {
        return false;
    }
    let Ok(created_at) = DateTime::parse_from_rfc3339(created_at.trim()) else {
        return false;
    };
    let Ok(now) = DateTime::parse_from_rfc3339(now.trim()) else {
        return false;
    };
    let age = now.signed_duration_since(created_at.with_timezone(&time_context::shanghai_offset()));
    age >= Duration::seconds(-MAX_SNAPSHOT_CLOCK_SKEW_SECONDS)
        && age <= Duration::seconds(ttl_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_snapshot_tolerates_small_wall_clock_rollback() {
        assert!(query_is_fresh_at(
            "2026-07-19T14:00:02+08:00",
            600,
            "2026-07-19T14:00:00+08:00",
        ));
    }

    #[test]
    fn fresh_snapshot_rejects_future_time_beyond_clock_skew_budget() {
        assert!(!query_is_fresh_at(
            "2026-07-19T14:00:06+08:00",
            600,
            "2026-07-19T14:00:00+08:00",
        ));
    }

    #[test]
    fn fresh_snapshot_still_enforces_ttl_boundary() {
        assert!(query_is_fresh_at(
            "2026-07-19T14:00:00+08:00",
            600,
            "2026-07-19T14:10:00+08:00",
        ));
        assert!(!query_is_fresh_at(
            "2026-07-19T14:00:00+08:00",
            600,
            "2026-07-19T14:10:01+08:00",
        ));
    }

    #[test]
    fn fresh_snapshot_rejects_negative_ttl_even_during_clock_rollback() {
        assert!(!query_is_fresh_at(
            "2026-07-19T14:00:02+08:00",
            -1,
            "2026-07-19T14:00:00+08:00",
        ));
    }
}
