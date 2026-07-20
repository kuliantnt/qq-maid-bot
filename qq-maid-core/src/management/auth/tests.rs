use super::*;
use crate::storage::APP_MIGRATIONS;

fn auth(name: &str) -> (AdminAuth, PathBuf) {
    let (database, directory) = SqliteDatabase::open_temp_directory(name, APP_MIGRATIONS).unwrap();
    let token_file = directory.join("config/secrets/bootstrap.token");
    (AdminAuth::open(database, token_file).unwrap(), directory)
}

fn bootstrap_token(path: &Path) -> String {
    bootstrap_token_text(path)
        .splitn(3, ':')
        .nth(2)
        .unwrap()
        .to_owned()
}

fn bootstrap_token_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().trim().to_owned()
}

fn bootstrap_prefix(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap()
        .trim()
        .split(':')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn bootstrap_token_input_normalizes_only_supported_complete_formats() {
    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAA";

    assert_eq!(
        normalize_bootstrap_token_input(&format!(" \n{TOKEN}\n ")).unwrap(),
        BootstrapTokenInput {
            purpose: None,
            token: TOKEN,
        }
    );
    assert_eq!(
        normalize_bootstrap_token_input(&format!(" \n{BOOTSTRAP_PREFIX}:123:{TOKEN}\n ")).unwrap(),
        BootstrapTokenInput {
            purpose: Some(BootstrapTokenPurpose::Initialize),
            token: TOKEN,
        }
    );
    assert_eq!(
        normalize_bootstrap_token_input(&format!("{PASSWORD_RESET_PREFIX}:456:{TOKEN}")).unwrap(),
        BootstrapTokenInput {
            purpose: Some(BootstrapTokenPurpose::PasswordReset),
            token: TOKEN,
        }
    );

    for invalid in [
        format!("unknown-prefix:123:{TOKEN}"),
        format!("{BOOTSTRAP_PREFIX}:123"),
        "random:text".to_owned(),
        format!("{BOOTSTRAP_PREFIX}:123:{TOKEN}:extra"),
        format!("{BOOTSTRAP_PREFIX}:not-a-time:{TOKEN}"),
    ] {
        assert_eq!(
            normalize_bootstrap_token_input(&invalid)
                .unwrap_err()
                .code(),
            "invalid_bootstrap_token_format"
        );
    }
}

#[test]
fn complete_token_input_preserves_purpose_and_expiry_checks() {
    let (admin_auth, directory) = auth("qq-maid-admin-complete-token-input");
    let path = directory.join("config/secrets/bootstrap.token");
    let bootstrap = bootstrap_token_text(&path);
    let wrong_reset = bootstrap.replacen(BOOTSTRAP_PREFIX, PASSWORD_RESET_PREFIX, 1);
    let setup = admin_auth.issue_preauth_for("operator").unwrap();
    assert_eq!(
        admin_auth
            .initialize_for(
                &setup.cookie_value,
                &setup.session.csrf_token,
                &wrong_reset,
                "admin",
                "123456",
                "operator",
            )
            .unwrap_err()
            .code(),
        "invalid_bootstrap_token"
    );
    admin_auth
        .initialize_for(
            &setup.cookie_value,
            &setup.session.csrf_token,
            &format!(" \n{bootstrap}\n "),
            "admin",
            "123456",
            "operator",
        )
        .unwrap();

    let reset = admin_auth.issue_preauth_for("reset-operator").unwrap();
    admin_auth
        .request_password_reset_for(
            &reset.cookie_value,
            &reset.session.csrf_token,
            "reset-operator",
        )
        .unwrap();
    let password_reset = bootstrap_token_text(&path);
    let wrong_bootstrap = password_reset.replacen(PASSWORD_RESET_PREFIX, BOOTSTRAP_PREFIX, 1);
    assert_eq!(
        admin_auth
            .reset_password_for(
                &reset.cookie_value,
                &reset.session.csrf_token,
                &wrong_bootstrap,
                "654321",
                "reset-operator",
            )
            .unwrap_err()
            .code(),
        "invalid_bootstrap_token"
    );
    admin_auth
        .reset_password_for(
            &reset.cookie_value,
            &reset.session.csrf_token,
            &password_reset,
            "654321",
            "reset-operator",
        )
        .unwrap();

    let (expired_auth, expired_directory) = auth("qq-maid-admin-expired-complete-token");
    let expired_path = expired_directory.join("config/secrets/bootstrap.token");
    let token = bootstrap_token(&expired_path);
    let expired = format!(
        "{BOOTSTRAP_PREFIX}:{}:{token}\n",
        unix_seconds() - BOOTSTRAP_TTL.as_secs() as i64 - 1
    );
    fs::write(&expired_path, &expired).unwrap();
    let expired_setup = expired_auth.issue_preauth_for("expired-operator").unwrap();
    assert_eq!(
        expired_auth
            .initialize_for(
                &expired_setup.cookie_value,
                &expired_setup.session.csrf_token,
                &expired,
                "admin",
                "123456",
                "expired-operator",
            )
            .unwrap_err()
            .code(),
        "invalid_bootstrap_token"
    );
}

fn audit_count(auth: &AdminAuth, event_type: &str, outcome: &str) -> i64 {
    auth.database
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM console_audit_events
             WHERE event_type = ?1 AND outcome = ?2",
            rusqlite::params![event_type, outcome],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn bootstrap_is_single_use_and_password_is_not_stored_in_plaintext() {
    let (auth, directory) = auth("qq-maid-admin-bootstrap");
    let path = directory.join("config/secrets/bootstrap.token");
    let token = bootstrap_token(&path);
    assert_eq!(token.len(), 22);
    let preauth = auth.issue_preauth().unwrap();
    let issued = auth
        .initialize(
            &preauth.cookie_value,
            &preauth.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();

    assert!(!path.exists());
    assert_eq!(issued.session.username, "admin");
    assert!(auth.bootstrap_status().unwrap().initialized);
    let connection = auth.database.connection().unwrap();
    let stored: String = connection
        .query_row("SELECT password_hash FROM console_admins", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(stored.starts_with("$argon2"));
    assert!(!stored.contains("correct horse"));

    let replay = auth.issue_preauth().unwrap();
    let error = auth
        .initialize(
            &replay.cookie_value,
            &replay.session.csrf_token,
            &token,
            "other",
            "another secure password",
        )
        .unwrap_err();
    assert_eq!(error.code(), "already_initialized");
}

#[test]
fn password_reset_uses_local_single_use_token_and_revokes_old_admin_sessions() {
    let (auth, directory) = auth("qq-maid-admin-password-reset");
    let path = directory.join("config/secrets/bootstrap.token");
    let initial_token = bootstrap_token(&path);
    let setup = auth.issue_preauth_for("operator").unwrap();
    let old_admin = auth
        .initialize_for(
            &setup.cookie_value,
            &setup.session.csrf_token,
            &initial_token,
            "admin",
            "old-password",
            "operator",
        )
        .unwrap();

    let reset_preauth = auth.issue_preauth_for("reset-operator").unwrap();
    let status = auth
        .request_password_reset_for(
            &reset_preauth.cookie_value,
            &reset_preauth.session.csrf_token,
            "reset-operator",
        )
        .unwrap();
    assert!(status.password_reset_pending);
    assert_eq!(status.token_file, "config/secrets/bootstrap.token");
    assert_eq!(bootstrap_prefix(&path), PASSWORD_RESET_PREFIX);
    let reset_token = bootstrap_token(&path);
    assert_eq!(reset_token.len(), 22);

    // 重复请求复用仍有效的文件令牌，不能让匿名请求通过轮换造成运维锁定。
    let repeated = auth
        .request_password_reset_for(
            &reset_preauth.cookie_value,
            &reset_preauth.session.csrf_token,
            "reset-operator",
        )
        .unwrap();
    assert!(repeated.password_reset_pending);
    assert_eq!(bootstrap_token(&path), reset_token);

    // 服务重启仍保留尚有效的密码重置令牌。
    let reopened = AdminAuth::open(auth.database.clone(), path.clone()).unwrap();
    assert!(reopened.bootstrap_status().unwrap().password_reset_pending);

    let new_admin = auth
        .reset_password_for(
            &reset_preauth.cookie_value,
            &reset_preauth.session.csrf_token,
            &reset_token,
            "654321",
            "reset-operator",
        )
        .unwrap();
    assert!(!path.exists());
    assert!(
        auth.authorize_admin(&new_admin.cookie_value, Some(&new_admin.session.csrf_token),)
            .is_ok()
    );
    assert_eq!(
        auth.authorize_admin(&old_admin.cookie_value, None)
            .unwrap_err()
            .code(),
        "unauthenticated"
    );

    let old_login = auth.issue_preauth_for("old-password-login").unwrap();
    assert_eq!(
        auth.login_for(
            &old_login.cookie_value,
            &old_login.session.csrf_token,
            "admin",
            "old-password",
            "old-password-login",
        )
        .unwrap_err()
        .code(),
        "invalid_credentials"
    );
    let new_login = auth.issue_preauth_for("new-password-login").unwrap();
    assert!(
        auth.login_for(
            &new_login.cookie_value,
            &new_login.session.csrf_token,
            "admin",
            "654321",
            "new-password-login",
        )
        .is_ok()
    );
}

#[test]
fn administrator_password_accepts_six_characters_but_not_five() {
    assert!(validate_password("123456").is_ok());
    assert_eq!(
        validate_password("12345").unwrap_err().code(),
        "validation_error"
    );
}

#[test]
fn csrf_is_stable_for_multiple_tabs_and_invalid_value_is_rejected() {
    let (auth, directory) = auth("qq-maid-admin-csrf");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let preauth = auth.issue_preauth().unwrap();
    let issued = auth
        .initialize(
            &preauth.cookie_value,
            &preauth.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();
    let refreshed = auth.refresh_admin_session(&issued.cookie_value).unwrap();
    let second_tab = auth.refresh_admin_session(&issued.cookie_value).unwrap();
    assert_eq!(refreshed.csrf_token, issued.session.csrf_token);
    assert_eq!(second_tab.csrf_token, issued.session.csrf_token);
    assert_eq!(
        auth.authorize_admin(&issued.cookie_value, Some("wrong"))
            .unwrap_err()
            .code(),
        "csrf_failed"
    );
    assert!(
        auth.authorize_admin(&issued.cookie_value, Some(&refreshed.csrf_token))
            .is_ok()
    );
    assert!(
        auth.authorize_admin(&issued.cookie_value, Some(&second_tab.csrf_token))
            .is_ok()
    );
}

#[test]
fn login_logout_replay_and_session_expiry_are_enforced() {
    let (auth, directory) = auth("qq-maid-admin-session-lifecycle");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let preauth = auth.issue_preauth().unwrap();
    let initialized = auth
        .initialize(
            &preauth.cookie_value,
            &preauth.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();
    let refreshed = auth
        .refresh_admin_session(&initialized.cookie_value)
        .unwrap();
    auth.logout(&initialized.cookie_value, &refreshed.csrf_token)
        .unwrap();
    assert_eq!(
        auth.authorize_admin(&initialized.cookie_value, None)
            .unwrap_err()
            .code(),
        "unauthenticated"
    );

    let login_preauth = auth.issue_preauth().unwrap();
    let logged_in = auth
        .login(
            &login_preauth.cookie_value,
            &login_preauth.session.csrf_token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();
    assert!(
        auth.authorize_admin(&login_preauth.cookie_value, None)
            .is_err()
    );
    let hash = token_hash(&logged_in.cookie_value);
    auth.sessions
        .lock()
        .unwrap()
        .get_mut(&hash)
        .unwrap()
        .last_seen_at = unix_seconds() - SESSION_IDLE_TTL.as_secs() as i64 - 1;
    assert_eq!(
        auth.authorize_admin(&logged_in.cookie_value, None)
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
}

#[test]
fn management_actions_have_an_independent_rate_limit() {
    let (auth, _directory) = auth("qq-maid-admin-management-limit");
    for _ in 0..MAX_MANAGEMENT_ACTIONS_PER_MINUTE {
        auth.check_management_rate_limit(1).unwrap();
    }
    assert_eq!(
        auth.check_management_rate_limit(1).unwrap_err().code(),
        "rate_limited"
    );
    assert!(auth.check_management_rate_limit(2).is_ok());
    assert!(auth.issue_preauth().is_ok());
}

#[test]
fn anonymous_bootstrap_limit_does_not_block_another_source_login() {
    let (auth, directory) = auth("qq-maid-admin-source-limits");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let setup = auth.issue_preauth_for("operator").unwrap();
    auth.initialize_for(
        &setup.cookie_value,
        &setup.session.csrf_token,
        &token,
        "Admin",
        "correct horse battery staple",
        "operator",
    )
    .unwrap();

    for _ in 0..MAX_BOOTSTRAP_ATTEMPTS_PER_MINUTE {
        auth.issue_preauth_for("attacker").unwrap();
    }
    assert_eq!(
        auth.issue_preauth_for("attacker").unwrap_err().code(),
        "rate_limited"
    );

    let login = auth.issue_preauth_for("other-operator").unwrap();
    assert!(
        auth.login_for(
            &login.cookie_value,
            &login.session.csrf_token,
            " admin ",
            "correct horse battery staple",
            "other-operator",
        )
        .is_ok()
    );
}

#[test]
fn anonymous_preauth_capacity_preserves_admin_and_prunes_expired_sessions() {
    let (auth, directory) = auth("qq-maid-admin-preauth-capacity");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let setup = auth.issue_preauth_for("operator").unwrap();
    let admin = auth
        .initialize_for(
            &setup.cookie_value,
            &setup.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
            "operator",
        )
        .unwrap();

    assert!(
        auth.authorize_admin(&admin.cookie_value, Some(&admin.session.csrf_token),)
            .is_ok()
    );

    // 每个请求使用不同来源，覆盖来源限流之外的全局 PreAuth 容量边界。
    for index in 0..(MAX_PREAUTH_SESSIONS + 32) {
        auth.issue_preauth_for(&format!("anonymous-{index}"))
            .unwrap();
    }

    assert!(
        auth.authorize_admin(&admin.cookie_value, Some(&admin.session.csrf_token),)
            .is_ok()
    );
    let expired_hash = {
        let mut sessions = auth.sessions.lock().unwrap();
        assert_eq!(session_count(&sessions, SessionKindFilter::Admin), 1);
        assert_eq!(
            session_count(&sessions, SessionKindFilter::PreAuth),
            MAX_PREAUTH_SESSIONS
        );
        assert_eq!(sessions.len(), MAX_PREAUTH_SESSIONS + 1);
        assert!(sessions.len() <= MAX_SESSIONS);

        let hash = oldest_session(&sessions, SessionKindFilter::PreAuth).unwrap();
        sessions.get_mut(&hash).unwrap().absolute_expires_at = unix_seconds() - 1;
        hash
    };

    auth.issue_preauth_for("anonymous-after-expiry").unwrap();
    let sessions = auth.sessions.lock().unwrap();
    assert!(!sessions.contains_key(&expired_hash));
    assert_eq!(session_count(&sessions, SessionKindFilter::Admin), 1);
    assert_eq!(
        session_count(&sessions, SessionKindFilter::PreAuth),
        MAX_PREAUTH_SESSIONS
    );
    assert!(sessions.len() <= MAX_SESSIONS);
}

#[test]
fn login_capacity_keeps_preauth_until_admin_session_is_inserted() {
    let (auth, directory) = auth("qq-maid-admin-login-capacity");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let setup = auth.issue_preauth().unwrap();
    let initialized = auth
        .initialize(
            &setup.cookie_value,
            &setup.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();

    // 直接补齐有效 Admin 会话，避免让容量测试依赖 31 次 Argon2 登录校验。
    let mut existing_admins = vec![initialized];
    for _ in 1..MAX_ADMIN_SESSIONS {
        existing_admins.push(auth.issue_admin_session(1, "admin").unwrap());
    }
    assert_eq!(
        session_count(&auth.sessions.lock().unwrap(), SessionKindFilter::Admin),
        MAX_ADMIN_SESSIONS
    );

    let retry = auth.issue_preauth_for("capacity-retry").unwrap();
    let successful_logins_before = audit_count(&auth, "admin.login", "success");
    let error = auth
        .login_for(
            &retry.cookie_value,
            &retry.session.csrf_token,
            "admin",
            "correct horse battery staple",
            "capacity-retry",
        )
        .unwrap_err();
    assert_eq!(error.code(), "session_capacity_reached");
    assert_eq!(
        audit_count(&auth, "admin.login", "success"),
        successful_logins_before
    );
    assert!(
        auth.require_preauth(&retry.cookie_value, &retry.session.csrf_token)
            .is_ok()
    );
    for session in &existing_admins {
        assert!(auth.authorize_admin(&session.cookie_value, None).is_ok());
    }

    // 释放一个 Admin 后，之前因容量失败的同一个 PreAuth 必须仍可重试。
    auth.remove_session(&existing_admins[0].cookie_value)
        .unwrap();
    let logged_in = auth
        .login_for(
            &retry.cookie_value,
            &retry.session.csrf_token,
            "admin",
            "correct horse battery staple",
            "capacity-retry",
        )
        .unwrap();
    assert_eq!(
        audit_count(&auth, "admin.login", "success"),
        successful_logins_before + 1
    );
    assert_eq!(
        auth.require_preauth(&retry.cookie_value, &retry.session.csrf_token)
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
    assert!(
        auth.authorize_admin(&logged_in.cookie_value, Some(&logged_in.session.csrf_token))
            .is_ok()
    );
}

#[test]
fn login_audit_storage_failure_rolls_back_admin_and_restores_preauth() {
    let (auth, directory) = auth("qq-maid-admin-login-audit-failure");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let setup = auth.issue_preauth().unwrap();
    let initialized = auth
        .initialize(
            &setup.cookie_value,
            &setup.session.csrf_token,
            &token,
            "admin",
            "correct horse battery staple",
        )
        .unwrap();
    // 登录测试只需要数据库中的管理员；移除初始化时签发的会话以便精确检查本次登录。
    auth.remove_session(&initialized.cookie_value).unwrap();

    let login_preauth = auth.issue_preauth_for("audit-failure").unwrap();
    auth.database
        .connection()
        .unwrap()
        .execute("DROP TABLE console_audit_events", [])
        .unwrap();

    let error = auth
        .login_for(
            &login_preauth.cookie_value,
            &login_preauth.session.csrf_token,
            "admin",
            "correct horse battery staple",
            "audit-failure",
        )
        .unwrap_err();
    assert_eq!(error.code(), "admin_storage_error");

    // 审计失败不能返回 Admin，也不能消耗原 PreAuth；客户端可以用同一组 Cookie 重试。
    assert!(
        auth.require_preauth(
            &login_preauth.cookie_value,
            &login_preauth.session.csrf_token,
        )
        .is_ok()
    );
    let sessions = auth.sessions.lock().unwrap();
    assert_eq!(session_count(&sessions, SessionKindFilter::Admin), 0);
    assert_eq!(session_count(&sessions, SessionKindFilter::PreAuth), 1);
}

#[test]
fn login_error_does_not_reveal_whether_username_exists() {
    let (auth, directory) = auth("qq-maid-admin-login-error-uniform");
    let token = bootstrap_token(&directory.join("config/secrets/bootstrap.token"));
    let setup = auth.issue_preauth().unwrap();
    auth.initialize(
        &setup.cookie_value,
        &setup.session.csrf_token,
        &token,
        "admin",
        "correct horse battery staple",
    )
    .unwrap();

    let known = auth.issue_preauth_for("known-source").unwrap();
    let known_error = auth
        .login_for(
            &known.cookie_value,
            &known.session.csrf_token,
            "admin",
            "wrong password",
            "known-source",
        )
        .unwrap_err();
    let unknown = auth.issue_preauth_for("unknown-source").unwrap();
    let unknown_error = auth
        .login_for(
            &unknown.cookie_value,
            &unknown.session.csrf_token,
            "not-an-admin",
            "wrong password",
            "unknown-source",
        )
        .unwrap_err();

    assert_eq!(known_error.code(), "invalid_credentials");
    assert_eq!(unknown_error.code(), "invalid_credentials");
    assert_eq!(known_error.message(), unknown_error.message());
}

#[test]
fn argon2_password_verification_has_an_independent_concurrency_limit() {
    let (auth, _directory) = auth("qq-maid-admin-argon2-limit");
    let encoded = hash_password("correct horse battery staple").unwrap();

    let attempts = (0..8)
        .map(|index| {
            let auth = auth.clone();
            let encoded = encoded.clone();
            std::thread::spawn(move || {
                let _ = index;
                assert!(
                    !auth
                        .verify_password_limited("definitely the wrong password", &encoded)
                        .unwrap()
                );
            })
        })
        .collect::<Vec<_>>();
    for attempt in attempts {
        attempt.join().unwrap();
    }

    let state = auth.argon2_limiter.state.lock().unwrap();
    assert_eq!(state.active, 0);
    assert_eq!(state.max_observed, MAX_ARGON2_VERIFICATIONS);
}

#[test]
fn bootstrap_token_outputs_only_when_a_new_token_is_generated() {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-admin-token-output", APP_MIGRATIONS).unwrap();
    let path = directory.join("config/secrets/bootstrap.token");
    let outputs = Arc::new(Mutex::new(Vec::new()));
    let captured = outputs.clone();
    let auth = AdminAuth::open_with_token_output(
        database,
        path.clone(),
        Some(Arc::new(move |token, _| {
            captured.lock().unwrap().push(token.to_owned());
        })),
    )
    .unwrap();
    assert_eq!(outputs.lock().unwrap().len(), 1);
    auth.bootstrap_status().unwrap();
    assert_eq!(outputs.lock().unwrap().len(), 1);

    let token = bootstrap_token(&path);
    let setup = auth.issue_preauth_for("operator").unwrap();
    auth.initialize_for(
        &setup.cookie_value,
        &setup.session.csrf_token,
        &token,
        "admin",
        "123456",
        "operator",
    )
    .unwrap();
    let reset = auth.issue_preauth_for("reset-operator").unwrap();
    auth.request_password_reset_for(
        &reset.cookie_value,
        &reset.session.csrf_token,
        "reset-operator",
    )
    .unwrap();
    assert_eq!(outputs.lock().unwrap().len(), 2);

    // 查询状态、复用有效重置令牌和重新打开认证服务都不能再次输出。
    auth.bootstrap_status().unwrap();
    auth.request_password_reset_for(
        &reset.cookie_value,
        &reset.session.csrf_token,
        "reset-operator",
    )
    .unwrap();
    assert_eq!(outputs.lock().unwrap().len(), 2);
}

#[test]
fn disabled_console_does_not_generate_bootstrap_credentials() {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-admin-disabled-console", APP_MIGRATIONS)
            .unwrap();
    let token_file = directory.join("config/secrets/bootstrap.token");

    let auth = AdminAuth::open_if_enabled(database, token_file.clone(), false).unwrap();

    assert!(auth.is_none());
    assert!(!token_file.exists());
}
