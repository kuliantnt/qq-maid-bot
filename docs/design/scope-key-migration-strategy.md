# Scope Key 历史数据迁移与兼容策略

本文补充 #294 Phase 4 / #303 的结论，评估 Phase 1-3 收口 conversation / actor / interaction / owner / delivery target 后，是否需要继续变更 `APP_DB_FILE` 中的持久化 key 或追加 SQLite migration。

## 结论

当前不新增 SQLite schema migration，也不做新的批量历史数据迁移。

理由：Phase 1-3 没有改变 `sessions`、`todos`、`memories`、`rss_subscriptions`、`notification_outbox` 的表结构，也没有要求把所有历史 session、pending、Todo 或 Memory 一次性重写到新 key。运行时已经按新入口生成 stable conversation scope，并在群聊个人交互中使用 actor-aware interaction scope；旧数据通过既有 schema migration、既有 QQ identity rebaseline 和运行时降级策略兼容。

需要保留的既有迁移能力：`qq-maid-core/src/storage/identity_rebaseline.rs` 会在统一入口 `src/main.rs` 中、Core runtime 打开业务 store 前执行。它依赖 QQ 官方 `app_id`，因此不能放进通用 `APP_MIGRATIONS`；它只把历史 QQ 官方裸 `private:` / `group:` 业务 key 归一为 stable scope，不把 RSS / Notification / Push 的真实投递目标改写成业务 key。

## 持久化 key 盘点

| 模块 | 当前持久化字段 | 兼容与迁移结论 |
| --- | --- | --- |
| Session | `sessions.scope_key`、`session_active.scope_key`、`pending_operation_json`、`last_todo_query_json`、`last_todo_action_json`、`last_memory_query_json` | 不新增 schema。conversation session 保留会话历史；群聊个人 pending / Todo / Memory 入口使用 interaction session。QQ rebaseline 改写裸 scope 时清空 pending 和最近查询/操作快照，避免旧 group 级临时状态继续生效。 |
| Todo | `todos.owner_key`、`todos.user_id`、`todos.scope_key` | 不新增 schema。stable scope 下 `TodoStore::owner` 使用 conversation + actor owner；旧裸 QQ 数据由 rebaseline 转换，缺少 `user_id` 的旧行保留为 scope owner，不强行猜测自然人。 |
| Memory | `memories.scope_type`、`memories.scope_id`、`memories.created_by_user_id`，以及旧兼容字段 `user_id` / `group_id` | 不新增 schema。既有 `MEMORY_SCOPE_SCHEMA_V2` 已把无法证明归属的旧记录放入 `legacy_unassigned`；QQ rebaseline 只给可确认的 personal/group `scope_id` 补 stable namespace。 |
| RSS | `rss_subscriptions.target_type`、`target_id`、`scope_key` | 不新增 schema。`scope_key` 用于订阅过滤和继承 platform/account；`target_id` 继续是平台真实投递目标。QQ rebaseline 只改 `scope_key`，并删除同 URL 的 legacy/stable 重复订阅。 |
| Notification | `notification_outbox.platform`、`account_id`、`target_type`、`target_id` | 不新增 schema。既有 V2 已持久化平台账号和真实 target；旧 V1 行通过默认 `qq_official` / 空 account 兼容读取。 |
| Push | 不单独持久化；由 `PushTarget` 承载 | 不迁移。`PushTarget::from_scope_key_or_qq_official` 只从 stable scope 继承 platform/account，目标 ID 必须来自调用方传入的当前 target。 |

Knowledge / RAG 表也位于 `APP_DB_FILE`，但不保存 conversation / actor / owner key，不属于本次 scope key 迁移评估范围。

## 既有 QQ Identity Rebaseline

统一入口启动顺序是：读取 Core 配置和 Gateway 配置，调用 `rebaseline_qq_official_identity(APP_DB_FILE, QQ_BOT_APP_ID)`，再构建 Core runtime 并打开各业务 store。

该 rebaseline 的边界如下：

- 只处理 QQ 官方历史裸 key：`private:*` 和 `group:*`。
- `sessions.scope_key` 与 `session_active.scope_key` 改为 `platform:qq_official:account:{appid}:private:{target}` 或 `platform:qq_official:account:{appid}:group:{target}`。
- Session 中的 pending、Todo 最近列表、Todo 最近操作和 Memory 最近列表会被清空；这些都是短期交互状态，清空比尝试跨 scope 迁移更安全。
- `todos.scope_key` 改为 stable scope；`owner_key` 在存在 `user_id` 时改为 `stable_scope:actor:{user_id}`，缺少 `user_id` 时退回 stable scope owner。
- `memories.scope_id` 对 personal/group 记录补 stable namespace；`legacy_unassigned` 不重新归类。
- `rss_subscriptions.scope_key` 改为 stable scope；`target_id` 保持原始投递 ID。
- `notification_outbox` 和 `PushTarget` 不参与该 rebaseline。

这是一类配置感知数据归一，不是普通 schema migration。`APP_MIGRATIONS` 只负责表结构和不依赖部署账号的历史数据修正；任何需要 `app_id`、平台账号或外部配置的迁移都不能直接塞进 `APP_MIGRATIONS`。

## 不迁移的数据如何兼容

- 历史 conversation session 保留为 conversation 维度，继续承载公开聊天历史；新的群聊个人 pending / Todo / Memory interaction session 会按 actor 重新创建。
- 被 rebaseline 清空的 pending 和最近编号快照会自然失效，用户需要重新发起确认、列表或编号操作。这比把 A 的旧 group 级快照迁给某个成员更安全。
- 旧 Memory 中没有稳定 `user_id` / `group_id` 的记录保持 `legacy_unassigned`，不会自动暴露给任意个人或群。
- Todo 每日提醒只扫描可解析私聊 target 的 owner 候选；scope 无法解析或同 owner 下存在冲突私聊 target 时跳过，不猜测投递目标。
- RSS / Notification / Push 始终使用保存或调用方传入的真实 target；即使 `scope_key` 里有 stale raw target，也不能反向替代 `target_id`。
- 非 QQ 官方或自定义调用方如果不提供 `account_id` / stable scope，会继续走旧兼容 key。当前不把 QQ 官方 rebaseline 泛化到 OneBot / 微信，避免把某个平台的账号语义写进 Core 通用迁移。

## 何时才需要新增 migration

后续只有出现以下变化时，才应单独开迁移任务：

- 新增、删除或改变 SQLite 列、索引、约束或 JSON 字段语义。
- 决定修改 stable scope 格式，导致当前 `parse_stable_scope_key` 不能兼容旧 key。
- 新增群共享 Todo / 群共享 Memory 产品能力，并需要把部分历史个人数据显式转为 group owner。
- 需要支持非 QQ 官方平台的账号感知 rebaseline，且能从历史数据中可靠区分平台账号和真实投递目标。

新增 migration 必须继续满足：兼容已有 `APP_DB_FILE`，不能在运行时业务方法里建表，不能把无法证明归属的数据自动迁到个人或群共享 owner，不能把业务 key 当作平台投递目标。

## 验证依据

当前代码已有以下回归点覆盖本文关键结论：

- `storage::identity_rebaseline::tests::rebaseline_updates_business_scope_without_rewriting_rss_target` 覆盖 QQ 裸 key 归一、RSS target 保持、session 临时状态清空。
- `storage::session::tests::session_schema_v2_keeps_legacy_rows_compatible` 覆盖旧 session schema 重开兼容。
- `runtime::tools::memory::storage::tests::legacy_v2_database_upgrades_to_v3_and_reopens_conservatively` 覆盖旧 Memory scope 映射、`legacy_unassigned` 隔离与 v3 重开兼容。
- `runtime::push::tests::stable_scope_only_supplies_platform_and_account_not_delivery_target` 覆盖 Push 不用 scope raw target 覆盖当前 target。
- `runtime::rss::scheduler::tests::rss_notification_uses_subscription_target_not_scope_payload` 覆盖 RSS 通知使用订阅保存的真实 target。

Memory v3 已通过 `APP_MIGRATIONS` 自动升级，不需要生产环境手工 SQL。升级前仍建议按常规流程备份 `APP_DB_FILE`。
