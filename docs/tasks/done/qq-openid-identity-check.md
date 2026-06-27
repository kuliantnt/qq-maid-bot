# QQ 官方机器人 OpenID 检查报告

## 结论

- 当前代码状态：Gateway 目前把 C2C `user_openid` 和群聊 `member_openid` 都传给 Core 的 `user_id`，但 `scope_key` 仍严格区分 `private:{user_openid}` 与 `group:{group_openid}`。
- 官方文档状态：未查最新资料；旧“唯一身份机制”页面属于较老资料，不能单独作为当前重构依据。
- 实际行为证据：2026-06-27 本地开发环境脱敏日志中，观察到同一 QQ 用户的 C2C `user_openid` 与两个不同群聊的 `member_openid` 脱敏尾号一致；两个群的 `group_openid` 脱敏尾号不同。
- 能否将 `user_openid` 与 `member_openid` 统一：代码层可以把二者作为“当前 AppID 下的 QQ 用户候选 ID”使用，但不应声明为官方永久稳定契约；如建全局键必须带 `app_id`。
- 推荐方案：采用方案 C，暂时不建用户表；保留 `scope_key/user_id/group_id`。本次实测只证明当前接口表现，不足以支撑立即引入方案 A 或 B。

文档声明：旧文档称同一用户不同群 `member_openid` 不同。

实际观察：本地开发环境在 2026-06-27 21:04-21:07 的脱敏日志中观察到 `c2c == group A == group B`。该结论只代表本次实测的当前接口表现。

是否属于稳定契约：当前未确认。即使本次实测相等，也只能按“当前接口实际表现”处理，除非官方新文档明确承诺。

## 本地实测记录

测试时间：2026-06-27 21:04-21:07。

测试方式：

1. 同一 QQ 用户向机器人发送一条私聊消息。
2. 同一 QQ 用户在群 A 发送一条触发机器人处理的消息。
3. 同一 QQ 用户在群 B 发送一条触发机器人处理的消息。

脱敏日志观察：

| 场景 | 脱敏用户标识 | 脱敏群标识 | 处理结果 |
|---|---|---|---|
| C2C 私聊 | `******7EC62A` | 无 | 收到消息并回复成功 |
| 群 A | `******7EC62A` | `******96D56F` | 收到消息并回复成功 |
| 群 B | `******7EC62A` | `******E274E4` | 收到消息并回复成功 |

实测结论：

- 同一 QQ 用户在私聊、群 A、群 B 中的用户侧脱敏标识一致。
- 群 A 与群 B 的群侧脱敏标识不同，说明 `scope_key = group:{group_openid}` 仍能区分不同群会话。
- 这次日志只包含项目现有脱敏输出，未记录原始 OpenID，也未输出 raw event envelope。
- 由于现有脱敏日志只显示尾部片段，本次结果适合作为运行观察证据；若要做强一致性诊断，仍建议增加带 salt 的短 hash 诊断日志。

## 当前数据链路

`C2C_MESSAGE_CREATE`

→ `qq-maid-gateway-rs/src/gateway/event.rs` 读取 `author.user_openid/openid/member_openid/id`，再 fallback 到顶层 `user_openid/openid`

→ `qq-maid-gateway-rs/src/respond.rs` 设置 `user_id = user_openid`，`scope_key = private:{user_openid}`

→ `qq-maid-core/src/http/routes.rs` 进入 Core `RespondRequest`

→ `qq-maid-core/src/runtime/respond.rs` 生成 `SessionMeta`

`GROUP_MESSAGE_CREATE / GROUP_AT_MESSAGE_CREATE`

→ `qq-maid-gateway-rs/src/gateway/event.rs` 读取 `author.member_openid/user_openid/openid/id`，再 fallback 到顶层 `user_openid`，其 serde alias 包含 `member_openid`

→ `qq-maid-gateway-rs/src/respond.rs` 设置 `user_id = member_openid`，`group_id = group_openid`，`scope_key = group:{group_openid}`

→ Core 按 `scope_key` 建会话，按业务选择是否使用 `user_id`

## 代码发现

- 文件：`qq-maid-gateway-rs/src/gateway/event.rs`
  行为：C2C 身份最终会 fallback 到 `author.id`。
  风险：`author.id` 未证明一定是 OpenID，作为最终用户身份兜底有串号风险。
  严重程度：中。

- 文件：`qq-maid-gateway-rs/src/gateway/event.rs`
  行为：`RawGroupMessage.user_openid` 使用 `alias = "member_openid"`。
  风险：若真实载荷同时出现顶层 `user_openid` 和 `member_openid`，serde 可能报 duplicate field；且字段命名容易误读。
  严重程度：中。

- 文件：`qq-maid-gateway-rs/src/gateway/group_filter.rs`
  行为：群缺 `member_openid` 时冷却键使用 `group_openid:unknown`。
  风险：同群所有缺成员 ID 的消息共享用户冷却槽。
  严重程度：低到中。

- 文件：`qq-maid-core/src/storage/session.rs`
  行为：活跃会话按 `scope_key` 维护。
  风险：群聊天然全群共用会话和 pending；这是当前设计语义，不是用户级隔离。
  严重程度：按预期，但需要明确。

- 文件：`qq-maid-core/src/storage/todo.rs`
  行为：Todo `owner_key` 优先使用 `user_id`，但查询/操作仍带 `scope_key`。
  风险：同一用户跨私聊/群聊不会自动共用 Todo；若未来 OpenID 变更，会拆号。
  严重程度：中。

- 文件：`qq-maid-core/src/runtime/respond/memory_flow.rs`
  行为：记忆列表和聊天记忆上下文未按当前 `user_id/group_id` 过滤。
  风险：记忆是当前更全局的业务状态；不是身份隔离验证的好证据。
  严重程度：高，需要另行审视，但不属于本次 OpenID 检查的直接修改范围。

## 身份作用域结论

| 场景 | 字段 | 当前是否稳定 | 是否可跨场景关联 |
|---|---|---:|---:|
| C2C | `user_openid` | 本次本地实测一致；非官方契约 | 当前可作为用户候选 ID |
| 群聊 | `member_openid` | 本次本地实测一致；非官方契约 | 当前可作为用户候选 ID |
| 同用户多群 | `member_openid` | 本次本地实测一致；非官方契约 | 当前可观察到可关联 |
| 当前 Core 会话 | `scope_key` | 代码稳定 | 不跨私聊/群聊 |
| 当前 Todo owner | `user_id + scope_key` | 代码稳定 | 不自动跨 scope |
| 当前 RSS | `scope_key` | 代码稳定 | 不跨目标 |

## 业务影响

- 会话：按 `scope_key` 隔离。群聊 `scope_key = group:{group_openid}`，所以全群共用会话。
- 记忆：创建时保存 `user_id/group_id`，但列表和聊天注入目前偏全局，群成员不隔离。
- Todo：`owner_key` 优先 `user_id`，操作查询还要求 `scope_key`；群内不同成员在 `member_openid` 存在时不会共享 Todo。
- RSS：按 `scope_key` 管理订阅，群订阅属于群，私聊订阅属于私聊。
- pending：保存在会话里。群会话下 pending 是全群共享；Todo pending 用 `owner_key` 阻止非发起人确认，Memory pending 没有 owner 概念。
- 权限与冷却：未发现 Core 权限表；Gateway 群冷却按群和 `group_openid:member_openid`，缺成员 ID 时退化为 `unknown`。

## 最小验证方案

- 操作步骤：
  1. 同一个 QQ 用户给机器人发一条私聊。
  2. 同一个 QQ 用户在群 A 发一条触发机器人处理的消息。
  3. 同一个 QQ 用户在群 B 发一条触发机器人处理的消息。
- 需要观察的字段：事件类型、`user_openid`、`member_openid`、`group_openid`、是否来自 `author.id` fallback。
- 脱敏方式：只记录 `sha256("{app_id}:{openid}:{salt}")` 的前 12 位，salt 来自临时环境变量，不输出原始 OpenID。
- 预期结果：

```text
c2c user_openid hash:   xxxxxxxxxxxx
group A member hash:    xxxxxxxxxxxx
group B member hash:    xxxxxxxxxxxx
c2c == group A:         true
group A == group B:     true
used author.id fallback:false
```

本次用现有脱敏日志完成了弱验证：三处用户侧脱敏尾号均为 `******7EC62A`，群 A 与群 B 脱敏尾号分别为 `******96D56F` 与 `******E274E4`。现有日志不能证明完整 OpenID 字符串逐字节相等，因此强验证仍建议使用上述带 salt hash 的诊断方式。

建议修改点，不提交补丁：

- 增加环境变量：`QQ_MAID_IDENTITY_DIAG=false`
- 增加可选 salt：`QQ_MAID_IDENTITY_DIAG_SALT`
- 仅在 Gateway 解析完成后打一条 debug/info 诊断日志，字段只含短哈希、事件类型、字段来源，不含 raw envelope。

## 最小修复建议

- 必须修改：本轮没有必须改的业务代码；单次实测不足以支撑重构身份系统。
- 建议修改：后续可移除或降级 `author.id` 作为最终身份兜底；至少把来源记录为“不可信 fallback”。同时给顶层 `member_openid/user_openid` 同时出现的场景加测试。
- 暂不修改：不建复杂用户表，不改会话作用域，不新增 migration。

## 架构判断

方案 A：`UNIQUE(app_id, openid)` 当前可作为继续验证后的轻量方案，但必须带 `app_id`。建议额外保存 `identity_kind` 或 `source_field`，避免未来区分 C2C/群身份时丢上下文。风险是官方若再次改变语义，会导致拆号或需要迁移。

方案 B：`persons + platform_identities` 更稳，但当前项目还没有昵称、权限、跨平台、跨 AppID 统一身份等需求，现阶段偏过度设计。

方案 C：继续保留 `scope_key/user_id/group_id`，最符合当前功能。它能支撑现有会话、RSS、Todo 和普通聊天，且不会因为旧文档直接推翻现有设计。

## 未能确认的事项

- 未通过原始 OpenID 或带 salt hash 诊断确认同一 AppID 的 C2C `user_openid` 与群 `member_openid` 是否完整相等；本次仅基于现有脱敏日志观察到尾号一致。
- 未通过原始 OpenID 或带 salt hash 诊断确认同一用户在多个群的 `member_openid` 是否完整相等；本次仅基于现有脱敏日志观察到尾号一致。
- 未确认该行为是否有官方稳定契约。
- 未读取或输出真实 OpenID；本次只记录现有脱敏日志中的短尾号。
- 本轮只做检查文档化，未改业务代码。

## 检查说明

- 调用了 2 个子 agent：Gateway 子 agent 完成字段链路检查；Core 子 agent 被关闭前未返回结果，Core 侧由主 agent 直接核查。
- 本次新增文档未写入真实用户 OpenID、群 ID、私聊内容、token、secret 或 API Key。
- 文档改动不需要运行 Rust 格式化或单元测试。
