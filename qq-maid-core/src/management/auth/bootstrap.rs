//! Bootstrap 与密码重置令牌的文件生命周期。
//!
//! 令牌只在本地受保护文件中保存，认证流程本身仍由父模块编排；这里集中处理
//! 令牌格式、过期时间、文件权限和安全摘要，避免这些文件操作混入会话逻辑。

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use super::{
    AdminAuth, AdminAuthError, BOOTSTRAP_PREFIX, BOOTSTRAP_TTL, PASSWORD_RESET_PREFIX,
    database_error, unix_seconds,
};

impl AdminAuth {
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

pub(super) fn random_bootstrap_token() -> String {
    use chacha20poly1305::XChaCha20Poly1305;
    use chacha20poly1305::aead::{Generate, Key};

    let random = Key::<XChaCha20Poly1305>::generate();
    // Bootstrap/重置令牌是短时单次且还要求读取本地文件；128-bit 随机强度足够，
    // 同时比 Cookie/CSRF 使用的 256-bit token 更便于人工输入。
    URL_SAFE_NO_PAD.encode(&random[..16])
}

pub(super) fn token_expiry(token: &BootstrapToken) -> i64 {
    token.issued_at + BOOTSTRAP_TTL.as_secs() as i64
}

pub(super) fn token_is_valid(token: &BootstrapToken) -> bool {
    unix_seconds() <= token_expiry(token)
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
