# Memory WebUI 身份授权模型与 API 契约

本文是 #475 的设计结论，也是后续 Memory WebUI 实现任务 #476 的门禁。本文只定义威胁模型、身份与权限边界、管理 API 契约和分阶段实施清单；不新增认证代码、HTTP 路由、数据库 migration 或 Memory 业务行为。

相关的业务范围原则以父任务 #468 为准。当前 #470 已在 `runtime/tools/memory/` 建立 Memory v3 schema、强类型范围和领域操作；本文在该实现之上定义后续 WebUI 边界，不能把本文描述的 HTTP 认证、授权或 API 能力理解为已经实现。

## 结论

1. 当前 `/console/` 不是面向最终用户的已认证管理后台，也不是已经完成身份识别的“部署管理员模式”。它是默认关闭、只适合本机或受控内网的只读运维页面。当前没有 HTTP 登录用户、服务端 Web 会话、CSRF、防重放、Memory 审计主体或可供 Web 请求使用的群成员角色授权。
2. 在上述能力补齐前，不得注册任何私人 Memory HTTP 读写端点。`WEB_CONSOLE_ENABLED=true`、来源 IP、CORS allowlist、浏览器提交的 `user_id/group_id/role`、昵称或现有业务 `scope_key` 都不能单独构成授权依据。
3. 首个后端交付阶段只允许显式启用的部署管理员访问，且必须经过可被 Core 验证的认证主体；不能仅依赖“8787 在内网”或反向代理已经弹出登录页。最终用户与群管理员自助管理必须在平台账号绑定和群角色复验完成后再启用。
4. 最终用户只能管理自己的个人记忆与自己的群内画像；群管理员的额外权限只作用于其已验证管理身份所在群的公共群组记忆。群管理员不能因此读取或修改任何成员的个人记忆、群内画像或其他群数据。
5. 所有 API 位于 `/api/v1/console/` 管理命名空间；不恢复通用 HTTP `/memory`，也不把领域内 `MemoryStore` 的持久化方法直接暴露为 HTTP。
6. 后端先交付稳定的认证、授权、领域操作和 API 契约，原生 TypeScript 前端随后消费该契约。前端不能承担授权判断，也不能通过隐藏按钮代替服务端校验。

## 当前实现依据

### HTTP 与控制台

- [`qq-maid-core/src/http/routes.rs`](../../qq-maid-core/src/http/routes.rs) 的 `build_router` 始终只注册 `/healthz`；只有 `web_console_enabled` 为真时才增加静态控制台、只读状态和 Markdown 预览路由。当前没有 Memory 路由或认证 middleware。
- 同一文件的 CORS helper 只在请求 `Origin` 与 allowlist 精确相等时回显 `Access-Control-Allow-Origin`；它不会认证非浏览器客户端，也不会在服务端拒绝所有无 Origin 请求。当前 preflight 只允许 `POST, OPTIONS` 与 `content-type`，没有 credential、CSRF header 或授权 header 契约。
- 控制台已有 `nosniff`、`X-Frame-Options: DENY` 和严格 CSP；这些响应头应保留，但不能替代身份认证、资源授权或 CSRF 防护。
- [`qq-maid-core/src/config/mod.rs`](../../qq-maid-core/src/config/mod.rs) 与 [`runtime/config/.env.example`](../../runtime/config/.env.example) 目前只有 `WEB_CONSOLE_ENABLED` 和 `WEB_CONSOLE_ALLOWED_ORIGINS`。开关默认关闭，模板也明确要求不要把 8787 裸露到公网。
- [`qq-maid-core/src/http/console.rs`](../../qq-maid-core/src/http/console.rs) 只构造运行、平台能力和存储的安全摘要，没有当前操作者或授权上下文。

因此，当前控制台能继续作为“部署运维人员在网络边界内查看只读摘要”的页面，但不能直接升级为 Memory 管理后台。

### 身份与群角色

- [`qq-maid-common/src/identity_context.rs`](../../qq-maid-common/src/identity_context.rs) 已区分权威执行上下文与模型可见展示上下文。`Event`、`MemberApi` 和由这些事实生成的新鲜 `Cache` 可作为平台身份候选；`LegacyFallback`、`TextWeak`、昵称和文本 `@` 不得用于强权限或数据归属。
- [`qq-maid-core/src/service/types.rs`](../../qq-maid-core/src/service/types.rs) 的 `CoreRequest` 身份来自 Gateway 归一化的 `platform/account_id/actor/conversation`，群角色为 `owner/admin/member/unknown`。这些事实只存在于消息入站调用链，并不会自动成为 HTTP 请求的身份。
- [`qq-maid-gateway-rs/src/gateway/platform/member_enrich.rs`](../../qq-maid-gateway-rs/src/gateway/platform/member_enrich.rs) 可用 QQ 成员详情接口或短期缓存补全群角色；[`qq-maid-gateway-rs/src/gateway/platform/onebot11/mod.rs`](../../qq-maid-gateway-rs/src/gateway/platform/onebot11/mod.rs) 从 OneBot 结构化事件读取发送者角色。不同平台可获得的复验能力并不完全一致。
- [`qq-maid-core/src/runtime/group_role.rs`](../../qq-maid-core/src/runtime/group_role.rs) 只把 `owner/admin` 归一为群管理权限，角色缺失、`member` 或 `unknown` 均不能执行群管理写操作。
- [`qq-maid-core/src/identity.rs`](../../qq-maid-core/src/identity.rs) 与 [`qq-maid-core/src/storage/session/model.rs`](../../qq-maid-core/src/storage/session/model.rs) 能生成带平台和机器人账号命名空间的业务隔离键。这些 key 是业务归属，不是登录凭据，也不能从中反推当前平台权限。

WebUI 必须单独建立“浏览器会话 -> 已验证平台主体 -> 当前资源能力”的链路，不能从浏览器参数重建一份 `CoreRequest` 来冒充可信入站消息。

当前 `CoreActor` 也没有与群角色字段单独绑定的 `verified_at/source/expires_at`，actor 级 `identity_source` 不能直接替代 Web grant 的角色来源与新鲜度记录。HTTP 自助管理需要独立保存并复验这些事实。

### Memory

- [`qq-maid-core/src/runtime/tools/memory/storage/schema.rs`](../../qq-maid-core/src/runtime/tools/memory/storage/schema.rs) 保留 v1，v2 增加 `personal/group/legacy_unassigned` 访问边界，v3 再把 `memory_kind`、画像/关系主体、可见性、来源、确认时间、生命周期、固定和冲突属性拆成独立字段，并用偏好表记录群画像 opt-in/opt-out。旧 `scope` 仍只是兼容业务分类，不是访问边界。
- [`qq-maid-core/src/runtime/tools/memory/ops.rs`](../../qq-maid-core/src/runtime/tools/memory/ops.rs) 的 `MemoryOperations` 已集中维护本人个人记忆、本人当前群画像和当前群公共记忆的授权、可见性与原子领域操作；群管理员权限不会扩大到成员画像，`legacy_unassigned` 也默认拒绝。HTTP 层必须复用或显式扩展该领域门面，不能直接公开 storage CRUD。
- [`qq-maid-core/src/runtime/tools/memory/storage/query.rs`](../../qq-maid-core/src/runtime/tools/memory/storage/query.rs) 已支持 v3 精确 target 与强类型筛选，但当前仍按 `pinned/row_id` 和 `LIMIT` 查询，没有 WebUI 所需的游标快照；更新也没有单调 revision 与 compare-and-swap。
- 当前 `MemoryOperations` 没有部署管理员跨主体读取的隐式后门，也没有 WebUI restore 操作。这些后续能力若被产品确认，必须新增带显式 capability、审计和范围限制的领域入口，不能让 HTTP handler 绕过授权直接调用 `MemoryStore`。
- [`qq-maid-core/src/runtime/tools/memory/storage/mod.rs`](../../qq-maid-core/src/runtime/tools/memory/storage/mod.rs) 与 v3 持久化路径会对内容和 `source_text` 使用通用敏感文本脱敏，v3 `source_ref` 也要求安全引用；但“已经脱敏过”不等于“可以向任意管理角色返回”。来源正文、稳定用户 ID、群 ID 和作用域 key 仍属于敏感数据。
- [`qq-maid-core/src/runtime/respond/chat_flow/mod.rs`](../../qq-maid-core/src/runtime/respond/chat_flow/mod.rs) 当前把个人与群记忆合并成聊天上下文，并主要依赖 Prompt 提示避免群聊披露个人记忆。WebUI 授权不能复用这种软约束。

#470 已提供个人记忆、群内用户画像和群组公共记忆三类范围，以及可见性、生命周期、固定、确认、来源、画像主体和关系主体等 v3 字段。本文 API 必须映射这些强类型和真实领域结果；尚缺的 Web 会话、部署管理员能力、游标、restore、并发版本和审计应在领域/API 对应边界补齐，不能在 HTTP 层自行拼 SQL。

### 原生 TypeScript 前端

[`web-console/README.md`](../../web-console/README.md) 规定 `web-console/src/` 是唯一人工维护源码，`dist/` 由构建生成并提交，Rust 用 `include_str!` 嵌入产物。当前 [`web-console/src/api.ts`](../../web-console/src/api.ts) 只有只读状态与 Markdown 请求，未发送 cookie、CSRF token 或身份凭据，也没有 Memory 数据类型和页面。

## 威胁模型与信任边界

需要保护的资产包括：记忆正文、来源信息、用户与群关联、不同群中的画像差异、历史归档、平台稳定 ID、管理权限、审计记录和写入完整性。

主要威胁如下：

- 未认证访问者或同一内网中的其他进程直接调用 8787。
- 恶意网页利用已登录浏览器发起 CSRF，或利用过宽 CORS 读取响应。
- 调用者篡改 `scope_ref/user_id/group_id/role`，越权读取他人或其他群记忆。
- 普通群管理员借群管理身份读取成员私聊记忆或群内画像。
- 过期的 owner/admin 缓存使已被撤权的成员继续管理群记忆。
- 可枚举 ID、分页总数、差异化错误或筛选结果泄露记录是否存在。
- 两个页面并发编辑造成静默覆盖，或重试造成重复创建/重复批量操作。
- 反向代理身份 header 被客户端伪造，或后端仍可绕过代理直接访问。
- 记忆正文、来源文本、平台 ID、cookie、CSRF token 或认证断言进入日志和审计扩展字段。
- 前端通过 `innerHTML`、URL、来源摘要或错误正文引入存储型 XSS。

目标信任链为：

```text
平台事件 / 成员接口                 受信认证代理
          │                              │
          └──── 服务端验证与账号绑定 ────┘
                         │
                  服务端 Web 会话
                         │
          principal + scope capabilities
                         │
             Memory 领域操作 + storage
                         │
                脱敏响应 + 持久审计
```

浏览器只持有不透明会话、CSRF token、服务端返回的 `scope_ref` 和资源 ID。浏览器提交的筛选条件只是“希望访问哪个已授权资源”，服务端必须在每次请求重新求交集：

```text
有效权限 = 已认证主体
         ∩ 当前仍有效的平台账号绑定
         ∩ 当前资源范围
         ∩ 当前群角色授权（若需要）
         ∩ 本次操作类型
```

## 主体、身份来源与会话

### 服务端主体

服务端会话至少包含以下概念，具体 Rust 类型名称可在实现阶段确定：

```text
principal_id        服务端内部不透明主体 ID
principal_kind      deployment_admin | platform_user
platform_identities 已验证的 platform + account_id + user_id 绑定
group_grants        按 platform + account_id + group 记录的短期角色授权
authenticated_at    本次认证时间
expires_at          绝对过期时间
idle_expires_at     空闲过期时间
auth_method         认证方法与强度，不含 credential
```

- `deployment_admin` 是显式配置和认证后的运维角色，不由“请求来自 localhost”自动获得。
- `platform_user` 必须绑定至少一个稳定平台身份。不同平台或不同机器人账号下相同文本 ID 不能自动合并。
- 同一自然人在私聊和群聊里可能收到不同的平台 ID。只有平台结构化 `union_id` 等事实的语义和作用域已被服务端确认时才能合并；否则需要用户分别在目标群完成挑战，将群 actor 作为另一个已验证 subject alias 绑定到同一 principal。昵称相同、浏览器声明或 ID 文本碰巧相同都不能合并。
- `group_admin` 不是全局 principal kind，而是 `platform_user` 针对某个已验证群获得的短期 capability。角色必须是 `owner/admin`；`member/unknown/缺失` 一律失败关闭。
- 展示名、群名片、昵称、LLM 可见 `MessageActorContext`、URL query 和表单字段永远不是授权事实。

### 部署管理员认证

首个可交付阶段可以只支持部署管理员，但必须满足以下最小边界：

1. 后端只监听 loopback 或仅允许认证代理访问的私有网络接口，公网不能直达 8787。
2. 认证代理完成独立用户认证，并向 Core 传递可验证、短时、带 `aud/iat/exp/jti/subject` 的签名断言；共享的裸 `X-User`、`X-Role` header 不可接受。
3. Core 验证签名、受众、时间和重放，并把 subject 映射到显式部署管理员 allowlist，再签发自己的服务端会话。
4. 代理必须删除所有客户端传入的内部身份 header；Core 必须拒绝来自非受信代理的断言。当前代码没有可信代理识别或断言验证，因此这仍是实现阻塞项。
5. 每名管理员使用可区分主体，禁止用一个共享密码把所有审计归为同一人。

如果部署环境不能满足这些条件，Memory 管理 API 必须保持关闭；仅保留现有只读控制台。

### 最终用户的平台账号绑定

最终用户自助管理的最小跨平台方案是“一次性挑战 + 可信平台消息确认”，不要求浏览器拥有平台 token：

1. 未登录浏览器先建立只允许认证流程使用的短时 pre-auth session，再请求一次性绑定挑战。服务端只返回随机挑战码和到期时间，不接受目标 `user_id`；挑战与该 pre-auth session 绑定。
2. 用户从要绑定的账号向机器人私聊发送专用确认命令和挑战码。Gateway 从结构化事件生成 `platform/account_id/user_id`；弱身份来源或缺少稳定 ID 时拒绝绑定。该绑定只证明这一个平台身份，不自动认领文本 ID 不同的群 actor。
3. Core 将可信入站主体绑定到挑战，挑战仅可使用一次，并限制尝试次数、来源场景和有效期。群消息不能用于建立个人登录主体。
4. 浏览器在同一 pre-auth session 下轮询挑战状态，并通过单独的 POST 换取不透明服务端会话。状态查询 GET 不签发 cookie；换取成功后立即轮换挑战和会话 ID，防止固定会话。
5. 账号解绑、会话撤销和全部设备登出必须有独立入口；绑定关系变化立即使相关会话或 capability 失效。

当前没有挑战存储、绑定命令、认证会话或撤销机制；在这些能力落地前，不得启用最终用户 API。

### 群角色复验

用户登录只证明“是谁”，不证明“当前仍管理哪个群”。访问群公共记忆前，服务端按以下优先级建立 `group_grant`：

1. 平台成员接口实时返回的 `owner/admin`；
2. 用户在目标群完成短时挑战后，由结构化入站事件携带的 `owner/admin`；
3. 由前两类事实生成、未超过严格 TTL 的服务端缓存。

不能使用昵称、群名片、文本声明、`LegacyFallback/TextWeak`、浏览器角色字段或没有时间戳的历史 session。平台不支持成员接口时，必须使用当前群挑战或保持群管理功能不可用。grant 到期、平台复验失败、角色为 `member/unknown` 或群与机器人账号不匹配时失败关闭；读请求也应重新检查群成员资格，不能让退群用户继续查看群公共记忆。

### Web 会话

- cookie 只保存随机不透明 session token；服务端只持久化 token 摘要、主体、过期和撤销状态。
- cookie 至少设置 `Secure`、`HttpOnly`、`SameSite=Strict` 和受限 Path；认证成功、提权、解绑和定期续期时轮换。
- 设置绝对过期和较短空闲过期；部署管理员与平台用户可以采用不同策略，但均不得成为永久 cookie。
- 认证会话不能复用聊天 `SessionRecord`。聊天 session 的作用域、内容和生命周期与 Web 登录会话不同。
- 浏览器不得把 session、绑定挑战、CSRF token、平台 ID 或完整记忆缓存到 `localStorage`。

## 角色权限矩阵

表中“部署管理员”指已经显式获得 `memory.admin` 的认证主体；“最终用户”指已绑定平台账号的普通用户；“群管理员”是在最终用户权限之上，额外持有目标群有效 `owner/admin` grant 的同一主体。所有范围同时受 platform 和 account_id 隔离。

| 数据范围 / 操作 | 部署管理员 | 最终用户 | 群管理员 |
| --- | --- | --- | --- |
| 本人的个人记忆 | 可查看、创建、编辑、归档、恢复、清空；读取和写入均审计 | 可查看、创建、编辑、归档、恢复、清空 | 与最终用户相同，仅限管理员本人 |
| 他人的个人记忆 | 可在显式 `memory.admin` 模式下查看和管理；高风险读取需原因与审计 | 不可见、不可操作 | 不可见、不可操作 |
| 本人在当前群的群内画像 | 可查看和管理 | 通过本人的稳定身份与群 scope 查看、创建、编辑、归档、恢复、清空或选择停止保存 | 与最终用户相同，仅限管理员本人 |
| 本人在其他群的群内画像 | 可查看和管理 | 只可按服务端已验证的 subject alias 分组管理本人的数据；不因此获得对应群公共记忆权限 | 与最终用户相同；群管理员身份不跨群扩张 |
| 他人在当前群的群内画像 | 可在显式 `memory.admin` 模式下查看和管理；必须审计 | 不可见、不可操作 | 不可见、不可操作，即使对方是普通群成员 |
| 他人在其他群的群内画像 | 可在显式 `memory.admin` 模式下查看和管理；必须审计 | 不可见、不可操作 | 不可见、不可操作 |
| 已验证成员所在群的公共群组记忆 | 可查看和管理 | 可查看 active 公共记忆；不能创建、编辑、归档、恢复或清空 | 可查看、创建、编辑、归档、恢复和清空，仅限 grant 对应群 |
| 未验证成员资格或其他群的公共群组记忆 | 可查看和管理 | 不可见、不可操作 | 不可见、不可操作；一个群的 grant 不适用于另一个群 |
| 历史归档 | 可查看和管理全部范围，读取仍审计 | 只能查看和恢复本人个人记忆与本人群内画像 | 在最终用户范围之外，只能查看和恢复其管理群的公共归档 |
| `legacy_unassigned` | 只允许带独立高风险 capability 的部署管理员诊断；不能批量自动认领 | 不可见、不可操作 | 不可见、不可操作 |
| 来源类型、确认时间、状态、可见性 | 对已授权记录可见 | 对本人记录和可读群公共记录可见 | 对本人记录和所管理群公共记录可见 |
| 来源摘要或平台引用 | 默认只返回脱敏、截断摘要和是否可追溯；诊断展开需独立授权和原因 | 仅在来源已被 v3 规则判定可向本人展示时返回安全摘要；不返回他人原文或 raw ID | 仅对群公共记忆返回安全摘要；不得借来源查看成员私人对话或画像 |
| 永久物理删除 | 仅限独立高风险操作、双阶段确认和持久审计；常规管理优先归档 | 默认不可用；产品明确要求删除时走领域定义的清除/匿名化语义 | 默认不可用；不能物理删除成员数据 |

“查看某个用户”的页面必须固定分成“全局个人画像 / 当前群画像 / 其他群画像”。普通用户只能查看自己；群管理员也不能把“当前群”筛选器当作查看其他成员画像的入口。部署管理员检索其他用户时需显式高风险权限，且每次详情读取进入审计。

表中的 WebUI 可读权限不等于聊天召回权限。个人记忆默认仍只用于私聊，不得因用户或管理员能在 WebUI 中看到就自动在群聊公开；群内画像始终绑定用户 + 群，群组公共记忆始终绑定单一群。API 也不得用通用 PATCH 绕过 v3 领域规则扩大可见性。

## 管理 API 命名空间

### 总体约束

- Memory API 统一使用 `/api/v1/console/` 前缀。禁止注册 `/memory`、`/api/v1/memory` 或绕过控制台开关的平行入口。
- 路由注册必须同时受独立 Memory API 开关和认证配置控制；不能因为现有只读控制台开启就自动开放 Memory。
- API handler 只负责请求解析、会话/CSRF 提取、调用授权服务和映射响应。范围、冲突、归档、清空、停止保存、替换和权限规则应收敛到 #470 建立的 Memory 领域操作中。
- 请求中的 `scope_ref/subject_ref/group_ref` 都是服务端签发或查询返回的不透明引用。即使引用不可猜，服务端仍必须逐次授权。
- 响应是面向 WebUI 的 DTO，不直接序列化 `MemoryRecord`，不返回内部 `scope_id`、`owner_key`、raw user/group ID、数据库 `row_id` 或完整 source reference。

### 建议路由

| 方法与路径 | 用途 | 最低权限 |
| --- | --- | --- |
| `GET /api/v1/console/auth/bootstrap` | 建立短时 pre-auth session 并返回认证流程 CSRF bootstrap | 匿名、同源、严格限流 |
| `POST /api/v1/console/auth/admin-sessions` | 校验受信代理签名断言并签发部署管理员会话 | pre-auth session、受信代理 |
| `POST /api/v1/console/auth/challenges` | 创建平台绑定挑战 | pre-auth session、严格限流 |
| `GET /api/v1/console/auth/challenges/{challenge_id}` | 查询挑战状态，不签发登录会话 | 挑战绑定的 pre-auth session |
| `POST /api/v1/console/auth/challenges/{challenge_id}/exchange` | 已确认挑战换取平台用户会话 | 挑战绑定的 pre-auth session |
| `POST /api/v1/console/auth/logout` | 注销当前 Web 会话 | 已登录 |
| `GET /api/v1/console/session` | 返回当前主体安全摘要、可用 capability 和 CSRF bootstrap | 已登录 |
| `GET /api/v1/console/memory-scopes` | 返回当前主体可选择的不透明范围，分组为个人、当前群、其他群和群公共 | 已登录 |
| `GET /api/v1/console/memories` | 授权范围内的分页列表与筛选 | 对目标范围可读 |
| `POST /api/v1/console/memories` | 在目标范围创建记录 | 对目标范围可写 |
| `GET /api/v1/console/memories/{memory_id}` | 读取单条详情 | 对记录可读 |
| `PATCH /api/v1/console/memories/{memory_id}` | 修改允许编辑的内容与 v3 属性 | 对记录可写，要求版本 |
| `POST /api/v1/console/memories/{memory_id}/archive` | 归档单条记录 | 对记录可写，要求版本和确认 |
| `POST /api/v1/console/memories/{memory_id}/restore` | 恢复单条归档 | 对记录可写，要求版本和确认 |
| `POST /api/v1/console/memory-operations/prepare` | 预检清空、停止保存、批量归档、物理删除等高风险操作 | 对目标范围具备对应能力 |
| `POST /api/v1/console/memory-operations/commit` | 使用一次性确认 token 提交预检结果 | 与 prepare 相同，且授权仍有效 |

后端管理员检索用户、群或平台账号时，可以增加独立的 `/api/v1/console/memory-subjects` 资源查询，但返回值仍是 opaque ref 和脱敏展示摘要；不得把自由输入的 raw ID 直接拼成 Memory scope。

### 列表与筛选

`GET /api/v1/console/memories` 支持：

```text
scope_ref       服务端返回的不透明范围引用，必填
kind            personal | group_profile | group
type            v3 记忆类型
visibility      v3 可见性
state           active | archived
pinned          true | false
source_type     v3 来源类型
q               内容关键词；默认不搜索原始来源正文
limit           默认 20，范围 1..100
cursor          不透明下一页游标
```

部署管理员专用筛选可以再包含 `platform_ref/account_ref/subject_ref/group_ref`，普通用户和群管理员只能使用 `/memory-scopes` 返回且仍有权限的引用。未授权、过期或不匹配的 scope ref 统一按不可发现资源处理，不能通过响应差异枚举真实 ID。

分页必须使用稳定 keyset，不使用 offset。游标至少绑定：主体、角色/授权版本、筛选条件摘要、排序方向、首屏快照边界和过期时间；每次翻页重新校验授权。底层可以使用不公开的顺序列和 UUID 形成稳定排序，但不能把 `row_id` 暴露给浏览器。响应不默认返回全局 `total`，避免跨范围数量侧信道；确需范围内总数时必须按同一授权条件计算。

建议列表响应：

```json
{
  "items": [
    {
      "id": "opaque-memory-id",
      "version": "opaque-version",
      "kind": "group_profile",
      "scope": {
        "ref": "opaque-scope-ref",
        "label": "当前群画像"
      },
      "content": "已脱敏的记忆内容",
      "type": "identity",
      "visibility": "context_only",
      "state": "active",
      "pinned": false,
      "source": {
        "type": "user_confirmed",
        "summary": "用户主动确认",
        "detail_available": false
      },
      "created_at": "2026-07-15T10:00:00+08:00",
      "updated_at": "2026-07-15T10:00:00+08:00",
      "last_confirmed_at": "2026-07-15T10:00:00+08:00",
      "capabilities": ["read", "edit", "archive"]
    }
  ],
  "page": {
    "next_cursor": null,
    "has_more": false
  },
  "request_id": "opaque-request-id"
}
```

`capabilities` 只用于前端呈现，服务端不能相信客户端回传的 capability。

### 写入、并发与幂等

- 创建请求必须带 `Idempotency-Key`；key 与主体、scope 和规范化请求摘要绑定并短期保存，网络重试返回同一真实结果，不重复写入。
- 详情响应返回 `ETag`，DTO 同时带不透明 `version`。`PATCH/archive/restore` 必须使用 `If-Match`；缺失返回 `428 precondition_required`，不匹配返回 `412 version_conflict` 和当前安全版本摘要。
- `updated_at` 字符串不能单独作为版本。后端需要 #470 领域操作或后续最小 migration 提供单调 revision 与原子 compare-and-swap；在此之前写 API 保持阻塞。
- `PATCH` 只接受领域允许修改的字段。`kind/scope_ref/subject/source/created_at` 等归属和来源字段不能通过通用 patch 搬移；跨范围迁移必须是单独领域操作和独立权限设计。
- 冲突、替换、归档、清空和停止保存必须返回持久化后的真实结果。不能让前端成功文案或模型输出代替执行结果。

### 确认协议

普通单字段编辑至少需要前端明确提交和 CSRF；以下操作还必须经过服务端双阶段确认：清空范围、批量归档、停止保存群内画像、跨范围变更（若未来允许）、永久删除，以及部署管理员读取受限来源详情。

`prepare` 接受操作类型和 opaque 目标，返回：影响条数的授权内摘要、风险提示、当前版本集合摘要、短时一次性 `confirmation_token` 和到期时间。token 必须绑定主体、会话、CSRF 会话、操作、目标、版本和随机 nonce。

`commit` 只接受 token 和必要的二次确认文本，不重新相信浏览器提交的目标列表。提交时再次验证会话、CSRF、权限、群角色和版本；任一变化则整个操作失败，不做部分成功。token 使用后立即失效，失败次数受限，正文和目标 ID 不写入 token 明文或日志。

### CSRF 与浏览器安全

- 所有改变状态的方法（包括创建挑战、管理员会话、logout、挑战换会话、POST、PATCH、DELETE）必须要求自定义 `X-CSRF-Token`，并校验 pre-auth 或已登录 session 中的同步 token 或等价的强绑定方案。匿名流程先通过同源 `GET /auth/bootstrap` 获得 pre-auth cookie 与 CSRF bootstrap，不能无会话直接 POST。
- 同时严格验证 `Origin`；缺失 Origin 的浏览器写请求可再校验 Referer。不能仅依赖 `SameSite`，也不能把 CORS 当作 CSRF 防护。
- 登录和提权后轮换 session 与 CSRF token。前端从 `/api/v1/console/session` 的安全响应获得 CSRF bootstrap，不能从 URL 或持久存储加载。
- 页面继续使用严格 CSP、`frame-ancestors 'none'`、`form-action 'none'` 与 `textContent`。Memory 正文和来源摘要不得直接写入 `innerHTML`；确需 Markdown 时只能经过现有受控 sanitizer，并为管理数据采用更严格展示策略。

### 错误模型

HTTP 错误统一返回：

```json
{
  "error": {
    "code": "version_conflict",
    "message": "记录已更新，请刷新后重试",
    "request_id": "opaque-request-id",
    "retryable": false,
    "details": {}
  }
}
```

| HTTP | code 示例 | 语义 |
| --- | --- | --- |
| 400 | `invalid_request`、`invalid_cursor` | 参数格式错误，不回显敏感原文 |
| 401 | `authentication_required`、`session_expired` | 未登录或会话失效 |
| 403 | `csrf_failed`、`capability_denied` | 已知操作本身被禁止；不用于暴露目标是否存在 |
| 404 | `resource_not_found` | 目标不存在或调用者无权发现，二者响应一致 |
| 409 | `domain_conflict`、`operation_changed` | v3 领域冲突或预检目标已变化 |
| 412 | `version_conflict` | `If-Match` 不一致 |
| 422 | `confirmation_required`、`confirmation_expired` | 缺少或失效的高风险确认 |
| 428 | `precondition_required` | 写请求缺少 `If-Match` |
| 429 | `rate_limited` | 登录、搜索或写操作限流 |
| 503 | `identity_verification_unavailable`、`storage_unavailable` | 平台身份/角色无法复验或存储暂不可用 |

禁止把当前 `MemoryError` 的 SQLite 文本、绝对路径、内部 scope、平台 ID 或认证细节直接放进 `message/details`。服务端日志用 `request_id` 关联内部错误，外部消息使用固定安全文案。

## 审计与脱敏

### 持久审计

Memory API 需要独立持久审计，不能只依赖可能轮转的应用日志。至少记录：

```text
event_id / occurred_at / request_id
principal_id（内部或不可逆摘要）/ principal_kind / auth_method
effective_capability / group_grant source + age（若使用）
action / target kind / opaque target digest / scope digest
before_version / after_version / outcome / safe_error_code
reason_code（高风险管理员操作）
```

以下事件都要审计：认证成功/失败、账号绑定/解绑、grant 建立/过期、敏感详情读取、列表导出、创建、编辑、归档、恢复、清空、停止保存、物理删除和所有拒绝。读取审计可采样普通公共列表，但个人记忆、他人画像、来源详情和管理员跨用户查询不得采样。

审计不得记录 Memory 正文、来源正文、raw user/group ID、scope key、cookie、challenge、CSRF token、签名断言、Authorization header 或确认 token。审计查询和导出本身也需要权限、限流和二次审计。

### API 脱敏

- 用服务端生成的显示标签和 opaque ref 代替 raw 平台 ID；必要的关联只在服务端完成。
- 列表默认不返回 `source_text`。返回来源类型、确认时间和安全摘要；来源详情必须经过 v3 可见性判断和独立 capability。
- 文本继续复用通用敏感信息脱敏，但不能因脱敏 helper 已执行就跳过范围授权。
- 搜索结果摘要截断并转义；错误不回显 q、正文、来源或目标 ID。
- 导出功能不属于首版；未来启用时必须异步生成、短时下载、再次认证、范围锁定和完整审计。

## 控制台关闭、CORS、代理与部署边界

### 控制台关闭

- `WEB_CONSOLE_ENABLED=false` 时，静态页面、现有控制台 API、未来认证路由和全部 Memory 路由都返回 404；不能留下只知道路径即可调用的 API。
- 即使只读控制台开启，Memory API 仍默认关闭。实现阶段需要独立的 Memory API 能力开关，并分别控制部署管理员与自助模式；具体环境变量名由 #476 落地并同步配置模板。
- 认证配置缺失、签名密钥不可用、审计存储不可写或 v3 migration 未就绪时，Memory API 应启动失败或保持不注册，不能降级为匿名访问。

### CORS

- 首版 Memory WebUI 只支持同源，页面与 API 由同一 origin 提供，不启用 credentialed cross-origin。
- 当前 `WEB_CONSOLE_ALLOWED_ORIGINS` 只适用于已有只读/Markdown 能力，不能自动授权跨源 Memory 请求。
- 若后续确需跨源，必须使用精确 HTTPS origin allowlist，禁止 `*` 与 `null`；响应增加 `Access-Control-Allow-Credentials: true`，preflight 只允许实际方法和 `content-type/x-csrf-token/if-match/idempotency-key` 等必要 header，并保留 `Vary: Origin`。启用前必须有独立测试覆盖恶意 origin、无 Origin、预检和 credential。
- CORS 只约束浏览器读取，服务端仍对每个请求认证、授权和校验 CSRF。

### 反向代理与网络

- 推荐 Core 继续监听 `127.0.0.1`，由 TLS 反向代理提供 `/console/` 与 `/api/v1/console/`。如果监听 `0.0.0.0/::`，必须有主机防火墙和私网 ACL，且仍不能裸露公网。
- 代理应限制请求体、速率和空闲连接，删除客户端传入的身份 header、Forwarded 链和内部审计 header，再写入自己的受信值。
- Core 当前没有可信代理网段、mTLS 或签名身份断言验证。实现这些能力前，不得信任 `X-Forwarded-User/X-Role/X-User-ID` 等 header。
- 平台回调、Gateway WebSocket 和 Memory 管理面应保持独立路径与访问策略；不要因为平台入口需要公网而顺带公开 8787。
- “受控内网”只降低暴露面，不代表所有内网主体都是部署管理员。身份认证、资源授权、CSRF、审计和脱敏在内网部署中仍为必需。

## 分阶段实施清单

### 阶段 0：前置领域门禁

输入：#470 的 Memory v3 schema、Rust 类型与 `runtime/tools/memory/` 领域操作。

完成条件：

- 三类范围、画像主体、visibility、source、state、pinned、last_confirmed_at 和结构化属性已由类型表达。
- personal、group_profile、group 的授权上下文与查询条件不会退回自由字符串拼装。
- #470 已把 archive/clear/opt-out/replace 收敛为返回真实结构化结果的领域操作；WebUI 如需 restore，必须先在同一领域层新增原子恢复入口。
- `legacy_unassigned` 保守隔离，旧数据库 migration 和跨平台/account/group 隔离测试通过。
- 当前 v3 尚无可用于 HTTP 条件写的单调 revision；后端阶段必须补充最小 migration 与领域 CAS，不能在 handler 中用 `updated_at` 模拟。

输出：Web API 可以复用而无需复制业务规则的领域边界。

### 阶段 1：后端部署管理员 API

输入：阶段 0，受信认证代理方案，独立部署管理员 allowlist，审计存储和默认关闭的配置。

实施项：

1. 新增独立 Web auth/session/CSRF 模块，不复用聊天 session。
2. 验证代理签名断言、可信来源和重放；建立可区分的 deployment_admin principal。
3. 在 `OpsHttpState` 注入 Memory 领域服务、授权服务和审计写入器，不让 Gateway 协议对象进入 Core HTTP 层。
4. 实现 `/session`、`/memory-scopes`、分页列表、详情、创建、条件更新和双阶段操作。
5. 所有 handler 统一经过认证、CSRF、capability、范围、版本、领域操作、审计与 DTO 脱敏链。
6. 增加路由关闭、匿名访问、ID 枚举、错误等价、并发写、确认重放、审计失败、CORS 和敏感信息测试。
7. 更新 [`qq-maid-core/README.md`](../../qq-maid-core/README.md)、[`runtime/README.md`](../../runtime/README.md) 与配置模板；仍明确禁止公网裸露。

输出：默认关闭、只允许已认证部署管理员的后端 API；尚不开放最终用户和群管理员自助。

阻塞条件：任一认证断言无法由 Core 验证、后端可绕过代理、审计不可持久化、v3/CAS 未完成或安全错误适配缺失。

### 阶段 2：最终用户与群管理员后端能力

输入：阶段 1、平台绑定挑战链路、跨平台账号绑定模型和可复验的群角色来源。

实施项：

1. 实现一次性平台绑定挑战、私聊确认、会话签发、解绑和撤销。
2. 实现按 group scope 的短期 grant；QQ 成员接口、群内挑战和缓存分别记录 source 与 age。
3. 为平台能力不足、角色 unknown、缓存过期、退群和撤权定义失败关闭行为。
4. 按权限矩阵增加本人个人/群画像与群公共数据授权测试，重点覆盖“群管理员看不到成员个人/群画像”。
5. 验证同一用户不同群画像、不同机器人账号、不同平台和其他群筛选不能串读。

输出：后端可安全返回最终用户与群管理员自助 capability；仍由独立配置开关控制。

阻塞条件：不能证明稳定用户身份，不能把平台身份绑定到当前 Web 会话，或目标平台不能提供新鲜群管理证明且没有群内挑战替代方案。

### 阶段 3：原生 TypeScript 前端

输入：已冻结的阶段 1/2 API DTO、错误码、版本/确认协议和测试环境。

实施项：

1. 在 `web-console/src/` 增加 session bootstrap、登录/绑定状态、Memory API client 和严格运行时响应解析。
2. 页面按“全局个人画像 / 当前群画像 / 其他群画像 / 群组公共记忆”固定分区；没有 capability 的入口不显示，但服务端仍独立校验。
3. 实现游标分页、筛选、详情、版本冲突刷新、归档/恢复和双阶段高风险确认；不做乐观伪成功。
4. 来源默认显示安全摘要，受限详情必须明确说明原因并触发后端审计。
5. 正文与错误统一使用安全 DOM API；不在 URL、浏览器日志、localStorage 或 telemetry 中保存身份和记忆正文。
6. 执行 `npm ci`、`npm run check`、`npm run build`，提交可复现 `dist/`；Rust 静态资源 allowlist 同步新增生成文件。
7. 增加键盘操作、焦点管理、窄屏、加载/空态/错误态和会话过期回归。

输出：消费既有后端能力的原生 TypeScript 管理界面，不新增前端授权规则。

### 阶段 4：部署验收

- 默认关闭与开启失败条件符合预期，现有 `/healthz` 和只读控制台兼容。
- 本机同源、受信代理、恶意 origin、绕过代理和内网直连场景均有验证。
- 用部署管理员、普通用户、普通群成员、群管理员、已撤权管理员分别执行权限矩阵。
- 检查响应、应用日志、审计、反向代理日志和浏览器存储均无 credential、raw ID 或记忆正文泄漏。
- 不运行真实用户数据的自动化测试；使用合成平台/account/user/group fixture。

## #476 开始实现前的阻塞条件

以下条件未满足时，#476 不得开放生产 Memory 端点：

- 必须以 #470 已建立的 v3 类型、范围、可见性、生命周期、来源和 `MemoryOperations` 为领域基线；当前尚缺的 restore 与部署管理员专用操作不能通过绕过领域层补位。
- 当前没有可被 Core 验证的部署管理员认证，只有网络位置或普通 proxy header。
- 当前没有平台用户绑定、服务端 Web 会话、CSRF token、撤销和会话轮换。
- 当前私聊身份、群 actor 与跨群主体之间没有可供 Web 授权直接复用的完整账号绑定；不能用昵称或裸 ID 自动认领群内画像。
- 当前 HTTP 请求无法获得可信且足够新鲜的群成员角色；浏览器角色参数不能补位。
- 当前 Memory 更新没有单调版本和原子 compare-and-swap。
- 当前没有独立持久审计，也没有审计写入失败时的安全策略。
- 当前错误模型可能暴露 SQLite 文本，Memory DTO 也尚未与内部记录解耦。
- 当前 CORS 只覆盖已有 JSON POST，未具备 credential/CSRF/If-Match/Idempotency-Key 契约；首版因此必须同源。
- 当前静态控制台没有登录/权限/Memory 页面，且 `dist` 静态资源 allowlist 需要随构建产物同步维护。

满足这些门禁后，应先交付后端部署管理员阶段，再启用平台用户/群管理员自助，最后接入前端。任何阶段都不得以“先把接口藏在页面后面”或“仅内网可达”替代服务端授权。
