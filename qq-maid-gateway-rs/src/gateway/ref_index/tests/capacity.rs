use super::*;

#[test]
fn scope_capacity_evicts_passive_observations_before_complete_entries() {
    let mut store = RefIndex::new(Duration::from_secs(60), 100, 3);
    let conversation = ConversationTarget::Group {
        target_id: "group-1".to_owned(),
    };
    store.insert_bot_outbound(
        crate::gateway::platform::Platform::QqOfficial,
        Some("app"),
        &conversation,
        Some("REFIDX_bot_protected".to_owned()),
        "机器人完整回复",
        None,
    );
    store.insert_inbound(&group_inbound(
        "gm-complete",
        Some("REFIDX_complete_protected"),
        "完整入站正文",
    ));
    for index in 0..20 {
        store.insert_passive_observation(&group_inbound(
            &format!("gm-passive-{index}"),
            Some(&format!("REFIDX_passive_{index}")),
            &format!("普通消息 {index}"),
        ));
    }

    assert_eq!(store.entries.len(), 3);
    assert!(quoted_group_lookup(&mut store, "REFIDX_bot_protected").lookup_found);
    assert!(quoted_group_lookup(&mut store, "REFIDX_complete_protected").lookup_found);
    assert!(!quoted_group_lookup(&mut store, "REFIDX_passive_0").lookup_found);
    assert!(quoted_group_lookup(&mut store, "REFIDX_passive_19").lookup_found);
    assert_eq!(store.scope_evictions, 19);
}

#[test]
fn global_capacity_evicts_passive_observations_before_complete_entries() {
    let mut store = RefIndex::new(Duration::from_secs(60), 3, 100);
    let protected_group = ConversationTarget::Group {
        target_id: "protected-group".to_owned(),
    };
    store.insert_bot_outbound(
        crate::gateway::platform::Platform::QqOfficial,
        Some("app"),
        &protected_group,
        Some("REFIDX_global_bot".to_owned()),
        "机器人完整回复",
        None,
    );
    let mut complete = group_inbound(
        "gm-global-complete",
        Some("REFIDX_global_complete"),
        "完整入站正文",
    );
    complete.conversation = ConversationTarget::Group {
        target_id: "complete-group".to_owned(),
    };
    store.insert_inbound(&complete);
    for index in 0..20 {
        let mut passive = group_inbound(
            &format!("gm-global-passive-{index}"),
            Some(&format!("REFIDX_global_passive_{index}")),
            &format!("普通消息 {index}"),
        );
        passive.conversation = ConversationTarget::Group {
            target_id: format!("passive-group-{index}"),
        };
        store.insert_passive_observation(&passive);
    }

    assert_eq!(store.entries.len(), 3);
    assert!(
        store
            .entries
            .keys()
            .any(|key| key.ref_id == "REFIDX_global_bot")
    );
    assert!(
        store
            .entries
            .keys()
            .any(|key| key.ref_id == "REFIDX_global_complete")
    );
    assert!(
        !store
            .entries
            .keys()
            .any(|key| key.ref_id == "REFIDX_global_passive_0")
    );
    assert!(
        store
            .entries
            .keys()
            .any(|key| key.ref_id == "REFIDX_global_passive_19")
    );
    assert_eq!(store.capacity_evictions, 19);
    assert_eq!(store.scope_evictions, 0);
}
