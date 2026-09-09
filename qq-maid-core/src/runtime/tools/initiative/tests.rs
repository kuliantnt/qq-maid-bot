use super::*;
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
