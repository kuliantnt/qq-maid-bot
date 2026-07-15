# 业务 Owner 策略与群共享入口

本文补充 #294 Phase 3 / #302 的业务归属规则，说明 Todo、Memory、Session、RSS、Notification 和 Push 如何区分 owner scope 与 delivery target。本文只收口当前实现语义，不新增群共享 Todo 能力，不改变 SQLite schema，不迁移历史数据；历史 key 迁移与兼容策略见 [Scope Key 历史数据迁移与兼容策略](./scope-key-migration-strategy.md)。

## 总体规则

- 消息来自群聊不等于业务数据归属群；默认归属必须由业务入口明确决定。
- 个人业务状态优先绑定 actor；群共享状态必须通过明确的 group 命令、权限或业务路径进入。
- `scope_key` / `owner_key` 是业务隔离键，不是平台发送目标。
- 主动推送必须使用 `PushTarget` / 通知 outbox 中保存的 platform、account、target type 和 target id。
- `parse_stable_scope_key` 只能辅助继承 platform / account，不能用其中的 raw target 替换当前可投递目标。

## 当前业务策略

| 业务 | 默认归属 | 群共享入口 | 投递目标规则 |
| --- | --- | --- | --- |
| Todo | `TodoStore::owner(user_id, conversation_scope)`；群聊中仍是发言人的个人 Todo | 当前没有隐式群共享 Todo；未来必须新增明确命令和权限策略 | 单次提醒按创建时的 conversation scope 推送，owner 不因此变成群 owner；每日提醒只从可解析私聊 target 的 owner 候选发送 |
| Memory | 私聊裸 `/memory 内容` 默认 personal；群聊裸写入仅在范围可靠时归入 personal、当前成员 group profile 或 group，否则先澄清 | `/memory personal`、`/memory profile`、`/memory group` 显式选择命名空间；group 公共记忆仅群主/管理员可写。明确新增意图校验通过后直接写入；清空、画像停用等破坏性管理继续由当前 actor 确认 | Memory 不直接生成主动推送；聊天上下文按 personal、当前成员 group profile 和当前 group 三层授权召回 |
| Session | 普通聊天 active session 使用 conversation scope | 群聊个人 pending、Tool Loop 和可见编号快照使用 interaction scope | Session 不作为发送目标 |
| RSS | 当前 conversation target 的订阅；群订阅属于群目标，私聊订阅属于私聊目标 | `/rss add/delete` 在群聊中需要群主或管理员；list 可供群成员查看 | `RssSubscription.target_id` 是真实投递目标；`scope_key` 只用于订阅过滤和继承 platform/account |
| Notification | 不拥有业务数据，只保存业务事件的通知快照 | 由上游业务显式传入 `NotificationUpsert.target` | outbox 持久化 `platform/account_id/target_type/target_id`，发送方只按这些字段投递 |
| Push | 不表达 owner，只表达目标平台账号和目标 ID | 无 | `PushTarget::from_scope_key_or_qq_official` 只从 stable scope 继承 platform/account，目标 ID 必须来自调用方传入的当前 target |

## 代码入口

- Todo owner：`qq-maid-core/src/storage/todo/mod.rs::TodoStore::owner`。
- Todo Tool Loop：`qq-maid-core/src/runtime/tools/todo/scope.rs::TodoToolScope::load`。
- Memory scope、自然语言写入与确认：`qq-maid-core/src/runtime/respond/memory_flow/scope.rs::memory_command_scope`、`qq-maid-core/src/runtime/tools/memory/save.rs`、`qq-maid-core/src/runtime/tools/memory/pending.rs` 和 `qq-maid-core/src/runtime/tools/memory/flow.rs`。
- RSS target：`qq-maid-core/src/runtime/respond/rss_flow.rs::rss_target_from_meta` 和 `qq-maid-core/src/runtime/rss/scheduler.rs::push_item`。
- Notification target：`qq-maid-core/src/storage/notification.rs::NotificationUpsert`。
- Push target：`qq-maid-core/src/runtime/push.rs::PushTarget::from_scope_key_or_qq_official`。

## 兼容性结论

本阶段不改变持久化 key 或数据格式，因此不新增 SQLite schema migration。旧 `private:` / `group:` scope 继续按现有兼容路径读取；QQ 官方历史裸 key 由启动期 identity rebaseline 按配置中的 AppID 归一到 stable scope。无法证明归属的旧 Memory 已由既有 migration 放入 `legacy_unassigned`，不在本阶段重新归类。
