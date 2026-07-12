//! RSS 订阅命令处理。
//!
//! `/rss` 和 `/订阅` 只管理当前 QQ 目标（私聊或群聊）的订阅；
//! 删除时始终用当前 scope_key 过滤，不能跨目标删除其它用户或群的订阅。

use qq_maid_common::time_context::format_rss_time_for_display;

use super::command_render::escape_markdown_inline;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::{SessionMeta, SessionRecord},
        tools::rss::{RssSubscription, RssTarget, RssTargetType, feed::RssFeedError},
    },
};

use super::{
    RespondRequest, RespondResponse, RustRespondService,
    common::{
        CommandBody, GROUP_ADMIN_REQUIRED_REPLY, group_management_allowed, rss_error,
        structured_command_body, truncate_chars,
    },
};

const RSS_RECENT_TITLE_MAX_CHARS: usize = 120;

impl RustRespondService {
    pub(super) async fn handle_rss_flow(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(command) = parse_rss_command(user_text) else {
            return Ok(None);
        };
        if matches!(command.action.as_str(), "rss_add" | "rss_delete")
            && !group_management_allowed(req)
        {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                GROUP_ADMIN_REQUIRED_REPLY,
                "group_admin_required",
            )?));
        }
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
                (
                    structured_command_body(format_rss_list_reply(&subscriptions)),
                    "rss_list",
                )
            }
            "rss_recent" => {
                let limit = match parse_recent_limit(&command.argument) {
                    Ok(limit) => limit,
                    Err(reply) => {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            reply,
                            "rss_recent",
                        )?));
                    }
                };
                let subscriptions = self
                    .rss_store
                    .list_by_scope(&target.scope_key)
                    .map_err(rss_error)?;
                let items = self
                    .rss_store
                    .recent_items_by_scope(&target.scope_key, None, limit)
                    .map_err(rss_error)?;
                (
                    format_rss_recent_reply(&subscriptions, &items),
                    "rss_recent",
                )
            }
            "rss_add" => {
                let Some(entries) = parse_add_arguments(&command.argument) else {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        "用法：/rss add RSS地址 [名称]；也支持多行：标题换行 RSS地址。",
                        "rss_add",
                    )?));
                };
                let outcome = self.add_rss_subscriptions(&target, entries).await?;
                (structured_command_body(outcome), "rss_add")
            }
            "rss_delete" => {
                let targets = parse_delete_arguments(&command.argument);
                if targets.is_empty() {
                    ("用法：/rss delete 编号或订阅ID".into(), "rss_delete")
                } else {
                    let subscriptions = self
                        .rss_store
                        .list_by_scope(&target.scope_key)
                        .map_err(rss_error)?;
                    let reply =
                        self.delete_rss_subscriptions(&target.scope_key, &subscriptions, &targets)?;
                    (structured_command_body(reply), "rss_delete")
                }
            }
            "rss_test" => {
                let url = command.argument.trim();
                if url.is_empty() {
                    ("用法：/rss test RSS地址".into(), "rss_test")
                } else {
                    match self
                        .rss_fetcher
                        .fetch(url, self.rss_summary_max_chars)
                        .await
                    {
                        Ok(feed) => (
                            structured_command_body(format!(
                                "RSS 测试成功：{}\n当前条目数：{}",
                                feed.title,
                                feed.items.len()
                            )),
                            "rss_test",
                        ),
                        Err(err) => (
                            structured_command_body(format!(
                                "RSS 测试失败：{}",
                                feed_error_reply(&err)
                            )),
                            "rss_test",
                        ),
                    }
                }
            }
            "rss_help" => (structured_command_body(rss_usage()), "rss"),
            _ => (structured_command_body(rss_usage()), "rss"),
        };

        Ok(Some(self.append_pending_response(
            session,
            user_text,
            reply,
            command_name,
        )?))
    }

    async fn add_rss_subscriptions(
        &self,
        target: &RssTarget,
        entries: Vec<RssAddEntry>,
    ) -> Result<String, LlmError> {
        let single = entries.len() == 1;
        let mut created = Vec::new();
        let mut failed = Vec::new();
        for entry in entries {
            match self
                .rss_fetcher
                .fetch(&entry.url, self.rss_summary_max_chars)
                .await
            {
                Ok(feed) => {
                    let feed_items_len = feed.items.len();
                    let title = entry.name.unwrap_or(feed.title);
                    match self.rss_store.create_subscription(
                        target,
                        &entry.url,
                        &title,
                        &feed.items,
                        self.rss_seen_retention,
                    ) {
                        Ok(subscription) => created.push((subscription, feed_items_len)),
                        Err(err) => failed.push((entry.url, err.message().to_owned())),
                    }
                }
                Err(err) => failed.push((entry.url, feed_error_reply(&err))),
            }
        }

        if single {
            if let Some((subscription, item_count)) = created.first() {
                return Ok(format!(
                    "已添加 RSS 订阅：{}\n地址：{}\n已将当前 {} 条历史条目标记为已见，首次添加不会推送历史文章。",
                    subscription.title, subscription.url, item_count
                ));
            }
            let message = failed
                .first()
                .map(|(_, message)| message.as_str())
                .unwrap_or("未知错误");
            return Ok(format!("RSS 地址无法访问或无法解析：{message}"));
        }

        let mut rows = vec!["RSS 批量添加结果：".to_owned()];
        if !created.is_empty() {
            rows.push(format!("已添加 {} 个订阅：", created.len()));
            for (index, (subscription, item_count)) in created.iter().enumerate() {
                rows.push(format!(
                    "{}. {} {}（历史条目 {} 条已标记为已见）",
                    index + 1,
                    truncate_chars(&subscription.title, 60),
                    subscription.url,
                    item_count
                ));
            }
        }
        if !failed.is_empty() {
            rows.push(format!("失败 {} 个：", failed.len()));
            for (index, (url, message)) in failed.iter().enumerate() {
                rows.push(format!(
                    "{}. {}：{}",
                    index + 1,
                    truncate_chars(url, 100),
                    message
                ));
            }
        }
        Ok(rows.join("\n"))
    }

    fn delete_rss_subscriptions(
        &self,
        scope_key: &str,
        subscriptions: &[RssSubscription],
        targets: &[String],
    ) -> Result<String, LlmError> {
        let mut resolved = Vec::<&RssSubscription>::new();
        let mut missing = Vec::<String>::new();
        for target in targets {
            if let Some(subscription) = resolve_subscription_target(subscriptions, target) {
                if !resolved.iter().any(|item| item.id == subscription.id) {
                    resolved.push(subscription);
                }
            } else {
                missing.push(target.clone());
            }
        }
        if resolved.is_empty() {
            return Ok("没有找到当前目标下对应的 RSS 订阅。".to_owned());
        }

        let single = targets.len() == 1 && missing.is_empty();
        let mut deleted = Vec::new();
        for subscription in resolved {
            if self
                .rss_store
                .delete_for_scope(scope_key, &subscription.id)
                .map_err(rss_error)?
            {
                deleted.push(subscription.title.clone());
            }
        }
        if single {
            if let Some(title) = deleted.first() {
                return Ok(format!("已删除 RSS 订阅：{title}"));
            }
            return Ok("没有找到当前目标下对应的 RSS 订阅。".to_owned());
        }

        let mut rows = vec![format!("已删除 {} 个 RSS 订阅：", deleted.len())];
        for (index, title) in deleted.iter().enumerate() {
            rows.push(format!("{}. {}", index + 1, truncate_chars(title, 60)));
        }
        if !missing.is_empty() {
            rows.push(format!(
                "未找到 {} 个目标：{}",
                missing.len(),
                missing.join("、")
            ));
        }
        Ok(rows.join("\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RssAddEntry {
    url: String,
    name: Option<String>,
}

pub(super) fn parse_rss_command(text: &str) -> Option<ParsedCommand> {
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
        "recent" | "updates" | "update" | "最近" | "更新" => "rss_recent",
        "add" | "new" | "create" | "添加" | "新增" | "订阅" => "rss_add",
        "delete" | "del" | "rm" | "remove" | "删除" | "取消订阅" => "rss_delete",
        "test" | "测试" => "rss_test",
        _ => "rss_help",
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument: if action == "rss_help" {
            argument.to_owned()
        } else {
            rest.to_owned()
        },
        raw_command: command.raw_command,
    })
}

fn rss_target_from_meta(meta: &SessionMeta) -> Option<RssTarget> {
    // RSS 订阅跟随当前可投递会话目标；scope_key 只用于订阅过滤，不能替代 target_id。
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

fn parse_add_arguments(argument: &str) -> Option<Vec<RssAddEntry>> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    let mut old_style_parts = argument.splitn(2, char::is_whitespace);
    let first = old_style_parts.next()?.trim();
    if is_rss_url(first) {
        return Some(vec![RssAddEntry {
            url: first.to_owned(),
            name: old_style_parts.next().and_then(clean_display_optional),
        }]);
    }

    let mut entries = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in argument.lines() {
        let line = strip_list_marker(line.trim());
        if line.is_empty() {
            continue;
        }
        if let Some((name_prefix, url, name_suffix)) = extract_url_from_line(line) {
            let name = clean_display_optional(name_prefix)
                .or_else(|| pending_name.take())
                .or_else(|| clean_display_optional(name_suffix));
            entries.push(RssAddEntry {
                url: url.to_owned(),
                name,
            });
            continue;
        }
        pending_name = clean_display_optional(line);
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn parse_delete_arguments(argument: &str) -> Vec<String> {
    argument
        .lines()
        .flat_map(|line| line.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '，')))
        .map(strip_list_marker)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn strip_list_marker(value: &str) -> &str {
    let value = value.trim();
    let digit_count = value.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count == 0 {
        return value;
    }
    let Some(rest) = value.get(digit_count..) else {
        return value;
    };
    let rest = rest.trim_start();
    let mut chars = rest.chars();
    match chars.next() {
        Some('.' | '。' | '、' | ')' | '）' | ':' | '：') => chars.as_str().trim_start(),
        _ => value,
    }
}

fn extract_url_from_line(line: &str) -> Option<(&str, &str, &str)> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let before = line[..start].trim();
    let after_start = &line[start..];
    let url_len = after_start
        .find(char::is_whitespace)
        .unwrap_or(after_start.len());
    let url = &after_start[..url_len];
    let after = after_start[url_len..].trim();
    if is_rss_url(url) {
        Some((before, url, after))
    } else {
        None
    }
}

fn is_rss_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
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
        return "当前目标没有 RSS 订阅。\n添加订阅：/rss add RSS地址 [名称]".to_owned();
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
                format_rss_check_time(subscription.last_checked_at.as_deref()),
                subscription.last_error.as_deref().unwrap_or("无")
            ));
        }
    }
    rows.push("操作：/rss add 地址 [名称]；/rss delete 1".to_owned());
    rows.join("\n")
}

const RSS_RECENT_DEFAULT_LIMIT: usize = 5;
const RSS_RECENT_MAX_LIMIT: usize = 20;
const RSS_RECENT_MAX_CHARS: usize = 3600;

fn parse_recent_limit(argument: &str) -> Result<usize, String> {
    let argument = argument.trim();
    if argument.is_empty() {
        return Ok(RSS_RECENT_DEFAULT_LIMIT);
    }
    let mut parts = argument.split_whitespace();
    let Some(raw_limit) = parts.next() else {
        return Ok(RSS_RECENT_DEFAULT_LIMIT);
    };
    if parts.next().is_some() {
        return Err(rss_recent_limit_usage());
    }
    match raw_limit.parse::<usize>() {
        Ok(limit) if (1..=RSS_RECENT_MAX_LIMIT).contains(&limit) => Ok(limit),
        _ => Err(rss_recent_limit_usage()),
    }
}

fn rss_recent_limit_usage() -> String {
    format!("用法：/rss recent [数量]；数量需为 1～{RSS_RECENT_MAX_LIMIT}。")
}

fn format_rss_recent_reply(
    subscriptions: &[RssSubscription],
    items: &[crate::runtime::tools::rss::RssRecentItem],
) -> CommandBody {
    if subscriptions.is_empty() {
        return CommandBody::plain("当前会话还没有 RSS 订阅，可使用 /rss add 地址 添加。");
    }
    if items.is_empty() {
        let failed_count = failed_subscription_count(subscriptions);
        let mut reply =
            "当前会话已有 RSS 订阅，但还没有抓到更新。可以等待后台检查，或使用 /rss test 地址 检查订阅源。"
                .to_owned();
        append_recent_failure_hint(&mut reply, failed_count);
        return CommandBody::plain(reply);
    }

    let mut text_rows = vec!["最近 RSS 更新：".to_owned(), String::new()];
    let mut markdown_rows = vec!["# 最近 RSS 更新".to_owned(), String::new()];
    for (index, item) in items.iter().enumerate() {
        let subscription_title = truncate_chars(&item.subscription_title, 40);
        let item_title = rss_recent_display_title(&item.title);
        text_rows.push(format!(
            "{}. [{}] {}",
            index + 1,
            subscription_title,
            item_title
        ));
        if let Some(link) = item.link.as_deref().filter(|link| !link.trim().is_empty()) {
            text_rows.push(format!("   {link}"));
            markdown_rows.push(format!(
                "{}. [{}] [{}](<{}>)",
                index + 1,
                escape_markdown_inline(&subscription_title),
                escape_markdown_inline(&item_title),
                link.trim()
            ));
        } else {
            text_rows.push("   链接：当前条目未提供".to_owned());
            markdown_rows.push(format!(
                "{}. **{}** {}",
                index + 1,
                escape_markdown_inline(&subscription_title),
                escape_markdown_inline(&item_title)
            ));
        }
        let time_line = format!(
            "{}：{}",
            rss_recent_time_label(item),
            format_rss_time_for_display(rss_recent_time_value(item))
        );
        text_rows.push(format!("   {time_line}"));
        markdown_rows.push(format!("   {time_line}"));
        text_rows.push(String::new());
        markdown_rows.push(String::new());
    }
    let failed_count = failed_subscription_count(subscriptions);
    if failed_count > 0 {
        let hint = format!("提示：{failed_count} 个订阅源最近检查失败，可能有更新延迟。");
        text_rows.push(hint.clone());
        markdown_rows.push(hint);
    }
    CommandBody::dual(
        truncate_chars(&text_rows.join("\n"), RSS_RECENT_MAX_CHARS),
        truncate_chars(&markdown_rows.join("\n"), RSS_RECENT_MAX_CHARS),
    )
}

fn rss_recent_display_title(raw: &str) -> String {
    truncate_chars(&raw.replace(['\r', '\n'], " "), RSS_RECENT_TITLE_MAX_CHARS)
}

fn failed_subscription_count(subscriptions: &[RssSubscription]) -> usize {
    subscriptions
        .iter()
        .filter(|subscription| {
            subscription
                .last_error
                .as_deref()
                .is_some_and(|error| !error.trim().is_empty())
        })
        .count()
}

fn append_recent_failure_hint(reply: &mut String, failed_count: usize) {
    if failed_count > 0 {
        reply.push_str(&format!(
            "\n提示：{failed_count} 个订阅源最近检查失败，可能有更新延迟。"
        ));
    }
}

fn rss_recent_time_label(item: &crate::runtime::tools::rss::RssRecentItem) -> &'static str {
    if item
        .published_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "发布时间"
    } else if item
        .updated_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "更新时间"
    } else if item
        .pushed_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        "推送时间"
    } else {
        "抓取时间"
    }
}

fn rss_recent_time_value(item: &crate::runtime::tools::rss::RssRecentItem) -> &str {
    item.published_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            item.updated_at
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            item.pushed_at
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or(&item.last_seen_at)
}

fn format_rss_check_time(value: Option<&str>) -> String {
    // RSS 检查时间在 SQLite 中保留 RFC3339；这里只做用户可读展示，不改变持久化语义。
    value
        .map(format_rss_time_for_display)
        .filter(|display| !display.trim().is_empty())
        .unwrap_or_else(|| "未检查".to_owned())
}

fn rss_usage() -> String {
    "用法：/rss；/rss list；/rss recent [数量]；/rss add RSS地址 [名称]；/rss delete 编号；/rss test RSS地址".to_owned()
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

fn clean_display_optional(value: &str) -> Option<String> {
    let value = clean_optional(value)?;
    if matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "null" | "none" | "undefined"
    ) {
        None
    } else {
        Some(value)
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
    fn stable_group_target_keeps_raw_delivery_target_separate() {
        let meta = SessionMeta::new_with_account(
            "platform:qq_official:account:app-1:group:stale-gid",
            Some("u1".to_owned()),
            Some("current-gid".to_owned()),
            None,
            None,
            "qq_official",
            Some("app-1".to_owned()),
        );
        let target = rss_target_from_meta(&meta).unwrap();
        assert_eq!(target.target_type, RssTargetType::Group);
        assert_eq!(target.target_id, "current-gid");
        assert_eq!(
            target.scope_key,
            "platform:qq_official:account:app-1:group:stale-gid"
        );
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

    #[test]
    fn rss_command_parses_recent_aliases_without_falling_back_to_list() {
        for input in ["/rss recent", "/rss 最近", "/rss updates", "/rss 更新"] {
            let command = parse_rss_command(input).unwrap();
            assert_eq!(command.action, "rss_recent", "input: {input}");
            assert_eq!(command.argument, "");
        }

        let limited = parse_rss_command("/rss recent 10").unwrap();
        assert_eq!(limited.action, "rss_recent");
        assert_eq!(limited.argument, "10");
    }

    #[test]
    fn rss_unknown_subcommand_returns_usage_instead_of_list() {
        let command = parse_rss_command("/rss nope").unwrap();
        assert_eq!(command.action, "rss_help");
        assert_ne!(command.action, "rss_list");
    }

    #[test]
    fn empty_rss_list_guides_user_to_add_subscription() {
        let reply = format_rss_list_reply(&[]);

        assert_eq!(
            reply,
            "当前目标没有 RSS 订阅。\n添加订阅：/rss add RSS地址 [名称]"
        );
    }

    #[test]
    fn rss_list_formats_check_time_for_display() {
        let subscriptions = vec![
            RssSubscription {
                id: "sub-1".to_owned(),
                target_type: RssTargetType::Group,
                target_id: "g1".to_owned(),
                scope_key: "group:g1".to_owned(),
                url: "https://example.test/feed.xml".to_owned(),
                title: "Feed".to_owned(),
                enabled: true,
                created_at: "2026-06-18T03:50:00+08:00".to_owned(),
                last_checked_at: Some("2026-06-18T03:51:44+08:00".to_owned()),
                last_success_at: None,
                last_error: None,
                consecutive_failures: 0,
                initialized: true,
            },
            RssSubscription {
                id: "sub-2".to_owned(),
                target_type: RssTargetType::Group,
                target_id: "g1".to_owned(),
                scope_key: "group:g1".to_owned(),
                url: "https://example.test/utc.xml".to_owned(),
                title: "UTC Feed".to_owned(),
                enabled: true,
                created_at: "2026-06-18T03:50:00+08:00".to_owned(),
                last_checked_at: Some("2026-06-17T19:51:44+00:00".to_owned()),
                last_success_at: None,
                last_error: Some("timeout".to_owned()),
                consecutive_failures: 1,
                initialized: true,
            },
        ];

        let reply = format_rss_list_reply(&subscriptions);

        assert!(reply.contains("最近检查：2026-06-18 03:51；错误：无"));
        assert!(reply.contains("最近检查：2026-06-18 03:51；错误：timeout"));
        assert!(!reply.contains("T03:51:44+08:00"));
        assert!(!reply.contains("T19:51:44+00:00"));
    }
}
