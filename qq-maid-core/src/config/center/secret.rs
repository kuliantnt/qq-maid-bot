use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, Key, KeyInit, Payload},
};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::storage::database::{SqliteDatabase, SqliteMigration};

use super::ConfigCenterError;

const MASTER_KEY_PREFIX: &str = "qq-maid-master-key-v1:";
const SECRET_ALGORITHM: &str = "xchacha20poly1305";
const SECRET_VERSION: i64 = 1;
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;

pub const CONFIG_SECRET_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "config_secret_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS config_secrets (
            key TEXT PRIMARY KEY,
            algorithm TEXT NOT NULL,
            version INTEGER NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL,
            updated_at INTEGER NOT NULL
          );",
};

#[derive(Clone)]
pub struct SecretStore {
    database: SqliteDatabase,
    cipher: XChaCha20Poly1305,
}

impl SecretStore {
    pub fn open(
        database: SqliteDatabase,
        master_key_path: &Path,
    ) -> Result<Self, ConfigCenterError> {
        let cipher = load_or_create_master_key(master_key_path)?;
        Ok(Self { database, cipher })
    }

    pub fn configured_keys(&self) -> Result<HashSet<String>, ConfigCenterError> {
        let connection = self.database.connection().map_err(|err| {
            ConfigCenterError::secret(format!("failed to open secret database: {err}"))
        })?;
        let mut statement = connection
            .prepare("SELECT key FROM config_secrets ORDER BY key")
            .map_err(secret_database_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(secret_database_error)?;
        let mut keys = HashSet::new();
        for row in rows {
            keys.insert(row.map_err(secret_database_error)?);
        }
        Ok(keys)
    }

    /// 返回密文 envelope 的不可逆指纹，用于判断启动后是否替换/清除了 secret。
    /// 指纹不包含明文，也不会通过 API 暴露。
    pub fn envelope_revisions(&self) -> Result<HashMap<String, String>, ConfigCenterError> {
        let connection = self.database.connection().map_err(|err| {
            ConfigCenterError::secret(format!("failed to open secret database: {err}"))
        })?;
        let mut statement = connection
            .prepare(
                "SELECT key, algorithm, version, nonce, ciphertext FROM config_secrets ORDER BY key",
            )
            .map_err(secret_database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })
            .map_err(secret_database_error)?;
        let mut revisions = HashMap::new();
        for row in rows {
            let (key, algorithm, version, nonce, ciphertext) =
                row.map_err(secret_database_error)?;
            let mut digest = Sha256::new();
            digest.update(algorithm.as_bytes());
            digest.update(version.to_be_bytes());
            digest.update(&nonce);
            digest.update(&ciphertext);
            let digest = digest.finalize();
            revisions.insert(
                key,
                format!("sha256:{}", STANDARD_NO_PAD.encode(&digest[..])),
            );
        }
        Ok(revisions)
    }

    pub fn replace(&self, key: &str, plaintext: &[u8]) -> Result<(), ConfigCenterError> {
        let nonce = XNonce::generate();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: key.as_bytes(),
                },
            )
            .map_err(|_| ConfigCenterError::secret("failed to encrypt secret value"))?;
        let updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| ConfigCenterError::secret("system clock is before Unix epoch"))?
            .as_secs() as i64;
        let connection = self.database.connection().map_err(|err| {
            ConfigCenterError::secret(format!("failed to open secret database: {err}"))
        })?;
        connection
            .execute(
                "INSERT INTO config_secrets (key, algorithm, version, nonce, ciphertext, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(key) DO UPDATE SET
                    algorithm = excluded.algorithm,
                    version = excluded.version,
                    nonce = excluded.nonce,
                    ciphertext = excluded.ciphertext,
                    updated_at = excluded.updated_at",
                params![
                    key,
                    SECRET_ALGORITHM,
                    SECRET_VERSION,
                    nonce.as_slice(),
                    ciphertext,
                    updated_at
                ],
            )
            .map_err(secret_database_error)?;
        Ok(())
    }

    pub fn read(&self, key: &str) -> Result<Option<Vec<u8>>, ConfigCenterError> {
        let connection = self.database.connection().map_err(|err| {
            ConfigCenterError::secret(format!("failed to open secret database: {err}"))
        })?;
        let row = connection
            .query_row(
                "SELECT algorithm, version, nonce, ciphertext FROM config_secrets WHERE key = ?1",
                [key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(secret_database_error)?;
        let Some((algorithm, version, nonce, ciphertext)) = row else {
            return Ok(None);
        };
        if algorithm != SECRET_ALGORITHM || version != SECRET_VERSION || nonce.len() != NONCE_BYTES
        {
            return Err(ConfigCenterError::secret(format!(
                "stored secret `{key}` uses an unsupported envelope"
            )));
        }
        let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| {
            ConfigCenterError::secret(format!("stored secret `{key}` has an invalid nonce"))
        })?;
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: key.as_bytes(),
                },
            )
            .map(Some)
            .map_err(|_| {
                ConfigCenterError::secret(format!("stored secret `{key}` failed authentication"))
            })
    }

    pub fn clear(&self, key: &str) -> Result<bool, ConfigCenterError> {
        let connection = self.database.connection().map_err(|err| {
            ConfigCenterError::secret(format!("failed to open secret database: {err}"))
        })?;
        let changed = connection
            .execute("DELETE FROM config_secrets WHERE key = ?1", [key])
            .map_err(secret_database_error)?;
        Ok(changed > 0)
    }
}

fn load_or_create_master_key(path: &Path) -> Result<XChaCha20Poly1305, ConfigCenterError> {
    match read_master_key(path)? {
        Some(cipher) => Ok(cipher),
        None => create_master_key(path),
    }
}

fn read_master_key(path: &Path) -> Result<Option<XChaCha20Poly1305>, ConfigCenterError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ConfigCenterError::secret(format!(
                "failed to inspect master key file: {err}"
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ConfigCenterError::secret(
            "master key path must be a regular file and must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(ConfigCenterError::secret(
                "master key file permissions must not grant group or other access",
            ));
        }
    }
    let mut text = String::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .and_then(|mut file| file.read_to_string(&mut text))
        .map_err(|err| {
            ConfigCenterError::secret(format!("failed to read master key file: {err}"))
        })?;
    let encoded = text
        .trim()
        .strip_prefix(MASTER_KEY_PREFIX)
        .ok_or_else(|| ConfigCenterError::secret("master key file has an invalid format"))?;
    let mut decoded = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| ConfigCenterError::secret("master key file has an invalid format"))?;
    if decoded.len() != KEY_BYTES {
        decoded.fill(0);
        return Err(ConfigCenterError::secret(
            "master key file has an invalid length",
        ));
    }
    let mut key = Key::<XChaCha20Poly1305>::try_from(decoded.as_slice())
        .map_err(|_| ConfigCenterError::secret("master key file has an invalid length"))?;
    let cipher = XChaCha20Poly1305::new(&key);
    key.as_mut_slice().fill(0);
    decoded.fill(0);
    Ok(Some(cipher))
}

fn create_master_key(path: &Path) -> Result<XChaCha20Poly1305, ConfigCenterError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    create_secret_directory(parent)?;
    if let Some(cipher) = read_master_key(path)? {
        return Ok(cipher);
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("master.key");
    let temp_path = parent.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut key = Key::<XChaCha20Poly1305>::generate();
    let encoded = format!(
        "{MASTER_KEY_PREFIX}{}\n",
        STANDARD_NO_PAD.encode(key.as_slice())
    );
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path).map_err(|err| {
            ConfigCenterError::secret(format!("failed to create master key temp file: {err}"))
        })?;
        file.write_all(encoded.as_bytes()).map_err(|err| {
            ConfigCenterError::secret(format!("failed to write master key temp file: {err}"))
        })?;
        file.sync_all().map_err(|err| {
            ConfigCenterError::secret(format!("failed to sync master key temp file: {err}"))
        })?;
        match fs::hard_link(&temp_path, path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return read_master_key(path)?.ok_or_else(|| {
                    ConfigCenterError::secret("master key disappeared during atomic creation")
                });
            }
            Err(err) => {
                return Err(ConfigCenterError::secret(format!(
                    "failed to install master key atomically: {err}"
                )));
            }
        }
        sync_directory(parent)?;
        Ok(XChaCha20Poly1305::new(&key))
    })();
    key.as_mut_slice().fill(0);
    let _ = fs::remove_file(&temp_path);
    result
}

fn create_secret_directory(path: &Path) -> Result<(), ConfigCenterError> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(path).map_err(|err| {
                ConfigCenterError::secret(format!("failed to create secret directory: {err}"))
            })?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path).map_err(|err| {
            ConfigCenterError::secret(format!("failed to create secret directory: {err}"))
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        ConfigCenterError::secret(format!("failed to inspect secret directory: {err}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigCenterError::secret(
            "master key parent must be a directory and must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|err| {
            ConfigCenterError::secret(format!(
                "failed to restrict master key directory permissions: {err}"
            ))
        })?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ConfigCenterError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| {
            ConfigCenterError::secret(format!("failed to sync secret directory: {err}"))
        })?;
    Ok(())
}

fn secret_database_error(err: rusqlite::Error) -> ConfigCenterError {
    ConfigCenterError::secret(format!("secret database operation failed: {err}"))
}
