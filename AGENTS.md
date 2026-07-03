# AGENTS.md

给 Codex / AI Agent 后续维护本仓库使用的长期规则。请使用中文回复。

项目运行、部署、排障和详细架构以 [README.md](./README.md)、[docs/DEVELOPMENT.md](./docs/DEVELOPMENT.md)、各 crate README、[Makefile](./Makefile)、[runtime/config/.env.example](./runtime/config/.env.example) 和源码为准；根 `AGENTS.md` 只保留每次进入仓库都应遵守的项目级硬约束。

## 项目概述

这是一个 Rust 编写的 QQ 官方机器人本地版项目，由根目录 Cargo Workspace 统一管理。

主要边界：

* `qq-maid-gateway-rs/`：QQ 官方 gateway 接入、事件解析、白名单、本地 `/ping` 和回复发送。
* `qq-maid-core/`：`CoreService`、普通聊天、查询、记忆、session、todo、RSS、命令、prompt 和业务 Tool。
* `qq-maid-llm/`：模型协议、Provider 路由、fallback、SSE、usage、健康观测、OpenAI Web Search 和 Tool Loop 协议。
* `qq-maid-common/`：两个及以上 crate 共用、无业务状态的基础工具。
* `runtime/`：部署运行目录，只放 release 二进制、运行配置和运行产物。
* `scripts/`：部署、进程控制和诊断脚本源码。

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

* 不要直接修改默认分支 `master`；代码或文档修改应在功能分支完成，提交后创建 PR，不要自行合并。
* 先检查工作区已有改动，不能回滚无关用户修改。
* 修改前按任务范围读取资料：普通代码修改读取相关源码、测试和邻近文档；涉及启动、配置、部署、依赖或环境变量时，再读取 `Makefile`、`runtime/config/.env.example` 和运行 / 部署文档；纯文档修改读取目标文档及其引用来源。
* 以当前代码和调用链为准，不根据旧文档、文件名或历史印象推测实现。
* 代码修改前搜索现有实现并优先复用现有模块、helper、错误类型和测试结构。
* 不确定的内容标注“当前未发现 / 需确认”，不要编造结论。
* 不要读取、打印或提交真实 `.env`、私有 prompt、知识资料、SQLite、日志、openid、群 ID、聊天记录、token、secret、API Key 或账号信息。

## 通用修改原则

* 默认做小改动，保持用户可见行为稳定；不要未经要求重写架构、迁移运行路径或引入大依赖。
* 不要恢复 Python 接入层、adapter、fallback、本地 LLM / 查询 / 记忆 / session / 命令 / prompt 入口。
* 不要恢复独立 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等旧入口；Rust HTTP 层只保留外部运维和控制台能力。
* 不要吞错误、返回空字符串或只生成成功文案来伪造成功状态；工具、构建、测试和发送结果必须以真实返回为准。
* 新增或修改代码时补充必要中文注释，并保留说明业务背景、边界条件、兼容原因、安全要求或设计意图的有效注释。
* 修改已有逻辑时同步检查附近注释是否仍准确；只有注释明显错误、重复或失去意义时才删除。
* 不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。
* 修改文档时避免复制 README 大段细节，优先链接到已有权威文档。

## 重要业务与兼容性约束

* Cargo 由根 workspace 统一管理：根 `Cargo.lock` 是唯一锁文件，release 产物位于根 `target/release/`；不要恢复子目录 `Cargo.lock` 或旧 `qq-maid-*/target/` 路径。
* Gateway 负责 QQ 平台字段解析、消息兼容、发送分支、`/ping` 和日志脱敏；Core / LLM 不应理解 QQ `msg_seq`、stream id、群 at 前缀等平台发送细节。
* Core 业务入口优先复用 `CoreService` 和 `qq-maid-core/src/runtime/respond/` 现有 flow；pending 类型与确认分类优先复用 `qq-maid-core/src/runtime/pending/` 和 `qq-maid-core/src/runtime/respond/pending.rs`。
* LLM 协议、Provider、路由、fallback、SSE、usage、健康观测、Web Search 和 Tool Loop 协议留在 `qq-maid-llm`；业务 prompt、session、todo、memory、RSS 和具体 Tool 留在 `qq-maid-core`。
* Tool Calling 只执行服务端显式注册的白名单 Tool。工具调用是否成功、Todo 是否写入、Memory 是否保存等必须以真实工具或持久化结果为准，不能让模型文案代替执行结果。
* 当前私聊普通聊天可进入 Tool Loop；群聊、slash 命令、pending 确认、文件处理和宿主机代码执行不得默认进入 Tool Loop。
* Todo 对用户展示的编号与数据库内部 ID 分离。后续“第一条”“刚刚那条”等指代必须依赖 session 中最近可见列表快照或最近操作对象，不能把内部 ID 暴露给模型或用户。
* Todo 删除/取消/恢复语义、session 作用域、记忆确认流程和已确认持久化数据格式不要随意改变。
* 长期记忆只能通过明确记忆指令生成草稿，并由用户确认后写入；普通聊天不要自动写长期记忆。
* SQLite schema 变更必须通过 migration，并考虑已有 `APP_DB_FILE` 历史数据兼容；业务模块不要在运行时方法里自行建表。
* C2C 流式发送首帧成功后，本轮回复归同一个 QQ stream 所有；中间帧或最终帧失败不得再补发第二条普通全文。
* 日志和诊断输出默认脱敏，不记录 QQ raw event envelope、Authorization header、AppSecret、token、完整 openid、群 ID 或聊天正文。`scripts/diagnose-network.sh` 只能打印 secret 是否存在、脱敏后的 ID/URL、代理和公网出口检查结果。
* 通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context/`。

## 测试与检查

CI 当前在 PR / push 到 `master` 时执行。PR 只在 Rust、Cargo、`runtime/`、`scripts/` 或 `Makefile` 等相关文件变更时运行 Rust 步骤；push 到 `master` 会忽略纯文档路径。Rust 步骤包括：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --all-features
```

本地按影响范围选择检查：

* 代码变更提交前至少执行影响范围对应的格式化检查、测试和 `cargo check`；涉及启动、配置、依赖或发布时再执行 release 构建。
* `make test` 执行 workspace 的 `cargo fmt --all -- --check`、`cargo test --workspace` 和 `cargo check --workspace`；它不等同于 CI 的 clippy、`--all-features` 测试或 release 构建。
* 只影响某个 crate 时可先使用 `make test-common`、`make test-llm`、`make test-core` 或 `make test-gateway` 做局部检查；跨模块或提交前执行 `make test`，并按需补充 CI 中的 clippy、`--all-features` 测试或 release 构建。
* 修改 `scripts/*.sh` 时至少执行 `bash -n` 对应脚本。
* 涉及诊断入口时执行 `make diagnose`。
* 修改启动、配置、依赖、QQ 事件或 OpenAI / DeepSeek / BigModel 调用时，需要本地启动或运行相应诊断验证。
* 修改 `qq-maid-llm` 的 Provider 协议、SSE 解析、模型候选链或 Tool Loop 时，至少跑 `make test-llm`，并确认 Core 调用链无回归。
* 纯文档变更不需要跑完整 Rust CI；至少执行 `git diff --check`，人工核对相对链接、文件路径、命令和敏感信息。

如果某项检查无法执行，最终说明里必须写明原因。不得伪造未执行的检查结果。

## 完成报告

最终总结默认说明：完成了什么、主要修改位置、执行了哪些验证、未验证内容及原因、残余风险。涉及代码注释、敏感信息、migration、兼容性或真实环境验证时，再专项说明对应处理结果。

commit message 使用简洁中文：`类型: 简短说明`，例如 `docs: 精简 Agent 维护规则`。一次 commit 只做一类事情，不要混入无关修改。
