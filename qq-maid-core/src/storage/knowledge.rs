//! 本地 Markdown 知识库索引存储。
//!
//! 知识库复用项目级 `APP_DB_FILE`，只保存自动扫描得到的文档与分段索引。
//! 这里不读取文件系统、不理解 Markdown 结构，避免 storage 层承载运行时扫描语义。

use rusqlite::{OptionalExtension, params};

use crate::storage::{
    database::{DatabaseError, SqliteDatabase, SqliteMigration},
    session::now_iso_cn,
};

/// Knowledge schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// 真实片段数据保存在 `knowledge_chunks`；`knowledge_chunks_fts` 只保存面向检索的
/// 规范化文本。两张表在同一事务中更新，便于文件修改和删除时精确清理旧索引。
pub const KNOWLEDGE_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "knowledge_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS knowledge_documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            relative_path TEXT NOT NULL UNIQUE,
            file_hash TEXT NOT NULL,
            modified_at TEXT,
            indexed_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS knowledge_chunks (
            row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            chunk_id TEXT NOT NULL UNIQUE,
            document_id INTEGER NOT NULL,
            relative_path TEXT NOT NULL,
            document_title TEXT,
            heading_path TEXT,
            body TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            file_hash TEXT NOT NULL,
            modified_at TEXT,
            indexed_at TEXT NOT NULL,
            search_text TEXT NOT NULL,
            FOREIGN KEY(document_id) REFERENCES knowledge_documents(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_document
            ON knowledge_chunks(document_id, row_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_chunks_fts USING fts5(search_text);",
};

pub const KNOWLEDGE_MIGRATIONS: &[SqliteMigration] = &[KNOWLEDGE_SCHEMA_V1];

/// 待写入数据库的知识片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeChunkDraft {
    pub chunk_id: String,
    pub relative_path: String,
    pub document_title: Option<String>,
    pub heading_path: Option<String>,
    pub body: String,
    pub content_hash: String,
    pub file_hash: String,
    pub modified_at: Option<String>,
    pub search_text: String,
}

/// 检索返回的知识片段。
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeSearchResult {
    pub chunk_id: String,
    pub relative_path: String,
    pub document_title: Option<String>,
    pub heading_path: Option<String>,
    pub body: String,
    pub score: f64,
}

/// 单个文档的索引同步状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeDocumentState {
    pub file_hash: String,
}

/// Markdown 知识库的 SQLite 存储封装。
#[derive(Debug, Clone)]
pub struct KnowledgeStore {
    database: SqliteDatabase,
}

impl KnowledgeStore {
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 启动时显式探测 FTS5 是否可用。
    ///
    /// migration 中 `CREATE VIRTUAL TABLE` 失败会阻止启动；这里保留独立探针，
    /// 让日志和错误信息更直接指向知识检索依赖。
    pub fn ensure_fts5_available(&self) -> Result<(), DatabaseError> {
        self.database
            .connection()?
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS temp.knowledge_fts5_probe USING fts5(value);
                 DROP TABLE temp.knowledge_fts5_probe;",
            )
            .map_err(DatabaseError::from_sql)
    }

    pub fn document_state(
        &self,
        relative_path: &str,
    ) -> Result<Option<KnowledgeDocumentState>, DatabaseError> {
        self.database
            .connection()?
            .query_row(
                "SELECT file_hash FROM knowledge_documents WHERE relative_path = ?1",
                params![relative_path],
                |row| {
                    Ok(KnowledgeDocumentState {
                        file_hash: row.get(0)?,
                    })
                },
            )
            .optional()
            .map_err(DatabaseError::from_sql)
    }

    pub fn list_document_paths(&self) -> Result<Vec<String>, DatabaseError> {
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare("SELECT relative_path FROM knowledge_documents ORDER BY relative_path")
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }

    pub fn replace_document(
        &self,
        relative_path: &str,
        file_hash: &str,
        modified_at: Option<&str>,
        chunks: &[KnowledgeChunkDraft],
    ) -> Result<(), DatabaseError> {
        let mut conn = self.database.connection()?;
        let tx = conn.transaction().map_err(DatabaseError::from_sql)?;
        let indexed_at = now_iso_cn();
        tx.execute(
            "INSERT INTO knowledge_documents (relative_path, file_hash, modified_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(relative_path) DO UPDATE SET
                file_hash = excluded.file_hash,
                modified_at = excluded.modified_at,
                indexed_at = excluded.indexed_at",
            params![relative_path, file_hash, modified_at, indexed_at],
        )
        .map_err(DatabaseError::from_sql)?;
        let document_id: i64 = tx
            .query_row(
                "SELECT id FROM knowledge_documents WHERE relative_path = ?1",
                params![relative_path],
                |row| row.get(0),
            )
            .map_err(DatabaseError::from_sql)?;
        let mut existing_rows = tx
            .prepare("SELECT row_id FROM knowledge_chunks WHERE document_id = ?1")
            .map_err(DatabaseError::from_sql)?
            .query_map(params![document_id], |row| row.get::<_, i64>(0))
            .map_err(DatabaseError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)?;
        for row_id in existing_rows.drain(..) {
            // 先删除 FTS 行再删除内容行，避免文件更新后旧倒排项继续参与匹配。
            tx.execute(
                "DELETE FROM knowledge_chunks_fts WHERE rowid = ?1",
                params![row_id],
            )
            .map_err(DatabaseError::from_sql)?;
        }
        tx.execute(
            "DELETE FROM knowledge_chunks WHERE document_id = ?1",
            params![document_id],
        )
        .map_err(DatabaseError::from_sql)?;

        for chunk in chunks {
            tx.execute(
                "INSERT INTO knowledge_chunks (
                    chunk_id, document_id, relative_path, document_title, heading_path,
                    body, content_hash, file_hash, modified_at, indexed_at, search_text
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    chunk.chunk_id,
                    document_id,
                    chunk.relative_path,
                    chunk.document_title,
                    chunk.heading_path,
                    chunk.body,
                    chunk.content_hash,
                    chunk.file_hash,
                    chunk.modified_at,
                    indexed_at,
                    chunk.search_text,
                ],
            )
            .map_err(DatabaseError::from_sql)?;
            let row_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO knowledge_chunks_fts(rowid, search_text) VALUES (?1, ?2)",
                params![row_id, chunk.search_text],
            )
            .map_err(DatabaseError::from_sql)?;
        }

        tx.commit().map_err(DatabaseError::from_sql)
    }

    pub fn delete_document(&self, relative_path: &str) -> Result<(), DatabaseError> {
        let mut conn = self.database.connection()?;
        let tx = conn.transaction().map_err(DatabaseError::from_sql)?;
        let mut stmt = tx
            .prepare(
                "SELECT c.row_id
                 FROM knowledge_chunks c
                 JOIN knowledge_documents d ON d.id = c.document_id
                 WHERE d.relative_path = ?1",
            )
            .map_err(DatabaseError::from_sql)?;
        let row_ids = stmt
            .query_map(params![relative_path], |row| row.get::<_, i64>(0))
            .map_err(DatabaseError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)?;
        drop(stmt);
        for row_id in row_ids {
            // 删除文档时先清 FTS，再依赖外键级联删除片段行，避免留下不可见倒排项。
            tx.execute(
                "DELETE FROM knowledge_chunks_fts WHERE rowid = ?1",
                params![row_id],
            )
            .map_err(DatabaseError::from_sql)?;
        }
        tx.execute(
            "DELETE FROM knowledge_documents WHERE relative_path = ?1",
            params![relative_path],
        )
        .map_err(DatabaseError::from_sql)?;
        tx.commit().map_err(DatabaseError::from_sql)
    }

    pub fn chunk_count(&self) -> Result<usize, DatabaseError> {
        self.database
            .connection()?
            .query_row("SELECT COUNT(*) FROM knowledge_chunks", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count.max(0) as usize)
            .map_err(DatabaseError::from_sql)
    }

    /// 使用 FTS5 BM25 排序检索少量高相关片段。
    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchResult>, DatabaseError> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    c.chunk_id,
                    c.relative_path,
                    c.document_title,
                    c.heading_path,
                    c.body,
                    bm25(knowledge_chunks_fts) AS rank
                 FROM knowledge_chunks_fts
                 JOIN knowledge_chunks c ON c.row_id = knowledge_chunks_fts.rowid
                 WHERE knowledge_chunks_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map(params![query, limit as i64], |row| {
                let rank: f64 = row.get(5)?;
                Ok(KnowledgeSearchResult {
                    chunk_id: row.get(0)?,
                    relative_path: row.get(1)?,
                    document_title: row.get(2)?,
                    heading_path: row.get(3)?,
                    body: row.get(4)?,
                    // bm25 越小越相关；对外转成越大越相关的分数，便于诊断理解。
                    score: -rank,
                })
            })
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::SqliteDatabase;

    fn test_store() -> KnowledgeStore {
        KnowledgeStore::new(
            SqliteDatabase::open_temp("qq-maid-knowledge", KNOWLEDGE_MIGRATIONS).unwrap(),
        )
    }

    #[test]
    fn replace_search_and_delete_document() {
        let store = test_store();
        store.ensure_fts5_available().unwrap();
        store
            .replace_document(
                "example.md",
                "file-hash",
                Some("2026-06-26T00:00:00Z"),
                &[KnowledgeChunkDraft {
                    chunk_id: "example-md-0001-abcd".to_owned(),
                    relative_path: "example.md".to_owned(),
                    document_title: Some("知识示例".to_owned()),
                    heading_path: Some("知识示例 / 中文检索".to_owned()),
                    body: "编号 RAG-407 用于验证中文知识检索。".to_owned(),
                    content_hash: "chunk-hash".to_owned(),
                    file_hash: "file-hash".to_owned(),
                    modified_at: Some("2026-06-26T00:00:00Z".to_owned()),
                    search_text: "编号 rag 407 中文 检索 知识".to_owned(),
                }],
            )
            .unwrap();

        assert_eq!(store.chunk_count().unwrap(), 1);
        assert_eq!(
            store
                .document_state("example.md")
                .unwrap()
                .unwrap()
                .file_hash,
            "file-hash"
        );
        let results = store.search("rag 407", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relative_path, "example.md");

        store.delete_document("example.md").unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);
        assert!(store.search("rag 407", 5).unwrap().is_empty());
    }
}
