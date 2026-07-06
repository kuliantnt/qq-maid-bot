//! QQ 群成员详情查询（#229）。
//!
//! 对应 `GET /v2/groups/{group_openid}/members/{member_openid}`，用于补全群昵称 /
//! 群角色 / 是否机器人 / `union_openid` 等展示字段。本模块只负责拉取与结构化解析，
//! 不缓存、不阻断聊天；缓存与降级策略由上层（#319 Phase 3）负责。

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

#[cfg(test)]
mod tests {
    use super::GroupMemberDetail;

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
}
