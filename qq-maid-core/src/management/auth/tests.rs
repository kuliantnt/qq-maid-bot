use super::*;
use crate::storage::APP_MIGRATIONS;

fn auth(name: &str) -> (AdminAuth, PathBuf) {
    let (database, directory) = SqliteDatabase::open_temp_directory(name, APP_MIGRATIONS).unwrap();
    let token_file = directory.join("config/secrets/bootstrap.token");
    (AdminAuth::open(database, token_file).unwrap(), directory)
}

fn bootstrap_token(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap()
        .trim()
        .splitn(3, ':')
        .nth(2)
        .unwrap()
        .to_owned()
}

#[test]
fn bootstrap_is_single_use_and_password_is_not_stored_in_plaintext() {
    let (auth, directory) = auth("qq-maid-admin-bootstrap");
    let path = directory.join("config/secrets/bootstrap.token");
    let token = bootstrap_token(&path);
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
fn bootstrap_token_output_is_disabled_by_default_including_renewal() {
    let (auth, directory) = auth("qq-maid-admin-token-output-default");
    assert!(auth.bootstrap_token_output.is_none());
    let path = directory.join("config/secrets/bootstrap.token");
    let token = bootstrap_token(&path);
    fs::write(
        &path,
        format!(
            "{BOOTSTRAP_PREFIX}:{}:{token}\n",
            unix_seconds() - BOOTSTRAP_TTL.as_secs() as i64 - 1
        ),
    )
    .unwrap();

    auth.bootstrap_status().unwrap();

    assert!(path.exists());
    assert!(auth.bootstrap_token_output.is_none());
    assert_ne!(bootstrap_token(&path), token);
}

#[test]
fn disabled_console_does_not_generate_bootstrap_credentials() {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("qq-maid-admin-disabled-console", APP_MIGRATIONS)
            .unwrap();
    let token_file = directory.join("config/secrets/bootstrap.token");

    let auth = AdminAuth::open_if_enabled(database, token_file.clone(), false, false).unwrap();

    assert!(auth.is_none());
    assert!(!token_file.exists());
}
