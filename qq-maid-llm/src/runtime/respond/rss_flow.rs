//! RSS 订阅命令处理。
//!
//! `/rss` 和 `/订阅` 只管理当前 QQ 目标（私聊或群聊）的订阅；
//! 删除时始终用当前 scope_key 过滤，不能跨目标删除其它用户或群的订阅。

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        rss::{RssSubscription, RssTarget, RssTargetType, feed::RssFeedError},
        session::{SessionMeta, SessionRecord},
    },
};

use super::{
    RespondResponse, RustRespondService,
    common::{rss_error, truncate_chars},
};

impl RustRespondService {
    pub(super) async fn handle_rss_flow(
        &self,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(command) = parse_rss_command(user_text) else {
            return Ok(None);
        };
        let target = match rss_target_from_meta(meta) {
            Some(target) => target,
            None => {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "当前消息缺少 QQ 目标标识，无法管理 RSS 订阅。",
                    "rss",
                )?));
            }
        };
        let (reply, command_name) = match command.action.as_str() {
            "rss_list" => {
                let subscriptions = self
                    .rss_store
                    .list_by_scope(&target.scope_key)
                    .map_err(rss_error)?;
                (format_rss_list_reply(&subscriptions), "rss_list")
            }
            "rss_add" => {
                let Some((url, name)) = parse_add_argument(&command.argument) else {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        "用法：/rss add RSS地址 [名称]",
                        "rss_add",
                    )?));
                };
                match self
                    .rss_fetcher
                    .fetch(&url, self.rss_summary_max_chars)
                    .await
                {
                    Ok(feed) => {
                        let title = name.unwrap_or(feed.title);
                        let created = self
                            .rss_store
                            .create_subscription(
                                &target,
                                &url,
                                &title,
                                &feed.items,
                                self.rss_seen_retention,
                            )
                            .map_err(rss_error)?;
                        (
                            format!(
                                "已添加 RSS 订阅：{}\n地址：{}\n已将当前 {} 条历史条目标记为已见，首次添加不会推送历史文章。",
                                created.title,
                                created.url,
                                feed.items.len()
                            ),
                            "rss_add",
                        )
                    }
                    Err(err) => (
                        format!("RSS 地址无法访问或无法解析：{}", feed_error_reply(&err)),
                        "rss_add",
                    ),
                }
            }
            "rss_delete" => {
                let argument = command.argument.trim();
                if argument.is_empty() {
                    ("用法：/rss delete 编号或订阅ID".to_owned(), "rss_delete")
                } else {
                    let subscriptions = self
                        .rss_store
                        .list_by_scope(&target.scope_key)
                        .map_err(rss_error)?;
                    let Some(subscription) = resolve_subscription_target(&subscriptions, argument)
                    else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            "没有找到当前目标下对应的 RSS 订阅。",
                            "rss_delete",
                        )?));
                    };
                    let deleted = self
                        .rss_store
                        .delete_for_scope(&target.scope_key, &subscription.id)
                        .map_err(rss_error)?;
                    if deleted {
                        (
                            format!("已删除 RSS 订阅：{}", subscription.title),
                            "rss_delete",
                        )
                    } else {
                        (
                            "没有找到当前目标下对应的 RSS 订阅。".to_owned(),
                            "rss_delete",
                        )
                    }
                }
            }
            "rss_test" => {
                let url = command.argument.trim();
                if url.is_empty() {
                    ("用法：/rss test RSS地址".to_owned(), "rss_test")
                } else {
                    match self
                        .rss_fetcher
                        .fetch(url, self.rss_summary_max_chars)
                        .await
                    {
                        Ok(feed) => (
                            format!(
                                "RSS 测试成功：{}\n当前条目数：{}",
                                feed.title,
                                feed.items.len()
                            ),
                            "rss_test",
                        ),
                        Err(err) => (
                            format!("RSS 测试失败：{}", feed_error_reply(&err)),
                            "rss_test",
                        ),
                    }
                }
            }
            _ => (rss_usage(), "rss"),
        };

        Ok(Some(self.append_pending_response(
            session,
            user_text,
            reply,
            command_name,
        )?))
    }
}

fn parse_rss_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    if command.action != "rss" {
        return None;
    }
    let argument = command.argument.trim();
    if argument.is_empty() {
        return Some(ParsedCommand {
            action: "rss_list".to_owned(),
            argument: String::new(),
            raw_command: command.raw_command,
        });
    }
    let mut parts = argument.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    let action = match first.to_ascii_lowercase().as_str() {
        "list" | "ls" | "列表" | "查看" => "rss_list",
        "add" | "new" | "create" | "添加" | "新增" | "订阅" => "rss_add",
        "delete" | "del" | "rm" | "remove" | "删除" | "取消订阅" => "rss_delete",
        "test" | "测试" => "rss_test",
        _ => "rss_list",
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument: if action == "rss_list" && !matches!(first, "list" | "ls" | "列表" | "查看") {
            argument.to_owned()
        } else {
            rest.to_owned()
        },
        raw_command: command.raw_command,
    })
}

fn rss_target_from_meta(meta: &SessionMeta) -> Option<RssTarget> {
    if meta.scope == "group" || meta.scope_key.starts_with("group:") {
        let target_id = meta
            .group_id
            .as_deref()
            .and_then(clean_optional)
            .or_else(|| {
                meta.scope_key
                    .strip_prefix("group:")
                    .and_then(clean_optional)
            })?;
        return Some(RssTarget {
            target_type: RssTargetType::Group,
            target_id,
            scope_key: meta.scope_key.clone(),
        });
    }
    let target_id = meta
        .user_id
        .as_deref()
        .and_then(clean_optional)
        .or_else(|| {
            meta.scope_key
                .strip_prefix("private:")
                .and_then(clean_optional)
        })?;
    Some(RssTarget {
        target_type: RssTargetType::Private,
        target_id,
        scope_key: meta.scope_key.clone(),
    })
}

fn parse_add_argument(argument: &str) -> Option<(String, Option<String>)> {
    let mut parts = argument.splitn(2, char::is_whitespace);
    let url = parts.next()?.trim();
    if url.is_empty() {
        return None;
    }
    let name = parts.next().and_then(clean_optional);
    Some((url.to_owned(), name))
}

fn resolve_subscription_target<'a>(
    subscriptions: &'a [RssSubscription],
    target: &str,
) -> Option<&'a RssSubscription> {
    let target = target.split_whitespace().next().unwrap_or("").trim();
    if target.chars().all(|ch| ch.is_ascii_digit()) {
        let index = target.parse::<usize>().ok()?;
        return subscriptions
            .get(index.saturating_sub(1))
            .filter(|_| index > 0);
    }
    subscriptions
        .iter()
        .find(|subscription| subscription.id == target || subscription.id.starts_with(target))
}

fn format_rss_list_reply(subscriptions: &[RssSubscription]) -> String {
    if subscriptions.is_empty() {
        return "当前目标没有 RSS 订阅。".to_owned();
    }
    let mut rows = vec!["RSS 订阅：".to_owned()];
    for (index, subscription) in subscriptions.iter().enumerate() {
        rows.push(format!(
            "{}. {} [{}] {}",
            index + 1,
            truncate_chars(&subscription.title, 40),
            if subscription.enabled {
                "启用"
            } else {
                "停用"
            },
            subscription.url
        ));
        if subscription.last_checked_at.is_some() || subscription.last_error.is_some() {
            rows.push(format!(
                "   最近检查：{}；错误：{}",
                subscription.last_checked_at.as_deref().unwrap_or("未检查"),
                subscription.last_error.as_deref().unwrap_or("无")
            ));
        }
    }
    rows.push("操作：/rss add 地址 [名称]；/rss delete 1".to_owned());
    rows.join("\n")
}

fn rss_usage() -> String {
    "用法：/rss；/rss add RSS地址 [名称]；/rss delete 编号；/rss test RSS地址".to_owned()
}

fn feed_error_reply(err: &RssFeedError) -> String {
    match err {
        RssFeedError::Status(status) => format!("HTTP {status}"),
        RssFeedError::UnsafeHost => "地址指向本机、内网或 metadata，已拦截".to_owned(),
        _ => err.to_string(),
    }
}

fn clean_optional(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_group_target_uses_group_scope() {
        let meta = SessionMeta::new(
            "group:g1",
            Some("u1".to_owned()),
            Some("g1".to_owned()),
            None,
            None,
            "qq_official",
        );
        let target = rss_target_from_meta(&meta).unwrap();
        assert_eq!(target.target_type, RssTargetType::Group);
        assert_eq!(target.target_id, "g1");
    }

    #[test]
    fn delete_target_resolves_list_number() {
        let subscriptions = vec![RssSubscription {
            id: "sub-1".to_owned(),
            target_type: RssTargetType::Private,
            target_id: "u1".to_owned(),
            scope_key: "private:u1".to_owned(),
            url: "https://example.test/feed.xml".to_owned(),
            title: "Feed".to_owned(),
            enabled: true,
            created_at: "2026-06-17T00:00:00+08:00".to_owned(),
            last_checked_at: None,
            last_success_at: None,
            last_error: None,
            consecutive_failures: 0,
            initialized: true,
        }];
        assert_eq!(
            resolve_subscription_target(&subscriptions, "1").map(|item| item.id.as_str()),
            Some("sub-1")
        );
    }
}
