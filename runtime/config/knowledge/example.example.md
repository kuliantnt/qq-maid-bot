# 本地知识检索示例

## Markdown 知识库

将公开或私有的 Markdown 文件放入 `config/knowledge/` 后，重启机器人会自动扫描、分段并写入 SQLite FTS5 索引。普通聊天只会按当前问题检索少量相关片段，不会整份注入文档。

## 中文检索样例

示例编号 RAG-407 用于验证中文人物名、编号和连续中文句子的检索效果。用户提到“知识检索样例”或“RAG-407”时，应能命中这一段。

## Mixed Terms

This example mentions SQLite FTS5, Markdown chunks, and OpenAI Web Search as mixed English terms. It is safe to commit because it contains no private worldbuilding, real group chat content, user IDs, or secrets.
