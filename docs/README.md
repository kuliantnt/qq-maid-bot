# 仓库文档导航

本目录保留开发边界、部署约定、设计基线和可 review 的历史记录。面向使用者的安装、配置和场景化教程优先查看 [项目 Wiki](https://github.com/kuliantnt/qq-maid-bot/wiki)；程序真实行为仍以当前源码、测试、配置模板和各 crate README 为准。

## 首先阅读

| 文档 | 用途 |
| --- | --- |
| [项目 README](../README.md) | 项目定位、快速开始、配置入口和用户可见能力 |
| [开发维护文档](./DEVELOPMENT.md) | 架构边界、工程结构、常用命令和检查要求 |
| [runtime 运行文档](../runtime/README.md) | 运行目录、部署产物、配置路径、控制脚本和诊断 |
| [环境变量模板](../runtime/config/.env.example) | 现行环境变量、默认值和字段说明 |

## 部署与运维

| 文档 | 用途 |
| --- | --- |
| [Docker 部署 · 人话版](./deployment/docker-simple.md) | 5 分钟跑起来，大白话步骤，大部分配置在网页完成 |
| [Docker 与 Compose 部署](./deployment/docker.md) | 镜像、持久化、多实例、自动部署与回滚 |
| [配置迁移、备份恢复与安全升级](./deployment/migration-backup.md) | 旧配置迁移、SQLite 一致性备份、恢复与 schema 边界 |
| [测试服务器 Docker 部署](./deployment/test_server.md) | 测试环境初始化、GHCR、GitHub Actions 和自动回滚 |
| [OneBot 11 / NapCat 接入](./development/onebot11-napcat.md) | OneBot 反向 WebSocket 的配置、能力和排障 |
| [`/ops` 白名单运维命令](./development/ops-command.md) | 固定程序、权限边界和回执 |
| [`/ops codex` 长任务](./development/ops-codex.md) | Codex CLI、固定工作区、进度与取消 |

## 开发与接口

| 文档 | 用途 |
| --- | --- |
| [配置中心设计与字段清单](./development/config-center.md) | 受管 TOML、环境覆盖、secret 与主密钥边界 |
| [管理 API 约定](./development/management-api.md) | 管理员认证、统一响应、分页与 Todo 管理 API |
| [控制台用户数据 API](./development/console-user-data-api.md) | 独立前端使用的用户偏好与通用文件接口契约 |
| [自定义 Tool 指南](./development/custom-tools.md) | Tool 注册、场景白名单、领域后处理与安全要求 |
| [Web Console API 契约](./API_CONTRACTS.md) | 前端 API、认证边界、响应状态和未来接口槽位 |
| [Web Console 组件注册表](./COMPONENT_REGISTRY.md) | 组件层级、状态契约和扩展规则 |
| [Web Console 交互契约](./INTERACTION_CONTRACTS.md) | 配置保存、冲突、Secret、校验和重启交互 |
| [Web Console 主题契约](./THEME.md) | 语义主题 token、预设和浏览器偏好持久化 |
| [Gateway README](../qq-maid-gateway-rs/README.md) | 平台事件、消息发送、主动推送与 `/ping` |
| [Core README](../qq-maid-core/README.md) | `CoreService`、会话、命令、业务 Tool 和 HTTP facade |
| [LLM README](../qq-maid-llm/README.md) | Provider、路由、fallback、SSE、Web Search 和 Agent Loop |
| [Web Console README](../web-console/README.md) | 部署管理控制台前端源码与可复现构建 |

## 设计基线

| 文档 | 用途 |
| --- | --- |
| [Scope 与 Identity 边界](./design/scope-identity-boundary.md) | conversation / actor / interaction / owner / delivery target 术语 |
| [业务 Owner 策略](./design/business-owner-strategy.md) | Todo、Memory、Session、RSS 和主动推送的归属 |
| [Scope Key 迁移策略](./design/scope-key-migration-strategy.md) | 历史数据兼容与 identity rebaseline |
| [统一响应事件流](./design/response-event-runtime.md) | Core 响应事件与 Gateway 渲染边界 |
| [Tool Calling 与 QQ 投递](./design/tool-calling-qq-delivery.md) | Agent / Tool 结果到 QQ 发送所有权 |
| [Memory WebUI 身份授权与 API](./design/memory-webui-auth-api.md) | 已落地管理员认证基线和未实现 Memory API 的门禁 |
| [Memory v3 与 Grok Build 对照](./design/memory-grok-build-evaluation.md) | Session Dream 移植来源、差异和安全边界 |

## 调研与历史记录

- [`analysis/`](./analysis/)：针对外部实现或特定问题的时点性调研，不是当前协议的权威来源。
- [任务与归档索引](./tasks/README.md)：说明当前任务文档状态，并按主题索引已完成或已取代的任务记录。
- [设计归档](./design/archive/README.md)：已被当前设计取代、但仍需要保留追溯价值的设计稿。

## 目录约定

- `deployment/`：安装、部署、升级、备份、回滚和环境排障。
- `development/`：开发接入、配置契约、管理 API 和受控运维能力。
- `design/`：仍约束当前实现或后续实现的设计基线；过时方案移入 `design/archive/`。
- `analysis/`：时点性调研、对比和排查报告，文首应标注对象或时间。
- `tasks/`：只保留尚未完成的可执行任务文档和索引；已完成或已取代的记录移入 `tasks/archive/`。
- `img/`：文档图片和品牌素材。

新增文档时应在文首说明目的和状态；如果文档只是某个时点的任务拆解或调研，不要伪装成“当前实现”。重命名或归档后必须同步更新仓库内相对链接。
