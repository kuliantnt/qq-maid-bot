use super::*;

pub(super) fn new_admin_session(
    id: i64,
    username: &str,
) -> ([u8; 32], SessionRecord, IssuedSession) {
    let now = unix_seconds();
    let (cookie_value, cookie_hash) = random_token();
    let (csrf_token, csrf_hash) = random_token();
    let absolute_expires_at = now + SESSION_ABSOLUTE_TTL.as_secs() as i64;
    let record = SessionRecord {
        kind: SessionKind::Admin {
            id,
            username: username.to_owned(),
        },
        csrf_token: csrf_token.clone(),
        csrf_hash,
        created_at: now,
        last_seen_at: now,
        absolute_expires_at,
    };
    let issued = IssuedSession {
        cookie_value,
        session: AdminSession {
            username: username.to_owned(),
            capabilities: admin_capabilities(),
            csrf_token,
            expires_at: absolute_expires_at,
        },
    };
    (cookie_hash, record, issued)
}

pub(super) fn insert_session_locked(
    sessions: &mut HashMap<[u8; 32], SessionRecord>,
    token_hash: [u8; 32],
    record: SessionRecord,
) -> Result<(), AdminAuthError> {
    match &record.kind {
        SessionKind::PreAuth => {
            if session_count(sessions, SessionKindFilter::PreAuth) >= MAX_PREAUTH_SESSIONS {
                // 匿名容量满时只回收最旧 PreAuth，绝不让匿名洪泛淘汰 Admin。
                let oldest = oldest_session(sessions, SessionKindFilter::PreAuth)
                    .ok_or_else(session_capacity_reached)?;
                sessions.remove(&oldest);
            }
        }
        SessionKind::Admin { .. } => {
            if session_count(sessions, SessionKindFilter::Admin) >= MAX_ADMIN_SESSIONS {
                // 有效 Admin 达到独立上限时拒绝新会话，不隐式登出其他管理员浏览器。
                return Err(session_capacity_reached());
            }
        }
    }
    sessions.insert(token_hash, record);
    debug_assert!(sessions.len() <= MAX_SESSIONS);
    Ok(())
}

impl AdminAuth {
    pub(super) fn remove_session(&self, cookie_value: &str) -> Result<(), AdminAuthError> {
        self.sessions
            .lock()
            .map_err(session_lock_error)?
            .remove(&token_hash(cookie_value));
        Ok(())
    }

    pub(super) fn admin_count(&self) -> Result<i64, AdminAuthError> {
        self.database
            .connection()
            .map_err(database_error)?
            .query_row("SELECT COUNT(*) FROM console_admins", [], |row| row.get(0))
            .map_err(database_error)
    }

    pub(super) fn ensure_bootstrap_state(&self) -> Result<(), AdminAuthError> {
        let _guard = self
            .bootstrap_lock
            .lock()
            .map_err(|_| AdminAuthError::storage("bootstrap token lock is poisoned"))?;
        if self.admin_count()? > 0 {
            return match self.read_bootstrap_token() {
                Ok(token)
                    if token.purpose == BootstrapTokenPurpose::PasswordReset
                        && token_is_valid(&token) =>
                {
                    Ok(())
                }
                Ok(_) => {
                    let _ = fs::remove_file(&self.bootstrap_token_file);
                    Ok(())
                }
                Err(error) if error.code() == "bootstrap_token_missing" => Ok(()),
                Err(error) => Err(error),
            };
        }
        match self.read_bootstrap_token() {
            Ok(token)
                if token.purpose == BootstrapTokenPurpose::Initialize && token_is_valid(&token) =>
            {
                Ok(())
            }
            Ok(_) => {
                fs::remove_file(&self.bootstrap_token_file).map_err(|error| {
                    AdminAuthError::storage(format!(
                        "failed to revoke expired bootstrap token file: {error}"
                    ))
                })?;
                self.create_bootstrap_token(BootstrapTokenPurpose::Initialize, true)
            }
            Err(error) if error.code() == "bootstrap_token_missing" => {
                self.create_bootstrap_token(BootstrapTokenPurpose::Initialize, true)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn create_bootstrap_token(
        &self,
        purpose: BootstrapTokenPurpose,
        allow_log_output: bool,
    ) -> Result<(), AdminAuthError> {
        let parent = self
            .bootstrap_token_file
            .parent()
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| {
            AdminAuthError::storage(format!(
                "failed to create bootstrap token directory: {error}"
            ))
        })?;
        restrict_directory(parent)?;
        let token = random_bootstrap_token();
        let content = format!("{}:{}:{token}\n", purpose.prefix(), unix_seconds());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.bootstrap_token_file).map_err(|error| {
            AdminAuthError::storage(format!("failed to create bootstrap token file: {error}"))
        })?;
        file.write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AdminAuthError::storage(format!("failed to persist bootstrap token file: {error}"))
            })?;
        if allow_log_output && let Some(output) = self.bootstrap_token_output.as_ref() {
            output(&token, BOOTSTRAP_TTL);
        }
        Ok(())
    }

    pub(super) fn read_bootstrap_token(&self) -> Result<BootstrapToken, AdminAuthError> {
        let metadata = fs::symlink_metadata(&self.bootstrap_token_file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AdminAuthError::new("bootstrap_token_missing", "bootstrap token file is missing")
            } else {
                AdminAuthError::storage(format!("failed to inspect bootstrap token file: {error}"))
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AdminAuthError::storage(
                "bootstrap token path must be a regular file and not a symbolic link",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(AdminAuthError::storage(
                    "bootstrap token file permissions must not grant group or other access",
                ));
            }
        }
        let mut text = String::new();
        OpenOptions::new()
            .read(true)
            .open(&self.bootstrap_token_file)
            .and_then(|file| file.take(512).read_to_string(&mut text))
            .map_err(|error| {
                AdminAuthError::storage(format!("failed to read bootstrap token file: {error}"))
            })?;
        let mut parts = text.trim().splitn(3, ':');
        let purpose = match parts.next() {
            Some(BOOTSTRAP_PREFIX) => Some(BootstrapTokenPurpose::Initialize),
            Some(PASSWORD_RESET_PREFIX) => Some(BootstrapTokenPurpose::PasswordReset),
            _ => None,
        };
        let issued_at = parts.next().and_then(|value| value.parse::<i64>().ok());
        let token = parts.next().filter(|value| !value.is_empty());
        if purpose.is_none() || issued_at.is_none() || token.is_none() {
            return Err(AdminAuthError::storage(
                "bootstrap token file has an invalid format",
            ));
        }
        Ok(BootstrapToken {
            purpose: purpose.unwrap(),
            issued_at: issued_at.unwrap(),
            token: token.unwrap().to_owned(),
        })
    }
}

pub(super) fn print_bootstrap_token(token: &str, ttl: Duration) {
    // 只在令牌新生成时调用；状态读取、有效令牌复用和重启不得重复输出。
    // 令牌仅进入这一条 info 日志，不写入 API 响应或长期状态；文件权限与平台边界
    // 见部署文档。
    tracing::info!(
        token = %token,
        ttl_minutes = ttl.as_secs() / 60,
        "部署管理员 Bootstrap / 密码重置令牌已生成（仅可使用一次）；请勿转发或长期保留启动日志"
    );
}

pub(super) fn dummy_password_hash() -> Result<&'static str, AdminAuthError> {
    static DUMMY_PASSWORD_HASH: OnceLock<String> = OnceLock::new();
    if let Some(value) = DUMMY_PASSWORD_HASH.get() {
        return Ok(value);
    }
    let value = hash_password("qq-maid-dummy-password-verification")?;
    let _ = DUMMY_PASSWORD_HASH.set(value);
    DUMMY_PASSWORD_HASH
        .get()
        .map(String::as_str)
        .ok_or_else(|| AdminAuthError::storage("failed to initialize dummy password hash"))
}

pub(super) struct BootstrapToken {
    pub(super) purpose: BootstrapTokenPurpose,
    pub(super) issued_at: i64,
    pub(super) token: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BootstrapTokenInput<'a> {
    pub(super) purpose: Option<BootstrapTokenPurpose>,
    pub(super) token: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootstrapTokenPurpose {
    Initialize,
    PasswordReset,
}

impl BootstrapTokenPurpose {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::Initialize => BOOTSTRAP_PREFIX,
            Self::PasswordReset => PASSWORD_RESET_PREFIX,
        }
    }
}

pub(super) fn normalize_bootstrap_token_input(
    input: &str,
) -> Result<BootstrapTokenInput<'_>, AdminAuthError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid_bootstrap_token_format());
    }
    if !input.contains(':') {
        return Ok(BootstrapTokenInput {
            purpose: None,
            token: input,
        });
    }

    let mut parts = input.split(':');
    let purpose = match parts.next() {
        Some(BOOTSTRAP_PREFIX) => BootstrapTokenPurpose::Initialize,
        Some(PASSWORD_RESET_PREFIX) => BootstrapTokenPurpose::PasswordReset,
        _ => return Err(invalid_bootstrap_token_format()),
    };
    let issued_at = parts.next().ok_or_else(invalid_bootstrap_token_format)?;
    let token = parts.next().ok_or_else(invalid_bootstrap_token_format)?;
    // 完整输入只接受本项目实际生成的三段格式，避免把任意冒号文本误当成令牌。
    if parts.next().is_some()
        || issued_at.is_empty()
        || !issued_at.bytes().all(|byte| byte.is_ascii_digit())
        || issued_at.parse::<i64>().is_err()
        || token.len() != 22
        || !matches!(URL_SAFE_NO_PAD.decode(token), Ok(decoded) if decoded.len() == 16)
    {
        return Err(invalid_bootstrap_token_format());
    }

    Ok(BootstrapTokenInput {
        purpose: Some(purpose),
        token,
    })
}

pub(super) fn invalid_bootstrap_token_format() -> AdminAuthError {
    AdminAuthError::new(
        "invalid_bootstrap_token_format",
        "bootstrap token input format is not recognized",
    )
}

pub(super) fn hash_password(password: &str) -> Result<String, AdminAuthError> {
    let random = Key::<XChaCha20Poly1305>::generate();
    let salt = SaltString::encode_b64(&random[..16])
        .map_err(|_| AdminAuthError::storage("failed to encode password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AdminAuthError::storage("failed to hash administrator password"))
}

pub(super) fn verify_password(password: &str, encoded: &str) -> Result<bool, AdminAuthError> {
    let parsed = PasswordHash::new(encoded)
        .map_err(|_| AdminAuthError::storage("stored administrator password hash is invalid"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub(super) fn random_token() -> (String, [u8; 32]) {
    let random = Key::<XChaCha20Poly1305>::generate();
    let value = URL_SAFE_NO_PAD.encode(random);
    let hash = token_hash(&value);
    (value, hash)
}

pub(super) fn random_bootstrap_token() -> String {
    let random = Key::<XChaCha20Poly1305>::generate();
    // Bootstrap/重置令牌是短时单次且还要求读取本地文件；128-bit 随机强度足够，
    // 同时比 Cookie/CSRF 使用的 256-bit token 更便于人工输入。
    URL_SAFE_NO_PAD.encode(&random[..16])
}

pub(super) fn token_hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

pub(super) fn rate_limit_key(parts: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.len().to_le_bytes());
        digest.update(part.as_bytes());
    }
    digest.finalize().into()
}

pub(super) fn normalize_username(username: &str) -> String {
    username.trim().to_ascii_lowercase()
}

pub(super) fn constant_time_token_eq(left: &str, right: &str) -> bool {
    token_hash(left).ct_eq(&token_hash(right)).unwrap_u8() == 1
}

pub(super) fn token_expiry(token: &BootstrapToken) -> i64 {
    token.issued_at + BOOTSTRAP_TTL.as_secs() as i64
}

pub(super) fn token_is_valid(token: &BootstrapToken) -> bool {
    unix_seconds() <= token_expiry(token)
}

pub(super) fn prune_sessions(sessions: &mut HashMap<[u8; 32], SessionRecord>, now: i64) {
    sessions.retain(|_, value| {
        now <= value.absolute_expires_at
            && now - value.last_seen_at
                <= match value.kind {
                    SessionKind::PreAuth => PREAUTH_TTL.as_secs() as i64,
                    SessionKind::Admin { .. } => SESSION_IDLE_TTL.as_secs() as i64,
                }
    });
}

#[derive(Clone, Copy)]
pub(super) enum SessionKindFilter {
    PreAuth,
    Admin,
}

pub(super) fn session_matches(kind: &SessionKind, filter: SessionKindFilter) -> bool {
    matches!(
        (kind, filter),
        (SessionKind::PreAuth, SessionKindFilter::PreAuth)
            | (SessionKind::Admin { .. }, SessionKindFilter::Admin)
    )
}

pub(super) fn session_count(
    sessions: &HashMap<[u8; 32], SessionRecord>,
    filter: SessionKindFilter,
) -> usize {
    sessions
        .values()
        .filter(|record| session_matches(&record.kind, filter))
        .count()
}

pub(super) fn oldest_session(
    sessions: &HashMap<[u8; 32], SessionRecord>,
    filter: SessionKindFilter,
) -> Option<[u8; 32]> {
    sessions
        .iter()
        .filter(|(_, record)| session_matches(&record.kind, filter))
        .min_by_key(|(_, record)| record.created_at)
        .map(|(key, _)| *key)
}

pub(super) fn validate_username(username: &str) -> Result<(), AdminAuthError> {
    let username = username.trim();
    let count = username.chars().count();
    if !(3..=64).contains(&count) || username.chars().any(char::is_control) {
        return Err(AdminAuthError::new(
            "validation_error",
            "administrator username must contain 3 to 64 visible characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_password(password: &str) -> Result<(), AdminAuthError> {
    if !(6..=256).contains(&password.chars().count()) {
        return Err(AdminAuthError::new(
            "validation_error",
            "administrator password must contain 6 to 256 characters",
        ));
    }
    Ok(())
}

pub(super) fn invalid_credentials() -> AdminAuthError {
    AdminAuthError::new("invalid_credentials", "invalid username or password")
}

pub(super) fn unauthenticated() -> AdminAuthError {
    AdminAuthError::new(
        "unauthenticated",
        "administrator session is missing or expired",
    )
}

pub(super) fn session_capacity_reached() -> AdminAuthError {
    AdminAuthError::new(
        "session_capacity_reached",
        "administrator session capacity has been reached; retry later",
    )
}

pub(super) fn session_lock_error<T>(_: std::sync::PoisonError<T>) -> AdminAuthError {
    AdminAuthError::storage("administrator session lock is poisoned")
}

pub(super) fn database_error(error: impl std::fmt::Display) -> AdminAuthError {
    AdminAuthError::storage(format!("administrator database operation failed: {error}"))
}

pub(super) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(super) fn admin_capabilities() -> Vec<String> {
    [
        "console.config.read",
        "console.config.write",
        "console.audit.write",
        "console.process.restart",
        "memory.admin",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub(super) fn safe_audit_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'.' || byte == b'_')
}

pub(super) fn safe_path_summary(path: &Path) -> String {
    if path.is_relative() {
        return path.to_string_lossy().replace('\\', "/");
    }
    if path.ends_with(Path::new("config/secrets/bootstrap.token")) {
        return "config/secrets/bootstrap.token".to_owned();
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("…/{name}"))
        .unwrap_or_else(|| "bootstrap.token".to_owned())
}

pub(super) fn restrict_directory(path: &Path) -> Result<(), AdminAuthError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AdminAuthError::storage(format!(
            "failed to inspect bootstrap token directory: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AdminAuthError::storage(
            "bootstrap token parent must be a directory and not a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            AdminAuthError::storage(format!(
                "failed to restrict bootstrap token directory permissions: {error}"
            ))
        })?;
    }
    Ok(())
}
