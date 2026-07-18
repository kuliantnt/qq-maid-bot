use std::{
    collections::{HashMap, VecDeque},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Generate, Key};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::storage::database::{SqliteDatabase, SqliteMigration};

pub const SESSION_COOKIE_NAME: &str = "qq_maid_console_session";
const BOOTSTRAP_PREFIX: &str = "qq-maid-bootstrap-v1";
const BOOTSTRAP_TTL: Duration = Duration::from_secs(30 * 60);
const PREAUTH_TTL: Duration = Duration::from_secs(10 * 60);
const SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const SESSION_ABSOLUTE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_SESSIONS: usize = 1_024;
const MAX_AUTH_ATTEMPTS_PER_MINUTE: usize = 30;
const MAX_MANAGEMENT_ACTIONS_PER_MINUTE: usize = 60;

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
    limiter: Arc<AttemptLimiter>,
    management_limiter: Arc<AttemptLimiter>,
    bootstrap_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct SessionRecord {
    kind: SessionKind,
    csrf_hash: [u8; 32],
    created_at: i64,
    last_seen_at: i64,
    absolute_expires_at: i64,
}

#[derive(Clone)]
enum SessionKind {
    PreAuth,
    Admin { id: i64, username: String },
}

struct AttemptLimiter {
    attempts: Mutex<VecDeque<Instant>>,
}

impl AttemptLimiter {
    fn check(&self, limit: usize) -> Result<(), AdminAuthError> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| AdminAuthError::storage("authentication limiter lock is poisoned"))?;
        let cutoff = Instant::now() - Duration::from_secs(60);
        while attempts.front().is_some_and(|value| *value < cutoff) {
            attempts.pop_front();
        }
        if attempts.len() >= limit {
            return Err(AdminAuthError::new(
                "rate_limited",
                "too many authentication attempts; retry later",
            ));
        }
        attempts.push_back(Instant::now());
        Ok(())
    }
}

impl AdminAuth {
    pub fn open(
        database: SqliteDatabase,
        bootstrap_token_file: PathBuf,
    ) -> Result<Self, AdminAuthError> {
        let auth = Self {
            database,
            bootstrap_token_file,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            limiter: Arc::new(AttemptLimiter {
                attempts: Mutex::new(VecDeque::new()),
            }),
            management_limiter: Arc::new(AttemptLimiter {
                attempts: Mutex::new(VecDeque::new()),
            }),
            bootstrap_lock: Arc::new(Mutex::new(())),
        };
        auth.ensure_bootstrap_state()?;
        Ok(auth)
    }

    pub fn bootstrap_status(&self) -> Result<AdminBootstrapStatus, AdminAuthError> {
        let initialized = self.admin_count()? > 0;
        let expires_at = if initialized {
            None
        } else {
            // 长时间停留在 setup_required 时，匿名 bootstrap GET 会撤销过期文件并安全
            // 生成新令牌；无需重启，也不会把新令牌通过 API 或日志返回。
            self.ensure_bootstrap_state()?;
            Some(self.read_bootstrap_token()?.issued_at + BOOTSTRAP_TTL.as_secs() as i64)
        };
        Ok(AdminBootstrapStatus {
            initialized,
            setup_required: !initialized,
            token_file: safe_path_summary(&self.bootstrap_token_file),
            expires_at,
        })
    }

    /// 匿名流程只能先领取短时 pre-auth cookie，再携带同步 CSRF token 提交初始化或登录。
    pub fn issue_preauth(&self) -> Result<IssuedSession, AdminAuthError> {
        self.limiter.check(MAX_AUTH_ATTEMPTS_PER_MINUTE)?;
        let now = unix_seconds();
        let (cookie_value, cookie_hash) = random_token();
        let (csrf_token, csrf_hash) = random_token();
        let record = SessionRecord {
            kind: SessionKind::PreAuth,
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
        self.limiter.check(MAX_AUTH_ATTEMPTS_PER_MINUTE)?;
        self.require_preauth(preauth_cookie, csrf_token)?;
        validate_username(username)?;
        validate_password(password)?;
        if self.admin_count()? != 0 {
            return Err(AdminAuthError::new(
                "already_initialized",
                "deployment administrator has already been initialized",
            ));
        }
        let expected = self.read_bootstrap_token()?;
        if unix_seconds() > expected.issued_at + BOOTSTRAP_TTL.as_secs() as i64
            || !constant_time_token_eq(bootstrap_token.trim(), &expected.token)
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

    pub fn login(
        &self,
        preauth_cookie: &str,
        csrf_token: &str,
        username: &str,
        password: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        self.limiter.check(MAX_AUTH_ATTEMPTS_PER_MINUTE)?;
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
        let Some((id, stored_username, password_hash, disabled)) = admin else {
            self.audit(None, "admin.login", "denied")?;
            return Err(invalid_credentials());
        };
        if disabled != 0 || !verify_password(password, &password_hash)? {
            self.audit(Some(id), "admin.login", "denied")?;
            return Err(invalid_credentials());
        }
        self.remove_session(preauth_cookie)?;
        self.audit(Some(id), "admin.login", "success")?;
        self.issue_admin_session(id, &stored_username)
    }

    /// 校验管理员会话并轮换 CSRF；页面刷新后不需要把 CSRF 放进持久存储。
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
        let (csrf_token, csrf_hash) = random_token();
        record.csrf_hash = csrf_hash;
        record.last_seen_at = now;
        Ok(AdminSession {
            username,
            capabilities: admin_capabilities(),
            csrf_token,
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
    pub fn check_management_rate_limit(&self) -> Result<(), AdminAuthError> {
        self.management_limiter
            .check(MAX_MANAGEMENT_ACTIONS_PER_MINUTE)
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

    fn issue_admin_session(
        &self,
        id: i64,
        username: &str,
    ) -> Result<IssuedSession, AdminAuthError> {
        let now = unix_seconds();
        let (cookie_value, cookie_hash) = random_token();
        let (csrf_token, csrf_hash) = random_token();
        let absolute_expires_at = now + SESSION_ABSOLUTE_TTL.as_secs() as i64;
        self.insert_session(
            cookie_hash,
            SessionRecord {
                kind: SessionKind::Admin {
                    id,
                    username: username.to_owned(),
                },
                csrf_hash,
                created_at: now,
                last_seen_at: now,
                absolute_expires_at,
            },
        )?;
        Ok(IssuedSession {
            cookie_value,
            session: AdminSession {
                username: username.to_owned(),
                capabilities: admin_capabilities(),
                csrf_token,
                expires_at: absolute_expires_at,
            },
        })
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
        if sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, value)| value.created_at)
                .map(|(key, _)| *key)
        {
            sessions.remove(&oldest);
        }
        sessions.insert(token_hash, record);
        Ok(())
    }

    fn remove_session(&self, cookie_value: &str) -> Result<(), AdminAuthError> {
        self.sessions
            .lock()
            .map_err(session_lock_error)?
            .remove(&token_hash(cookie_value));
        Ok(())
    }

    fn admin_count(&self) -> Result<i64, AdminAuthError> {
        self.database
            .connection()
            .map_err(database_error)?
            .query_row("SELECT COUNT(*) FROM console_admins", [], |row| row.get(0))
            .map_err(database_error)
    }

    fn ensure_bootstrap_state(&self) -> Result<(), AdminAuthError> {
        let _guard = self
            .bootstrap_lock
            .lock()
            .map_err(|_| AdminAuthError::storage("bootstrap token lock is poisoned"))?;
        if self.admin_count()? > 0 {
            let _ = fs::remove_file(&self.bootstrap_token_file);
            return Ok(());
        }
        match self.read_bootstrap_token() {
            Ok(token) if unix_seconds() <= token.issued_at + BOOTSTRAP_TTL.as_secs() as i64 => {
                Ok(())
            }
            Ok(_) => {
                fs::remove_file(&self.bootstrap_token_file).map_err(|error| {
                    AdminAuthError::storage(format!(
                        "failed to revoke expired bootstrap token file: {error}"
                    ))
                })?;
                self.create_bootstrap_token()
            }
            Err(error) if error.code() == "bootstrap_token_missing" => {
                self.create_bootstrap_token()
            }
            Err(error) => Err(error),
        }
    }

    fn create_bootstrap_token(&self) -> Result<(), AdminAuthError> {
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
        let (token, _) = random_token();
        let content = format!("{BOOTSTRAP_PREFIX}:{}:{token}\n", unix_seconds());
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
        #[cfg(not(test))]
        print_bootstrap_token_once(&token, BOOTSTRAP_TTL);
        Ok(())
    }

    fn read_bootstrap_token(&self) -> Result<BootstrapToken, AdminAuthError> {
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
        let prefix = parts.next();
        let issued_at = parts.next().and_then(|value| value.parse::<i64>().ok());
        let token = parts.next().filter(|value| !value.is_empty());
        if prefix != Some(BOOTSTRAP_PREFIX) || issued_at.is_none() || token.is_none() {
            return Err(AdminAuthError::storage(
                "bootstrap token file has an invalid format",
            ));
        }
        Ok(BootstrapToken {
            issued_at: issued_at.unwrap(),
            token: token.unwrap().to_owned(),
        })
    }
}

#[cfg(not(test))]
fn print_bootstrap_token_once(token: &str, ttl: Duration) {
    // 首次部署需要能直接从 Docker/终端完成初始化，因此只在新令牌成功落盘后
    // 输出一次；状态接口、后续登录和常规日志都不会再次读取或展示该值。
    eprintln!(
        "\n[qq-maid] 首次部署管理员初始化令牌（{} 分钟内有效，仅可使用一次）：\n{token}\n[qq-maid] 初始化后令牌立即失效；请勿转发或长期保留启动日志。\n",
        ttl.as_secs() / 60
    );
}

struct BootstrapToken {
    issued_at: i64,
    token: String,
}

fn hash_password(password: &str) -> Result<String, AdminAuthError> {
    let random = Key::<XChaCha20Poly1305>::generate();
    let salt = SaltString::encode_b64(&random[..16])
        .map_err(|_| AdminAuthError::storage("failed to encode password salt"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AdminAuthError::storage("failed to hash administrator password"))
}

fn verify_password(password: &str, encoded: &str) -> Result<bool, AdminAuthError> {
    let parsed = PasswordHash::new(encoded)
        .map_err(|_| AdminAuthError::storage("stored administrator password hash is invalid"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn random_token() -> (String, [u8; 32]) {
    let random = Key::<XChaCha20Poly1305>::generate();
    let value = URL_SAFE_NO_PAD.encode(random);
    let hash = token_hash(&value);
    (value, hash)
}

fn token_hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn constant_time_token_eq(left: &str, right: &str) -> bool {
    token_hash(left).ct_eq(&token_hash(right)).unwrap_u8() == 1
}

fn prune_sessions(sessions: &mut HashMap<[u8; 32], SessionRecord>, now: i64) {
    sessions.retain(|_, value| {
        now <= value.absolute_expires_at
            && now - value.last_seen_at
                <= match value.kind {
                    SessionKind::PreAuth => PREAUTH_TTL.as_secs() as i64,
                    SessionKind::Admin { .. } => SESSION_IDLE_TTL.as_secs() as i64,
                }
    });
}

fn validate_username(username: &str) -> Result<(), AdminAuthError> {
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

fn validate_password(password: &str) -> Result<(), AdminAuthError> {
    if !(12..=256).contains(&password.chars().count()) {
        return Err(AdminAuthError::new(
            "validation_error",
            "administrator password must contain 12 to 256 characters",
        ));
    }
    Ok(())
}

fn invalid_credentials() -> AdminAuthError {
    AdminAuthError::new("invalid_credentials", "invalid username or password")
}

fn unauthenticated() -> AdminAuthError {
    AdminAuthError::new(
        "unauthenticated",
        "administrator session is missing or expired",
    )
}

fn session_lock_error<T>(_: std::sync::PoisonError<T>) -> AdminAuthError {
    AdminAuthError::storage("administrator session lock is poisoned")
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

fn admin_capabilities() -> Vec<String> {
    [
        "console.config.read",
        "console.config.write",
        "console.audit.write",
        "memory.admin",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn safe_audit_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'.' || byte == b'_')
}

fn safe_path_summary(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("…/{name}"))
        .unwrap_or_else(|| "bootstrap.token".to_owned())
}

fn restrict_directory(path: &Path) -> Result<(), AdminAuthError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::APP_MIGRATIONS;

    fn auth(name: &str) -> (AdminAuth, PathBuf) {
        let (database, directory) =
            SqliteDatabase::open_temp_directory(name, APP_MIGRATIONS).unwrap();
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
    fn csrf_rotates_and_invalid_value_is_rejected() {
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
        assert_ne!(refreshed.csrf_token, issued.session.csrf_token);
        let error = auth
            .authorize_admin(&issued.cookie_value, Some("wrong"))
            .unwrap_err();
        assert_eq!(error.code(), "csrf_failed");
        assert!(
            auth.authorize_admin(&issued.cookie_value, Some(&refreshed.csrf_token))
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
            auth.check_management_rate_limit().unwrap();
        }
        assert_eq!(
            auth.check_management_rate_limit().unwrap_err().code(),
            "rate_limited"
        );
        // 管理动作限流不会消耗匿名登录流程自己的额度。
        assert!(auth.issue_preauth().is_ok());
    }
}
