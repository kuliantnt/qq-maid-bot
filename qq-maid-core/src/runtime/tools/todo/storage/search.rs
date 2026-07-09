//! 待办搜索打分 helper。
//!
//! 得分梯度：标题命中 > 详情命中 > 原文命中，命中得分相同时再按待办排序顺序稳定排序。
//! 这里只做用户可见内容（标题 / 详情 / 原文）的匹配，不暴露内部 ID 直查。

use super::TodoItem;

/// 计算待办事项与查询关键词的匹配得分（标题 > 详情 > 原文）。
pub(super) fn search_score(item: &TodoItem, query: &str) -> Option<i32> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Some(1);
    }
    let title = item.title.to_ascii_lowercase();
    let detail = item.detail.clone().unwrap_or_default().to_ascii_lowercase();
    let raw = item
        .raw_text
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let haystack = format!("{title}\n{detail}\n{raw}");
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if !tokens.is_empty() && !tokens.iter().all(|token| haystack.contains(token)) {
        return None;
    }
    if !tokens.is_empty() {
        return Some(if title.contains(&query) {
            80
        } else if detail.contains(&query) {
            55
        } else {
            45
        });
    }
    if title == query {
        Some(100)
    } else if title.contains(&query) {
        Some(80)
    } else if detail.contains(&query) {
        Some(55)
    } else if raw.contains(&query) {
        Some(45)
    } else {
        None
    }
}
