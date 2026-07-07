//! QQ 群成员详情查询（#229）与 Phase 3 成员详情补全缓存（#319）。
//!
//! `GET /v2/groups/{group_openid}/members/{member_openid}`，用于补全群昵称 /
//! 群角色 / 是否机器人 / `union_openid` 等展示字段。
//!
//! `get_group_member` 只负责拉取与结构化解析（#229）；`get_group_member_cached`
//! 在其上加 TTL 缓存 + 负缓存（#319），避免高频群聊每条消息强制请求。拉取失败
//! 返回 `Unavailable`，由上层降级为 `source=Event`，不阻断聊天。

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tracing::{info, warn};

use crate::logging::{mask_openid, reqwest_error_summary};

use super::{ApiError, QqApiClient};

/// QQ 群成员详情（`GET /v2/groups/{group_openid}/members/{member_openid}`）。
///
/// 字段以 #229 接口示例为准；所有字段可选，QQ 可能按权限 / 场景省略部分字段。
/// `member_role` 保留原始字符串（如 `owner` / `admin` / `member`），由上层映射为
/// `GroupMemberRole`，避免 api 层反向依赖 gateway 事件枚举。
/// `username` 是群昵称，只用于展示和 LLM 理解，不是稳定身份。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GroupMemberDetail {
    #[serde(default)]
    pub member_openid: Option<String>,
    /// 群昵称，仅展示用，不是稳定身份。
    #[serde(default)]
    pub username: Option<String>,
    /// 原始角色字符串（`owner` / `admin` / `member` 等），由上层映射。
    #[serde(default)]
    pub member_role: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
    /// ISO 8601 入群时间，原样保留，不在此层解析。
    #[serde(default)]
    pub joined_at: Option<String>,
    #[serde(default)]
    pub union_openid: Option<String>,
}

impl QqApiClient {
    /// 查询单个群成员详情（`GET /v2/groups/{group_openid}/members/{member_openid}`）。
    ///
    /// 用于补全群昵称 / 群角色 / 是否机器人 / `union_openid` 等展示字段。本方法只负责
    /// 拉取与结构化解析，不缓存、不阻断聊天；缓存与降级策略由上层（#319 Phase 3）负责。
    ///
    /// 调用失败返回 `ApiError`，由上层决定是否降级为已有结构化 ID（`source=Event`）。
    pub async fn get_group_member(
        &self,
        group_openid: &str,
        member_openid: &str,
    ) -> Result<GroupMemberDetail, ApiError> {
        let url = format!(
            "{}/v2/groups/{group_openid}/members/{member_openid}",
            self.api_base
        );
        let masked_group = mask_openid(group_openid);
        let masked_member = mask_openid(member_openid);
        let response = self
            .client
            .get(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    group = %masked_group,
                    member = %masked_member,
                    error = %reqwest_error_summary(&error),
                    "QQ group member detail request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            warn!(
                group = %masked_group,
                member = %masked_member,
                status = %status,
                "QQ group member detail returned non-success status"
            );
            return Err(ApiError::Status { status, body });
        }

        let detail = response
            .json::<GroupMemberDetail>()
            .await
            .map_err(|error| {
                warn!(
                    group = %masked_group,
                    member = %masked_member,
                    error = %reqwest_error_summary(&error),
                    "QQ group member detail response decode failed"
                );
                ApiError::Http(error)
            })?;
        info!(
            group = %masked_group,
            member = %masked_member,
            has_username = detail.username.is_some(),
            role = detail.member_role.as_deref().unwrap_or(""),
            is_bot = detail.bot.unwrap_or(false),
            "qq group member detail fetched"
        );
        Ok(detail)
    }
}

/// 成员详情缓存命中结果，区分来源以满足 #319 `source` 标注要求。
///
/// - `Fresh`：实时拉取成功，上层应标 `source=MemberApi`。
/// - `Cached`：TTL 缓存命中，上层应标 `source=Cache`。
/// - `Unavailable`：拉取失败或负缓存命中，上层保持 `source=Event` 不伪造。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberFetchResult {
    Fresh(GroupMemberDetail),
    Cached(GroupMemberDetail),
    Unavailable,
}

/// 群成员详情 TTL 缓存条目。
#[derive(Clone)]
struct CachedEntry {
    /// `Some` = 正缓存；`None` = 负缓存（拉取失败短期不重试）。
    detail: Option<GroupMemberDetail>,
    fetched_at: Instant,
}

/// 群成员详情缓存（#319）。
///
/// 正缓存 TTL 默认 10 分钟，负缓存 TTL 默认 30 秒（避免失败风暴）。
/// 仅作 best-effort 缓存，不保证强一致；成员信息变更后最迟在正缓存 TTL 后刷新。
#[derive(Clone)]
pub(crate) struct MemberDetailCache {
    positive_ttl: Duration,
    negative_ttl: Duration,
    inner: std::sync::Arc<Mutex<HashMap<(String, String), CachedEntry>>>,
}

impl std::fmt::Debug for MemberDetailCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberDetailCache")
            .field("positive_ttl", &self.positive_ttl)
            .field("negative_ttl", &self.negative_ttl)
            .finish()
    }
}

impl MemberDetailCache {
    pub(crate) fn new(positive_ttl: Duration, negative_ttl: Duration) -> Self {
        Self {
            positive_ttl,
            negative_ttl,
            inner: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 默认 TTL 缓存（正 10min / 负 30s）。
    pub(crate) fn default_ttl() -> Self {
        Self::new(Duration::from_secs(600), Duration::from_secs(30))
    }

    /// 查询未过期的缓存条目；返回 `Some(Option<detail>)`（外层 Some = 命中，内层 None = 负缓存）。
    fn get(&self, group_openid: &str, member_openid: &str) -> Option<Option<GroupMemberDetail>> {
        let key = (group_openid.to_owned(), member_openid.to_owned());
        let mut guard = self.inner.lock().ok()?;
        let (fresh, detail) = guard.get(&key).map(|entry| {
            let ttl = if entry.detail.is_some() {
                self.positive_ttl
            } else {
                self.negative_ttl
            };
            (entry.fetched_at.elapsed() < ttl, entry.detail.clone())
        })?;
        if fresh {
            Some(detail)
        } else {
            // 过期条目顺手清理，避免长时间运行后缓存仅增不减。
            guard.remove(&key);
            None
        }
    }

    fn insert(&self, group_openid: &str, member_openid: &str, detail: Option<GroupMemberDetail>) {
        let key = (group_openid.to_owned(), member_openid.to_owned());
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(
                key,
                CachedEntry {
                    detail,
                    fetched_at: Instant::now(),
                },
            );
        }
    }
}

impl QqApiClient {
    /// 带缓存的群成员详情查询（#319）。
    ///
    /// 先查 TTL 缓存（正缓存命中返回 `Cached`，负缓存命中返回 `Unavailable`）；
    /// 未命中则调用 `get_group_member` 实时拉取，成功写正缓存返回 `Fresh`，
    /// 失败写负缓存返回 `Unavailable`。上层据 `MemberFetchResult` 标注 `source`，
    /// `Unavailable` 时保持 `source=Event` 不伪造，不阻断聊天。
    pub(crate) async fn get_group_member_cached(
        &self,
        group_openid: &str,
        member_openid: &str,
    ) -> MemberFetchResult {
        if let Some(cached) = self.member_cache.get(group_openid, member_openid) {
            return match cached {
                Some(detail) => MemberFetchResult::Cached(detail),
                None => MemberFetchResult::Unavailable,
            };
        }
        match self.get_group_member(group_openid, member_openid).await {
            Ok(detail) => {
                self.member_cache
                    .insert(group_openid, member_openid, Some(detail.clone()));
                MemberFetchResult::Fresh(detail)
            }
            Err(error) => {
                // 拉取失败写负缓存，避免失败风暴；上层降级 source=Event。
                warn!(
                    group = %mask_openid(group_openid),
                    member = %mask_openid(member_openid),
                    error = %error.log_summary(),
                    "group member detail fetch failed; caching negative"
                );
                self.member_cache.insert(group_openid, member_openid, None);
                MemberFetchResult::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GroupMemberDetail, MemberDetailCache, MemberFetchResult};
    use std::time::Duration;

    #[test]
    fn group_member_detail_dto_parses_qq_example() {
        // #229 接口示例返回，全部字段存在时能完整解析。
        let payload = r#"{
            "member_openid": "member-id",
            "username": "群昵称",
            "member_role": "owner",
            "bot": false,
            "joined_at": "2026-03-23T14:46:25+08:00",
            "union_openid": "union-id"
        }"#;
        let detail: GroupMemberDetail = serde_json::from_str(payload).expect("parse example");
        assert_eq!(detail.member_openid.as_deref(), Some("member-id"));
        assert_eq!(detail.username.as_deref(), Some("群昵称"));
        assert_eq!(detail.member_role.as_deref(), Some("owner"));
        assert_eq!(detail.bot, Some(false));
        assert_eq!(
            detail.joined_at.as_deref(),
            Some("2026-03-23T14:46:25+08:00")
        );
        assert_eq!(detail.union_openid.as_deref(), Some("union-id"));
    }

    #[test]
    fn group_member_detail_dto_tolerates_missing_fields() {
        // QQ 可能按权限 / 场景省略字段；空对象不报错，所有字段为 None。
        let detail: GroupMemberDetail = serde_json::from_str("{}").expect("parse empty");
        assert_eq!(detail.member_openid, None);
        assert_eq!(detail.username, None);
        assert_eq!(detail.member_role, None);
        assert_eq!(detail.bot, None);
        assert_eq!(detail.joined_at, None);
        assert_eq!(detail.union_openid, None);
    }

    #[test]
    fn group_member_detail_dto_tolerates_extra_fields() {
        // 接口后续新增字段不应破坏解析（serde 默认忽略未知字段）。
        let payload = r#"{"member_openid":"m1","future_field":42}"#;
        let detail: GroupMemberDetail = serde_json::from_str(payload).expect("parse extra");
        assert_eq!(detail.member_openid.as_deref(), Some("m1"));
        assert_eq!(detail.bot, None);
    }

    fn sample_detail() -> GroupMemberDetail {
        GroupMemberDetail {
            member_openid: Some("m".to_owned()),
            username: Some("昵称".to_owned()),
            member_role: Some("admin".to_owned()),
            bot: Some(false),
            joined_at: None,
            union_openid: Some("u".to_owned()),
        }
    }

    #[test]
    fn member_detail_cache_returns_positive_hit_within_ttl() {
        let cache = MemberDetailCache::new(Duration::from_secs(600), Duration::from_secs(30));
        assert!(cache.get("g", "m").is_none());
        cache.insert("g", "m", Some(sample_detail()));
        // 正缓存命中：返回 Some(Some(detail))。
        let hit = cache.get("g", "m").expect("positive hit");
        let detail = hit.expect("some detail");
        assert_eq!(detail.username.as_deref(), Some("昵称"));
        assert_eq!(detail.member_role.as_deref(), Some("admin"));
    }

    #[test]
    fn member_detail_cache_returns_negative_hit_within_ttl() {
        let cache = MemberDetailCache::new(Duration::from_secs(600), Duration::from_secs(30));
        // 负缓存：拉取失败时 insert None，TTL 内命中返回 Some(None)。
        cache.insert("g", "m", None);
        let hit = cache.get("g", "m").expect("negative hit");
        assert!(hit.is_none(), "负缓存应返回内层 None");
    }

    #[test]
    fn member_detail_cache_expires_after_ttl() {
        // TTL 为 0 时立即过期：insert 后立刻 get 应未命中。
        let cache = MemberDetailCache::new(Duration::from_secs(0), Duration::from_secs(0));
        cache.insert("g", "m", Some(sample_detail()));
        assert!(cache.get("g", "m").is_none(), "0 TTL 应立即过期");
    }

    #[test]
    fn member_detail_cache_keys_by_group_and_member() {
        let cache = MemberDetailCache::default_ttl();
        cache.insert("g1", "m", Some(sample_detail()));
        // 不同 group / member 不命中。
        assert!(cache.get("g2", "m").is_none());
        assert!(cache.get("g1", "other").is_none());
    }

    #[test]
    fn member_fetch_result_source_and_detail_helpers() {
        // 确保 MemberFetchResult 变体可构造且可区分（不调用真实 HTTP）。
        let fresh = MemberFetchResult::Fresh(sample_detail());
        let cached = MemberFetchResult::Cached(sample_detail());
        let unavail = MemberFetchResult::Unavailable;
        assert!(matches!(fresh, MemberFetchResult::Fresh(_)));
        assert!(matches!(cached, MemberFetchResult::Cached(_)));
        assert!(matches!(unavail, MemberFetchResult::Unavailable));
    }
}
