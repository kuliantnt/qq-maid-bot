//! Web 管理面的通用安全边界。
//!
//! 这里的部署管理员与聊天 session、平台用户和群角色完全分离。配置 WebUI 与后续
//! Memory WebUI 必须复用同一认证主体、服务端会话、CSRF 和审计能力，不能各自签发身份。

mod auth;

pub use auth::{
    AdminAuth, AdminAuthError, AdminBootstrapStatus, AdminSession, CONSOLE_ADMIN_SCHEMA_V1,
    SESSION_COOKIE_NAME,
};
