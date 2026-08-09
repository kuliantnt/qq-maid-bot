use super::*;

#[test]
fn dream_trigger_accepts_each_production_path_at_threshold() {
    let now = 2_000_000_000_i64;

    let (_, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        "session-path",
        5,
        &repeated_timestamps(30, &["2026-07-20T10:00:00+08:00"]),
    );
    let claim = store
        .claim_dream(
            &private_storage_context("session-path"),
            production_dream_limits(),
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.messages.len(), 30);

    let (_, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        "active-date-path",
        1,
        &repeated_timestamps(
            50,
            &[
                "2026-07-18T10:00:00+08:00",
                "2026-07-19T10:00:00+08:00",
                "2026-07-20T10:00:00+08:00",
            ],
        ),
    );
    let claim = store
        .claim_dream(
            &private_storage_context("active-date-path"),
            production_dream_limits(),
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.messages.len(), 50);

    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        "interval-path",
        1,
        &repeated_timestamps(60, &["2026-07-20T10:00:00+08:00"]),
    );
    seed_successful_dream_state(&database, "interval-path", 0, now - 7 * 24 * 60 * 60);
    let claim = store
        .claim_dream(
            &private_storage_context("interval-path"),
            production_dream_limits(),
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.messages.len(), 60);
}

#[test]
fn dream_trigger_rejects_each_threshold_one_below_without_advancing_checkpoint() {
    let now = 2_000_000_000_i64;
    let cases = [
        (
            "session-count-minus-one",
            4,
            repeated_timestamps(30, &["2026-07-20T10:00:00+08:00"]),
            None,
        ),
        (
            "session-message-minus-one",
            5,
            repeated_timestamps(29, &["2026-07-20T10:00:00+08:00"]),
            None,
        ),
        (
            "active-date-minus-one",
            1,
            repeated_timestamps(
                50,
                &["2026-07-19T10:00:00+08:00", "2026-07-20T10:00:00+08:00"],
            ),
            None,
        ),
        (
            "active-date-message-minus-one",
            1,
            repeated_timestamps(
                49,
                &[
                    "2026-07-18T10:00:00+08:00",
                    "2026-07-19T10:00:00+08:00",
                    "2026-07-20T10:00:00+08:00",
                ],
            ),
            None,
        ),
        (
            "interval-minus-one",
            1,
            repeated_timestamps(60, &["2026-07-20T10:00:00+08:00"]),
            Some(7 * 24 * 60 * 60 - 1),
        ),
        (
            "interval-message-minus-one",
            1,
            repeated_timestamps(59, &["2026-07-20T10:00:00+08:00"]),
            Some(7 * 24 * 60 * 60),
        ),
    ];

    for (user, session_count, timestamps, checkpoint_age) in cases {
        let (database, store, sessions) = test_stores_with_database();
        add_private_messages_with_timestamps(&sessions, user, session_count, &timestamps);
        let expected_checkpoint = checkpoint_age.map(|age| {
            let last_run_at_epoch = now - age;
            seed_successful_dream_state(&database, user, 0, last_run_at_epoch);
            (0, last_run_at_epoch, None)
        });

        assert!(
            store
                .claim_dream(
                    &private_storage_context(user),
                    production_dream_limits(),
                    now,
                )
                .unwrap()
                .is_none(),
            "{user} should not trigger"
        );
        assert_eq!(dream_checkpoint(&database, user), expected_checkpoint);
    }
}

#[test]
fn truncated_state_bypasses_production_trigger_thresholds() {
    let now = 2_000_000_000_i64;
    let user = "truncated-below-threshold";
    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        5,
        &repeated_timestamps(29, &["2026-07-20T10:00:00+08:00"]),
    );
    let last_run_at_epoch = now - 24 * 60 * 60;
    seed_successful_dream_state(&database, user, 0, last_run_at_epoch);
    database
        .connection()
        .unwrap()
        .execute(
            "UPDATE memory_dream_state SET truncated = 1
             WHERE scope_type = 'personal' AND scope_id = ?1
               AND memory_kind = 'personal' AND subject_key = ''",
            params![user],
        )
        .unwrap();

    let claim = store
        .claim_dream(
            &private_storage_context(user),
            production_dream_limits(),
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.messages.len(), 29);
    assert_eq!(
        dream_state(&database, user),
        Some((0, last_run_at_epoch, true, Some(claim.token.clone())))
    );
    store
        .release_dream(&private_storage_context(user), &claim.token)
        .unwrap();
}

#[test]
fn base_cooldown_still_blocks_otherwise_matching_trigger() {
    let now = 2_000_000_000_i64;
    let user = "cooldown";
    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        5,
        &repeated_timestamps(30, &["2026-07-20T10:00:00+08:00"]),
    );
    let last_run_at_epoch = now - 60 * 60;
    seed_successful_dream_state(&database, user, 0, last_run_at_epoch);
    let mut limits = production_dream_limits();
    limits.min_interval_seconds = 24 * 60 * 60;

    assert!(
        store
            .claim_dream(&private_storage_context(user), limits, now)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        dream_checkpoint(&database, user),
        Some((0, last_run_at_epoch, None))
    );
}

#[test]
fn multiple_trigger_paths_still_allow_only_one_concurrent_claim() {
    let now = 2_000_000_000_i64;
    let user = "all-paths";
    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        5,
        &repeated_timestamps(
            60,
            &[
                "2026-07-18T10:00:00+08:00",
                "2026-07-19T10:00:00+08:00",
                "2026-07-20T10:00:00+08:00",
            ],
        ),
    );
    let last_run_at_epoch = now - 7 * 24 * 60 * 60;
    seed_successful_dream_state(&database, user, 0, last_run_at_epoch);

    let context = private_storage_context(user);
    let first = store
        .claim_dream(&context, production_dream_limits(), now)
        .unwrap()
        .unwrap();
    assert!(
        store
            .claim_dream(&context, production_dream_limits(), now)
            .unwrap()
            .is_none()
    );
    let state = dream_checkpoint(&database, user).unwrap();
    assert_eq!((state.0, state.1), (0, last_run_at_epoch));
    assert_eq!(state.2.as_deref(), Some(first.token.as_str()));

    store.release_dream(&context, &first.token).unwrap();
    assert_eq!(
        dream_checkpoint(&database, user),
        Some((0, last_run_at_epoch, None))
    );
    assert!(
        store
            .claim_dream(&context, production_dream_limits(), now)
            .unwrap()
            .is_some()
    );
}

#[test]
fn seventy_two_messages_across_four_sessions_trigger_after_seven_days() {
    let now = 2_000_000_000_i64;
    let user = "seventy-two-regression";
    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        4,
        &repeated_timestamps(72, &["2026-07-20T10:00:00+08:00"]),
    );
    seed_successful_dream_state(&database, user, 0, now - 8 * 24 * 60 * 60);

    let claim = store
        .claim_dream(
            &private_storage_context(user),
            production_dream_limits(),
            now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(claim.messages.len(), 72);
}

#[test]
fn active_dates_use_shanghai_boundaries_instead_of_utc_dates() {
    let now = 2_000_000_000_i64;
    let user = "timezone-boundary";
    let (_, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        1,
        &repeated_timestamps(
            50,
            &[
                "2026-07-20T15:30:00Z",
                "2026-07-20T16:30:00Z",
                "2026-07-21T16:30:00Z",
            ],
        ),
    );

    assert!(
        store
            .claim_dream(
                &private_storage_context(user),
                production_dream_limits(),
                now,
            )
            .unwrap()
            .is_some()
    );
}
