# Memory v3 与 Grok Build 记忆机制对照

本文是 Issue #499 的调研与最小实现说明。结论以 2026-07-17 的当前调用链，以及
`xai-org/grok-build` 提交
[`8adf9013a0929e5c7f1d4e849492d2387837a28d`](https://github.com/xai-org/grok-build/tree/8adf9013a0929e5c7f1d4e849492d2387837a28d/crates/codegen/xai-grok-memory)
为准。Grok Build 对应代码采用 Apache-2.0；本项目只参考机制，没有复制其实现代码。

## 当前 Memory v3 基线

当前真实链路如下：

1. `runtime/tools/memory/` 通过 `MemoryOperations` 校验 actor、target、可见性和群管理权限。
2. `memory/storage/` 在 SQL 中按 `scope_type + scope_id + memory_kind + subject_id` 隔离
   Personal、GroupProfile 和 Group，并执行冲突归档与 opt-out 事务。
3. 普通聊天和 Tool Loop 共用 `build_memory_context`；未授权记录在 SQL 阶段就被排除，
   Respond 只负责分层字符预算和安全提示。
4. `sessions` 已保存会话历史与压缩摘要；压缩不会写入长期 Memory。每次普通聊天都会重新
   构建 Memory 上下文，因此新会话首轮和压缩后的下一轮都能重新注入，但此前只按近期顺序取值。
5. 长期 Memory 仍只允许用户明确记忆指令写入；普通聊天不会自动升级为长期事实。

## 能力对照

| 能力 | Grok Build | 本项目当前处理 |
|---|---|---|
| 作用域 | Global、Workspace、Session，面向单用户编程工作区 | Personal、当前成员 GroupProfile、Group；由平台、机器人账号、会话和 actor 的稳定 scope 隔离，不能照搬工作区模型 |
| 自动会话记录 | 会话结束保存低成本元数据摘要，`/flush` 可生成丰富摘要 | `sessions` 已有原始历史和压缩摘要；本阶段不复制聊天表，也不自动提升为长期 Memory |
| Dream 门槛 | 时间、会话数和锁；输入最多 32,000 字符 | 时间、新记录数、不同安全来源数、单次记录数和输入字符数；SQLite `IMMEDIATE` 事务与检查点防重复领取 |
| Dream 输出 | 模型合并 Markdown，成功后清理已处理 session 文件 | MVP 仅归档同一完整 target、语义键和正文完全相同的重复项；模糊相似和无法判断的冲突不改写 |
| 全文/向量混合检索 | FTS5 BM25 + 可选 sqlite-vec，默认向量 0.7、文本 0.3 | 不新增向量扩展；在现有分层 SQL 候选内按本轮问题的词/中文字符特征、来源和置顶状态排序 |
| 时间衰减 | Session 来源指数衰减；Global/Workspace 不衰减 | 仅 `SystemDerived` 指数衰减，半衰期 30 天；UserConfirmed、ManualImport 和 Legacy 不衰减 |
| 去重排序 | 可选 MMR，使用文本 Jaccard，相似结果降权 | 授权后的每层候选使用 MMR 风格重排；语义主体不同的相同正文不会被误去重 |
| 首轮/压缩后注入 | 首轮搜索，压缩后再次搜索 | 每轮普通聊天都按当前请求重建，天然覆盖首轮、话题变化和压缩后场景；仍受分层字符预算约束 |
| 人工管理 | `/remember`、`/forget`、`/memory`、直接编辑文件 | 保留自然语言写入、`/memory`、删除/归档、opt-out、冲突归档和后续 WebUI 边界 |

Grok Build 的 `search.rs` 会把 FTS 和向量候选合并，再应用来源权重、Session 时间衰减、
访问次数轻量加权和 MMR；`dream.rs` 使用时间/会话门槛、32,000 字符输入上限、输出结构校验、
锁和成功后清理。上述顺序值得参考，但它默认信任单用户工作区，不能承担本项目的多人权限判断。

## 本阶段实现

### 查询相关召回

SQL 仍先执行 target 和 visibility 过滤，并为每层多取少量候选。Memory 领域随后执行：

- 本轮问题与 Memory 正文的词、中文字符和相邻字符特征重合度；
- UserConfirmed、ManualImport、SystemDerived、Legacy 的来源权重；
- pinned 加权；
- 仅对 SystemDerived 应用 30 天半衰期；时间无法可靠解析时不衰减；
- 精确重复过滤和 MMR 风格多样性重排。

最终仍只返回私聊最多 12 条、群内每层最多 4 条。排序过程不接触其他 target，也不把内部 ID、
scope 或权限字段交给模型。

### 确定性后台整理

后台任务默认关闭。启用后按以下门槛运行：

- `MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS`：检查周期；
- `MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS`：同一 target 两次整理的最小间隔；
- `MEMORY_CONSOLIDATION_MIN_NEW_RECORDS`：最少新增 active 记录；
- `MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES`：最少不同安全 `source_ref`；
- `MEMORY_CONSOLIDATION_MAX_RECORDS`：单 target 最大处理数量；
- `MEMORY_CONSOLIDATION_MAX_INPUT_CHARS`：单 target 最大正文字符数。

整理只在同一 `scope_type + scope_id + memory_kind + subject_id` 内比较，并把正文、类别、
visibility、attribute key 和关系主体都相同的记录视为确定性重复。保留顺序为 pinned、
UserConfirmed、ManualImport、Legacy、SystemDerived，再以确认状态和新记录兜底。其余重复项改为
`archived`，不会物理删除。模糊相似、不同关系主体和事实冲突保持原状，冲突数记为 0，不伪装为已解决。

候选复核、归档和检查点更新位于同一个 SQLite `IMMEDIATE` 事务；并发进程只能有一个成功提交。
任一步失败会整体回滚，原始 active 记录和检查点均保持不变，下次仍可重试。

日志只记录门槛跳过原因、target 数、输入/输出数量、去重数、冲突数、截断数、耗时和失败阶段，
不记录 Memory 正文、scope ID、用户/群 ID 或聊天内容。当前模式是本地确定性算法，因此 provider
记为 `local`、model 记为 `deterministic_exact_duplicate`。

## 聊天记录与 Dream 候选设计

项目可以把聊天记录作为未来 Dream 的低层候选，但不应再建一份无边界的原始聊天副本。
现有 `sessions.history` 和 `sessions.summary` 已是权威会话来源，下一阶段应从这里产生有保留期的
候选，而不是直接写入 `memories`。

建议边界：

- 私聊只生成同一 actor Personal 范围的候选；候选与长期 Memory 分表、分状态保存。
- 群聊历史继续属于共享 conversation session。普通成员内容不得自动提升为 Group 公共记忆，
  也不得绕过 GroupProfile opt-out；如需形成画像或群规则，仍需明确授权路径。
- 候选只保存必要摘要和安全来源引用，设置最大长度、默认保留期和处理状态；原始聊天历史沿用
  Session 自己的清理策略，不复制到 Memory 表。
- 敏感信息、工具噪声、寒暄和临时状态在候选生成前过滤；日志不输出候选正文。
- 模型只提出结构化合并建议。服务端根据原始 target、actor capability、visibility 和 opt-out
  再次校验后，才能保存为 `SystemDerived`；模型不能选择真实用户、群或平台账号。
- 无法判断真假的冲突进入待确认或可追溯归档，不自动覆盖。模型失败、输出非法或数据库失败时
  不删除 Session 与候选。

若实现该阶段，需要新增独立 migration，例如 `memory_candidates`、领取租约、过期时间和处理状态；
同时补充 Session 清理、候选保留期、用户 opt-out、跨平台/账号/用户/群隔离、并发领取和失败重试测试。
在这些条件落地前，本项目不会把普通聊天默认保存成长期事实。

## 暂缓机制

- 不引入 `sqlite-vec` 或新 embedding Provider：当前 Memory 规模和现有依赖尚不足以证明收益，
  先用查询相关的本地重排建立基线。
- 不让模型自动改写 UserConfirmed：当前没有可供用户查看和恢复的完整版本管理界面。
- 不做模糊语义合并或自动冲突裁决：缺少可靠置信度与人工复核面时，保留记录比误删安全。
- 不把 Session 原始正文全量注入 Memory：这会扩大隐私保留面并违反现有明确写入边界。

后续建议按顺序拆分：会话候选与保留期 → 结构化 Dream 建议和写前授权 → 可选 FTS5/向量基线评估
→ WebUI 候选、归档与冲突复核。每一步都应先有独立 migration、回滚和跨作用域测试。
