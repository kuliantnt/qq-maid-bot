use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::storage::database::{SqliteDatabase, SqliteMigration};

pub const SESSION_COOKIE_NAME: &str = "qq_maid_console_session";
pub const PREAUTH_COOKIE_NAME: &str = "qq_maid_console_preauth";
pub const SECURE_SESSION_COOKIE_NAME: &str = "__Host-qq_maid_console_session";
pub const SECURE_PREAUTH_COOKIE_NAME: &str = "__Host-qq_maid_console_preauth";
const BOOTSTRAP_PREFIX: &str = "qq-maid-bootstrap-v1";
const PASSWORD_RESET_PREFIX: &str = "qq-maid-password-reset-v1";
const BOOTSTRAP_TTL: Duration = Duration::from_secs(30 * 60);
const PREAUTH_TTL: Duration = Duration::from_secs(10 * 60);
const SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const SESSION_ABSOLUTE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
// 部署控制台通常只有一位管理员，但允许其在少量浏览器中分别登录。Admin 与匿名
// PreAuth 使用独立容量，避免匿名请求耗尽共享配额后挤掉仍有效的管理员会话。
const MAX_ADMIN_SESSIONS: usize = 32;
const MAX_PREAUTH_SESSIONS: usize = 1_024;
const MAX_SESSIONS: usize = MAX_ADMIN_SESSIONS + MAX_PREAUTH_SESSIONS;
const MAX_BOOTSTRAP_ATTEMPTS_PER_MINUTE: usize = 30;
const MAX_LOGIN_ATTEMPTS_PER_MINUTE: usize = 10;
const MAX_INITIALIZE_ATTEMPTS_PER_MINUTE: usize = 10;
const MAX_MANAGEMENT_ACTIONS_PER_MINUTE: usize = 60;
const MAX_ARGON2_VERIFICATIONS: usize = 2;
const MAX_LIMITER_KEYS: usize = 4_096;

type BootstrapTokenOutput = Arc<dyn Fn(&str, Duration) + Send + Sync>;

pub const CONSOLE_ADMIN_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "console_admin_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS console_admins (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL COLLATE NOCASE UNIQUE,
            password_hash TEXT NOT NULL,
            disabled INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
          );
          CREATE TABLE IF NOT EXISTS console_audit_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            actor_admin_id INTEGER,
            event_type TEXT NOT NULL,
            outcome TEXT NOT NULL,
            FOREIGN KEY(actor_admin_id) REFERENCES console_admins(id)
          );
          CREATE INDEX IF NOT EXISTS idx_console_audit_created_at
            ON console_audit_events(created_at);",
};

/// 管理审计 v2：在既有 `console_audit_events` 上补充请求和版本摘要。
///
/// 这些字段只接受稳定安全元数据；Memory 正文、raw identity、token 和 CSRF 永远不进表。
pub const CONSOLE_AUDIT_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "console_audit_schema_v2_management_metadata",
    sql: "ALTER TABLE console_audit_events ADD COLUMN request_id TEXT;
        ALTER TABLE console_audit_events ADD COLUMN target_digest TEXT;
        ALTER TABLE console_audit_events ADD COLUMN before_version INTEGER;
        ALTER TABLE console_audit_events ADD COLUMN after_version INTEGER;
        ALTER TABLE console_audit_events ADD COLUMN safe_error_code TEXT;
        CREATE INDEX IF NOT EXISTS idx_console_audit_request_id
            ON console_audit_events(request_id);",
};

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AdminAuthError {
    code: &'static str,
    message: String,
}

impl AdminAuthError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::new("admin_storage_error", message)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AdminBootstrapStatus {
    pub initialized: bool,
    pub setup_required: bool,
    pub password_reset_pending: bool,
    pub token_file: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AdminSession {
    pub username: String,
    pub capabilities: Vec<String>,
    pub csrf_token: String,
    pub expires_at: i64,
}

/// 管理资源审计所需的最小安全元数据；不携带资源正文或平台 raw identity。
#[derive(Debug, Clone, Copy)]
pub struct ManagementAudit<'a> {
    pub actor_admin_id: i64,
    pub request_id: &'a str,
    pub action: &'a str,
    pub result: &'a str,
    pub target_digest: Option<&'a str>,
    pub before_version: Option<u64>,
    pub after_version: Option<u64>,
    pub safe_error_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct IssuedSession {
    pub cookie_value: String,
    pub session: AdminSession,
}

#[derive(Clone)]
pub struct AdminAuth {
    database: SqliteDatabase,
    bootstrap_token_file: PathBuf,
    sessions: Arc<Mutex<HashMap<[u8; 32], SessionRecord>>>,
    bootstrap_limiter: Arc<KeyedAttemptLimiter>,
    login_limiter: Arc<KeyedAttemptLimiter>,
    initialize_limiter: Arc<KeyedAttemptLimiter>,
    management_limiter: Arc<KeyedAttemptLimiter>,
    argon2_limiter: Arc<Argon2ConcurrencyLimiter>,
    bootstrap_lock: Arc<Mutex<()>>,
    bootstrap_token_output: Option<BootstrapTokenOutput>,
}

#[derive(Clone)]
struct SessionRecord {
    kind: SessionKind,
    csrf_token: String,
    csrf_hash: [u8; 32],
    created_at: i64,
    last_seen_at: i64,
    absolute_expires_at: i64,
}

struct AdminSessionPromotion {
    issued: IssuedSession,
    admin_cookie_hash: [u8; 32],
    preauth_cookie_hash: [u8; 32],
    preauth_record: SessionRecord,
}

#[derive(Clone)]
enum SessionKind {
    PreAuth,
    Admin { id: i64, username: String },
}

#[derive(Default)]
struct KeyedAttemptLimiter {
    attempts: Mutex<HashMap<[u8; 32], VecDeque<Instant>>>,
}

impl KeyedAttemptLimiter {
    fn check(&self, key: [u8; 32], limit: usize) -> Result<(), AdminAuthError> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| AdminAuthError::storage("authentication limiter lock is poisoned"))?;
        let cutoff = Instant::now() - Duration::from_secs(60);
        attempts.retain(|_, values| {
            while values.front().is_some_and(|value| *value < cutoff) {
                values.pop_front();
            }
            !values.is_empty()
        });
        if !attempts.contains_key(&key) && attempts.len() >= MAX_LIMITER_KEYS {
            // 固定容量避免可信代理后的大量真实来源或用户名组合耗尽内存；淘汰最久未使用
            // 的键只会让该键重新计数，不会形成可锁死其他来源的全局额度。
            if let Some(oldest) = attempts
                .iter()
                .min_by_key(|(_, values)| values.back().copied())
                .map(|(key, _)| *key)
            {
                attempts.remove(&oldest);
            }
        }
        let values = attempts.entry(key).or_default();
        if values.len() >= limit {
            return Err(AdminAuthError::new(
                "rate_limited",
                "too many authentication attempts; retry later",
            ));
        }
        values.push_back(Instant::now());
        Ok(())
    }
}

struct Argon2ConcurrencyLimiter {
    state: Mutex<Argon2ConcurrencyState>,
    available: Condvar,
    limit: usize,
}

#[derive(Default)]
struct Argon2ConcurrencyState {
    active: usize,
    max_observed: usize,
}

impl Argon2ConcurrencyLimiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(Argon2ConcurrencyState::default()),
            available: Condvar::new(),
            limit,
        }
    }

    fn acquire(&self) -> Result<Argon2Permit<'_>, AdminAuthError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdminAuthError::storage("Argon2 limiter lock is poisoned"))?;
        while state.active >= self.limit {
            state = self
                .available
                .wait(state)
                .map_err(|_| AdminAuthError::storage("Argon2 limiter lock is poisoned"))?;
        }
        state.active += 1;
        state.max_observed = state.max_observed.max(state.active);
        Ok(Argon2Permit { limiter: self })
    }
}

struct Argon2Permit<'a> {
    limiter: &'a Argon2ConcurrencyLimiter,
}

impl Drop for Argon2Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.limiter.state.lock() {
            state.active = state.active.saturating_sub(1);
            self.limiter.available.notify_one();
        }
    }
}

impl AdminAuth {
    pub fn open(
        database: SqliteDatabase,
        bootstrap_token_file: PathBuf,
    ) -> Result<Self, AdminAuthError> {
        Self::open_with_token_output(
            database,
            bootstrap_token_file,
            Some(Arc::new(print_bootstrap_token)),
        )
    }

    #[cfg(test)]
    pub(crate) fn open_silent(
        database: SqliteDatabase,
        bootstrap_token_file: PathBuf,
    ) -> Result<Self, AdminAuthError> {
        Self::open_with_token_output(database, bootstrap_token_file, None)
    }

    pub fn open_if_enabled(
        database: SqliteDatabase,
        bootstrap_token_file: PathBuf,
        enabled: bool,
    ) -> Result<Option<Self>, AdminAuthError> {
        if !enabled {
            return Ok(None);
        }
        Self::open(database, bootstrap_token_file).map(Some)
    }

    fn open_with_token_output(
        database: SqliteDatabase,
        bootstrap_token_file: PathBuf,
        bootstrap_token_output: Option<BootstrapTokenOutput>,
    ) -> Result<Self, AdminAuthError> {
        let auth = Self {
            database,
            bootstrap_token_file,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_limiter: Arc::new(KeyedAttemptLimiter::default()),
            login_limiter: Arc::new(KeyedAttemptLimiter::default()),
            initialize_limiter: Arc::new(KeyedAttemptLimiter::default()),
            management_limiter: Arc::new(KeyedAttemptLimiter::default()),
            argon2_limiter: Arc::new(Argon2ConcurrencyLimiter::new(MAX_ARGON2_VERIFICATIONS)),
            bootstrap_lock: Arc::new(Mutex::new(())),
            bootstrap_token_output,
        };
        auth.ensure_bootstrap_state()?;
        Ok(auth)
    }

    pub fn bootstrap_status(&self) -> Result<AdminBootstrapStatus, AdminAuthError> {
        let initialized = self.admin_count()? > 0;
        let (password_reset_pending, expires_at) = if initialized {
            match self.read_bootstrap_token() {
                Ok(token)
                    if token.purpose == BootstrapTokenPurpose::PasswordReset
                        && token_is_valid(&token) =>
                {
                    (true, Some(token_expiry(&token)))
                }
                Ok(token) if token.purpose == BootstrapTokenPurpose::PasswordReset => {
                    let _ = fs::remove_file(&self.bootstrap_token_file);
                    (false, None)
                }
                Ok(_) => (false, None),
                Err(error) if error.code() == "bootstrap_token_missing" => (false, None),
                Err(error) => return Err(error),
            }
        } else {
            // 长时间停留在 setup_required 时，匿名 bootstrap GET 会撤销过期文件并安全
            // 生成新令牌；无需重启，原文不通过 API 返回，只由生成入口输出一次。
            self.ensure_bootstrap_state()?;
            let token = self.read_bootstrap_token()?;
            (false, Some(token_expiry(&token)))
        };
        Ok(AdminBootstrapStatus {
            initialized,
            setup_required: !initialized,
            password_reset_pending,
            token_file: safe_path_summary(&self.bootstrap_token_file),
            expires_at,
        })
    }

    /// 匿名流程只能先领取短时 pre-auth cookie，再携带同步 CSRF token 提交初始化或登录。
    pub fn check_bootstrap_rate_limit(&self, client_source: &str) -> Result<(), AdminAuthError> {
        self.bootstrap_limiter.check(
            rate_limit_key(&[client_source]),
            MAX_BOOTSTRAP_ATTEMPTS_PER_MINUTE,
        )
    }

    pub fn issue_preauth(&self) -> Result<IssuedSession, AdminAuthError> {
        self.issue_preauth_for("local")
    }

    pub fn issue_preauth_for(&self, client_source: &str) -> Result<IssuedSession, AdminAuthError> {
        self.check_bootstrap_rate_limit(client_source)?;
        let now = unix_seconds();
        let (cookie_value, cookie_hash) = random_token();
        let (csrf_token, csrf_hash) = random_token();
        let record = SessionRecord {
            kind: SessionKind::PreAuth,
            csrf_token: csrf_token.clone(),
            csrf_hash,
            created_at: now,
            last_seen_at: now,
            absolute_expires_at: now + PREAUTH_TTL.as_secs() as i64,
        };
        self.insert_session(cookie_hash, record)?;
        Ok(IssuedSession {
            cookie_value,
            session: AdminSession {
                username: String::new(),
                capabilities: Vec::new(),
                csrf_token,
                expires_at: now + PREAUTH_TTL.as_secs() as i64,
            },
        })
    }

    pub fn initialize(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        bootstrap_token: &str,
        username: &str,
        password: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        self.initialize_for(
            preauth_cookie,
            csrf_token,
            bootstrap_token,
            username,
            password,
            "local",
        )
    }

    pub fn initialize_for(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        bootstrap_token: &str,
        username: &str,
        password: &str,
        client_source: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        self.initialize_limiter.check(
            rate_limit_key(&[client_source]),
            MAX_INITIALIZE_ATTEMPTS_PER_MINUTE,
        )?;
        self.require_preauth(preauth_cookie, csrf_token)?;
        validate_username(username)?;
        validate_password(password)?;
        if self.admin_count()? != 0 {
            return Err(AdminAuthError::new(
                "already_initialized",
                "deployment administrator has already been initialized",
            ));
        }
        let provided = match normalize_bootstrap_token_input(bootstrap_token) {
            Ok(provided) => provided,
            Err(error) => {
                self.audit(None, "admin.initialize", "denied")?;
                return Err(error);
            }
        };
        let expected = self.read_bootstrap_token()?;
        if expected.purpose != BootstrapTokenPurpose::Initialize
            || !token_is_valid(&expected)
            || provided
                .purpose
                .is_some_and(|purpose| purpose != BootstrapTokenPurpose::Initialize)
            || !constant_time_token_eq(provided.token, &expected.token)
        {
            self.audit(None, "admin.initialize", "denied")?;
            return Err(AdminAuthError::new(
                "invalid_bootstrap_token",
                "bootstrap token is invalid or expired",
            ));
        }

        let password_hash = hash_password(password)?;
        let now = unix_seconds();
        let admin_id = {
            let mut connection = self.database.connection().map_err(database_error)?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database_error)?;
            let count: i64 = transaction
                .query_row("SELECT COUNT(*) FROM console_admins", [], |row| row.get(0))
                .map_err(database_error)?;
            if count != 0 {
                return Err(AdminAuthError::new(
                    "already_initialized",
                    "deployment administrator has already been initialized",
                ));
            }
            transaction
                .execute(
                    "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                     VALUES (?1, ?2, 0, ?3)",
                    params![username.trim(), password_hash, now],
                )
                .map_err(database_error)?;
            let id = transaction.last_insert_rowid();
            transaction
                .execute(
                    "INSERT INTO console_audit_events
                     (created_at, actor_admin_id, event_type, outcome)
                     VALUES (?1, ?2, 'admin.initialize', 'success')",
                    params![now, id],
                )
                .map_err(database_error)?;
            transaction.commit().map_err(database_error)?;
            id
        };

        // 数据库中的首位管理员是唯一授权事实；即使文件删除失败，旧令牌也无法重放。
        let _ = fs::remove_file(&self.bootstrap_token_file);
        self.remove_session(preauth_cookie)?;
        self.issue_admin_session(admin_id, username.trim())
    }

    pub fn request_password_reset_for(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        client_source: &str,
    ) -> Result<AdminBootstrapStatus, AdminAuthError> {
        self.initialize_limiter.check(
            rate_limit_key(&[client_source, "password_reset_request"]),
            MAX_INITIALIZE_ATTEMPTS_PER_MINUTE,
        )?;
        self.require_preauth(preauth_cookie, csrf_token)?;
        let _guard = self
            .bootstrap_lock
            .lock()
            .map_err(|_| AdminAuthError::storage("bootstrap token lock is poisoned"))?;
        if self.admin_count()? == 0 {
            return Err(AdminAuthError::new(
                "not_initialized",
                "deployment administrator has not been initialized",
            ));
        }

        let token = match self.read_bootstrap_token() {
            Ok(token)
                if token.purpose == BootstrapTokenPurpose::PasswordReset
                    && token_is_valid(&token) =>
            {
                token
            }
            Ok(_) => {
                fs::remove_file(&self.bootstrap_token_file).map_err(|error| {
                    AdminAuthError::storage(format!(
                        "failed to replace administrator password reset token: {error}"
                    ))
                })?;
                self.create_bootstrap_token(BootstrapTokenPurpose::PasswordReset, true)?;
                self.read_bootstrap_token()?
            }
            Err(error) if error.code() == "bootstrap_token_missing" => {
                self.create_bootstrap_token(BootstrapTokenPurpose::PasswordReset, true)?;
                self.read_bootstrap_token()?
            }
            Err(error) => return Err(error),
        };
        Ok(AdminBootstrapStatus {
            initialized: true,
            setup_required: false,
            password_reset_pending: true,
            token_file: safe_path_summary(&self.bootstrap_token_file),
            expires_at: Some(token_expiry(&token)),
        })
    }

    pub fn reset_password_for(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        bootstrap_token: &str,
        password: &str,
        client_source: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        self.initialize_limiter.check(
            rate_limit_key(&[client_source, "password_reset_commit"]),
            MAX_INITIALIZE_ATTEMPTS_PER_MINUTE,
        )?;
        self.require_preauth(preauth_cookie, csrf_token)?;
        validate_password(password)?;
        let _guard = self
            .bootstrap_lock
            .lock()
            .map_err(|_| AdminAuthError::storage("bootstrap token lock is poisoned"))?;
        let provided = match normalize_bootstrap_token_input(bootstrap_token) {
            Ok(provided) => provided,
            Err(error) => {
                self.audit(None, "admin.password_reset", "denied")?;
                return Err(error);
            }
        };
        let expected = self.read_bootstrap_token()?;
        if expected.purpose != BootstrapTokenPurpose::PasswordReset
            || !token_is_valid(&expected)
            || provided
                .purpose
                .is_some_and(|purpose| purpose != BootstrapTokenPurpose::PasswordReset)
            || !constant_time_token_eq(provided.token, &expected.token)
        {
            self.audit(None, "admin.password_reset", "denied")?;
            return Err(AdminAuthError::new(
                "invalid_bootstrap_token",
                "bootstrap token is invalid or expired",
            ));
        }

        let password_hash = hash_password(password)?;
        let now = unix_seconds();
        let (admin_id, username) = {
            let mut connection = self.database.connection().map_err(database_error)?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database_error)?;
            let admin = transaction
                .query_row(
                    "SELECT id, username FROM console_admins
                     WHERE disabled = 0 ORDER BY id ASC LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(database_error)?
                .ok_or_else(|| {
                    AdminAuthError::new(
                        "not_initialized",
                        "deployment administrator is unavailable",
                    )
                })?;
            transaction
                .execute(
                    "UPDATE console_admins SET password_hash = ?1 WHERE id = ?2",
                    params![password_hash, admin.0],
                )
                .map_err(database_error)?;
            transaction
                .execute(
                    "INSERT INTO console_audit_events
                     (created_at, actor_admin_id, event_type, outcome)
                     VALUES (?1, ?2, 'admin.password_reset', 'success')",
                    params![now, admin.0],
                )
                .map_err(database_error)?;
            transaction.commit().map_err(database_error)?;
            admin
        };

        // 密码重置成功后撤销所有旧 Admin 会话；匿名 PreAuth 仍按独立容量管理。
        let preauth_hash = token_hash(preauth_cookie);
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        sessions.retain(|key, record| {
            *key != preauth_hash && !matches!(record.kind, SessionKind::Admin { .. })
        });
        drop(sessions);
        let _ = fs::remove_file(&self.bootstrap_token_file);
        self.issue_admin_session(admin_id, &username)
    }

    pub fn login(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        username: &str,
        password: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        self.login_for(preauth_cookie, csrf_token, username, password, "local")
    }

    pub fn login_for(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        username: &str,
        password: &str,
        client_source: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        let normalized_username = normalize_username(username);
        self.login_limiter.check(
            rate_limit_key(&[client_source, &normalized_username]),
            MAX_LOGIN_ATTEMPTS_PER_MINUTE,
        )?;
        self.require_preauth(preauth_cookie, csrf_token)?;
        let connection = self.database.connection().map_err(database_error)?;
        let admin = connection
            .query_row(
                "SELECT id, username, password_hash, disabled
                 FROM console_admins WHERE username = ?1 COLLATE NOCASE",
                [username.trim()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        let dummy_hash = dummy_password_hash()?;
        let password_hash = admin
            .as_ref()
            .map(|(_, _, password_hash, _)| password_hash.as_str())
            .unwrap_or(dummy_hash);
        let password_valid = self.verify_password_limited(password, password_hash)?;
        let Some((id, stored_username, _, disabled)) = admin else {
            self.audit(None, "admin.login", "denied")?;
            return Err(invalid_credentials());
        };
        if disabled != 0 || !password_valid {
            self.audit(Some(id), "admin.login", "denied")?;
            return Err(invalid_credentials());
        }
        // 只有 Admin 会话已经在锁内插入并完成 PreAuth 替换后，才能记录成功审计。
        // 这样容量检查失败时不会消耗仍可重试的 PreAuth，也不会伪造成功登录。
        let promotion =
            self.promote_preauth_to_admin(preauth_cookie, csrf_token, id, &stored_username)?;
        match self.audit(Some(id), "admin.login", "success") {
            Ok(()) => Ok(promotion.issued),
            Err(audit_error) => {
                // 成功审计未落库时，客户端不会拿到 Admin Cookie；必须撤销刚签发的
                // Admin 并恢复原 PreAuth，避免内存中留下客户端无法访问的幽灵会话。
                self.rollback_admin_session_promotion(promotion);
                Err(audit_error)
            }
        }
    }

    /// 返回管理员会话快照。CSRF 在同一管理员会话生命周期内保持稳定，使多个标签页
    /// 获取会话后都能继续提交受保护请求；登录和重新登录仍会签发全新会话与 token。
    pub fn refresh_admin_session(
        &self,
        cookie_value: &str,
    ) -> Result<AdminSession, AdminAuthError> {
        let cookie_hash = token_hash(cookie_value);
        let now = unix_seconds();
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        prune_sessions(&mut sessions, now);
        let record = sessions.get_mut(&cookie_hash).ok_or_else(unauthenticated)?;
        let SessionKind::Admin { username, .. } = &record.kind else {
            return Err(unauthenticated());
        };
        if now - record.last_seen_at > SESSION_IDLE_TTL.as_secs() as i64 {
            sessions.remove(&cookie_hash);
            return Err(unauthenticated());
        }
        let username = username.clone();
        record.last_seen_at = now;
        Ok(AdminSession {
            username,
            capabilities: admin_capabilities(),
            csrf_token: record.csrf_token.clone(),
            expires_at: record.absolute_expires_at,
        })
    }

    pub fn authorize_admin(
        &self,
        cookie_value: &str,
        csrf_token: Option<&str>,
    ) -> Result<(i64, String), AdminAuthError> {
        let cookie_hash = token_hash(cookie_value);
        let now = unix_seconds();
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        prune_sessions(&mut sessions, now);
        let record = sessions.get_mut(&cookie_hash).ok_or_else(unauthenticated)?;
        if now - record.last_seen_at > SESSION_IDLE_TTL.as_secs() as i64 {
            sessions.remove(&cookie_hash);
            return Err(unauthenticated());
        }
        if let Some(csrf_token) = csrf_token {
            let supplied = token_hash(csrf_token);
            if record.csrf_hash.ct_eq(&supplied).unwrap_u8() != 1 {
                return Err(AdminAuthError::new("csrf_failed", "CSRF validation failed"));
            }
        }
        record.last_seen_at = now;
        match &record.kind {
            SessionKind::Admin { id, username } => Ok((*id, username.clone())),
            SessionKind::PreAuth => Err(unauthenticated()),
        }
    }

    pub fn logout(&self, cookie_value: &str, csrf_token: &str) -> Result<(), AdminAuthError> {
        let (id, _) = self.authorize_admin(cookie_value, Some(csrf_token))?;
        self.remove_session(cookie_value)?;
        self.audit(Some(id), "admin.logout", "success")
    }

    /// 对配置写入、secret 变更、连接测试等已认证管理动作执行独立限流。
    pub fn check_management_rate_limit(&self, admin_id: i64) -> Result<(), AdminAuthError> {
        self.management_limiter.check(
            rate_limit_key(&[&admin_id.to_string()]),
            MAX_MANAGEMENT_ACTIONS_PER_MINUTE,
        )
    }

    fn verify_password_limited(
        &self,
        password: &str,
        encoded: &str,
    ) -> Result<bool, AdminAuthError> {
        let _permit = self.argon2_limiter.acquire()?;
        #[cfg(test)]
        std::thread::sleep(Duration::from_millis(20));
        verify_password(password, encoded)
    }

    pub fn audit(
        &self,
        actor_admin_id: Option<i64>,
        event_type: &str,
        outcome: &str,
    ) -> Result<(), AdminAuthError> {
        // 审计字段是服务端固定枚举；不接收正文、配置值、平台标识或请求参数。
        if !safe_audit_value(event_type) || !safe_audit_value(outcome) {
            return Err(AdminAuthError::storage("invalid audit event metadata"));
        }
        let connection = self.database.connection().map_err(database_error)?;
        connection
            .execute(
                "INSERT INTO console_audit_events
                 (created_at, actor_admin_id, event_type, outcome)
                 VALUES (?1, ?2, ?3, ?4)",
                params![unix_seconds(), actor_admin_id, event_type, outcome],
            )
            .map_err(database_error)?;
        Ok(())
    }

    /// 复用现有管理审计表记录资源操作的安全摘要。
    pub fn audit_management(&self, event: ManagementAudit<'_>) -> Result<(), AdminAuthError> {
        let ManagementAudit {
            actor_admin_id,
            request_id,
            action,
            result,
            target_digest,
            before_version,
            after_version,
            safe_error_code,
        } = event;
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
            || !safe_audit_value(action)
            || !safe_audit_value(result)
            || target_digest.is_some_and(|value| {
                value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || safe_error_code.is_some_and(|value| !safe_audit_value(value))
        {
            return Err(AdminAuthError::storage("invalid management audit metadata"));
        }
        let before_version = before_version
            .map(|value| i64::try_from(value).map_err(|_| ()))
            .transpose()
            .map_err(|_| AdminAuthError::storage("management audit version is too large"))?;
        let after_version = after_version
            .map(|value| i64::try_from(value).map_err(|_| ()))
            .transpose()
            .map_err(|_| AdminAuthError::storage("management audit version is too large"))?;
        let connection = self.database.connection().map_err(database_error)?;
        connection
            .execute(
                "INSERT INTO console_audit_events
                 (created_at, actor_admin_id, event_type, outcome, request_id,
                  target_digest, before_version, after_version, safe_error_code)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    unix_seconds(),
                    actor_admin_id,
                    action,
                    result,
                    request_id,
                    target_digest,
                    before_version,
                    after_version,
                    safe_error_code,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn issue_admin_session(
        &self,
        id: i64,
        username: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        let (cookie_hash, record, issued) = new_admin_session(id, username);
        self.insert_session(cookie_hash, record)?;
        Ok(issued)
    }

    /// 在同一个锁临界区内校验并完成 PreAuth → Admin 转换。
    ///
    /// 登录密码校验在进入本方法前完成，但 PreAuth 会话必须在这里再次校验，
    /// 避免校验、容量检查和插入之间被其他请求改变状态。
    fn promote_preauth_to_admin(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        id: i64,
        username: &str,
    ) -> Result<AdminSessionPromotion, AdminAuthError> {
        let preauth_hash = token_hash(preauth_cookie);
        let csrf_hash = token_hash(csrf_token);
        let now = unix_seconds();
        let (admin_cookie_hash, admin_record, issued) = new_admin_session(id, username);
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        prune_sessions(&mut sessions, now);

        let preauth = sessions.get(&preauth_hash).ok_or_else(unauthenticated)?;
        if !matches!(&preauth.kind, SessionKind::PreAuth)
            || preauth.csrf_hash.ct_eq(&csrf_hash).unwrap_u8() != 1
        {
            return Err(AdminAuthError::new("csrf_failed", "CSRF validation failed"));
        }
        let preauth_record = preauth.clone();

        // 此处的容量检查与插入共用同一把锁；满额时不删除任何 Admin 或 PreAuth。
        insert_session_locked(&mut sessions, admin_cookie_hash, admin_record)?;
        sessions.remove(&preauth_hash);
        debug_assert!(sessions.len() <= MAX_SESSIONS);
        Ok(AdminSessionPromotion {
            issued,
            admin_cookie_hash,
            preauth_cookie_hash: preauth_hash,
            preauth_record,
        })
    }

    fn rollback_admin_session_promotion(&self, promotion: AdminSessionPromotion) {
        // 这是审计失败后的认证一致性补偿路径。即使锁此前已因其他 panic 中毒，仍应
        // 取出其中的 session map 完成撤销和恢复，不能因为返回锁错误而留下幽灵 Admin。
        let mut sessions = match self.sessions.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions.remove(&promotion.admin_cookie_hash);
        sessions.insert(promotion.preauth_cookie_hash, promotion.preauth_record);
        debug_assert!(sessions.len() <= MAX_SESSIONS);
    }

    fn require_preauth(&self, cookie_value: &str, csrf_token: &str) -> Result<(), AdminAuthError> {
        let cookie_hash = token_hash(cookie_value);
        let csrf_hash = token_hash(csrf_token);
        let now = unix_seconds();
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        prune_sessions(&mut sessions, now);
        let record = sessions.get_mut(&cookie_hash).ok_or_else(unauthenticated)?;
        if !matches!(record.kind, SessionKind::PreAuth)
            || record.csrf_hash.ct_eq(&csrf_hash).unwrap_u8() != 1
        {
            return Err(AdminAuthError::new("csrf_failed", "CSRF validation failed"));
        }
        record.last_seen_at = now;
        Ok(())
    }

    fn insert_session(
        &self,
        token_hash: [u8; 32],
        record: SessionRecord,
    ) -> Result<(), AdminAuthError> {
        let mut sessions = self.sessions.lock().map_err(session_lock_error)?;
        prune_sessions(&mut sessions, unix_seconds());
        insert_session_locked(&mut sessions, token_hash, record)
    }
}

fn database_error(error: impl std::fmt::Display) -> AdminAuthError {
    AdminAuthError::storage(format!("administrator database operation failed: {error}"))
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn safe_audit_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'.' || byte == b'_')
}

mod bootstrap;
mod credentials;
mod session;

use bootstrap::*;
use credentials::*;
use session::*;

#[cfg(test)]
mod tests;
