use super::command::parse_entries;
use super::*;

#[test]
fn compact_clear_parses_like_spaced_clear() {
    for input in ["/initclr", "/init clr", "/init clear", " /INITCLR "] {
        assert!(
            matches!(parse_command(input), Some(InitiativeCommand::Clear)),
            "{input}"
        );
    }
    for input in ["/initclr extra", "/init clr extra", "/init clear extra"] {
        assert!(
            matches!(parse_command(input), Some(InitiativeCommand::Invalid)),
            "{input}"
        );
    }
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
        assert_eq!(entries[0].0, "脸脸", "{input}");
        assert_eq!(entries[0].1.to_string(), expected_expression, "{input}");
    }

    let entries = parse_entries("d20+4 哥布林", Some("脸脸"), false).unwrap();
    assert_eq!(entries[0].0, "哥布林");
    assert_eq!(entries[0].1.to_string(), "1d20+4");
    assert!(parse_entries("d20+4", None, false).is_err());
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
