//! 管理员密码、随机令牌和输入校验。
//!
//! 这些函数只提供认证流程需要的安全原语和确定性校验，不负责会话生命周期或令牌文件。

use std::sync::OnceLock;

use argon2::{Argon2, PasswordHasher, PasswordVerifier, password_hash::phc::PasswordHash};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Generate, Key};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::AdminAuthError;

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

pub(super) fn hash_password(password: &str) -> Result<String, AdminAuthError> {
    // Argon2 0.6 由 password-hash 使用系统安全随机源生成推荐长度 salt；保留
    // Argon2 默认 Argon2id 参数，历史 PHC 字符串仍由 verify_password 兼容读取。
    Argon2::default()
        .hash_password(password.as_bytes())
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
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let random = Key::<XChaCha20Poly1305>::generate();
    let value = URL_SAFE_NO_PAD.encode(random);
    let hash = token_hash(&value);
    (value, hash)
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
