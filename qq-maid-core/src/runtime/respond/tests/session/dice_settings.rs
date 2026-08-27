use super::super::support::*;

#[tokio::test]
async fn sealdice_set_commands_switch_the_default_die_for_repeated_bare_d() {
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
