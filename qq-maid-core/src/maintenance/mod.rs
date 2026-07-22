//! 无 Web 环境也可复用的配置迁移、备份和恢复能力。
//!
//! CLI 只负责参数与结果渲染；涉及配置来源、SQLite 一致性和文件安全的规则集中在这里，
//! 避免 Docker、Web 与 Release 脚本各自维护一套近似实现。

mod backup;
mod config_migration;

pub use backup::{
    BackupError, BackupManifest, BackupOptions, BackupReport, RestorePlan, create_backup,
    plan_restore, restore_backup, verify_backup,
};
pub use config_migration::{
    ConfigMigrationAction, ConfigMigrationEntry, ConfigMigrationError, ConfigMigrationKind,
    ConfigMigrationPlan, ConfigMigrationReport, apply_config_migration, plan_config_migration,
};
