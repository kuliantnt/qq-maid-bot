# Memory v3 与 Grok Build 记忆机制对照

本文是 Issue #499 的调研与最小实现说明。结论以 2026-07-17 的当前调用链，以及
`xai-org/grok-build` 提交
[`8adf9013a0929e5c7f1d4e849492d2387837a28d`](https://github.com/xai-org/grok-build/tree/8adf9013a0929e5c7f1d4e849492d2387837a28d/crates/codegen/xai-grok-memory)
为准。Grok Build 对应代码采用 Apache-2.0；本项目的 Dream 门槛、输入截断、锁/检查点、
`NO_REPLY` 校验、失败回滚和提示词规则基于该实现移植，并按本项目多人作用域与 SQLite
事务边界重写。归属说明见仓库根目录 `THIRD_PARTY-NOTICES.md`。

## 当前 Memory v3 基线

当前真实链路如下：

1. `runtime/tools/memory/` 通过 `MemoryOperations` 校验 actor、target、可见性和群管理权限。
2. `memory/storage/` 在 SQL 中按 `scope_type + scope_id + memory_kind + subject_id` 隔离
   Personal、GroupProfile 和 Group，并执行冲突归档与 opt-out 事务。
3. 普通聊天和 Tool Loop 共用 `build_memory_context`；未授权记录在 SQL 阶段就被排除，
   Respond 只负责分层字符预算和安全提示。
4. `sessions` 已保存会话历史、压缩摘要和群聊 turn actor；Dream 直接读取这些数据，不复制聊天表。
5. 显式记忆指令继续写入 `UserConfirmed`。`MEMORY_CONSOLIDATION_ENABLED` 只启用确定性整理；
   普通聊天异步提取 `SystemDerived` 由独立的 `MEMORY_DREAM_ENABLED` 控制。两者默认都关闭。

## 能力对照

| 能力 | Grok Build | 本项目当前处理 |
|---|---|---|
| 作用域 | Global、Workspace、Session，面向单用户编程工作区 | Personal、当前成员 GroupProfile、Group；由平台、机器人账号、会话和 actor 的稳定 scope 隔离，不能照搬工作区模型 |
| 自动会话记录 | 会话结束保存低成本元数据摘要，`/flush` 可生成丰富摘要 | 复用 `sessions` 原始历史、压缩摘要和归档历史，不新增第二份聊天记录 |
| Dream 门槛 | 时间、会话数和锁；输入最多 32,000 字符 | 每 target 的时间、新 Session 数、单次 Session 数和输入字符门槛；SQLite 短租约防并发重复执行 |
| Dream 输出 | 模型合并 Markdown，成功后清理已处理 session 文件 | 严格 JSON 候选；服务端决定 target/visibility，安全结果原子写为 `SystemDerived`，Session 不删除 |
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

精确去重保留 SQL 返回中的第一条。若本轮没有实质查询命中，去重后直接按 SQL 的置顶、
确认时间和近期顺序截断，不应用来源权重、时间衰减或 MMR；存在命中时才做相关性排序，
相关性或 MMR 分数相同均以原 SQL 位置靠前者优先，结果不依赖集合遍历顺序。

最终仍只返回私聊最多 12 条、群内每层最多 4 条。排序过程不接触其他 target，也不把内部 ID、
scope 或权限字段交给模型。

### 确定性后台整理

后台任务默认关闭。启用后按以下门槛运行：

- `MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS`：检查周期；
- `MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS`：同一 target 两次整理的最小间隔；
- `MEMORY_CONSOLIDATION_MIN_NEW_RECORDS`：最少新增 active 记录；
- `MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES`：最少不同的非空安全 `source_ref`；`NULL` 或空值
  不计入来源数，也不会用 Memory ID 伪装成多个来源；
- `MEMORY_CONSOLIDATION_MAX_RECORDS`：单 target 最大处理数量；
- `MEMORY_CONSOLIDATION_MAX_INPUT_CHARS`：单 target 最大正文字符数。

整理只在同一 `scope_type + scope_id + memory_kind + subject_id` 内比较，并把正文、类别、
visibility、attribute key 和关系主体都相同的记录视为确定性重复。保留顺序为 pinned、
UserConfirmed、ManualImport、Legacy、SystemDerived，再以确认状态和新记录兜底。其余重复项改为
`archived`，不会物理删除。模糊相似、不同关系主体和事实冲突保持原状，冲突数记为 0，不伪装为已解决。

检查点按完整 target 独立维护。每轮只从 `last_processed_row_id` 之后按 row_id 升序读取最旧的
未扫描 active 记录，并同时应用记录数和字符上限；检查点只推进到本轮最后一条实际读取的记录。
若批次截断，`truncated` 会使同一 target 在满足最小整理间隔后继续处理尾批，不再要求尾批重新
满足最少记录数或来源数。即使单条正文超过字符上限也会单独处理一条，避免检查点永久停滞。

当前精确去重只比较同一个有界批次内的记录；被数量或字符上限拆到不同批次的相同记录不会自动
跨批归档。`last_processed_row_id` 因此只表示记录已被某个成功批次扫描，不表示整个 target 已完成
全历史去重。后续若要跨批去重，应增加不复制正文的稳定指纹索引及独立 migration，而不是无界读取
历史 Memory。

候选复核、归档和检查点更新位于同一个 SQLite `IMMEDIATE` 事务；并发进程只能有一个成功提交。
任一步失败会整体回滚，原始 active 记录和检查点均保持不变，下次仍可重试。

日志只记录门槛跳过原因、target 数、输入/输出数量、去重数、冲突数、截断数、耗时和失败阶段，
不记录 Memory 正文、scope ID、用户/群 ID 或聊天内容。当前模式是本地确定性算法，因此 provider
记为 `local`、model 记为 `deterministic_exact_duplicate`。

## Session Dream 自动记忆

Dream 由普通聊天成功写入 Session 后通过 `tokio::spawn` 调度，不阻塞本轮回复，并由独立的
`MEMORY_DREAM_ENABLED` 开关控制；确定性整理继续只受 `MEMORY_CONSOLIDATION_ENABLED` 控制。
两个功能可以分别启停。Dream 还使用以下门槛：

- `MEMORY_DREAM_MIN_INTERVAL_SECONDS`：同一完整 target 两次成功 Dream 的最小间隔；
- `MEMORY_DREAM_MIN_NEW_SESSIONS`：检查点后的最少新增 Session；
- `MEMORY_DREAM_MAX_SESSIONS`：单批最多 Session；
- `MEMORY_DREAM_MAX_INPUT_CHARS`：单批实际送入模型的最大历史字符数；
- `MEMORY_DREAM_MAX_OUTPUT_MEMORIES`：模型单批最多候选数。

作用域由服务端上下文固定：私聊只允许当前用户 Personal；群聊只允许当前发言人在当前群的
GroupProfile，并按 SessionMessage 中脱敏 `actor_ref` 精确过滤历史；Group 公共记忆、Channel、
ServiceAccount、Unknown 和身份不完整请求全部跳过。平台、机器人账号、群和用户的现有稳定 scope
继续作为隔离键。GroupProfile 在领取和提交时都检查 opt-out。

Dream 只读取用户消息；私聊可以附带现有压缩摘要，群聊不使用多人共享摘要。归档历史仍从 Session
的 `extra.archived_history` 读取，不创建候选表或聊天副本。每条 `SessionMessage` 首次持久化时取得
稳定的 SQLite 消息 ID；普通整会话保存显式复用原 ID，压缩归档也序列化同一 ID。输入先过滤
寒暄、敏感文本和空内容。升级前已经存在且缺少 ID 的归档会在首次 Dream 领取时持久化补齐负数
兼容 ID，使其稳定排在现有活跃消息的正数 SQLite ID 之前；
工具输出、助手回复不会进入输入，临时状态和主体不明信息由固定提示词要求模型丢弃。

模型只能返回 `content`、`category`、可选 `attribute_key` 和 `worth_saving` 的严格 JSON，未知字段
会使整批失败。模型不能提交 ID、target、scope、visibility 或权限。服务端再次清洗内容、过滤敏感
信息和伪造身份字段，然后在完整 target 内执行精确重复检查。与同一 `attribute_key` 的 active
`UserConfirmed` 冲突时跳过并计数，绝不覆盖或归档已确认事实。

并发边界分为两个短 SQLite `IMMEDIATE` 事务：第一个读取稳定消息边界并领取带过期时间的 target
租约，提交后才调用模型；第二个复核租约，在一个事务内写入所有安全候选并推进本轮实际输入末尾的
Session 用户消息稳定 ID 检查点（同时保留 Session 时间和 ID 供诊断）。模型调用期间没有数据库事务。
标题或其他不新增消息的 Session 更新不会触发重复处理。模型失败、非法输出、写入失败或
任务中断都不推进检查点；中断遗留租约过期后可重试。`NO_REPLY` 和“候选全部被安全过滤”属于成功
批次，仍推进实际输入范围。输入按稳定消息 ID 从旧到新逐条加入，字符上限截断时只推进到最后一条
实际加入模型输入的用户消息；同一 Session 的剩余消息和其他尾批下次继续，不再受最少 Session 数
限制。首条单独消息超限时只截断并消费该消息，后续消息不会被检查点永久跳过。

日志只记录成功数量、重复数、冲突数、过滤数和截断状态，不记录 Session/Memory 正文、模型原始
输出或用户/群 ID。v4 尚未部署，因此 Dream 状态表直接加入现有 v4 migration，没有增加 v5。

## 暂缓机制

- 不引入 `sqlite-vec` 或新 embedding Provider：当前 Memory 规模和现有依赖尚不足以证明收益，
  先用查询相关的本地重排建立基线。
- 不让模型自动改写 UserConfirmed：当前没有可供用户查看和恢复的完整版本管理界面。
- 不做模糊语义合并或自动冲突裁决：缺少可靠置信度与人工复核面时，保留记录比误删安全。
- 不把 Session 原始正文复制进 Memory：仅保存经双重校验的独立长期事实，`source_text` 留空。

后续建议按顺序拆分：真实流量质量评估与 prompt 调优 → 可选 FTS5/向量基线评估 → WebUI 候选、
归档与冲突复核 → 明确授权后的群公共候选。首版不实现人工审核队列或自动 Group 公共记忆。
