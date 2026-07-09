//! Todo SQLite schema migrations.

use crate::storage::database::SqliteMigration;

/// Todo schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// Todo 使用 SQLite 自增整数作为稳定内部 ID；运行时结构仍以字符串展示 ID，
/// 是为了保持 session 快照、pending 序列化和用户可见 `[id]` 格式稳定。
pub const TODO_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "todo_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            owner_key TEXT NOT NULL,
            user_id TEXT,
            scope_key TEXT NOT NULL,
            title TEXT NOT NULL,
            detail TEXT,
            raw_text TEXT,
            due_date TEXT,
            due_at TEXT,
            time_precision TEXT NOT NULL DEFAULT 'none',
            status TEXT NOT NULL DEFAULT 'pending',
            completed INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_todos_owner_status
            ON todos(owner_key, scope_key, status);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_due
            ON todos(owner_key, scope_key, due_at, due_date, id);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_created
            ON todos(owner_key, scope_key, created_at, id);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_completed
            ON todos(owner_key, scope_key, completed_at, id);",
};

pub const TODO_REMINDER_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "todo_reminder_schema_v2",
    sql: "ALTER TABLE todos ADD COLUMN reminder_at TEXT;
          CREATE INDEX IF NOT EXISTS idx_todos_owner_reminder
              ON todos(owner_key, scope_key, reminder_at, id);",
};

pub const TODO_RECURRENCE_SCHEMA_V3: SqliteMigration = SqliteMigration {
    name: "todo_recurrence_schema_v3",
    sql: "ALTER TABLE todos ADD COLUMN recurrence_kind TEXT NOT NULL DEFAULT 'none';
          ALTER TABLE todos ADD COLUMN recurrence_interval_days INTEGER NOT NULL DEFAULT 0;
          CREATE INDEX IF NOT EXISTS idx_todos_owner_recurrence
              ON todos(owner_key, scope_key, recurrence_kind, recurrence_interval_days, id);",
};

pub const TODO_RECURRENCE_RULE_SCHEMA_V4: SqliteMigration = SqliteMigration {
    name: "todo_recurrence_rule_schema_v4",
    sql: "ALTER TABLE todos ADD COLUMN recurrence_interval INTEGER NOT NULL DEFAULT 0;
          ALTER TABLE todos ADD COLUMN recurrence_unit TEXT NOT NULL DEFAULT 'day';
          CREATE INDEX IF NOT EXISTS idx_todos_owner_recurrence_rule
              ON todos(owner_key, scope_key, recurrence_unit, recurrence_interval, id);",
};

pub const TODO_DAILY_REMINDER_PREF_SCHEMA_V5: SqliteMigration = SqliteMigration {
    name: "todo_daily_reminder_pref_schema_v5",
    sql: "CREATE TABLE IF NOT EXISTS todo_daily_reminder_prefs (
            owner_key TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(owner_key, scope_key)
        );
        CREATE INDEX IF NOT EXISTS idx_todo_daily_reminder_prefs_enabled
            ON todo_daily_reminder_prefs(enabled, owner_key, scope_key);",
};

#[cfg(test)]
pub const TODO_MIGRATIONS: &[SqliteMigration] = &[
    TODO_SCHEMA_V1,
    TODO_REMINDER_SCHEMA_V2,
    TODO_RECURRENCE_SCHEMA_V3,
    TODO_RECURRENCE_RULE_SCHEMA_V4,
    TODO_DAILY_REMINDER_PREF_SCHEMA_V5,
];
