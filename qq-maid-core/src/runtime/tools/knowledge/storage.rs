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

/// Chunking V2 元数据 migration。
///
/// 知识索引是可重建派生数据，但它和 Todo/Session 等业务数据共用同一个 app.db。
/// 因此这里仅对知识表做幂等增量扩展，不删除或重建整个数据库。
pub const KNOWLEDGE_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "knowledge_schema_v2_chunk_metadata",
    sql: "ALTER TABLE knowledge_documents ADD COLUMN chunking_version INTEGER NOT NULL DEFAULT 1;
        ALTER TABLE knowledge_chunks ADD COLUMN chunk_index INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE knowledge_chunks ADD COLUMN chunk_type TEXT NOT NULL DEFAULT 'text';
        ALTER TABLE knowledge_chunks ADD COLUMN start_line INTEGER;
        ALTER TABLE knowledge_chunks ADD COLUMN end_line INTEGER;
        ALTER TABLE knowledge_chunks ADD COLUMN code_language TEXT;
        ALTER TABLE knowledge_chunks ADD COLUMN chunking_version INTEGER NOT NULL DEFAULT 1;
        CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_document_index
            ON knowledge_chunks(document_id, chunk_index);",
};

/// 本地语义向量独立保存，避免把具体模型和维度固化进可重建的 chunk 主表。
pub const KNOWLEDGE_SCHEMA_V3: SqliteMigration = SqliteMigration {
    name: "knowledge_schema_v3_embeddings",
    sql: "CREATE TABLE IF NOT EXISTS knowledge_chunk_embeddings (
            chunk_id TEXT NOT NULL,
            model TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            embedding_version INTEGER NOT NULL,
            content_hash TEXT NOT NULL,
            vector BLOB NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(chunk_id, model, embedding_version),
            FOREIGN KEY(chunk_id) REFERENCES knowledge_chunks(chunk_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_knowledge_embeddings_model
            ON knowledge_chunk_embeddings(model, embedding_version, dimensions);",
};

pub const KNOWLEDGE_MIGRATIONS: &[SqliteMigration] = &[
    KNOWLEDGE_SCHEMA_V1,
    KNOWLEDGE_SCHEMA_V2,
    KNOWLEDGE_SCHEMA_V3,
];

/// 待写入数据库的知识片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeChunkDraft {
    pub chunk_id: String,
    pub relative_path: String,
    pub document_title: Option<String>,
    pub heading_path: Option<String>,
    pub chunk_index: usize,
    pub chunk_type: String,
    pub body: String,
    pub content_hash: String,
    pub file_hash: String,
    pub modified_at: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub code_language: Option<String>,
    pub chunking_version: i64,
    pub search_text: String,
}

/// 检索返回的知识片段。
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeSearchResult {
    pub document_id: i64,
    pub chunk_id: String,
    pub relative_path: String,
    pub document_title: Option<String>,
    pub heading_path: Option<String>,
    pub chunk_index: usize,
    pub chunk_type: String,
    pub body: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub code_language: Option<String>,
    pub search_text: String,
    pub origin: KnowledgeSearchOrigin,
    pub score: f64,
}

/// 候选来自哪条召回或扩展路径；融合排序在 index 层完成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeSearchOrigin {
    Lexical,
    Semantic,
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeEmbeddingSource {
    pub chunk_id: String,
    pub content_hash: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeEmbeddingRecord {
    pub chunk_id: String,
    pub content_hash: String,
    pub vector: Vec<f32>,
}

/// 单个文档的索引同步状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeDocumentState {
    pub file_hash: String,
    pub chunking_version: i64,
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

    #[cfg(test)]
    pub(crate) fn database_for_test(&self) -> &SqliteDatabase {
        &self.database
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
                "SELECT file_hash, chunking_version FROM knowledge_documents WHERE relative_path = ?1",
                params![relative_path],
                |row| {
                    Ok(KnowledgeDocumentState {
                        file_hash: row.get(0)?,
                        chunking_version: row.get(1)?,
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
        let chunking_version = chunks
            .first()
            .map(|chunk| chunk.chunking_version)
            .unwrap_or(2);
        tx.execute(
            "INSERT INTO knowledge_documents (
                relative_path, file_hash, modified_at, indexed_at, chunking_version
             )
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(relative_path) DO UPDATE SET
                file_hash = excluded.file_hash,
                modified_at = excluded.modified_at,
                indexed_at = excluded.indexed_at,
                chunking_version = excluded.chunking_version",
            params![
                relative_path,
                file_hash,
                modified_at,
                indexed_at,
                chunking_version
            ],
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
            "DELETE FROM knowledge_chunk_embeddings
             WHERE chunk_id IN (
                SELECT chunk_id FROM knowledge_chunks WHERE document_id = ?1
             )",
            params![document_id],
        )
        .map_err(DatabaseError::from_sql)?;
        tx.execute(
            "DELETE FROM knowledge_chunks WHERE document_id = ?1",
            params![document_id],
        )
        .map_err(DatabaseError::from_sql)?;

        for chunk in chunks {
            tx.execute(
                "INSERT INTO knowledge_chunks (
                    chunk_id, document_id, relative_path, document_title, heading_path,
                    chunk_index, chunk_type, body, content_hash, file_hash, modified_at,
                    indexed_at, search_text, start_line, end_line, code_language, chunking_version
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    chunk.chunk_id,
                    document_id,
                    chunk.relative_path,
                    chunk.document_title,
                    chunk.heading_path,
                    chunk.chunk_index as i64,
                    chunk.chunk_type,
                    chunk.body,
                    chunk.content_hash,
                    chunk.file_hash,
                    chunk.modified_at,
                    indexed_at,
                    chunk.search_text,
                    chunk.start_line.map(|line| line as i64),
                    chunk.end_line.map(|line| line as i64),
                    chunk.code_language,
                    chunk.chunking_version,
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
        tx.execute(
            "DELETE FROM knowledge_chunk_embeddings
             WHERE chunk_id IN (
                SELECT c.chunk_id
                FROM knowledge_chunks c
                JOIN knowledge_documents d ON d.id = c.document_id
                WHERE d.relative_path = ?1
             )",
            params![relative_path],
        )
        .map_err(DatabaseError::from_sql)?;
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
                    c.document_id,
                    c.relative_path,
                    c.document_title,
                    c.heading_path,
                    c.chunk_index,
                    c.chunk_type,
                    c.body,
                    c.start_line,
                    c.end_line,
                    c.code_language,
                    c.search_text,
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
                let rank: f64 = row.get(12)?;
                Ok(KnowledgeSearchResult {
                    chunk_id: row.get(0)?,
                    document_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    document_title: row.get(3)?,
                    heading_path: row.get(4)?,
                    chunk_index: row.get::<_, i64>(5)?.max(0) as usize,
                    chunk_type: row.get(6)?,
                    body: row.get(7)?,
                    start_line: row
                        .get::<_, Option<i64>>(8)?
                        .map(|line| line.max(0) as usize),
                    end_line: row
                        .get::<_, Option<i64>>(9)?
                        .map(|line| line.max(0) as usize),
                    code_language: row.get(10)?,
                    search_text: row.get(11)?,
                    origin: KnowledgeSearchOrigin::Lexical,
                    // bm25 越小越相关；对外转成越大越相关的分数，便于诊断理解。
                    score: -rank,
                })
            })
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }

    pub fn section_chunks(
        &self,
        document_id: i64,
        heading_path: Option<&str>,
    ) -> Result<Vec<KnowledgeSearchResult>, DatabaseError> {
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    chunk_id,
                    document_id,
                    relative_path,
                    document_title,
                    heading_path,
                    chunk_index,
                    chunk_type,
                    body,
                    start_line,
                    end_line,
                    code_language,
                    search_text
                 FROM knowledge_chunks
                 WHERE document_id = ?1
                   AND ((?2 IS NULL AND heading_path IS NULL) OR heading_path = ?2)
                 ORDER BY chunk_index",
            )
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map(params![document_id, heading_path], |row| {
                Ok(KnowledgeSearchResult {
                    chunk_id: row.get(0)?,
                    document_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    document_title: row.get(3)?,
                    heading_path: row.get(4)?,
                    chunk_index: row.get::<_, i64>(5)?.max(0) as usize,
                    chunk_type: row.get(6)?,
                    body: row.get(7)?,
                    start_line: row
                        .get::<_, Option<i64>>(8)?
                        .map(|line| line.max(0) as usize),
                    end_line: row
                        .get::<_, Option<i64>>(9)?
                        .map(|line| line.max(0) as usize),
                    code_language: row.get(10)?,
                    search_text: row.get(11)?,
                    origin: KnowledgeSearchOrigin::Section,
                    score: 0.0,
                })
            })
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }

    /// `auto` 紧急回退保留旧邻接策略，正式 Tool 路径使用章节扩展。
    pub fn adjacent_chunks(
        &self,
        document_id: i64,
        chunk_index: usize,
    ) -> Result<Vec<KnowledgeSearchResult>, DatabaseError> {
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT chunk_id, document_id, relative_path, document_title,
                        heading_path, chunk_index, chunk_type, body, start_line,
                        end_line, code_language, search_text
                 FROM knowledge_chunks
                 WHERE document_id = ?1 AND chunk_index IN (?2, ?3)
                 ORDER BY chunk_index",
            )
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map(
                params![document_id, chunk_index as i64 - 1, chunk_index as i64 + 1],
                |row| {
                    Ok(KnowledgeSearchResult {
                        chunk_id: row.get(0)?,
                        document_id: row.get(1)?,
                        relative_path: row.get(2)?,
                        document_title: row.get(3)?,
                        heading_path: row.get(4)?,
                        chunk_index: row.get::<_, i64>(5)?.max(0) as usize,
                        chunk_type: row.get(6)?,
                        body: row.get(7)?,
                        start_line: row
                            .get::<_, Option<i64>>(8)?
                            .map(|line| line.max(0) as usize),
                        end_line: row
                            .get::<_, Option<i64>>(9)?
                            .map(|line| line.max(0) as usize),
                        code_language: row.get(10)?,
                        search_text: row.get(11)?,
                        origin: KnowledgeSearchOrigin::Section,
                        score: 0.0,
                    })
                },
            )
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }

    pub fn missing_embedding_sources(
        &self,
        model: &str,
        embedding_version: i64,
    ) -> Result<Vec<KnowledgeEmbeddingSource>, DatabaseError> {
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT c.chunk_id, c.content_hash,
                        trim(coalesce(c.document_title, '') || char(10) ||
                             coalesce(c.heading_path, '') || char(10) || c.body)
                 FROM knowledge_chunks c
                 LEFT JOIN knowledge_chunk_embeddings e
                   ON e.chunk_id = c.chunk_id
                  AND e.model = ?1
                  AND e.embedding_version = ?2
                  AND e.content_hash = c.content_hash
                 WHERE e.chunk_id IS NULL
                 ORDER BY c.document_id, c.chunk_index",
            )
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map(params![model, embedding_version], |row| {
                Ok(KnowledgeEmbeddingSource {
                    chunk_id: row.get(0)?,
                    content_hash: row.get(1)?,
                    text: row.get(2)?,
                })
            })
            .map_err(DatabaseError::from_sql)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)
    }

    pub fn upsert_embeddings(
        &self,
        model: &str,
        embedding_version: i64,
        records: &[KnowledgeEmbeddingRecord],
    ) -> Result<(), DatabaseError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self.database.connection()?;
        let tx = conn.transaction().map_err(DatabaseError::from_sql)?;
        let updated_at = now_iso_cn();
        for record in records {
            let dimensions = record.vector.len() as i64;
            let vector = encode_vector(&record.vector);
            tx.execute(
                "INSERT INTO knowledge_chunk_embeddings (
                    chunk_id, model, dimensions, embedding_version,
                    content_hash, vector, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(chunk_id, model, embedding_version) DO UPDATE SET
                    dimensions = excluded.dimensions,
                    content_hash = excluded.content_hash,
                    vector = excluded.vector,
                    updated_at = excluded.updated_at",
                params![
                    record.chunk_id,
                    model,
                    dimensions,
                    embedding_version,
                    record.content_hash,
                    vector,
                    updated_at,
                ],
            )
            .map_err(DatabaseError::from_sql)?;
        }
        tx.commit().map_err(DatabaseError::from_sql)
    }

    pub fn semantic_search(
        &self,
        model: &str,
        embedding_version: i64,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<KnowledgeSearchResult>, DatabaseError> {
        if query_vector.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.database.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    c.chunk_id, c.document_id, c.relative_path, c.document_title,
                    c.heading_path, c.chunk_index, c.chunk_type, c.body,
                    c.start_line, c.end_line, c.code_language, c.search_text, e.vector
                 FROM knowledge_chunk_embeddings e
                 JOIN knowledge_chunks c ON c.chunk_id = e.chunk_id
                 WHERE e.model = ?1
                   AND e.embedding_version = ?2
                   AND e.dimensions = ?3
                   AND e.content_hash = c.content_hash",
            )
            .map_err(DatabaseError::from_sql)?;
        let rows = stmt
            .query_map(
                params![model, embedding_version, query_vector.len() as i64],
                |row| {
                    let vector = row.get::<_, Vec<u8>>(12)?;
                    Ok((
                        KnowledgeSearchResult {
                            chunk_id: row.get(0)?,
                            document_id: row.get(1)?,
                            relative_path: row.get(2)?,
                            document_title: row.get(3)?,
                            heading_path: row.get(4)?,
                            chunk_index: row.get::<_, i64>(5)?.max(0) as usize,
                            chunk_type: row.get(6)?,
                            body: row.get(7)?,
                            start_line: row
                                .get::<_, Option<i64>>(8)?
                                .map(|line| line.max(0) as usize),
                            end_line: row
                                .get::<_, Option<i64>>(9)?
                                .map(|line| line.max(0) as usize),
                            code_language: row.get(10)?,
                            search_text: row.get(11)?,
                            origin: KnowledgeSearchOrigin::Semantic,
                            score: 0.0,
                        },
                        vector,
                    ))
                },
            )
            .map_err(DatabaseError::from_sql)?;
        let mut results = Vec::new();
        for row in rows {
            let (mut result, bytes) = row.map_err(DatabaseError::from_sql)?;
            let Some(vector) = decode_vector(&bytes) else {
                continue;
            };
            result.score = cosine_similarity(query_vector, &vector);
            results.push(result);
        }
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        results.truncate(limit);
        Ok(results)
    }
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(std::mem::size_of::<f32>()) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(std::mem::size_of::<f32>())
            .map(|chunk| {
                let bytes: [u8; std::mem::size_of::<f32>()] =
                    chunk.try_into().expect("chunks_exact keeps f32 width");
                f32::from_le_bytes(bytes)
            })
            .collect(),
    )
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (left, right) in left.iter().zip(right) {
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
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
                    chunk_index: 0,
                    chunk_type: "text".to_owned(),
                    body: "编号 RAG-407 用于验证中文知识检索。".to_owned(),
                    content_hash: "chunk-hash".to_owned(),
                    file_hash: "file-hash".to_owned(),
                    modified_at: Some("2026-06-26T00:00:00Z".to_owned()),
                    start_line: Some(3),
                    end_line: Some(3),
                    code_language: None,
                    chunking_version: 2,
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
        assert_eq!(results[0].chunk_index, 0);
        assert_eq!(results[0].start_line, Some(3));

        let missing = store.missing_embedding_sources("fixture-model", 1).unwrap();
        assert_eq!(missing.len(), 1);
        store
            .upsert_embeddings(
                "fixture-model",
                1,
                &[KnowledgeEmbeddingRecord {
                    chunk_id: missing[0].chunk_id.clone(),
                    content_hash: missing[0].content_hash.clone(),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        assert!(
            store
                .missing_embedding_sources("fixture-model", 1)
                .unwrap()
                .is_empty()
        );
        let semantic = store
            .semantic_search("fixture-model", 1, &[1.0, 0.0], 5)
            .unwrap();
        assert_eq!(semantic.len(), 1);
        assert_eq!(semantic[0].origin, KnowledgeSearchOrigin::Semantic);
        assert!((semantic[0].score - 1.0).abs() < f64::EPSILON);

        store.delete_document("example.md").unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);
        assert!(store.search("rag 407", 5).unwrap().is_empty());
        assert!(
            store
                .semantic_search("fixture-model", 1, &[1.0, 0.0], 5)
                .unwrap()
                .is_empty()
        );
    }
}
