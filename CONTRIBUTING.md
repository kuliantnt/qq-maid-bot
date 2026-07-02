# 贡献指南

欢迎为 QQ Maid Bot 贡献代码、文档或反馈问题。

## 行为准则

- 保持友善、专业，就事论事。
- 不提交包含攻击性、歧视性或侵犯他人隐私的内容。
- Issue 和 PR 讨论围绕项目本身，不做无关推广。

## 快速开始

### 环境要求

- Rust 工具链（推荐通过 [rustup](https://rustup.rs) 安装，版本与 `rust-toolchain.toml` 一致）
- Git

### 克隆并构建

```bash
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot
make build
```

### 运行测试

提交前至少运行对应模块的测试：

```bash
make test-core      # 修改 qq-maid-core 后
make test-gateway   # 修改 qq-maid-gateway-rs 后
make test-common    # 修改 qq-maid-common 后
make test           # 跨模块修改或提交前（含格式检查）
make diagnose       # 涉及诊断入口时
```

## 提交规范

commit message 使用简洁中文，格式为：

```text
类型: 简短说明
```

常用类型：

| 类型 | 说明 |
| --- | --- |
| `feat` | 新增功能 |
| `fix` | 修复问题 |
| `docs` | 文档更新 |
| `refactor` | 重构代码 |
| `style` | 格式调整（不影响行为） |
| `test` | 测试相关 |
| `chore` | 构建、依赖、配置、脚本等杂项 |

一次 commit 只做一类事情，不混入无关修改。

## 代码风格

- 新增或修改逻辑时，用中文为边界条件、兼容原因和安全要求添加注释。
- 使用 `cargo fmt` 格式化 Rust 代码。项目已有格式化配置，不需要额外安装格式化工具。
- 单个 Rust 源文件超过 **1000 行** 时，应检查是否存在可自然拆分的职责边界，但行数本身不是强制拆分理由。
- 优先拆分自包含、输入输出清晰、可独立测试，或具有不同变化原因的逻辑；拆分后应使主模块职责明显更清晰。
- 不要为了降低行数机械移动代码。高内聚的存储事务、协议模型、集中测试用例等，可以保留为大文件。
- 拆分不应引入无意义转发层、复杂泛型、循环依赖，或为了跨文件调用而大量扩大 `pub` / `pub(crate)` 可见性。
- 纯结构拆分必须保持用户可见行为、公开接口、配置项、日志语义和持久化格式不变，并运行与影响范围匹配的测试。
- 新增或修改逻辑时，用中文为边界条件、兼容原因和安全要求添加注释。
- 注释优先说明“为什么这样做”和“需要保持什么约束”，不要只重复代码表面行为。
- 保留已有的有效注释，不要为压缩代码或重构而批量删除。
- 如果代码修改导致原注释不再准确，应同步更新注释。

## 项目结构

| 目录 | 说明 |
| --- | --- |
| `qq-maid-gateway-rs/` | Rust QQ 官方 Gateway 接入层 |
| `qq-maid-core/` | Rust Core 模块（聊天、记忆、Todo、RSS、查询） |
| `qq-maid-common/` | Gateway 和 Core 共享的基础工具 |
| `runtime/` | 服务器部署运行目录 |
| `scripts/` | 部署、进程控制和诊断脚本 |

构建由根目录 Cargo Workspace 统一管理，release 产物位于 `target/release/`。

## 代码修改原则

- 优先复用现有模块、helper、错误类型和测试结构，不在具体命令里重复实现。
- 不要随意改变 QQ 消息入口、slash 指令、记忆确认流程、session 作用域和持久化数据格式。
- 通用逻辑优先复用（例如日期和时区用 `qq-maid-common/src/time_context.rs`）。
- 保持 Gateway 与 Core 的职责分层：
  - Gateway 只负责 QQ 接入、事件解析、消息转换和回复发送。
  - Core 通过 `CoreService` 负责查询、记忆、session、todo、命令、prompt 和 provider 调用。

## 测试要求

| 修改范围 | 最低测试要求 |
| --- | --- |
| `qq-maid-core/` | `make test-core` |
| `qq-maid-gateway-rs/` | `make test-gateway` |
| `qq-maid-common/` | `make test-common` |
| 跨模块 | `make test` |
| `scripts/*.sh` | `bash -n` 对应脚本 |
| 启动、配置、QQ 事件或 provider 调用 | 本地启动验证 |

如果某项检查无法执行，在 PR 说明中写清原因。

## 安全与隐私

### 禁止提交

以下内容**绝对不能**出现在 commit 或 PR 中：

- Token、AppSecret、API Key、Bot AppID、私钥
- 真实 QQ 群聊记录、私聊内容、OpenID、群 ID、用户数据
- 各目录下的真实 `.env` 文件
- `runtime/data/`、`runtime/logs/`、`runtime/run/` 中的运行产物

### 日志与调试

- 日志和调试输出默认脱敏。
- `scripts/diagnose-network.sh` 只能打印 secret 是否存在、脱敏后的 ID/URL、代理和公网出口检查结果。

### 配置方式

- 公开仓库只提供 `.example` 模板（如 `runtime/config/.env.example`）。
- 真实 Prompt、世界观、知识资料等私人配置放在 `.gitignore` 忽略的运行目录中。

## 提 Issue

提交 bug 报告或功能建议时，请包含：

- 运行环境（操作系统、Rust 版本）
- 复现步骤
- 期望行为与实际行为的对比
- 相关日志（注意脱敏）

## 提 PR

1. Fork 本仓库并创建分支。
2. 按上述规范编写代码和 commit message。
3. 运行对应模块测试，确保通过。
4. 确保 CI 工作流（`make test`）能通过。
5. 在 PR 描述中说明：改了什么、为什么这样改、测试情况。
6. 确认没有写入敏感信息。

提交后 CI 会自动运行格式检查、编译和测试。PR 合并由维护者负责。

## 文档

- 项目总览：[README.md](./README.md)
- 开发维护文档：[DEVELOPMENT.md](docs/DEVELOPMENT.md)
- Core 模块文档：[qq-maid-core/README.md](./qq-maid-core/README.md)
- Gateway 文档：[qq-maid-gateway-rs/README.md](./qq-maid-gateway-rs/README.md)
- 运行目录说明：[runtime/README.md](./runtime/README.md)
- AI Agent 维护说明：[AGENTS.md](./AGENTS.md)

## 鸣谢

特别感谢以下贡献者对项目的支持：

- [@YsLtr](https://github.com/YsLtr) 提供 Agent 工具路线建议
- [@Su5vaR4](https://github.com/Su5vaR4) 协助管理项目
- [@Dysonnnn](https://github.com/Dysonnnn) 对项目的贡献
- [@GrainRR](https://github.com/GrainRR) 对项目的赞助

## License

贡献的代码将采用与项目一致的 [MIT License](./LICENSE)。
