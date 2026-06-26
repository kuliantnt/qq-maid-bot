# 任务：RAG 切片与检索 V2 改造

## 1. 状态

Planned。

本文档基于当前仓库实现整理，目标是为后续开发提供可直接执行的任务规格。本轮只编写规划文档，不修改 Rust 生产代码、不执行数据库 migration、不引入 embedding 依赖、不改变现有检索行为。

当前实现是基于 Markdown 文件、SQLite FTS5、BM25 和 n-gram 的轻量级本地关键词检索，不是语义向量 RAG。V2 应保留现有 FTS5 精确检索能力，并为未来 embedding 混合检索预留边界。

## 2. 现状分析

当前本地知识检索主链路如下：

```text
Markdown 文件
→ qq-maid-core/src/runtime/knowledge/mod.rs::scan_markdown_files
→ qq-maid-core/src/runtime/knowledge/mod.rs::chunk_markdown
→ ChunkBuilder 维护 document_title / heading_path / body / content_hash / search_text
→ qq-maid-core/src/runtime/knowledge/mod.rs::build_index_text
→ qq-maid-core/src/storage/knowledge.rs::KnowledgeStore::replace_document
→ knowledge_documents / knowledge_chunks / knowledge_chunks_fts 写入
→ qq-maid-core/src/runtime/respond/chat_flow.rs::handle_chat
→ KnowledgeIndex::search_context
→ build_search_query
→ KnowledgeStore::search
→ SQLite FTS5 MATCH + bm25(knowledge_chunks_fts)
→ select_results
→ render_context
→ RespondRequest.knowledge_context
→ qq-maid-core/src/runtime/respond/llm_service.rs::build_chat_messages
→ LLM system message
```

启动期同步链路：

* `qq-maid-core/src/app/mod.rs::LlmRuntime::from_config` 打开 `APP_DB_FILE` 指向的统一 SQLite，并执行 `APP_MIGRATIONS`。
* `APP_MIGRATIONS` 在 `qq-maid-core/src/storage/migrations.rs` 中聚合各业务 schema，末尾包含 `KNOWLEDGE_SCHEMA_V1`。
* `KnowledgeIndex::sync()` 启动时执行知识目录同步。目录不存在或为空会正常降级；数据库或 FTS5 错误会阻止启动，避免索引损坏被伪装成“无命中”。
* 默认知识目录为 `qq-maid-core/src/config.rs::DEFAULT_KNOWLEDGE_DIR = "config/knowledge"`，可通过 `KNOWLEDGE_DIR` 覆盖。

文档扫描规则：

* 递归扫描知识目录。
* 跳过隐藏文件、临时文件、备份文件和符号链接。
* 仅接受 `.md` / `.markdown`。
* 跳过 `*.example.md` / `*.example.markdown`，因此公开模板不会进入真实索引。
* 相对路径统一为 slash 分隔。

当前切片入口：

```rust
fn chunk_markdown(relative_path: &str, content: &str, _file_hash: &str) -> Vec<MarkdownChunk>
```

当前常量位于 `qq-maid-core/src/runtime/knowledge/mod.rs`：

```text
MAX_CHUNK_CHARS = 1200
MIN_CHUNK_CHARS = 8
SEARCH_CONTEXT_LIMIT = 4
SEARCH_TOTAL_CHAR_BUDGET = 3200
MAX_RESULTS_PER_FILE = 2
MAX_SEARCH_QUERY_TOKENS = 64
SEARCH_CANDIDATE_LIMIT = SEARCH_CONTEXT_LIMIT * MAX_RESULTS_PER_FILE * 4 = 32
```

当前 Markdown 切片规则：

* 使用 `content.lines()` 逐行扫描。
* 非代码块内遇到 Markdown 标题时先 `flush()` 当前 buffer，再通过 `set_heading()` 更新标题栈。
* 标题行本身不进入 `body` 正文，只进入 `document_title`、`heading_path` 和 `search_text`。
* 非代码块内遇到空行时 `flush()`。
* 非代码块内当前 buffer 字符数达到 `MAX_CHUNK_CHARS` 时 `flush()`。
* `is_markdown_heading()` 只识别 `#` 到 `######` 后面跟空白的 ATX 标题。
* 第一个一级标题作为 `document_title`。
* 标题栈按层级维护，生成 `heading_path`，当前格式为用 ` / ` 拼接。
* `flush()` 时，少于 `MIN_CHUNK_CHARS` 的正文直接丢弃。
* 如果正文超过 `MAX_CHUNK_CHARS` 且不包含代码围栏，则调用 `split_long_text()`。

当前代码围栏识别规则：

* trim 后以 ````` 或 `~~~` 开头时翻转 `in_code_block`。
* 不区分围栏类型、围栏长度、代码语言，也不校验闭合围栏是否与开启围栏一致。
* 代码块内不识别标题、不因空行 flush、不因普通字符上限 flush。
* 含代码围栏的 body 不调用 `split_long_text()`，因此长代码块可能生成超过 1200 字符的 chunk。

当前 `split_long_text()`：

* 位于 `qq-maid-core/src/runtime/knowledge/mod.rs`。
* 仅在 `body.chars().count() > MAX_CHUNK_CHARS && !contains_code_fence(&body)` 时调用。
* 按 Rust `chars()` 逐字符累积，达到 `max_chars` 就硬切。
* 不识别段落、列表、句子、标点或 Markdown 结构。

当前 chunk 字段：

* 运行时内部 `MarkdownChunk` 包含 `chunk_id`、`relative_path`、`document_title`、`heading_path`、`body`、`content_hash`、`search_text`。
* 写入存储时转换为 `KnowledgeChunkDraft`，额外包含 `file_hash` 和 `modified_at`。
* `chunk_id` 形如 `stable_path_id(relative_path)-{path_hash}:{index:04}:{short_hash}`。slug 只用于可读性，路径 hash 用于避免 `a-b.md` / `a/b.md` 或中文路径归一化碰撞。

当前存储结构：

* `qq-maid-core/src/storage/knowledge.rs::KNOWLEDGE_SCHEMA_V1` 创建 `knowledge_documents`、`knowledge_chunks`、`idx_knowledge_chunks_document` 和 `knowledge_chunks_fts`。
* `knowledge_chunks` 保存正文和元数据，字段包括 `row_id`、`chunk_id`、`document_id`、`relative_path`、`document_title`、`heading_path`、`body`、`content_hash`、`file_hash`、`modified_at`、`indexed_at`、`search_text`。
* `knowledge_chunks_fts` 是 `USING fts5(search_text)` 的独立虚表，不是 external-content 表。
* 写入时手动保持 `knowledge_chunks.row_id == knowledge_chunks_fts.rowid`。
* 更新文档时，`replace_document()` 先删除旧 FTS 行，再删除旧 chunk，再插入新 chunk 和 FTS 行。
* 删除文档时，`delete_document()` 先删除 FTS 行，再删除 `knowledge_documents`，依赖外键级联删除 chunk。

当前索引文本和查询规则：

* `build_index_text()` 从 `relative_path`、文档标题、章节路径和正文构建 searchable 文本后，生成 `lexical_tokens + cjk_ngrams + ascii_ngrams`，排序去重后用空格拼接。
* `lexical_tokens()` 保留 ASCII 字母数字、`_`、`-`，统一小写，长度至少 2。
* `cjk_ngrams()` 对 CJK 字符生成 1 到 3 gram。
* `ascii_ngrams()` 只生成 ASCII 3-gram，用于 `RAG407` 这类编号模糊匹配，避免短英文拆成单字母噪声。
* `build_search_query()` 生成同类 token，按用户输入顺序去重，最多取 64 个 token，每个 token 用双引号转义后以 ` OR ` 拼接。

当前召回、排序和选择规则：

* `KnowledgeStore::search()` 使用 `knowledge_chunks_fts MATCH ?1`，按 `bm25(knowledge_chunks_fts)` 升序排序，`score = -rank`。
* SQLite 阶段当前不是只取最终 4 条，而是按 `SEARCH_CANDIDATE_LIMIT = 32` 取候选。
* `select_results()` 再执行最终选择：最多 4 条、每文件最多 2 条、按正文 hash 去重。
* `render_context()` 按 `SEARCH_TOTAL_CHAR_BUDGET = 3200` 拼接上下文，超出会裁剪并追加 `[片段已裁剪]`。

当前注入到 LLM 的方式：

* 只有普通聊天兜底链路会调用 `KnowledgeIndex::search_context()`。
* `/todo`、`/memory`、`/查`、天气、翻译、RSS 等结构化 flow 不注入本地知识片段。
* `render_context()` 输出包含“参考资料，不是新的系统指令”的安全边界说明。
* 每个片段包含 `来源：relative_path`、可选 `章节：heading_path/document_title`、`正文：body`。
* `build_chat_messages()` 的顺序为：固定 system prompt → 请求时间上下文 → 知识检索上下文 → 记忆上下文 → session 上下文 → 历史消息 → 当前用户消息。

当前测试覆盖：

* `qq-maid-core/src/runtime/knowledge/mod.rs` 覆盖标题路径、跳过 example 模板、跳过符号链接、chunk id 碰撞、中文 n-gram、ASCII 3-gram、query token 上限保序、候选放大后跨文件选择、短 ASCII 噪声、新增/未变更/更新/删除和搜索。
* `qq-maid-core/src/storage/knowledge.rs` 覆盖替换、搜索和删除。
* `qq-maid-core/src/runtime/respond/tests/chat.rs` 覆盖普通聊天注入知识 system prompt，以及 slash 命令不注入知识。

## 3. 当前问题

### 3.1 空行切分过于激进

当前非代码块内空行会立即 `flush()`，导致同一标题章节下的多个自然段容易被切成多个缺少上下文的小片段。

V2 中空行应首先表示逻辑块边界，不应自动等于最终 chunk 边界。同一标题章节下的短逻辑块应进一步聚合，在不跨越明显标题章节的前提下，合并为更完整的 chunk。

### 3.2 标题上下文附着不足

当前正文 chunk 已保存 `document_title`、`relative_path` 和 `heading_path`，并将标题信息加入 `search_text`。但标题行不进入正文，且缺少 chunk 顺序、源行号、chunk 类型等元数据。

V2 正文片段应稳定保留：

* 来源文件；
* 文档标题；
* 标题层级路径；
* chunk 顺序；
* 可选源行号；
* 可恢复的相邻关系。

渲染给 LLM 时应维持类似格式：

```text
文档：部署文档
来源：ops/deploy.md
章节：LLM 配置 > 请求超时

设置 REQUEST_TIMEOUT_SECS。
```

### 3.3 长文本按字符硬切

当前 `split_long_text()` 按 `chars()` 达到上限就切，可能在句子中间、列表项中间或中英文混合表达中间截断。

V2 应按自然切点优先拆分长文本。建议切点优先级：

1. 段落或逻辑块边界；
2. 列表项边界；
3. 中文句号、问号、感叹号；
4. 英文句号、问号、感叹号；
5. 分号；
6. 逗号；
7. 最后才按字符硬切。

具体实现可以根据现有代码结构调整，但行为目标必须明确：一般情况下不在句子中间硬切，除非不存在合理切点或单句本身超过硬上限。

### 3.4 无上下文重叠或邻接关系

当前 chunk 没有稳定持久化的 `chunk_index`、源行号或可查询的邻接能力，也没有句子级 overlap。跨 chunk 的信息可能无法完整召回，尤其是定义在前一段、配置值在后一段的文档。

V2 默认采用 `chunk_index + 检索后邻接补全`：

* 必须持久化同一文档内连续的 `chunk_index`。
* 相邻关系优先通过 `document_id + chunk_index` 动态恢复，不默认持久化 `previous_chunk_id` / `next_chunk_id`。
* 允许检索命中后按需补充同文档相邻 chunk，但必须受最终上下文预算控制。
* 仅当一个超长自然语言逻辑块被拆成多个连续 chunk 时，允许保留上一片段最后一个完整句子作为有限 overlap。
* 普通段落聚合、代码块和表格默认不添加 overlap，避免检索命中、overlap 和邻接补全叠加后产生大量重复。
* 不建议使用固定字符数 overlap 作为默认方案，因为它会制造重复内容、浪费上下文，并可能截断 Unicode 或 Markdown 结构。

### 3.5 长代码块没有大小限制

当前包含代码围栏的 body 不调用 `split_long_text()`，代码块内也不会因字符上限 flush。超长代码块可能形成巨型 chunk，降低 BM25 精度并浪费 LLM 上下文。

V2 应定义代码块策略：

* 小代码块与前置说明尽量保留在同一 chunk。
* 大代码块允许按行数或字符数拆分。
* 拆分后每段应重新补全代码围栏，避免渲染给 LLM 时结构损坏。
* 保留代码语言、标题路径、源文件和原始顺序。
* 第一版不要求完整语言语法分析，不做 AST 级切分。

### 3.6 过短片段处理粗糙

当前少于 `MIN_CHUNK_CHARS = 8` 的 body 直接丢弃。短片段可能是关键配置项、命令、错误码或短定义，不应仅因字符少而丢失。

V2 应改为：

* 短片段优先与同章节前后逻辑块合并。
* 代码块、列表项、表格单元和带行内代码的内容，不因长度过短直接丢弃。
* 包含 ASCII 标识符、数字编号、路径、环境变量形式或键值形式的短文本可以保留，例如 `REQUEST_TIMEOUT`、`/foo`、`E1001`、`config.toml`、`timeout = 30`。
* 仅纯标点、分隔符、空内容和无法合并的装饰文本允许丢弃。
* 不应只依靠固定字符数判断内容价值。
* 不调用 LLM 判断短片段是否重要，保留规则必须是确定性、可测试的。

### 3.7 FTS 查询噪声和过早选择

当前 `build_search_query()` 使用大量 OR token，有利于中文和编号召回，但可能引入噪声。当前实现已经先取 32 个候选再最终选择，不是直接从 SQLite 取 4 条；但候选数量、去重、多样性、邻接补全和最终预算仍散落在单个模块常量中，选择逻辑也较简单。

V2 应明确保留并强化以下流程：

```text
FTS5 初召回较多候选
→ BM25 初排
→ 去重
→ 文件与章节多样性处理
→ 可选相邻 chunk 补全
→ 总上下文预算控制
→ 最终选择有限片段
```

“每文件最多若干片段”的限制应保留在最终选择阶段，而不是 SQLite 初召回阶段。候选数、最终片段数、每文件限制和总字符预算应集中为常量或配置结构，避免散落硬编码。

## 4. Chunking V2 目标结构

V2 应引入逻辑块层，但不在本文档中锁死具体 Rust 类型。可参考概念：

```rust
enum MarkdownBlock {
    Heading,
    Paragraph,
    List,
    BlockQuote,
    Code,
    Table,
}
```

实现约束：

* 以仓库现有依赖和代码结构为准。
* 当前未发现 Markdown parser 依赖；若后续实现发现已有 parser，应先评估复用。
* 如果没有 parser，第一版可实现轻量状态机。
* 不要为了 V2 引入过重依赖。
* 第一版不要求完整支持所有 Markdown 语法，但必须覆盖测试计划中的代表性样本。

建议处理流程：

```text
Markdown 原文
→ 识别逻辑块
→ 维护标题栈
→ 给逻辑块附加标题路径、源文件和源行号
→ 同章节内聚合逻辑块
→ 超限内容按自然边界拆分
→ 对长代码块单独拆分并补全围栏
→ 持久化 chunk_index、源行号和 chunking_version
→ 生成最终 chunk
→ 构建 index_text
```

逻辑块边界应承担“结构识别”职责，最终 chunk 边界应由章节、目标大小、软上限、硬上限和内容类型共同决定。

## 5. Chunk 大小策略

V2 应从单一 `MAX_CHUNK_CHARS` 改为目标大小、软上限、硬上限的概念：

```text
目标大小：600～900 字符
软上限：约 1200 字符
硬上限：约 1600 字符
```

要求：

* 具体数值需结合当前上下文窗口、文档特点和测试结果确定。
* 字符数计算方式应继续使用 Rust `chars()`，除非实施阶段明确说明调整原因。
* 不为凑目标大小跨越明显标题章节。
* 普通文本超过软上限时优先寻找自然切点。
* 单句或单个列表项超过硬上限时允许最后按字符硬切。
* 代码块应有单独限制，例如按字符数和行数双阈值控制。
* 表格可按行聚合，超长表格按表头加行块拆分，第一版不要求复杂表格语义分析。

## 6. 元数据和存储设计

当前 schema 已有：

```text
knowledge_documents:
  id
  relative_path
  file_hash
  modified_at
  indexed_at

knowledge_chunks:
  row_id
  chunk_id
  document_id
  relative_path
  document_title
  heading_path
  body
  content_hash
  file_hash
  modified_at
  indexed_at
  search_text

knowledge_chunks_fts:
  search_text
```

V2 建议字段：

```text
id / row_id
document_id
source_path / relative_path
document_title
heading_path
chunk_index
chunk_type
body
index_text / search_text
content_hash
file_hash
start_line
end_line
code_language
chunking_version
```

Chunking V2 必须字段：

* `chunk_index`：同一文档内连续顺序，用于恢复邻接关系和稳定调试。
* `chunk_type`：至少区分普通文本、代码、表格或混合内容。
* `start_line` / `end_line`：用于定位来源和测试断言。
* `code_language`：代码块 chunk 可记录围栏语言。
* `chunking_version`：切片算法版本，用于算法升级后强制重建索引。

可后续增加字段：

* rerank 分数或融合分数。
* 更细粒度块类型。

相邻关系策略：

* `chunk_index` 是必须持久化字段。
* 相邻 chunk 优先通过 `document_id + chunk_index` 动态查询，例如 `chunk_index = current_index - 1` 或 `+ 1`。
* 不默认持久化 `previous_chunk_id` / `next_chunk_id`，避免整篇 `replace_document()` 后维护三套关系、插入删除中间 chunk 后产生悬空引用。
* 只有实际查询性能或跨版本稳定性证明有必要时，才考虑显式邻接字段。

Embedding 预留策略：

* Chunking V2 不在 `knowledge_chunks` 中预留具体 embedding 列。
* 未来接入向量检索时，优先使用独立 embedding 表或独立向量存储，通过 `chunk_id + content_hash` 关联。
* 独立表可参考字段：`chunk_id`、`provider`、`model`、`dimensions`、`embedding_version`、`content_hash`、`updated_at`、`vector` 或 `external_vector_id`。
* 这样可以支持不同模型、维度、供应商和重建中的新旧向量，避免当前 schema 与某个 embedding 模型绑定。

兼容策略：

* 本次文档不执行 migration。
* 后续实现若只在运行时内部新增字段，可以先不改 schema。
* 若需要持久化 chunk 顺序、源行号、chunk 类型或 `chunking_version`，应新增 `KNOWLEDGE_SCHEMA_V2` 或等价幂等 migration。
* V2 实施时必须设计切片算法版本机制。切片版本变化时，即使 Markdown 文件内容、`file_hash` 和 `modified_at` 未变化，也必须重新建立该文档的 chunk 和 FTS 索引。
* `chunking_version` 可放在 `knowledge_documents`、独立 metadata 表或全局知识索引版本配置中，不要求每个 chunk 都重复保存。
* migration 必须幂等、前向兼容，并且只能修改知识相关表或知识索引 metadata。
* 知识索引属于可重建派生数据，但统一 `app.db` 中的 Todo、RSS、Session、Memory 等业务数据不可丢失。
* 升级前可以备份整个数据库；正常重建只能清理并重建 `knowledge_documents`、`knowledge_chunks`、`knowledge_chunks_fts` 等知识索引数据，不得删除或重建整个 `app.db`。
* 不要求生产代码自动执行 down migration。回退旧版本前需要备份，或通过清理知识索引表并重新同步 Markdown 恢复。

## 7. FTS 检索优化方案

V2 必须保留 SQLite FTS5，不直接替换为 embedding。

优化要求：

1. 当前 n-gram 精确检索能力继续保留，尤其是配置名、函数名、错误码、环境变量、编号等。
2. FTS 初召回应保留较多候选，候选数量集中管理。
3. 每文件数量限制放在最终选择阶段。
4. 对高度重复结果去重，当前按正文 hash 去重可保留，但应评估相邻 chunk 和有限 overlap 带来的近重复。
5. 对多个来源文件做适度多样化，避免单一文件吞掉全部结果。
6. 同一文档强命中时允许返回更多片段，但必须受最终总数、每文件上限或上下文预算控制；可以考虑“强命中文档相邻补全不计入普通多样性槽位”，但要测试防止过量。
7. 命中 chunk 后可按需补充 `previous` / `next` chunk，补全内容应标记来源并受预算控制。
8. 控制最终交给 LLM 的总字符数或 token 预算；当前 3200 字符预算可作为基线。
9. 保留 BM25。FTS 多列和字段权重不属于 Chunking V2 必做项；只有基准查询证明单列 `search_text` 的排序存在稳定问题时，才作为独立优化任务实施。
10. 查询无结果时保持现有可理解的回退行为：返回空知识上下文，不注入无关内容。

查询构造建议：

* 继续转义 FTS token，避免用户输入破坏 MATCH 表达式。
* 对 OR token 数量保留上限。
* 评估按 token 类型分层：精确 ASCII token、CJK 2/3-gram、CJK 1-gram 的优先级可以不同。
* 对短 ASCII 输入继续避免误命中，不能为了召回牺牲现有 `hi ok` 类测试。
* 仅优化切片和 FTS 不应声称能解决同义词问题。

## 8. Embedding 预留方案

Embedding 不属于 Chunking V2 的实际实现范围。

未来混合检索边界可设计为：

```text
FTS5 关键词召回
+
Embedding 向量召回
→ 分数归一化或 RRF 融合
→ 去重和多样性选择
→ 可选 rerank
→ 上下文预算控制
→ LLM
```

要求：

* FTS5 不应被删除。
* 配置名、函数名、错误码、命令、环境变量等精确内容仍优先依赖关键词检索。
* embedding provider、模型、向量存储和维度暂不确定。
* Chunking V2 不引入具体供应商依赖。
* Chunking V2 不在 `knowledge_chunks` 中预留具体 embedding 列；未来优先通过独立 embedding 表或独立向量存储，以 `chunk_id + content_hash` 关联。
* 同义表达如“timeout”和“怎么配置超时”不能在 Chunking V2 验收中伪装成已解决；该能力主要依赖未来 embedding、同义词扩展或查询改写。

## 9. 实施阶段拆分

每个阶段应能独立开发、测试和提交。

实际开发优先级：

* 第一优先级：逻辑块识别、同章节短段落聚合、自然边界切割、长代码块限制、`chunk_index`、`start_line` / `end_line`、`chunking_version`。
* 第二优先级：命中后邻接补全、近重复去重、章节多样性。
* 后续实验：FTS 多列权重、query token 分级、embedding、RRF、rerank。

真正交给 Codex 编码时，不要一次性要求“按本文档全部实现”，应按阶段拆成独立 PR。

### 阶段一：测试基线与现状固化

目标是不先改生产逻辑，先固定当前行为和风险样本。

任务：

* 为当前切片行为补充必要测试。
* 建立代表性 Markdown fixture。
* 明确哪些当前行为必须兼容，哪些只是 V2 要改进的问题。
* 补充现状测试时不要改变 chunking 行为。

测试样本至少包含：

* 多级标题；
* 多个短段落；
* 无空行的长中文；
* 中英文混合；
* 列表；
* 引用；
* 短代码块；
* 超长代码块；
* 表格；
* 单个超长句；
* 关键配置项短文本。

### 阶段二：Chunking V2

目标是替换切片纯函数策略，并尽量保持存储和检索调用边界稳定。第一步可以先不改数据库，先验证切片输入输出。

任务：

* 实现逻辑块识别。
* 维护标题栈。
* 同章节聚合短逻辑块。
* 按自然边界拆分长文本。
* 按确定性规则合并或保留短片段。
* 限制长代码块大小并补全围栏。
* 生成 chunk 元数据。
* 生成连续 `chunk_index` 和源行号。

### 阶段三：存储与索引兼容

目标是让 V2 chunk 元数据可靠落库，并保持文档更新、删除和重建一致。

任务：

* 评估 schema 变化。
* 新增 migration。
* 设计 `chunking_version` 和索引重建策略。
* 保持文档新增、修改、删除的增量同步。
* 切片版本变化时，即使文件 hash 未变化，也必须重建该文档知识索引。
* 明确历史索引兼容和回滚方式。

### 阶段四：FTS 检索选择优化

目标是优化召回后的选择，而不是替换 FTS5。

任务：

* 扩大或集中管理候选集。
* 保留 BM25 初排。
* 增强去重逻辑。
* 文件和章节多样性处理。
* 基于 `document_id + chunk_index` 命中后相邻 chunk 补全。
* 总上下文预算控制。
* 最终结果数量控制。
* 保持精确查询能力不退化。
* FTS 多列权重、query token 分级等仅作为有基准证据后的独立实验。

### 阶段五：评估与文档

目标是能比较 V1/V2 行为，并让部署者理解边界。

任务：

* 对比旧版和新版切片结果。
* 对比典型查询召回。
* 补充配置说明。
* 记录未来 embedding 扩展点。
* 明确哪些语义检索问题仍未解决。

## 10. 明确不做的内容

以下内容不属于 Chunking V2：

* 不引入图数据库。
* 不引入知识图谱。
* 不引入 Agent 多轮检索。
* 不接入具体 embedding 服务。
* 不实现 LLM 全量 rerank。
* 不重写整个 RAG 模块。
* 不替换 SQLite。
* 不删除现有 FTS5 精确检索。
* 不恢复旧世界观或旧上下文模块机制。
* 不进行与切片检索无关的重构。
* 不改变 QQ 消息入口、slash 指令、记忆确认流程、session 作用域或 todo 语义。

## 11. 验收标准

后续实现完成时至少满足：

1. 同一章节下的多个短段落不会仅因空行被全部拆成孤立 chunk。
2. 每个正文 chunk 能追溯来源文件和标题路径。
3. 超长普通文本优先在自然语言边界拆分。
4. 一般情况下不会在句子中间硬切，除非不存在合理切点或单句超过硬上限。
5. 超长代码块不会无限增长为单个 chunk。
6. 短而关键的配置项、命令、函数名或错误码不会仅因字符数少而被丢弃。
7. 不跨越明显标题章节进行无意义合并。
8. chunk 顺序和相邻关系可以被恢复。
9. FTS 初召回不只保留最终数量的候选。
10. 最终检索结果能去除高度重复片段。
11. 精确查询配置名、函数名、错误码时，FTS5 能力不退化。
12. 文档新增、修改、删除后，索引保持一致。
13. 原有正常文档可以重新建立索引。
14. 相关测试、格式化、静态检查和构建通过。
15. 若 schema 发生变化，具备明确 migration 和回滚说明。
16. 切片算法版本变化时，即使 Markdown 文件内容未变，也会重建对应知识索引。

## 12. 测试计划

### 单元测试

* 标题栈维护。
* 空行逻辑块处理。
* 同章节短块合并。
* 自然切点选择。
* 最坏情况下字符硬切。
* 超长自然语言逻辑块的有限 overlap。
* 基于 `document_id + chunk_index` 的邻接关系。
* 代码围栏识别。
* 长代码块拆分。
* 短文本保留和丢弃规则。
* chunk index 连续性。
* heading path 正确性。
* n-gram 索引不退化。
* 查询候选去重。
* 每文件结果限制。
* 总上下文预算。

### 集成测试

* Markdown 导入到 SQLite。
* FTS 索引创建和重建。
* `chunking_version` 变化触发未修改文件重建。
* 文档更新。
* 文档删除。
* 典型查询结果。
* 命中 chunk 后相邻片段补全。
* 多文件结果多样性。
* 旧数据或旧 schema 兼容。

### 对比测试

至少为下列问题建立固定查询：

* 文档写 `timeout`，用户问“怎么配置超时”。
* 查询精确环境变量。
* 查询函数名。
* 查询跨两个相邻段落的信息。
* 查询位于长代码块中间的内容。
* 查询短配置说明。
* 查询同一文件内多个相关章节。

对于“超时”和 `timeout` 这类同义表达，应如实记录：

* 仅优化切片和 FTS 不一定能够完全解决。
* 该能力主要依赖未来 embedding、同义词扩展或查询改写。
* 不要在 Chunking V2 验收中伪造语义检索能力。

## 13. 建议排查范围

真正实施前先阅读：

* `AGENTS.md`
* `README.md`
* `runtime/README.md`
* `qq-maid-core/README.md`
* `runtime/.env.example`
* `docs/tasks/done/rag-v1.md`
* `qq-maid-core/src/runtime/knowledge/mod.rs`
* `qq-maid-core/src/storage/knowledge.rs`
* `qq-maid-core/src/storage/migrations.rs`
* `qq-maid-core/src/app/mod.rs`
* `qq-maid-core/src/runtime/respond/chat_flow.rs`
* `qq-maid-core/src/runtime/respond/llm_service.rs`
* `qq-maid-core/src/runtime/respond/tests/chat.rs`

建议搜索关键词：

```text
chunk_markdown
split_long_text
MIN_CHUNK_CHARS
MAX_CHUNK_CHARS
build_index_text
build_search_query
KnowledgeStore::search
bm25
content_hash
chunk_id
SEARCH_CONTEXT_LIMIT
MAX_RESULTS_PER_FILE
SEARCH_CANDIDATE_LIMIT
SEARCH_TOTAL_CHAR_BUDGET
knowledge_chunks
knowledge_chunks_fts
```

## 14. 禁止事项

实施 V2 时禁止：

* 未写测试就直接替换切片行为。
* 把 embedding 写成本轮必须实现。
* 删除现有 FTS5 精确检索能力。
* 只根据函数名猜测调用链。
* 伪造测试结果或声称尚未执行的功能已经完成。
* 为了切片重构无关模块。
* 引入大型 Markdown parser 或向量库而不证明必要性。
* 读取、打印或提交真实 `.env`、私有知识资料、QQ openid、群 ID、token、API Key。
* 修改 `runtime/config/knowledge/*.example.md` 使其包含真实私人资料。

## 15. 完成后输出要求

后续实现完成后，最终说明必须包含：

* 改了什么。
* 复用了哪些现有代码、helper 或测试结构。
* 是否新增或更新中文注释。
* 是否删除已有注释及删除原因。
* 是否发生 schema 变化，migration 和回滚策略是什么。
* 执行了什么格式化。
* 执行了什么测试。
* 没执行的检查及原因。
* 是否确认没有写入敏感信息。
