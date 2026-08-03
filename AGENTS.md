# AGENTS.md

给 Codex / AI Agent 后续维护本仓库使用的长期规则。请使用中文回复。

项目运行、部署、排障和详细架构以 [README.md](./README.md)、[docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)、各 crate README、[Makefile](./Makefile)、`runtime/config/` 下的公开配置模板和源码为准；根 `AGENTS.md` 只保留每次进入仓库都应遵守的项目级硬约束。

## 项目概述

这是一个 Rust 编写的单进程、多入口小女仆 AI Agent 项目，由根目录 Cargo Workspace 统一管理。当前主要支持 QQ 官方机器人、OneBot 11 反向 WebSocket，以及可选的微信服务号文本回调；Core、LLM、Gateway 和可选 Web Console 在同一程序中协作。

## 目录边界

- `src/`：统一 `qq-maid-bot` 程序入口和配置、migration、备份等运维 CLI；只负责装配 Core 与 Gateway，不承载具体业务域规则。
- `qq-maid-gateway-rs/`：QQ 官方、OneBot 11、微信服务号的协议 adapter，负责事件解析、统一入站转换、群消息过滤、媒体处理、`/ping`、回复发送和主动推送出口。
- `qq-maid-core/`：`CoreService`、聊天与查询、记忆、session、Todo、RSS、知识库、`/ops`、prompt、业务 Tool、Notification Outbox、Worker 和受保护管理 HTTP API。
- `qq-maid-llm/`：模型协议、Provider 路由、候选链/fallback、SSE、usage、健康观测、上下文预算、Web Search 和 Agent/Tool Loop 协议。
- `qq-maid-common/`：两个及以上 crate 共用、无业务状态的基础工具。
- `web-console/`：Web Console 的 TypeScript 源码、测试和可复现生成的 `dist/`；Rust 会把 `dist/` 静态资源嵌入二进制，不能直接编辑生成物。
- `docs/`：开发、部署、设计和接口契约文档；文档细节不在根规则中重复维护。
- `runtime/`：部署运行目录，部署时放 release 二进制、控制脚本、公开配置模板、私有配置和运行产物；真实配置、数据库、日志和 prompt 不得提交。
- `scripts/`：部署、进程控制、打包、同步、诊断和跨平台回归脚本源码。
- `qq-maid-core/src/runtime/tools/`：业务工具领域目录。Todo、提醒、记忆、RSS、搜索、知识库、天气、列车、语音、`/ops` 等工具及其领域规则，应优先收敛到对应 `tools/<domain>/` 子目录。

## 业务代码边界

新增或修改业务逻辑时，优先遵守：

- Gateway 只负责平台协议接入、事件/媒体解析、消息收发、平台侧权限与过滤，不写 Core 业务规则；各平台先转换为平台无关的入站模型再进入 Core。
- LLM crate 只负责模型协议、Provider、Tool Loop 和模型能力封装，不写具体 Todo/RSS/命令等业务规则。
- `runtime/respond/` 只负责确定性路由、命令/session/pending 编排、Agent 调用、结果投影和必要上下文维护；不要在这里直接枚举业务 Tool 或维护领域状态。
- Todo、提醒、记忆、定期任务、通知、搜索、知识库、命令执行等领域规则必须放在 `qq-maid-core/src/runtime/tools/<domain>/` 内。
- 新增工具能力时，业务逻辑优先放在 `qq-maid-core/src/runtime/tools/<domain>/` 内。
- Tool 文件只作为工具入口，负责参数解析、上下文校验和结果返回。
- 多步业务流程应抽到 `<domain>/ops.rs`，例如同时更新任务、取消 outbox、生成下一次提醒。
- storage 只负责底层持久化读写；只有需要新增数据库读写或事务语义时才扩展 storage。
- 领域模块必须通过 `tools/<domain>/mod.rs` 提供少量明确门面；不得用 `pub use storage::*` 等通配导出把 Repository 内部类型重新暴露给 Runtime。
- 通用 Tool 注册、整轮投影和状态提示层只能装配领域门面或消费通用 adapter；具体 Tool 名称、业务动作、结果索引合并、成功验真和领域状态分类必须留在 `tools/<domain>/`。
- `CoreService` 是 Gateway 调用 Core 的进程内业务入口；HTTP 层只承载 `/healthz`、受保护的 Web Console/管理 API 和 Markdown 渲染等运维能力，不重新增加旧的 `/query`、`/memory` 或 `/v1/chat` 业务入口。
- 不要在 respond/chat_flow/session/prompt 层新增零散 Todo/Reminder/Memory/Command 业务判断。

依赖方向保持：

```text
qq-maid-gateway-rs
        ↓
qq-maid-core
        ↓
qq-maid-llm
        ↓
qq-maid-common / reqwest / serde / tokio
```

禁止让 `qq-maid-llm` 反向依赖 `qq-maid-core`，也不要让 `qq-maid-core` 绕过 `qq-maid-llm` 直接维护 Provider 协议实现。

## 开始工作前

- 不要直接修改默认分支 `master`；代码或文档修改应在功能分支完成，提交后创建 PR，不要自行合并。
- 先检查工作区已有改动，不能回滚无关用户修改。
- 修改前按任务范围读取资料：普通代码修改读取相关源码、测试和邻近文档；涉及启动、配置、部署、依赖或环境变量时，再读取 `Makefile`、`runtime/config/.env.example` 和运行 / 部署文档；纯文档修改读取目标文档及其引用来源。
- 以当前代码和调用链为准，不根据旧文档、文件名或历史印象推测实现。
- 代码修改前搜索现有实现并优先复用现有模块、helper、错误类型和测试结构。
- 不确定的内容标注“当前未发现 / 需确认”，不要编造结论。
- 不要读取、打印或提交真实 `.env`、私有 prompt、知识资料、SQLite、日志、openid、群 ID、聊天记录、token、secret、API Key 或账号信息。

## 通用修改原则

- 默认做小改动，保持用户可见行为稳定；不要未经要求重写架构、迁移运行路径或引入大依赖。
- 不要恢复 Python 接入层、adapter、fallback、本地 LLM / 查询 / 记忆 / session / 命令 / prompt 入口。
- 不要恢复独立 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等旧入口；Rust HTTP 层只保留 `/healthz`、外部运维、Web Console/管理 API 和 Markdown 渲染等受控能力。
- Web Console 只修改 `web-console/src/`，通过构建生成并提交 `web-console/dist/`；不得手工编辑 `dist/`，也不得把控制台端口直接暴露到公网。
- 不要吞错误、返回空字符串或只生成成功文案来伪造成功状态；工具、构建、测试和发送结果必须以真实返回为准。
- 新增或修改代码时补充必要中文注释，并保留说明业务背景、边界条件、兼容原因、安全要求或设计意图的有效注释。
- 修改已有逻辑时同步检查附近注释是否仍准确；只有注释明显错误、重复或失去意义时才删除。
- 不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。
- 修改文档时避免复制 README 大段细节，优先链接到已有权威文档。
- 代码尽量不要超过1000行，函数不要超过100行，单个文件不要超过3个模块；必要时拆分到 `mod.rs` + `<submodule>.rs`。

## 重要业务与兼容性约束

- Cargo 由根 workspace 统一管理：根 `Cargo.lock` 是唯一锁文件，release 产物位于根 `target/release/`；不要恢复子目录 `Cargo.lock` 或旧 `qq-maid-*/target/` 路径。
- Gateway 负责 QQ 官方、OneBot 11、微信服务号的平台字段解析、消息兼容、发送分支、媒体处理、`/ping` 和日志脱敏；Core / LLM 不应理解 QQ `msg_seq`、stream id、群 at 前缀、OneBot segment 或微信 XML/加密字段等平台协议细节。
- Core 业务入口优先复用 `CoreService` 和 `qq-maid-core/src/runtime/respond/` 现有 flow；跨工具 pending envelope 与通用确认分类优先复用 `qq-maid-core/src/runtime/pending/`，Todo 专属 pending payload、确认/澄清状态机和文案必须放在 `qq-maid-core/src/runtime/tools/todo/`，`qq-maid-core/src/runtime/respond/pending.rs` 只保留会话写入 helper。
- LLM 协议、Provider、路由、fallback、SSE、usage、健康观测、上下文预算、Web Search 和 Agent/Tool Loop 协议留在 `qq-maid-llm`；业务 prompt、session、todo、memory、RSS、知识库和具体 Tool 留在 `qq-maid-core`。
- Tool Calling 只执行服务端显式注册的白名单 Tool。工具调用是否成功、Todo 是否写入、Memory 是否保存等必须以真实工具或持久化结果为准，不能让模型文案代替执行结果。
- 私聊普通消息默认按 `agent.toml` 场景白名单进入可调用 Tool 的 Agent Runtime；群聊完整 Tool Loop 默认关闭，只有场景显式开启且白名单允许时才进入，关闭时仅保留受控 Memory-only 路径。slash 命令、pending 确认、文件处理和宿主机代码执行不得进入 Tool Loop；`/查` 仍是显式联网查询入口。
- Todo 对用户展示的编号与数据库内部 ID 分离。后续“第一条”“刚刚那条”等指代必须依赖 session 中最近可见列表快照或最近操作对象，不能把内部 ID 暴露给模型或用户。
- Todo 删除/取消/恢复语义、session 作用域、记忆确认流程和已确认持久化数据格式不要随意改变。
- 用户明确记忆指令写入 `UserConfirmed`；新增记忆在服务端完成范围、权限和敏感信息校验后可直接保存，不再二次确认。默认关闭的 Session Dream 可从普通聊天提取 `SystemDerived`，但只能写当前用户 Personal 或当前群成员 GroupProfile，不得写 Group 公共记忆、覆盖 `UserConfirmed` 或绕过 opt-out；清空、停用群画像等破坏性操作仍需确认。
- SQLite schema 变更必须通过 migration，并考虑已有 `APP_DB_FILE` 历史数据兼容；业务模块不要在运行时方法里自行建表。
- RSS、Todo 提醒和每日摘要等主动推送必须先写入 Notification Outbox，由统一 Worker 通过 `PushSink` 投递并记录重试/终态；业务模块不得绕过 Outbox 直接发送。`PushTarget` 必须保存真实平台/账号/目标，不能从 `scope_key` 或 `owner_key` 反解析投递目标；成员提醒使用平台无关结构，由 Gateway 生成 QQ `<@...>` 或 OneBot `at`。
- 多平台必须区分平台原始 `ReplyTarget`/`DeliveryTarget`、conversation `scope_key`、业务 `owner_key` 和发言人 `actor`；Core、LLM 和工具不得把平台 raw ID 当作权限、会话或 Todo/Memory owner 的替代品。
- QQ C2C 流式发送首帧成功后，本轮回复归同一个 QQ stream 所有；中间帧或最终帧失败不得再补发第二条普通全文。OneBot/微信当前按各自支持范围发送，不得把 QQ 流式协议扩散到 Core。
- Web Console 使用部署管理员 Session、同源校验和 CSRF 保护；secret 只保存或返回状态，不回传原文。生产应绑定本机或受控内网，并显式配置受信代理与安全 Cookie。
- 日志和诊断输出默认脱敏，不记录 QQ/OneBot/微信 raw event envelope、Authorization header、AppSecret、token、完整 openid、群 ID 或聊天正文。`scripts/diagnose-network.sh` 只能打印 secret 是否存在、脱敏后的 ID/URL、代理和公网出口检查结果。
- 通用日期、时间、身份上下文、输入输出结构、Markdown 转换和脱敏优先复用 `qq-maid-common/` 现有模块。

## 测试与检查

CI 当前在 PR / push 到 `master` 时执行：Rust、前端、Shell 和 Windows 脚本检查分别按变更范围运行。只有 Rust、Cargo、`rust-toolchain`、`runtime/`、`Makefile` 或 CI 工作流等相关文件变更时才运行 Rust 步骤；仅有 Web Console 变更时只运行前端检查，混合变更按各自范围运行，Shell/Windows 脚本由独立检查负责。push 到 `master` 会忽略纯文档路径。Rust 步骤包括：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --all-features
```

本地按影响范围选择检查：

- 代码变更提交前至少执行影响范围对应的格式化检查、测试和 `cargo check`；涉及启动、配置、依赖或发布时再执行 release 构建。
- `make test` 执行 workspace 的 `cargo fmt --all -- --check`、`cargo test --workspace` 和 `cargo check --workspace`；它不等同于 CI 的 clippy、`--all-features` 测试或 release 构建。
- 只影响某个 crate 时可先使用 `make test-common`、`make test-llm`、`make test-core` 或 `make test-gateway` 做局部检查；跨模块或提交前执行 `make test`，并按需补充 CI 中的 clippy、`--all-features` 测试或 release 构建。
- 修改 `scripts/*.sh` 时至少执行对应的 `bash -n`；修改 `scripts/*.ps1` 或 Windows 控制脚本时执行 PowerShell 语法检查和对应 smoke/regression 测试。
- 修改 `web-console/src/` 时在 `web-console/` 执行 `npm ci`、`npm run check`、`npm run build`、`npm test`，并确认 `git diff --exit-code -- web-console/dist`；`dist/` 是 Rust 嵌入的可复现产物，不能漏提交或手工修补。
- 修改 Docker/Compose 或容器部署脚本时至少执行 `make test-docker`；涉及部署、配置迁移、备份恢复或 release 包时按对应 `docs/deployment/` 文档补充检查。
- 涉及诊断入口时执行 `make diagnose`。
- 修改启动、配置、依赖、QQ/OneBot/微信事件或任意 Provider 调用时，需要本地启动或运行相应诊断验证；可按范围使用 `scripts/validate-runtime.sh check|glm|console|restart-source`。
- 修改 `qq-maid-llm` 的 Provider 协议、SSE 解析、模型候选链、上下文预算或 Agent/Tool Loop 时，至少跑 `make test-llm`，并确认 Core 调用链无回归。
- 纯文档变更不需要跑完整 Rust CI；至少执行 `git diff --check`，人工核对相对链接、文件路径、命令和敏感信息。

如果某项检查无法执行，最终说明里必须写明原因。不得伪造未执行的检查结果。

## 完成报告

最终总结默认说明：完成了什么、主要修改位置、执行了哪些验证、未验证内容及原因、残余风险。涉及代码注释、敏感信息、migration、兼容性或真实环境验证时，再专项说明对应处理结果。

commit message 使用简洁中文：`类型: 简短说明`，例如 `docs: 精简 Agent 维护规则`。一次 commit 只做一类事情，不要混入无关修改。
