//! Memory SQLite schema，由应用启动阶段统一执行。

use crate::storage::database::SqliteMigration;

/// Memory schema v1：保留早期长期记忆字段与稳定插入顺序。
pub const MEMORY_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "memory_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS memories (
            row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            memory_type TEXT NOT NULL DEFAULT 'note',
            scope TEXT NOT NULL DEFAULT 'general',
            user_id TEXT,
            group_id TEXT,
            content TEXT NOT NULL,
            source_text TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memories_scope_type_order
            ON memories(scope, memory_type, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_user_group_order
            ON memories(user_id, group_id, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_created_order
            ON memories(row_id);",
};

/// Memory schema v2：补充真正的访问边界字段。
///
/// 旧字段 `scope` 只表示记忆业务分类，不能作为权限边界；`scope_type/scope_id`
/// 才表示个人或群记忆归属。缺少稳定标识的旧记录统一进入 `legacy_unassigned`。
pub const MEMORY_SCOPE_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "memory_scope_schema_v2",
    sql: "ALTER TABLE memories ADD COLUMN scope_type TEXT NOT NULL DEFAULT 'legacy_unassigned';
        ALTER TABLE memories ADD COLUMN scope_id TEXT;
        ALTER TABLE memories ADD COLUMN created_by_user_id TEXT;
        UPDATE memories
           SET scope_type = 'personal',
               scope_id = user_id,
               created_by_user_id = user_id
         WHERE user_id IS NOT NULL AND trim(user_id) <> '';
        UPDATE memories
           SET scope_type = 'group',
               scope_id = group_id
         WHERE (scope_type IS NULL OR scope_type = 'legacy_unassigned')
           AND group_id IS NOT NULL AND trim(group_id) <> '';
        UPDATE memories
           SET scope_type = 'legacy_unassigned',
               scope_id = NULL,
               created_by_user_id = NULL
         WHERE scope_type NOT IN ('personal', 'group');
        CREATE INDEX IF NOT EXISTS idx_memories_scope_boundary_order
            ON memories(scope_type, scope_id, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_group_creator_order
            ON memories(scope_type, scope_id, created_by_user_id, row_id);",
};

/// Memory schema v3：把访问边界、记忆范围、画像主体和关系主体拆成独立字段。
///
/// 旧 personal/group 分别确定映射为个人/群组公共记忆；无法证明归属的旧记录保持
/// 隔离并归档。画像 opt-out 使用独立偏好表，便于与画像归档执行原子事务。
pub const MEMORY_DOMAIN_SCHEMA_V3: SqliteMigration = SqliteMigration {
    name: "memory_domain_schema_v3",
    sql: "ALTER TABLE memories ADD COLUMN memory_kind TEXT NOT NULL DEFAULT 'legacy_unassigned';
        ALTER TABLE memories ADD COLUMN subject_id TEXT;
        ALTER TABLE memories ADD COLUMN relation_subject_id TEXT;
        ALTER TABLE memories ADD COLUMN relation_object_id TEXT;
        ALTER TABLE memories ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';
        ALTER TABLE memories ADD COLUMN source_type TEXT NOT NULL DEFAULT 'legacy';
        ALTER TABLE memories ADD COLUMN source_ref TEXT;
        ALTER TABLE memories ADD COLUMN last_confirmed_at TEXT;
        ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
        ALTER TABLE memories ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1));
        ALTER TABLE memories ADD COLUMN attribute_key TEXT;
        UPDATE memories
           SET memory_kind = 'personal',
               visibility = 'private',
               source_type = 'legacy',
               last_confirmed_at = created_at,
               status = 'active',
               subject_id = NULL,
               relation_subject_id = NULL,
               relation_object_id = NULL
         WHERE scope_type = 'personal' AND scope_id IS NOT NULL AND trim(scope_id) <> '';
        UPDATE memories
           SET memory_kind = 'group',
               visibility = 'group_members',
               source_type = 'legacy',
               last_confirmed_at = created_at,
               status = 'active',
               subject_id = NULL,
               relation_subject_id = NULL,
               relation_object_id = NULL
         WHERE scope_type = 'group' AND scope_id IS NOT NULL AND trim(scope_id) <> '';
        UPDATE memories
           SET memory_kind = 'legacy_unassigned',
               visibility = 'private',
               source_type = 'legacy',
               last_confirmed_at = NULL,
               status = 'archived',
               pinned = 0,
               subject_id = NULL,
               relation_subject_id = NULL,
               relation_object_id = NULL,
               attribute_key = NULL
         WHERE scope_type = 'legacy_unassigned' OR scope_id IS NULL OR trim(scope_id) = '';
        CREATE TABLE memory_profile_preferences (
            group_scope_id TEXT NOT NULL,
            subject_id TEXT NOT NULL,
            profile_enabled INTEGER NOT NULL DEFAULT 1 CHECK (profile_enabled IN (0, 1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (group_scope_id, subject_id)
        );
        CREATE INDEX idx_memories_domain_boundary_status_order
            ON memories(scope_type, scope_id, memory_kind, status, row_id);
        CREATE INDEX idx_memories_domain_subject_status_order
            ON memories(scope_type, scope_id, memory_kind, subject_id, status, row_id);
        CREATE INDEX idx_memories_domain_conflict
            ON memories(scope_type, scope_id, memory_kind, subject_id,
                        relation_subject_id, relation_object_id, attribute_key, status);
        CREATE UNIQUE INDEX idx_memories_domain_active_attribute_unique
            ON memories(scope_type, scope_id, memory_kind,
                        ifnull(subject_id, ''), ifnull(relation_subject_id, ''),
                        ifnull(relation_object_id, ''), attribute_key)
         WHERE status = 'active' AND attribute_key IS NOT NULL;
        CREATE INDEX idx_memories_domain_filters
            ON memories(status, visibility, memory_type, pinned, row_id);
        CREATE INDEX idx_memory_profile_preferences_enabled
            ON memory_profile_preferences(profile_enabled, group_scope_id, subject_id);",
};

/// Memory schema v4：后台整理检查点与去重运行摘要。
///
/// `subject_key` 仅用于把 nullable subject_id 变成稳定主键；真实作用域仍由
/// scope_type/scope_id/memory_kind/subject_id 四元组决定。`last_processed_row_id`
/// 是成功批次实际扫描到的边界，不代表全历史已完成跨批去重。表中不保存聊天正文。
pub const MEMORY_CONSOLIDATION_SCHEMA_V4: SqliteMigration = SqliteMigration {
    name: "memory_consolidation_schema_v4",
    sql: "CREATE TABLE IF NOT EXISTS memory_consolidation_state (
            scope_type TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            memory_kind TEXT NOT NULL,
            subject_key TEXT NOT NULL DEFAULT '',
            last_processed_row_id INTEGER NOT NULL DEFAULT 0,
            last_run_at_epoch INTEGER NOT NULL DEFAULT 0,
            last_status TEXT NOT NULL DEFAULT 'never',
            input_count INTEGER NOT NULL DEFAULT 0,
            output_count INTEGER NOT NULL DEFAULT 0,
            duplicate_count INTEGER NOT NULL DEFAULT 0,
            conflict_count INTEGER NOT NULL DEFAULT 0,
            truncated INTEGER NOT NULL DEFAULT 0 CHECK (truncated IN (0, 1)),
            PRIMARY KEY (scope_type, scope_id, memory_kind, subject_key)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_consolidation_due
            ON memory_consolidation_state(last_run_at_epoch, last_processed_row_id);

        CREATE TABLE IF NOT EXISTS memory_dream_state (
            scope_type TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            memory_kind TEXT NOT NULL,
            subject_key TEXT NOT NULL DEFAULT '',
            conversation_scope_key TEXT NOT NULL,
            actor_ref TEXT NOT NULL DEFAULT '',
            last_processed_message_id INTEGER NOT NULL DEFAULT 0,
            last_processed_updated_at TEXT NOT NULL DEFAULT '',
            last_processed_session_id TEXT NOT NULL DEFAULT '',
            last_run_at_epoch INTEGER NOT NULL DEFAULT 0,
            last_status TEXT NOT NULL DEFAULT 'never',
            input_count INTEGER NOT NULL DEFAULT 0,
            output_count INTEGER NOT NULL DEFAULT 0,
            duplicate_count INTEGER NOT NULL DEFAULT 0,
            conflict_count INTEGER NOT NULL DEFAULT 0,
            truncated INTEGER NOT NULL DEFAULT 0 CHECK (truncated IN (0, 1)),
            lock_token TEXT,
            lock_until_epoch INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (scope_type, scope_id, memory_kind, subject_key)
        );
        CREATE INDEX IF NOT EXISTS idx_memory_dream_due
            ON memory_dream_state(last_run_at_epoch, last_processed_updated_at,
                                  last_processed_session_id);",
};

pub const MEMORY_MIGRATIONS: &[SqliteMigration] = &[
    MEMORY_SCHEMA_V1,
    MEMORY_SCOPE_SCHEMA_V2,
    MEMORY_DOMAIN_SCHEMA_V3,
    MEMORY_CONSOLIDATION_SCHEMA_V4,
];
