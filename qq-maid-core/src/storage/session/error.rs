//! Session 存储错误类型与底层错误映射。

use std::fmt;

use crate::storage::database::DatabaseError;

/// 会话操作错误类型。
#[derive(Debug, Clone)]
pub struct SessionError {
    code: &'static str,
    message: String,
}

impl SessionError {
    pub fn code(&self) -> &str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(super) fn encode(message: impl Into<String>) -> Self {
        Self {
            code: "encode_error",
            message: message.into(),
        }
    }

    pub(super) fn decode(message: impl Into<String>) -> Self {
        Self {
            code: "decode_error",
            message: message.into(),
        }
    }

    pub(super) fn data(message: impl Into<String>) -> Self {
        Self {
            code: "data_error",
            message: message.into(),
        }
    }

    pub(super) fn from_database(err: DatabaseError) -> Self {
        Self {
            code: "database_error",
            message: format!("sqlite database failed: {}", err.message()),
        }
    }

    pub(super) fn from_sql(err: rusqlite::Error) -> Self {
        Self {
            code: "database_error",
            message: format!("sqlite session failed: {err}"),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}
