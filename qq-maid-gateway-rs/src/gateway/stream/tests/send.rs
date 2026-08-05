use super::*;

#[test]
fn cumulative_prefix_is_submitted_as_a_replace_update() {
    assert_eq!(
        reconcile_cumulative_text("你好", "你好，这是"),
        CumulativeTextAction::Update("你好，这是".to_owned())
    );
    assert_eq!(
        reconcile_cumulative_text("你好", "你好"),
        CumulativeTextAction::Keep
    );
}

#[test]
fn candidate_rollback_requests_a_new_reply_instead_of_overwriting_prefix() {
    assert_eq!(
        reconcile_cumulative_text("已经展示的正文", "新的"),
        CumulativeTextAction::Rollover("新的".to_owned())
    );
}

#[test]
fn mismatched_candidate_only_appends_a_safe_tail() {
    assert_eq!(
        reconcile_cumulative_text("你 好", "你 呀"),
        CumulativeTextAction::Update("你 好呀".to_owned())
    );
}

#[test]
fn unicode_markdown_and_emoji_are_not_split_or_normalized() {
    let accepted = "# 标题\n\n👩‍💻 中";
    let incoming =
        format!("{accepted}\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n[链接](https://example.com)");
    assert_eq!(
        reconcile_cumulative_text(accepted, &incoming),
        CumulativeTextAction::Update(incoming)
    );
}
