# Scope 与 Identity 边界

本文定义多入口场景下的业务隔离键和平台投递目标语义。它只约束术语、helper 和调用边界，不迁移历史数据，不改变 session / pending / todo 持久化 key。各业务默认 owner 策略和群共享入口见 [业务 Owner 策略与群共享入口](./business-owner-strategy.md)，历史 key 迁移与兼容策略见 [Scope Key 历史数据迁移与兼容策略](./scope-key-migration-strategy.md)。

## 术语

| 术语 | 含义 | 典型用途 | 不应用于 |
| --- | --- | --- | --- |
| conversation scope | 消息发生的对话空间 | session、会话级 pending、ref_index、群聊聚合串行 | 判断自然人身份、平台发送 |
| actor scope | 当前对话空间里的实际操作者 | 权限、审计、群聊成员身份判断 | 替代 conversation scope |
| interaction scope | 一次个人交互状态的隔离键 | 群聊多人 pending、visible snapshot、“第 N 条”引用解析 | 跨会话合并长期数据 |
| owner scope | Todo / Memory 等业务数据归属 | Todo owner、长期数据归属 | 平台投递目标 |
| delivery target | 平台真实投递目标 | ReplyTarget、DeliveryTarget、PushTarget、sender | Session / Todo / Memory 归属 |

## Helper 边界

`qq-maid-core/src/identity.rs` 是业务隔离键 helper 的主入口：

- `conversation_scope_key(...)` / `stable_scope_key(...)`：构造 conversation scope。新代码优先使用 `conversation_scope_key`，旧名保留兼容。
- `actor_scope_key(user_id, scope_key)`：表达当前 conversation scope 内的操作者；缺少 actor 时返回 `None`。
- `interaction_scope_key(user_id, scope_key)`：优先使用 conversation + actor，缺少 actor 时回退 conversation scope，用于 pending / visible snapshot 等个人交互状态。
- `owner_scope_key(user_id, scope_key)` / `actor_owner_key(...)`：构造 Todo / Memory 等业务 owner key。旧名保留兼容。
- `parse_stable_scope_key(...)`：只解析业务 key 中的平台、账号和目标类型信息；不得把解析结果当作最新投递目标。

`scope_key` / `owner_key` 可包含平台和账号命名空间，但仍是业务 key。发送阶段必须使用 `ReplyTarget` / `DeliveryTarget` / `PushTarget` 携带的真实目标，不允许从业务 key 反推出 raw openid、group_id 或微信 FromUserName。

## QQ 官方示例

私聊：

```text
conversation scope = platform:qq_official:account:{appid}:private:{user_openid}
actor scope        = platform:qq_official:account:{appid}:private:{user_openid}:actor:{user_openid}
delivery target    = qq_official + appid + private + 当前可投递 openid
```

群聊：

```text
conversation scope = platform:qq_official:account:{appid}:group:{group_openid}
actor scope        = platform:qq_official:account:{appid}:group:{group_openid}:actor:{member_openid}
delivery target    = qq_official + appid + group + 当前可投递 group_openid
```

同一个自然人在私聊和群聊里可能出现相同或不同的用户标识。即使 actor 可疑似归一，conversation scope 也必须按实际对话空间分开，避免 session、pending、visible snapshot 和 ref_index 串用。

## 维护规则

- Gateway adapter 负责把平台协议字段归一化为 `InboundMessage` 的 `ConversationTarget` 与 `Actor`。
- Core 根据 `CoreConversation` 生成 conversation scope；Core、LLM 和 Tool Loop 不理解 QQ `msg_seq`、OneBot CQ 片段或微信 XML 字段。
- 群聊内个人 pending、visible snapshot 或 Todo Tool 选择状态应使用 interaction scope 或 owner scope，不要只用裸 `user_id`。
- Todo / Memory 等长期业务数据可以使用 owner scope 归属，但不要反向影响当前消息的 conversation scope。
- RSS、Notification、Todo 提醒和 Push 必须显式保存或携带平台投递目标；业务 key 只能辅助继承平台 / account 信息，不能替代最新 raw target。
