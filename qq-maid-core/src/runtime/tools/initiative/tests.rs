use super::command::parse_entries;
use super::*;

#[test]
fn compact_init_commands_parse_like_spaced_commands() {
    for (compact, spaced) in [
        ("/initlist", "/init list"),
        ("/initend", "/init end"),
        ("/inited", "/init ed"),
        ("/initclr", "/init clr"),
        ("/initclear", "/init clear"),
        ("/inithelp", "/init help"),
        ("/initset A d20+2", "/init set A d20+2"),
        ("/initdel A", "/init del A"),
        ("/initrm A", "/init rm A"),
        (" /INITEND ", " /INIT END "),
    ] {
        assert_eq!(
            format!("{:?}", parse_command(compact)),
            format!("{:?}", parse_command(spaced)),
            "{compact} != {spaced}"
        );
    }
    for input in [
        "/initlist extra",
        "/initend extra",
        "/inited extra",
        "/initclr extra",
        "/initclear extra",
        "/inithelp extra",
        "/initdel",
        "/initrm",
    ] {
        assert!(
            matches!(parse_command(input), Some(InitiativeCommand::Invalid)),
            "{input}"
        );
    }
}

#[test]
fn compact_init_suffix_rejects_unknown_commands() {
    for input in ["/initialize", "/initial", "/initabc", "/initfoo"] {
        assert!(parse_command(input).is_none(), "{input}");
    }
}

#[test]
fn compact_records_match_spaced_records() {
    for (compact, spaced, expected) in [
        ("/ri18 哥布林", "/ri 18 哥布林", "18 哥布林"),
        ("/ri5 哥布林", "/ri 5 哥布林", "5 哥布林"),
        ("/ri100 哥布林", "/ri 100 哥布林", "100 哥布林"),
        (
            "/ri18 哥布林1，+2 哥布林2",
            "/ri 18 哥布林1，+2 哥布林2",
            "18 哥布林1，+2 哥布林2",
        ),
        ("/ri+5 哥布林1", "/ri +5 哥布林1", "+5 哥布林1"),
        (
            "/ri+5 哥布林1，+2 哥布林2",
            "/ri +5 哥布林1，+2 哥布林2",
            "+5 哥布林1，+2 哥布林2",
        ),
        ("/ri-1 哥布林", "/ri -1 哥布林", "-1 哥布林"),
        ("/ri优势+4 哥布林", "/ri 优势+4 哥布林", "优势+4 哥布林"),
        ("/ri劣势-1 哥布林", "/ri 劣势-1 哥布林", "劣势-1 哥布林"),
    ] {
        let Some(InitiativeCommand::Record(compact_input)) = parse_command(compact) else {
            panic!("{compact} should parse as a compact initiative record");
        };
        let Some(InitiativeCommand::Record(spaced_input)) = parse_command(spaced) else {
            panic!("{spaced} should parse as an initiative record");
        };
        assert_eq!(compact_input, expected, "{compact}");
        assert_eq!(spaced_input, expected, "{spaced}");
    }
}

#[test]
fn compact_record_suffix_rejects_unknown_ri_commands() {
    for input in ["/rich", "/right", "/ring", "/rid20", "/riabc"] {
        assert!(parse_command(input).is_none(), "{input}");
    }
}

#[test]
fn compact_fixed_records_skip_roller_and_batch_reuses_record_parser() {
    let service = InitiativeService::default();
    let reply = service
        .execute_with_roller(
            "fixed",
            &parse_command("/ri18 哥布林").unwrap(),
            None,
            &mut |_| panic!("固定先攻不应调用 Roller"),
        )
        .unwrap();
    assert!(reply.contains("哥布林：18 = 18"), "{reply}");

    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "fixed-batch",
            &parse_command("/ri18 哥布林1，+2 哥布林2").unwrap(),
            None,
            &mut |sides| {
                rolled_sides.push(sides);
                3
            },
        )
        .unwrap();
    assert_eq!(rolled_sides, vec![20]);
    assert!(reply.contains("哥布林1：18 = 18"), "{reply}");
    assert!(reply.contains("哥布林2：3 + 2 = 5"), "{reply}");
}

#[test]
fn compact_modifier_rolls_d20_and_keeps_batch_entries_in_one_table() {
    let service = InitiativeService::default();
    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "compact",
            &parse_command("/ri+5 哥布林1，+2 哥布林2").unwrap(),
            None,
            &mut |sides| {
                rolled_sides.push(sides);
                if rolled_sides.len() == 1 { 10 } else { 3 }
            },
        )
        .unwrap();

    assert_eq!(rolled_sides, vec![20, 20]);
    assert!(reply.contains("哥布林1：10 + 5 = 15"), "{reply}");
    assert!(reply.contains("哥布林2：3 + 2 = 5"), "{reply}");
    assert!(reply.contains("1. 哥布林1：15"), "{reply}");
    assert!(reply.contains("2. 哥布林2：5"), "{reply}");

    let service = InitiativeService::default();
    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "negative",
            &parse_command("/ri-1 哥布林").unwrap(),
            None,
            &mut |sides| {
                rolled_sides.push(sides);
                10
            },
        )
        .unwrap();

    assert_eq!(rolled_sides, vec![20]);
    assert!(reply.contains("哥布林：10 - 1 = 9"), "{reply}");

    let service = InitiativeService::default();
    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "advantage",
            &parse_command("/ri优势+4 哥布林").unwrap(),
            None,
            &mut |sides| {
                rolled_sides.push(sides);
                if rolled_sides.len() == 1 { 10 } else { 15 }
            },
        )
        .unwrap();

    assert_eq!(rolled_sides, vec![20, 20]);
    assert!(reply.contains("哥布林："), "{reply}");
    assert!(reply.contains("= 19"), "{reply}");
}

#[test]
fn clear_aliases_reset_table_actor_and_round_equally() {
    let service = InitiativeService::default();
    let empty = run(&service, "fresh", "/init");
    for input in ["/initclr", "/init clr", "/init clear"] {
        run(&service, "a", "/ri 10 A, 5 B");
        for _ in 0..3 {
            run(&service, "a", "/init end");
        }
        assert!(run(&service, "a", "/init").contains("第 2 轮 · 当前：B"));
        assert_eq!(run(&service, "a", input), empty);
        assert_eq!(run(&service, "a", "/init"), empty);
        assert!(
            service
                .execute("a", &InitiativeCommand::End, None)
                .contains("为空")
        );
        assert!(run(&service, "a", "/ri 1 C").contains("第 1 轮 · 当前：C"));
        // 清空后尚未开战，补录更高先攻者应重新选择表首。
        assert!(run(&service, "a", "/ri 20 D").contains("第 1 轮 · 当前：D"));
        assert!(run(&service, "a", "/init end").contains("第 1 轮 · 当前：C"));
        run(&service, "a", "/init clr");
    }
}

fn run(service: &InitiativeService, scope: &str, input: &str) -> String {
    service
        .execute_with_roller(
            scope,
            &parse_command(input).unwrap(),
            Some("玩家"),
            &mut |_| 5,
        )
        .unwrap()
}
#[test]
fn records_all_common_forms_and_scopes_without_character() {
    let service = InitiativeService::default();
    for input in [
        "/ri 12 张三",
        "/ri +2 李四",
        "/ri =d10+3 王五",
        "/ri 张三, +2 李四, =d10+3 王五",
        "/ri 优势 张三, 劣势-1 李四",
    ] {
        run(&service, "a", input);
    }
    let list = run(&service, "a", "/init");
    assert!(list.contains("王五：8"));
    assert!(list.contains("李四：4"));
    assert!(run(&service, "b", "/init").contains("为空"));
    assert!(run(&service, "a", "/init set 哥布林 d20+2").contains("哥布林：7"));
    assert!(!run(&service, "a", "/init del 哥布林").contains("哥布林"));
}
#[test]
fn stable_order_turns_deletion_and_reset() {
    let service = InitiativeService::default();
    let list = run(&service, "a", "/ri 10 B, 10 A, 5 C");
    assert!(list.contains("当前：A"));
    assert!(run(&service, "a", "/init end").contains("当前：B"));
    assert!(run(&service, "a", "/init del B").contains("当前：C"));
    let list = run(&service, "a", "/init end");
    assert!(list.contains("第 2 轮 · 当前：A"));
    assert!(run(&service, "a", "/init set X 99").contains("当前：A"));
    assert!(run(&service, "a", "/init set A 1").contains("当前：A"));
    let list = run(&service, "a", "/init del A");
    assert!(list.contains("第 3 轮 · 当前：X"));
    assert!(run(&service, "a", "/init del X C").contains("为空"));
    assert!(run(&service, "a", "/ri 5 A").contains("第 1 轮"));
    assert!(run(&service, "a", "/init clr").contains("重置"));
}
#[test]
fn errors_are_atomic_and_rng_is_injected() {
    let service = InitiativeService::default();
    let before = run(&service, "a", "/ri 10 A");
    for input in [
        "/ri +2 B, =d0 C",
        "/ri 2d20 B",
        "/ri =2#d20 B",
        "/ri =1/0 B",
        "/init del A Z",
        "/init set B d1000",
    ] {
        assert!(
            service
                .execute_with_roller("a", &parse_command(input).unwrap(), None, &mut |_| 1)
                .is_err(),
            "{input}"
        );
        assert_eq!(
            run(&service, "a", "/init"),
            before.lines().skip(1).collect::<Vec<_>>().join("\n")
        );
    }
    assert!(
        service
            .execute_with_roller("a", &parse_command("/ri +2 B").unwrap(), None, &mut |_| 0)
            .is_err()
    );
    assert!(run(&service, "a", "/ri").contains("玩家：5"));
}

#[test]
fn limits_aliases_and_single_actor_wrap_are_explicit() {
    let service = InitiativeService::default();
    run(&service, "a", "/ri 1 A");
    assert!(run(&service, "a", "/init ed").contains("第 2 轮 · 当前：A"));
    assert!(run(&service, "a", "/init rm A").contains("为空"));
    for input in [
        format!("/ri {}", "A".repeat(65)),
        format!("/ri {}", vec!["+1 A"; 21].join(",")),
        "/ri =100d6 A, =100d6 B, =d6 C".to_owned(),
    ] {
        assert!(
            service
                .execute_with_roller("a", &parse_command(&input).unwrap(), None, &mut |_| 1)
                .is_err()
        );
    }
    assert!(run(&service, "a", "/init list").contains("为空"));
    assert!(run(&service, "a", "/init clear").contains("重置"));
}

#[test]
fn cloned_services_share_atomic_updates() {
    let service = InitiativeService::default();
    std::thread::scope(|scope| {
        for index in 0..12 {
            let clone = service.clone();
            scope.spawn(move || {
                run(&clone, "shared", &format!("/ri 10 单位{index}"));
            });
        }
    });
    assert_eq!(run(&service, "shared", "/init").lines().count(), 13);
}

#[test]
fn bare_dice_uses_default_name_and_explicit_name_prefix() {
    for (input, expected_expression) in [("d20", "1d20"), ("d20+4", "1d20+4"), ("d8+2", "1d8+2")] {
        let entries = parse_entries(input, Some("脸脸"), false).unwrap();
        assert_eq!(entries[0].name, "脸脸", "{input}");
        assert_eq!(
            entries[0].expression.to_string(),
            expected_expression,
            "{input}"
        );
        assert!(entries[0].binds_actor, "{input}");
    }

    let entries = parse_entries("d20+4 哥布林", Some("脸脸"), false).unwrap();
    assert_eq!(entries[0].name, "哥布林");
    assert_eq!(entries[0].expression.to_string(), "1d20+4");
    assert!(!entries[0].binds_actor);
    assert!(parse_entries("d20+4", None, false).is_err());
}

fn actor(user_id: &str, display_name: &str) -> qq_maid_common::identity_context::MentionIdentity {
    use qq_maid_common::identity_context::{
        IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext,
    };

    MentionIdentity {
        raw_text: None,
        target: MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(display_name.to_owned()),
            source: IdentitySource::Event,
            ..MessageActorContext::default()
        },
        is_self: false,
        confidence: MentionConfidence::Event,
    }
}

fn run_actor(
    service: &InitiativeService,
    scope: &str,
    input: &str,
    actor: &qq_maid_common::identity_context::MentionIdentity,
) -> super::ops::InitiativeReply {
    service.execute_for_actor(scope, &parse_command(input).unwrap(), Some(actor))
}

#[test]
fn end_mentions_current_default_named_player() {
    let service = InitiativeService::default();
    let player_a = actor("user-a", "玩家A");
    let player_b = actor("user-b", "玩家B");
    run_actor(&service, "mentions", "/ri18", &player_a);
    run_actor(&service, "mentions", "/ri12", &player_b);

    let reply = run_actor(&service, "mentions", "/initend", &player_a);
    assert!(reply.text.contains("当前：玩家B"), "{}", reply.text);
    assert_eq!(reply.mentions.len(), 1);
    assert_eq!(reply.mentions[0].target.user_id.as_deref(), Some("user-b"));
    assert_eq!(
        reply.mentions[0].target.display_name.as_deref(),
        Some("玩家B")
    );
}

#[test]
fn explicit_names_never_bind_the_command_actor() {
    let service = InitiativeService::default();
    let player = actor("user-a", "玩家A");
    run_actor(&service, "explicit", "/ri18", &player);
    run_actor(&service, "explicit", "/ri17 哥布林", &player);

    let reply = run_actor(&service, "explicit", "/initend", &player);
    assert!(reply.text.contains("当前：哥布林"), "{}", reply.text);
    assert!(reply.mentions.is_empty());
}

#[test]
fn updating_or_explicitly_replacing_an_entry_refreshes_actor_binding() {
    let service = InitiativeService::default();
    let player = actor("user-a", "玩家A");
    run_actor(&service, "update", "/ri10", &player);
    run_actor(&service, "update", "/ri20", &player);
    let reply = run_actor(&service, "update", "/inited", &player);
    assert_eq!(reply.mentions[0].target.user_id.as_deref(), Some("user-a"));

    run_actor(&service, "replace", "/ri18", &player);
    run_actor(
        &service,
        "replace",
        "/ri18 玩家A",
        &actor("npc-owner", "其他人"),
    );
    let reply = run_actor(&service, "replace", "/initend", &player);
    assert!(reply.mentions.is_empty());
}

#[test]
fn deleting_and_clearing_entries_do_not_leave_stale_mentions() {
    let service = InitiativeService::default();
    let player_a = actor("user-a", "玩家A");
    let player_b = actor("user-b", "玩家B");
    run_actor(&service, "lifecycle", "/ri18", &player_a);
    run_actor(&service, "lifecycle", "/ri12", &player_b);
    let reply = run_actor(&service, "lifecycle", "/initend", &player_a);
    assert_eq!(reply.mentions[0].target.user_id.as_deref(), Some("user-b"));

    run_actor(&service, "lifecycle", "/initdel 玩家B", &player_a);
    let reply = run_actor(&service, "lifecycle", "/initend", &player_a);
    assert_eq!(reply.mentions[0].target.user_id.as_deref(), Some("user-a"));

    let reply = run_actor(&service, "lifecycle", "/initclear", &player_a);
    assert!(reply.mentions.is_empty());
    run_actor(&service, "lifecycle", "/ri5", &player_b);
    let reply = run_actor(&service, "lifecycle", "/initend", &player_a);
    assert_eq!(reply.mentions[0].target.user_id.as_deref(), Some("user-b"));
}

#[test]
fn bare_dice_executes_the_parsed_expression_with_deterministic_roller() {
    let service = InitiativeService::default();
    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "default-name",
            &parse_command("/ri d20+4").unwrap(),
            Some("脸脸"),
            &mut |sides| {
                rolled_sides.push(sides);
                10
            },
        )
        .unwrap();
    assert_eq!(rolled_sides, vec![20]);
    assert!(reply.contains("脸脸：10 + 4 = 14"), "{reply}");
    assert!(!reply.contains("d20+4："), "{reply}");

    let mut rolled_sides = Vec::new();
    let reply = service
        .execute_with_roller(
            "explicit-name",
            &parse_command("/ri d20+4 哥布林").unwrap(),
            Some("脸脸"),
            &mut |sides| {
                rolled_sides.push(sides);
                10
            },
        )
        .unwrap();
    assert_eq!(rolled_sides, vec![20]);
    assert!(reply.contains("哥布林：10 + 4 = 14"), "{reply}");
    assert!(!reply.contains("脸脸："), "{reply}");
}
