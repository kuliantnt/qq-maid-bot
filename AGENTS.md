
# AGENTS.md

给 Codex / AI Agent 后续维护本仓库使用的简短说明。

项目运行、部署、排障细节以根 `README.md`、`qq-maid-gateway-rs/README.md`、`qq-maid-core/` 配置说明、`Makefile` 和相关源码为准；不要在这里重复 README。

请使用中文回复。

新增或修改代码时，应补充必要的中文注释，并保留已有的有效注释。

## 项目定位

这是一个 QQ 官方机器人本地版项目。

当前主线只保留：

* `qq-maid-gateway-rs/`：Rust QQ 官方 gateway 接入层，负责 QQ 事件、消息转换、白名单、本地 `/ping` 和回复发送。
* `qq-maid-core/`：Rust Core 模块，负责 `CoreService`、查询、记忆、session、todo、命令、prompt 和业务模型路由；通过 `qq-maid-llm` 调用模型。
* `qq-maid-llm/`：Rust LLM 基础设施 crate，负责模型调用协议、Provider 路由、fallback、SSE、usage、健康观测和 OpenAI Web Search 协议；不依赖 `qq-maid-core`。
* `qq-maid-common/`：Rust 共享基础工具，仅承载两个及以上 crate 共用的通用逻辑，且不得依赖业务状态。
* `runtime/`：服务器部署运行目录，只放 release 二进制、运行配置和运行产物。
* `scripts/`：部署、进程控制和诊断脚本源码。

依赖方向固定为：

```text
qq-maid-gateway-rs
        ↓
qq-maid-core
        ↓
qq-maid-llm
        ↓
qq-maid-common / reqwest / serde / tokio
```

禁止 `qq-maid-llm` 反向依赖 `qq-maid-core`，也不要让 `qq-maid-core` 绕过 `qq-maid-llm` 直接维护 Provider 协议实现。

当前 Rust 构建由仓库根目录 Cargo Workspace 统一管理：

* 根 `Cargo.toml` 维护 workspace members；
* 根 `Cargo.lock` 是唯一锁文件；
* release 产物位于根 `target/release/`；
* 不要恢复子目录 `Cargo.lock`，也不要在文档或脚本中继续引用 `qq-maid-*/target/` 旧路径。

不要恢复或新增：

* Python 接入层；
* Python adapter；
* Python fallback；
* Python 本地 LLM / 查询 / 记忆 / session / 命令 / prompt 入口；
* 独立 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等旧入口。

Rust HTTP 层只公开外部运维和控制台能力：

* `GET /healthz`
* 启用控制台时的 `/console/` 和 `/api/v1/markdown/render`

## 分支与 PR 策略

* 不要直接修改默认分支（`master`）。
* 所有修改请新建功能分支完成。
* 修改完成后运行格式化和测试。
* 提交并推送功能分支后创建 Pull Request。
* 不要自行合并 Pull Request。

## 工作方式

* 默认做小改动，保持用户可见行为稳定。
* 修改前先读相关 README、Makefile、`runtime/config/.env.example` 和邻近源码。
* 不确定的内容标注“当前未发现 / 需确认”，不要编造。
* 不要未经要求重写架构、迁移运行路径或引入大依赖。
* 不要把具体人设、群聊内容、真实用户信息或业务材料写死进代码。
* 不要回滚无关用户改动。
* 不要为了压缩代码、统一风格或重构而随意删除已有注释。
* 如果代码修改导致原注释不再准确，应同步更新注释，而不是直接删除。
* 只有在注释明显错误、完全重复或已经失去意义时才可以删除。

## 子 Agent 使用规则

任务过程中应积极调用可用的子 agents / subagents。

优先让子 agents 并行处理：

* 阅读不同模块；
* 搜索相关实现；
* 检查重复逻辑；
* 梳理测试覆盖；
* 执行格式化、编译、单测或脚本语法检查；
* 对改动方案做独立 review。

主 agent 需要汇总子 agent 结论，并在修改前去重、交叉验证。

不要把未经验证的子 agent 结论直接写进代码或最终说明。

## 代码修改原则

优先复用现有代码、现有模块、现有 helper、现有错误类型和现有测试结构。

重点保持这些边界：

* gateway 负责 QQ 接入、事件解析、`/ping` 本地诊断和回复发送。
* LLM 协议、Provider、路由、fallback、SSE、usage、健康观测和 Web Search 协议放在 `qq-maid-llm`。
* 业务 prompt、查询、记忆、session、todo、命令和业务模型路由放在 `qq-maid-core`，通过 `qq-maid-llm` 的统一入口调用模型。
* 新功能优先通过 `CoreService` 或 Rust 内部模块承载，不要恢复内部 HTTP respond 主入口。
* 具体 session/search/todo/memory/chat flow 在 `runtime/respond/` 下维护。
* pending operation 类型与确认分类优先复用 `runtime/pending/`。
* respond pending 分发逻辑保留在 `runtime/respond/pending.rs`。
* provider-facing chat primitives 保留在 `qq-maid-llm/src/provider/types.rs`；core 侧 `provider/` 仅作为兼容 re-export 入口。
* 查询模块保留在 `runtime/query/`；`/查` 的模型协议实现保留在 `qq-maid-llm/src/web_search.rs`。
* 文件持久化实现放在 `storage/session.rs`、`storage/memory.rs`、`storage/todo.rs`。

不要随意改变：

* QQ 消息入口；
* slash 指令；
* 记忆确认流程；
* session 作用域；
* todo 软删除语义；
* OpenAI / DeepSeek fallback；
* Rust `CoreService` 调用路径；
* 已确认持久化数据格式。

不要在 `qq-maid-core` 中重新实现已迁入 `qq-maid-llm` 的 Provider 协议、SSE frame 解析、模型候选链或健康观测逻辑；需要这些能力时直接复用 `qq-maid-llm` 的公开入口（`LlmService::chat` / `web_search` / `web_search_stream` / `upstream_status`）。

通用逻辑优先复用，不要在具体命令里重复实现。

例如日期、时间和时区语义优先复用 `qq-maid-common/src/time_context.rs`；Core 内部原 `util/time_context.rs` 仅作为兼容入口保留。

### 注释规则

* 保留已有能够说明业务背景、边界条件、兼容原因、安全要求或设计意图的注释。
* 不要在重构、抽取 helper、合并测试或格式化时批量删除注释。
* 移动代码时，应将相关注释一并移动到新的实现位置。
* 修改已有逻辑时，应检查附近注释是否仍然准确，并同步更新。
* 新增或修改非显而易见的逻辑时，需要添加必要的中文注释。
* 下列内容应优先添加注释：

  * 业务边界和特殊规则；
  * fallback、兼容分支和降级行为；
  * 时间、日期和时区语义；
  * session、pending、todo、memory 的状态转换；
  * 持久化格式和兼容约束；
  * QQ 平台字段或特殊消息处理；
  * 脱敏、安全和敏感信息保护逻辑；
  * 看似可以简化但实际不能简化的实现原因。
* 注释应优先说明“为什么这样做”和“需要保持什么约束”，不要只重复代码表面行为。
* 不要给每一行代码添加机械翻译式注释。
* 不要保留已经失真、误导或与当前实现冲突的注释。

## QQ Gateway 注意事项

修改 QQ 事件、intent、权限、白名单、群聊 / 私聊响应逻辑前，先读：

* `README.md`
* `qq-maid-gateway-rs/README.md`
* gateway 相关源码

要求：

* 平台字段解析和发送分支优先放在 `qq-maid-gateway-rs/`。
* 图片、贴纸、emoji、结构化消息处理要保持现有兼容行为。
* 日志和调试输出默认脱敏。
* 不记录 QQ raw event envelope、Authorization header、AppSecret、token 或完整 openid。

## Core / Todo / Memory / Session 注意事项

* 修改 session、prompt、会话命令、记忆确认、查询命令时，优先改 Rust `CoreService` 入口及其复用的 respond flow。

* 修改指令、记忆或 session 逻辑时，优先保持已确认的持久化数据兼容。

* 一次性待确认 pending 状态可通过 README 迁移说明清理，不必在运行时长期兼容。

* `/resume` / `/恢复` 是推荐恢复会话入口。

* `/list` 只作为 deprecated 兼容别名保留。

* 待办查询、完成、删除和批量删除优先在 Rust `CoreService` 调用链与 `runtime/todo.rs` 业务出口中维护。

* todo 文件持久化在 `storage/todo.rs`，删除应保持软删除语义。

* 记忆指令保持 Rust `CoreService` 语义：

  * `/memory`、`/记忆`、`/记` 不带内容用于查看长期记忆列表；
  * 带内容才创建待确认记忆草稿。

* 长期记忆只能通过明确记忆指令生成草稿，并由用户确认后写入。

* 普通聊天不要自动写长期记忆。

## 常用验证

`.github/workflows/ci.yml` 在 PR / push 到 master 时会跑以下四步，提交前在本地直接复跑，避免 CI 失败往返：

```bash
# 1. 格式化检查（不写入，仅校验）
cargo fmt --all -- --check

# 2. Clippy，警告视为错误
cargo clippy --workspace --all-targets --all-features -- -D warnings

# 3. 全 workspace 测试
cargo test --workspace --all-features

# 4. release 构建
cargo build --workspace --release --all-features
```

最低要求：

* 代码变更提交前至少跑完 1–3 步。
* 纯文档变更（仅 `docs/`、`README.md`、`AGENTS.md` 等说明文档）不需要走一遍整套 CI；按影响范围做必要的文本自检即可。
  * CI 行为：`ci.yml` 在 PR 阶段永远触发（保证状态检查不缺失），job 内通过 `git diff` 检测变更文件，纯文档则自动跳过 Rust 步骤。push 到 master 时通过 `paths-ignore` 从源头跳过纯文档推送。
* 改动涉及启动、配置、依赖或发布时，再跑第 4 步。
* 修改 `scripts/*.sh`：至少执行 `bash -n` 对应脚本。
* 涉及诊断入口时执行 `make diagnose`。
* 修改启动、配置、依赖、QQ 事件或 OpenAI / DeepSeek 调用：需要本地启动验证。
* 修改 `qq-maid-llm` 的 Provider 协议、SSE 解析或模型候选链：需要跑 `qq-maid-llm` 的单测，并确认 core 侧调用链无回归。
* 修改代码后按项目已有命令格式化，不要为了格式化引入新依赖。

如果某项检查无法执行，需要在最终说明里写明原因。

## 配置与敏感信息

环境变量模板在：

```text
runtime/config/.env.example
```

不要读取、打印或提交真实 `.env`。

禁止提交：

* token、secret、API Key、bot appid、私钥、账号信息；
* 真实 QQ 群聊、私聊内容、openid、群 ID、用户数据；
* `qq-maid-core/data/`
* `runtime/data/`
* `runtime/logs/`
* `runtime/run/`
* `runtime/qq-maid-bot`
* `runtime/diagnose-network.sh`
* 各目录下真实 `.env`

日志和调试输出默认脱敏。

`scripts/diagnose-network.sh` 只能打印 secret 是否存在、脱敏后的 ID/URL、代理和公网出口检查结果。

## 提交规范

commit message 使用简洁中文：

```text
类型: 简短说明
```

常用类型：

* `feat`：新增功能
* `fix`：修复问题
* `docs`：文档更新
* `refactor`：重构代码
* `style`：格式调整
* `test`：测试相关
* `chore`：构建、依赖、配置、脚本等杂项

要求：

* 一次 commit 只做一类事情。
* 不要混入无关修改。
* 涉及配置、启动方式、环境变量时，commit 说明里要体现。
* 不要提交密钥、token、账号、私聊记录或真实用户数据。

## 发版本流程

每个发布版本对应一个 `vX.Y.Z` git tag（历史 tag 见 `git tag --list 'v*'`）。
发版本时按顺序执行：

1. 改版本号：根 `Cargo.toml` 的 `version` 字段（如 `0.4.1` → `0.4.2`）。
   * 子 crate（`qq-maid-core`、`qq-maid-gateway-rs`、`qq-maid-llm`、`qq-maid-common`）的版本号独立维护，不跟随根包，除非该 crate 本身有变更需要发布。
   * 改完 `cargo build` 让 `Cargo.lock` 同步更新，一并提交。
2. 更新 `CHANGELOG.md`：在顶部新增 `## [vX.Y.Z] - YYYY-MM-DD` 条目，按 keep a changelog 格式写变更。
3. 提交：`git commit -m "chore: 发布 vX.Y.Z"` 或与版本内容合并的 commit。
4. 打 tag：`git tag vX.Y.Z`（annotated tag 可加 `-a -m "release vX.Y.Z"`，历史均为轻量 tag，保持一致即可）。
5. 推送：`git push && git push --tags`（或 `git push origin vX.Y.Z` 单独推 tag）。

注意：

* tag 必须打在对应版本的 commit 上，不要漏打或打错版本号。
* tag 必须打在 master 分支上。`release.yml` 的 `preflight` job 会在矩阵构建前校验：
  * tag commit 是否为 master 的祖先（`git merge-base --is-ancestor`），不在 master 上则立即失败，不启动 5 平台构建。
  * tag commit 相对第一父提交是否包含有效代码变更（通过 `git diff` 检查变更文件），纯文档 tag 会跳过构建和发布。
* 发版本前确保版本 commit 已合并到 master，再打 tag 推送。
* 发版本前先跑完"常用验证"里的 CI 四步。
* 不要把未发布的版本号写进 README 或文档里提前预告。

## 修改前后检查

修改前：

* 先查看 README、Makefile、`runtime/config/.env.example` 和相关源码。
* 检查工作区已有改动，不要回滚无关用户修改。
* 搜索现有实现，优先复用已有代码。
* 检查相关代码附近是否存在说明业务约束或设计原因的注释。
* 能并行拆分的检查任务，优先交给子 agents。

修改后：

* 执行对应格式化命令。
* 按影响范围执行测试。
* 检查已有有效注释是否被误删。
* 检查新增或修改的复杂逻辑是否补充了必要中文注释。
* 检查代码与注释是否一致，不保留失真或过期注释。
* 文档改动需确认没有写入敏感信息，也没有复制 README 大段细节。
* 如果涉及结构变更，需要同步更新本文及 README 文件。

最终总结需要说明：

* 改了什么；
* 复用了哪些现有代码 / helper；
* 调用了哪些子 agents，以及它们分别检查了什么；
* 添加或更新了哪些必要注释；
* 是否删除了已有注释，以及删除原因；
* 执行了什么格式化；
* 执行了什么测试；
* 没执行的检查及原因；
* 是否确认没有写入敏感信息。
