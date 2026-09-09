use super::super::support::*;

#[tokio::test]
async fn sealdice_set_commands_switch_the_default_die_for_bare_d_syntax() {
    let service = test_service();

    let response = service.respond(message(".set coc")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("CoC（D100）"));
    assert!(text.contains("点数不高于目标值"));

    let response = service.respond(message(".r2#d+1")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("roll"));
    assert!(text.contains("1d100+1"), "{text}");
    assert!(text.contains("第1轮"));
    assert!(text.contains("第2轮"));

    for (input, expected) in [
        (".r测试", "/ 100"),
        ("/r测试", "/ 100"),
        (".rd+1", "1d100+1"),
    ] {
        let response = service.respond(message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("roll"));
        assert!(text.contains(expected), "{input}: {text}");
    }

    let response = service.respond(message(".set dnd")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("DND（D20）"));
    assert!(text.contains("达到或超过 DC"));

    let response = service.respond(message(".r2#d+1")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("1d20+1"), "{text}");
}

#[tokio::test]
async fn dice_rule_query_reports_the_current_comparison_direction() {
    let service = test_service();
    service.respond(message("/set coc")).await.unwrap();

    let response = service.respond(message("/set 骰子")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("CoC（D100）"));
    assert!(text.contains("点数 ≤ 目标值时成功"));
}

#[tokio::test]
async fn initiative_and_hidden_rolls_are_deterministic_commands() {
    use crate::runtime::push::{PushTarget, PushTargetType};
    use qq_maid_common::identity_context::ConversationKind;
    let service = test_service();
    service.respond(message("/set 昵称 玩家甲")).await.unwrap();
    let response = service.respond(message(".ri 12")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("initiative"));
    assert!(response.text.unwrap().contains("玩家甲：12"));
    let mut other = message("/init");
    other.scope_key = "other-conversation".to_owned();
    assert!(
        service
            .respond(other)
            .await
            .unwrap()
            .text
            .unwrap()
            .contains("为空")
    );

    let mut hidden = message("/rh 1d1 私有原因");
    hidden.conversation_kind = ConversationKind::Group;
    let denied = service.respond(hidden.clone()).await.unwrap().text.unwrap();
    assert!(denied.contains("本次未投骰"));
    hidden.private_reply_target = Some(PushTarget::onebot11(
        "test-bot",
        PushTargetType::Private,
        "test-user",
    ));
    hidden.message_id = Some("hidden-roll-msg-1".to_owned());
    let response = service.respond(hidden).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("私发队列"));
    assert!(!text.contains("私有原因"));
    assert!(!text.contains("1d1"));
    let tasks = service.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].target.target_type, PushTargetType::Private);
    assert!(tasks[0].payload.to_string().contains("私有原因"));
    assert!(tasks[0].payload.to_string().contains("= 1"));

    for input in ["/rx d20", "/rxh d20", "/rhx d20"] {
        let response = service.respond(message(input)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("roll"));
        assert!(response.text.unwrap().contains("暂不执行"));
    }
}

#[tokio::test]
async fn initiative_bare_dice_reuses_manual_display_name() {
    let service = test_service();
    service.respond(message("/nn 脸脸")).await.unwrap();

    let response = service.respond(message("/ri d20+4")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("initiative"));
    let text = response.text.unwrap();
    assert!(text.contains("脸脸："), "{text}");
    assert!(!text.contains("d20+4："), "{text}");

    let response = service.respond(message("/init")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("当前：脸脸"), "{text}");
    assert!(text.contains("1. 脸脸："), "{text}");
    assert!(!text.contains("d20+4："), "{text}");

    let response = service.respond(message("/ri d20+4 哥布林")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("哥布林："), "{text}");
    assert!(!text.contains("d20+4："), "{text}");

    let response = service.respond(message("/init")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("脸脸："), "{text}");
    assert!(text.contains("哥布林："), "{text}");
    assert!(!text.contains("d20+4："), "{text}");
}

#[tokio::test]
async fn initiative_compact_modifiers_match_spaced_forms_and_dot_prefix() {
    let service = test_service();

    let response = service
        .respond(message("/ri+5 哥布林1，+2 哥布林2"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("initiative"));
    let text = response.text.unwrap();
    assert!(text.contains("哥布林1："), "{text}");
    assert!(text.contains("哥布林2："), "{text}");

    let table = service
        .respond(message("/init"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(table.contains("哥布林1："), "{table}");
    assert!(table.contains("哥布林2："), "{table}");

    for (input, expected_name) in [
        ("/ri+5 哥布林1", "哥布林1"),
        ("/ri-1 哥布林", "哥布林"),
        (".ri+5 哥布林1", "哥布林1"),
    ] {
        let response = service.respond(message(input)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("initiative"), "{input}");
        let text = response.text.unwrap();
        assert!(text.contains(expected_name), "{input}: {text}");
    }

    for input in ["/rich", "/right", "/ring"] {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_eq!(
            response.command.as_deref(),
            Some("unknown_command"),
            "{input}"
        );
    }
}

#[tokio::test]
async fn initiative_compact_clear_uses_shared_prefix_compatibility() {
    let service = test_service();
    let empty = service.respond(message("/init")).await.unwrap().text;
    for input in ["/init clr", "/initclr", ".initclr", "。initclr"] {
        service.respond(message("/ri 10 A")).await.unwrap();
        service.respond(message("/init end")).await.unwrap();
        let response = service.respond(message(input)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("initiative"), "{input}");
        assert_eq!(response.text, empty, "{input}");
        let response = service.respond(message("/ri 5 B")).await.unwrap();
        assert!(response.text.unwrap().contains("第 1 轮 · 当前：B"));
        service.respond(message("/init clr")).await.unwrap();
    }
}

#[tokio::test]
async fn private_hidden_roll_uses_preference_and_group_players_share_initiative() {
    let service = test_service();
    service.respond(private_message("/set coc")).await.unwrap();
    let text = service
        .respond(private_message("/rh"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("1d100"));
    assert!(
        service
            .notification_store
            .list_all_for_test()
            .unwrap()
            .is_empty()
    );
    service.respond(message("/ri 10 A")).await.unwrap();
    let mut second = message("/ri 5 B");
    second.user_id = Some("u2".to_owned());
    let text = service.respond(second).await.unwrap().text.unwrap();
    assert!(text.contains("A：10"));
    assert!(text.contains("B：5"));
}
