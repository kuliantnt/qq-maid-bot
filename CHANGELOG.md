# Changelog

本文档基于 [keep a changelog](https://keepachangelog.com/zh-CN/1.0.0/) 格式，记录每个已发布版本的变更。

## [v0.21.4] - 2026-07-25

### Changed

* **QQ 引用消息解析重构**（PR #582）：按 QQ 最新 `message_type=103` 结构递归保留引用元素，分离引用正文与当前正文，支持嵌套引用和缺失 `ref_msg_idx` 的 payload 保留；引用索引缺失或正文混合污染时 fail-closed，避免引用文字重复进入模型上下文。
* **QQ API v2 富媒体上传**（PR #584）：URL 图片继续走场景对应的 `/files` 接口；本地文件和 Base64 图片改为 `upload_prepare -> PUT 分片 -> upload_part_finish -> merge`，支持 C2C 与群聊端点，并校验分片索引、块范围和文件格式。
* **QQ 入站消息归一化**（PR #584）：支持 0、3、101、102 消息内容类型；ARK 仅保留白名单文本摘要并掩码 URL，平行消息递归处理且限制深度、节点、文本和媒体数量，103 继续沿用已有引用正文路径。
* **群聊 @ 与消息去重**（PR #584）：`GROUP_AT_MESSAGE_CREATE` 直接视为 @ 机器人；普通群消息优先匹配 READY 学到的稳定 ID，并在 `msg_idx` 存在时使用场景、会话、消息和分片序号组成的复合去重键。
* **非流式被动回复规划**（PR #584）：发送前统一规划可见分段，C2C 最多 4 条、群聊最多 5 条；超限时合并分段并降级为同序文本，追加可见截断提示。

### Fixed

* **上传错误敏感信息脱敏**（PR #584）：不缓存 `file_info` 之外的临时上传信息，不把预签名 URL、`raw_url` 或 `auth_token` 写入模型上下文、日志或错误正文。
* **QQ 消息协议兼容**（PR #584）：兼容缺失 `msg_idx` 的旧事件，并保留 C2C 聚合 `event_id` 去重；不再依赖普通群事件的 `is_you` 作为唯一 @ 判断依据。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.21.4`，QQ Gateway crate 版本号提升到 `0.1.11`；其他内部 crate 版本不统一提升。
* 本版本无数据库 migration、无必需配置迁移。现有 Todo、Memory、RSS、Session 和 Agent 配置格式保持不变。
* 保持现有图片发送能力；本版本不新增视频、语音、普通文件或 `msg_type=3` 出站能力。OneBot / 微信入口的业务行为不变。
* 未进行真实 QQ 环境联调；上传协议通过本地模拟 HTTP 服务覆盖 URL、本地文件、Base64、分片各阶段失败及敏感错误脱敏。

## [v0.21.3] - 2026-07-24

### Changed

* **工具领域边界收敛**（PR #576）：将 Todo 工具注册、结果投影、状态提示和成功校验收敛到 Todo 领域 facade；为 RSS、Knowledge、Memory、Search、Train、Weather 增加各自状态 adapter，通用 Tool Loop 只保留跨领域的结果顺序、重试覆盖和回退。
* **领域存储导出收紧**（PR #576）：Todo、Memory、RSS 的存储导出改为显式 facade；维护文档同步明确二开时的领域边界，避免公共状态模块继续承载具体领域名称和意图判断。

### Fixed

* **QQ 引用图文消息超时**（PR #580）：按 QQ 官方 `message_type=103` 正确解析引用正文与附件，下载引用图片并在失败、超时或文件过大时降级为媒体摘要，避免不可达临时 URL 拖垮多模态链路；下载后清除临时远端 URL，防止鉴权参数进入 Core / LLM / 普通日志。
* **`/ping` 应用版本注入**（PR #579）：QQ 官方 Dispatcher 复用已注入主包版本的命令服务，私聊与群聊 `/ping` 与根程序 `--version` 使用同一版本来源，不再回退到 Gateway 子 crate 版本。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.21.3`，内部 crate 版本不统一提升。
* 本版本无数据库 migration、无必需配置迁移。Todo 工具名称、参数、返回语义，以及 `/todo` 可见编号与跨轮编号语义保持不变。
* 引用图文修复仅影响 QQ 官方入口的引用媒体解析与下载降级路径；OneBot / 微信入口行为不变。

## [v0.21.2] - 2026-07-23

### Added

* **`/ping` 显示主包版本**（PR #572）：`/ping` 展示根包 `qq-maid-bot` 的版本号，由主程序入口注入 Gateway，避免误用 Gateway 子 crate 版本。

### Changed

* **工具状态语义收紧**（PR #572）：自然语言状态分类只生成用户提示和 diagnostics，不再参与 Tool 暴露或执行；Search / RSS 等弱关键词与技术讨论更不易被误判成正在调用工具。
* **Web Search 结果投影收敛**（PR #572）：同轮重复只读搜索只展示首次真实结果，缓存命中仍保留在 Agent 原始轨迹；不再为缓存命中发送额外 Started / Finished / Failed 进度事件。
* **Todo 成功验真覆盖省略式请求**（PR #572）：当模型声称已新增、已记录、已修改、已完成或已删除 Todo 时，要求本轮存在真实成功的 Todo 写工具结果；“明天开会”“下午整理一下”等省略式任务声明同样进入验真。

### Fixed

* **后置 Todo 创建声明拦截**（PR #572）：模型未调用写工具却声称“已记录”时返回 `todo_success_not_verified`，不再把伪造成功文案展示给用户；普通创作、解释和分析中的“已完成”不会被误拦截。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.21.2`，内部 crate 版本不统一提升。
* 本版本无数据库 migration、无必需配置迁移。Todo pending、用户编号快照、权限与持久化语义不变。
* 状态提示继续保持严格，不影响模型调用已注册白名单 Tool；重复只读搜索的 `tool_results` / `tool_attempts` / `deduplicated` 与模型可见原始轨迹保持完整。

## [v0.21.1] - 2026-07-23

### Added

* **群管理员管理 Todo**（PR #568）：群主或管理员可在群聊使用 `/todo group` 查看当前群未完成 Todo / 提醒，并用 `/todo group delete <编号>` 删除；列表编号绑定操作者、平台、机器人账号与完整群 scope，删除时再次校验目标记录所属群，避免跨群或个人快照误删。
* **Agent 配置示例模板**（PR #568）：仓库与 Release 包中的默认策略文件改为 `agent.example.toml`；首次启动从二进制内嵌的同版模板生成未跟踪的 `config/agent.toml`，修改外部示例不会改变首次生成内容。只读 `config check` 在默认路径缺失时直接校验内嵌模板而不落盘。

### Fixed

* **Todo 提醒并发取消**（PR #568）：取消或删除带提醒的 Todo 时更可靠地终止已租约的提醒通知，降低重复推送风险。
* **容器发布门禁竞态**（PR #567）：收紧 GHCR 正式 Release 门禁对 tag / digest 就绪状态的判定，降低并发构建误判失败的概率。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.21.1`，内部 crate 版本不统一提升。
* 本版本无数据库 migration、无必需配置迁移。已有 `config/agent.toml` 不会被覆盖；新实例首次启动才生成活动配置。
* 群 Todo 管理仅限当前群、且操作者为群主或管理员；普通成员和个人 `/todo` 查询语义不变。编号来自最近一次 `/todo group` 列表快照，过期或跨会话后需重新列出。
* Release / Docker 中的 `agent.example.toml` 仅供参考与升级迁移；活动策略仍为本地 `config/agent.toml`。

## [v0.21.0] - 2026-07-22

### Release Focus

* **多平台部署与统一运维版本**：在 20.x 配置中心与多入口业务能力之上，收口 Docker / GHCR 容器发布、配置迁移备份恢复和可选 Web 安装，让 QQ 官方、OneBot、微信以及 Linux / Windows / 容器部署共用同一套二进制与运维边界。

### Added

* **Docker Compose 与 GHCR 发布**（PR #562）：新增 `Dockerfile`、基础 `compose.yaml` 以及控制台 / OneBot / 微信端口 override；Container workflow 在 `linux/amd64` 与 `linux/arm64` 原生 runner 构建并推送 GHCR 多架构镜像，正式 Release 门禁与测试环境自动部署解耦。
* **测试环境自动部署文档与脚本**（PR #562）：补充 `scripts/docker-deploy.sh`、`scripts/docker-host-init.sh` 与 [Docker 与 Compose 部署](./docs/deployment/docker.md)、[测试服务器 Docker 部署教程](./docs/deployment/test_server.md)；部署只接受受信仓库 digest，失败时按上一 digest 回滚镜像。
* **配置迁移、备份恢复 CLI**（PR #565）：统一二进制新增 `migration status`、`config check`、`config sources`、`config migrate` 与 `backup create/verify/restore`；默认 dry-run，使用 SQLite Online Backup 与 manifest SHA-256，恢复只写入干净目标目录。
* **可选 Web 控制台安装**（PR #565）：`qbot install --web true|false` 与 `QBOT_INSTALL_WEB_CONSOLE` 支持新装时选择是否启用 `/console/`；关闭后继续使用 CLI 与文件配置，重复安装默认不改写已有选择。
* **配置迁移与备份文档**（PR #565）：新增 [配置迁移、备份恢复与安全升级](./docs/deployment/migration-backup.md)，说明旧 dotenv 保守导入、含/不含 secret 的恢复包边界，以及 Docker 升级前自动备份与 schema 回滚边界。

### Changed

* **部署路径并列收口**：Release 包、源码与 Docker 共用同一配置注册表、SQLite migration 和备份格式；Web 控制台只是另一种交互入口，不是独立运行方式。
* **容器运行模型**：镜像以非 root 用户、只读根文件系统运行，默认不映射管理端口；按需叠加 console / OneBot / 微信 compose override，并把容器内监听改为 `0.0.0.0`。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.21.0`，内部 crate 版本不统一提升。
* 本版本无新增业务数据库 migration 门槛，也无必需配置迁移。已有 `.env` / `runtime.toml` / `agent.toml` 与 20.x 配置中心部署可直接升级；旧 dotenv 可先 `config migrate` dry-run，再显式 `--apply` 填补空缺，不覆盖已有受管值。
* Docker 升级前默认生成 `data/backups/pre-upgrade-*` 恢复包；镜像回滚与 schema 恢复不是同一动作——旧二进制读不懂新 schema 时必须恢复同期备份到干净目录。
* `WEB_CONSOLE_ENABLED` 仍可由安装选项或部署侧环境关闭；显式关闭时登录页、认证和配置 API 均不注册，也不会生成 Bootstrap token。
* 真实 GHCR 拉取、跨架构镜像、微信/OneBot 公网回调和含 secret 的灾备演练仍需在目标部署环境验证。

## [v0.20.7] - 2026-07-21

### Release Focus

* **20.x 版本线收尾版本**：收敛 Session Dream 的触发、续批和失败恢复边界，保持默认关闭的可选记忆整理能力稳定运行。

### Changed

* **Session Dream 触发条件**（PR #561）：首次批次支持新增 Session、活跃日期和距上次成功检查点间隔三条触发路径，并分别设置消息数量门槛；触发诊断记录未满足的门槛，便于排查默认关闭的后台任务。
* **Session Dream 续批处理**（PR #561）：字符或 Session 数量截断后的成功批次保存待续状态，后续调度直接继续处理尾部消息，不重新累计首次触发门槛；最终批次完成后清除续批状态。

### Fixed

* **Session Dream 失败恢复**（PR #561）：模型调用失败、数据库提交失败或并发调度时保留可重试检查点，避免丢失未处理消息或重复执行同一续批；日期门槛统一按项目时区计算。

### Compatibility

* 本版本无数据库 migration、无必需配置迁移。现有记忆、Session 和 Agent 配置格式保持兼容。
* `MEMORY_DREAM_ENABLED` 默认仍为 `false`；启用后，首次触发默认需要 5 个新增 Session 且至少 30 条用户消息，或 3 个活跃日期且至少 50 条消息，或距上次成功 Dream 至少 7 天且至少 60 条消息。字符 / Session 截断后的续批不重新累计这些首次门槛。
* 根包 `qq-maid-bot` 版本号提升到 `0.20.7`，内部 crate 版本不统一提升。

## [v0.20.6] - 2026-07-21

### Changed

* **自然语言待办查询统一进入 Tool Loop**（PR #555）：保留 `/todo` 查询命令的确定性路径，普通自然语言查询改由受控 Todo Tool 处理，并支持周期类型、日期范围、状态和关键词组合筛选；查询结果恢复时继续使用用户可见快照。
* **Prompt Cache 会话前缀稳定**（PR #559）：整理会话历史和请求上下文的拼接边界，保持稳定前缀并补充缓存诊断，降低无关变动对缓存命中的影响。

### Fixed

* **知识工具上下文预算保护**（PR #557）：工具结果超过输入预算时先压缩可压缩输出、关闭后续工具调用并进入最终回答；只有必要协议内容本身超限时才返回上下文预算错误，避免工具循环继续消耗已不足的上下文。

### Compatibility

* 本版本无数据库 migration、无必需配置迁移。现有 Todo、Session、知识库和 Agent 配置格式保持兼容。
* 自然语言待办查询需要当前场景允许 Agent Tool Loop 且启用 `list_todos`；`/todo`、`/todo all`、`/todo search`、`/todo done`、`/todo undo` 等 slash 查询仍保留确定性路径。
* 根包 `qq-maid-bot` 版本号提升到 `0.20.6`，内部 crate 版本不统一提升。

## [v0.20.5] - 2026-07-20

### Added

* **Tavily 可选联网搜索后端**（PR #551）：`/查` 和自然语言 `web_search` 共用统一搜索后端配置，支持 Provider 原生搜索、Tavily Search 和关闭搜索；Tavily API Key 可通过配置中心的受管密钥或兼容环境变量 `TAVILY_API_KEY` 注入，不会写入 `agent.toml`。

### Changed

* **联网搜索配置统一**（PR #551）：将搜索后端、结果数量、搜索深度、主题、时间范围和超时收敛到 `agent.toml` 的 `[tools.web_search]`，WebUI 可维护搜索后端与参数，并保留原生 Provider 搜索 route。
* **响应链路收口**（PR #548）：移除 Core、Gateway 和多入口发送链路中的冗余响应包装，统一响应状态与测试支持，保持现有平台发送语义。

### Fixed

* **多目标联网查询可信结果卡片**（PR #552）：多目标搜索逐项展示模型事实摘要和来源链接，保留最终综合回答；部分失败时显示成功/失败数量，不再把已有有效结果误报为“没查到明确结果”。
* **Agent 工具重试结果聚合**（PR #554）：同一轮中工具失败后模型重试时只展示最终结果，避免重复显示失败卡片；跨 Provider 候选回退时将重试关系转为累计全局下标，不再误隐藏前一个候选的有效结果。
* **未闭合 Markdown 反引号兼容**（PR #549）：消息中的未闭合代码标记不再破坏 Markdown 处理和后续回复链路。

### Compatibility

* 本版本无数据库 migration。默认联网搜索后端仍为 `provider_native`，不配置 Tavily 时原有 OpenAI / Gemini 原生搜索行为保持不变。
* 旧版 `agent.toml` 中的顶层 `[search_routes.*]` 会由 Unix / Windows 更新脚本迁移到 `[tools.web_search.routes.*]`，并在迁移前生成备份；直接替换二进制而不经过更新脚本的部署需要手工完成该配置迁移。
* Tavily 为可选能力；将 `[tools.web_search].backend` 设置为 `tavily` 时必须配置 `tools.web_search.tavily.api_key` 或 `TAVILY_API_KEY`，否则实际搜索会明确报告配置错误。
* 根包 `qq-maid-bot` 版本号提升到 `0.20.5`，内部 crate 版本不统一提升。

## [v0.20.4] - 2026-07-20

### Fixed

* **管理员完整令牌输入**（PR #547）：管理员初始化和密码重置接口支持裸 token，以及项目生成的 `qq-maid-bootstrap-v1:<时间>:<token>` 和 `qq-maid-password-reset-v1:<时间>:<token>` 完整令牌字符串；严格校验前缀、字段与用途，并区分输入格式错误和 token 无效或过期。
* **群聊 Markdown 输出保留**（PR #546）：Provider 同时返回 Text part 与 Markdown 通道时优先保留 Markdown，避免群聊非流式回复被降级为纯文本。

### Compatibility

* 本版本无数据库 migration、配置项变更或令牌有效期调整；原有裸 token 输入保持兼容，Bootstrap token 与密码重置 token 仍不可跨用途使用。
* 根包 `qq-maid-bot` 版本号提升到 `0.20.4`，内部 crate 版本不统一提升。

## [v0.20.3] - 2026-07-19

### Added

* **Luna 图片生成与多平台发送**（PR #542）：将图片生成结果接入统一输出链路，支持 QQ 官方与 OneBot 11 图片发送，并兼容 Windows 本地图片的 `file://` URI。

### Fixed

* **Windows 启动示例提示**（PR #543）：启动失败时保留非零退出码，启动成功后保留控制台窗口并输出本地 Web 控制台地址，便于从 Windows 启动文件夹确认服务状态。

### Compatibility

* 旧环境变量 `QQ_MAID_ENABLE_IMAGE` 不再作为运行时图片开关读取；升级脚本会在迁移旧配置时将其归入废弃变量清理范围。

## [v0.20.2] - 2026-07-19

### Release Focus

* **20.x Agent 能力与运维收口版本**：本版本在 v0.20.0 配置中心和 v0.20.1 多入口交互基础上，完成知识库 Agent 化检索、控制台工具治理和 Agent 配置升级兼容，并统一跨平台 TLS 构建链路。

### Added

* **知识检索 Agent Tool 化**（PR #528）：将知识检索从普通聊天链路中的自动注入迁移为服务端注册的只读 `knowledge_search` 工具，新增结构化证据、证据展示、知识评测基线和 Agent 集成测试；知识检索实现与相关配置统一收敛到 `runtime/tools/knowledge/`。
* **知识预检与混合召回**（PR #534）：知识库新增查询预检、词法与语义混合召回、章节扩展、统一证据预算和知识评测 fixture；检索结果保留 lexical、semantic、section 来源与搜索文本，Agent 可按需调用 `knowledge_search`，避免每轮对话自动注入整库内容。
* **控制台工具白名单与重启入口**（PR #530）：`/console/` 展示服务端已注册工具，支持按场景维护启用工具白名单，并提供受保护的重启入口；配置保存后会重新加载私有和群聊场景策略。

### Changed

* **Agent 配置升级兼容**（PR #538，关联 #533）：v0.20.2 首次跨版本升级时，Unix、Windows 更新器和远程部署会先将现有 `config/agent.toml` 备份为不覆盖历史备份的 `.old` 文件，再启用新版模板；升级标记确保该动作只执行一次，后续新增普通可选字段由代码默认值兼容，不再覆盖用户策略。
* **Agent 配置迁移提示**（PR #529）：更新器在检测到旧版本 Agent 配置时提供替换提示，并补充 Unix / Windows 安装回归测试；该交互在 PR #538 中收敛为 v0.20.2 的自动备份与一次性迁移流程。
* **rustls 构建后端统一**（PR #537）：跨 Core、Gateway 和 LLM 的 HTTP / WebSocket 调用统一使用 rustls `ring` 后端，减少对 `aws-lc` 和系统 native TLS 构建环境的依赖。
* **知识库与 Agent 配置边界收口**（PR #534）：知识检索实现归拢到 `runtime/tools/knowledge/`，模型路线、工具白名单和证据预算由 `agent.toml` 管理；同时补充语义召回、证据排序与 Agent 集成测试。

### Fixed

* **知识检索相关性与证据边界**（PR #534）：修正相关性过滤、相邻内容扩展和评测口径，统一部分结果、搜索超时和回答证据预算，降低无关知识进入最终回答的概率。
* **时间快照测试稳定性**（PR #531）：固定相关测试时钟，避免系统时间回拨或北京时间日期边界导致随机失败。

### Compatibility

* 本版本包含 `knowledge_schema_v3_embeddings` SQLite migration，用于独立保存知识片段语义向量；程序启动时会升级已有数据库，升级前应备份 `APP_DB_FILE`。
* `agent.toml` 是 v0.20.2 唯一一次自动备份并替换模板的升级边界。升级前请确认自定义 Provider、模型路线、Scene 和工具白名单已可从 `.old` 备份恢复；已经完成 v0.20.2 标记的部署不会重复替换。
* PR #529 的“是否替换 Agent 配置”交互已不再作为 v0.20.2 的最终升级策略；脚本会按版本门槛执行一次备份和模板替换，非交互部署也遵循同一规则。
* 控制台工具白名单仍受服务端注册表、场景开关和平台边界约束；群聊、slash 命令、pending 确认、文件处理和宿主机代码执行不会因控制台配置而默认进入 Tool Loop。重启入口仅适合受保护的本地或内网控制台。
* 根包 `qq-maid-bot` 版本号提升到 `0.20.2`，内部 crate 版本不统一提升。真实 Provider、知识库大规模重建、控制台反向代理和跨平台 Release 安装仍需在目标部署环境验证。

## [v0.20.1] - 2026-07-19

### Release Focus

* **QQ 语音转写与统一命令前缀版本**：本补丁版本落地 [#526](https://github.com/kuliantnt/qq-maid-bot/issues/526)，让 QQ 官方入口可以直接利用平台语音转写继续对话，并为所有聊天入口提供统一、可配置的命令前缀。

### Added

* **QQ 官方语音转写**（#526）：解析 QQ 音频附件中的平台 `asr_refer_text`，将有效转写作为用户输入补充传入 Core/LLM，同时保留语音附件和预转换 WAV 地址等媒体元数据；引用语音也沿用同一规则处理，空转写或非音频附件不会误注入文本。
* **统一聊天命令前缀**（#526）：新增 `CHAT_COMMAND_PREFIX` 与配置中心 `command.prefix`，默认值为 `/`，支持一个可见非空白字符；QQ 官方、OneBot 11、Core 命令分发、通知文案和每日提醒统一使用当前前缀。

### Changed

* **控制台与命令处理**（#526）：Web 控制台可在 `/`、`#`、`*` 中选择命令前缀，修改后重启生效；旧前缀、重复前缀和消息中间出现的前缀不会被当作命令，平台入口的本地命令与 Core 命令保持一致。
* **初始化配置文件**（#526）：`runtime.toml` 在首次启动时安全创建最小空文件，已有文件不会被覆盖；通知推送中的命令引导会按配置前缀渲染。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.20.1`，内部 crate 版本不统一提升；本版本无数据库 migration。
* 默认命令前缀仍为 `/`，不配置 `CHAT_COMMAND_PREFIX` 的已有部署行为保持不变。修改前缀后需重启，旧 `/` 不再触发命令；请同步更新外部自动化、群公告和运维白名单文案。
* QQ 语音转写依赖平台事件提供音频附件和 `asr_refer_text`；平台未提供转写时仍按原有附件文本行为处理。OneBot 11 与微信服务号的既有语音限制不变。
* `runtime.toml` 的首次空文件创建不包含凭证或 secret；已有 `.env`、`runtime.toml`、`agent.toml`、SQLite 和主密钥不被覆盖。

## [v0.20.0] - 2026-07-18

### Release Focus

* **安全配置中心与部署管理控制台版本**：本版本在 v0.19.x 基础上建立安全配置中心（`runtime.toml` + SQLite 加密敏感值 + 独立主密钥），实现部署管理员初始化与可写 Web 控制台，让新实例可在 `/console/` 完成 Provider、平台入口与主要功能开关配置，不再必须编辑 `config/.env`。本版本也是 [Epic #194](https://github.com/kuliantnt/qq-maid-bot/issues/194) Docker 容器化部署与 Web 配置中心的 Phase 2 + 3 交付；Phase 1 Docker Compose / 镜像发布（#511）和 Phase 4 备份升级验收（#513）仍在进行。

### Added

* **安全配置中心**（PR #514 / Phase 2 #510）：新增受管 `config/runtime.toml`、SQLite 认证加密敏感值、独立 `secrets/master.key` 与字段注册表；普通运行配置与 Agent 策略分层，`agent.toml` 仍是模型路线 / Scene / Tool Calling 的唯一事实来源。对已登记字段，`.env` / 进程环境仅作首次启动兜底；WebUI 保存后受管值优先，快照以 `overridden=true` 标记已覆盖的同名外部兜底。
* **部署管理员初始化与配置控制台**（PR #520 / Phase 3 #512）：未配置 Provider 或平台入口时以 `setup_required` 启动，保留 `/healthz` 与受保护 `/console/`。首位管理员通过 `config/secrets/bootstrap.token`（约 22 字符、约 30 分钟、单次有效）建立；支持忘记密码重置令牌、HttpOnly SameSite cookie、轮换 CSRF、同源 Origin 与脱敏审计。配置页可分步保存 Provider、QQ / OneBot / 微信入口、主要功能开关、模型路线与 Tool Calling；本地启动预检与显式触发的 Provider 连接测试分开，自定义地址会拒绝私网 / 环回目标以避免 SSRF。

### Changed

* **配置优先级收敛为受管优先**：对配置中心已登记字段，管理员在 WebUI 保存后，`runtime.toml` 普通值与 SQLite 加密 secret 优先于旧 `.env` / 进程环境同名值；未登记的 Bootstrap 与高风险部署项（监听地址、数据库路径、`ops.toml` 等）仍只由文件 / 环境管理。
* **控制台从只读面板扩展为可写配置入口**：保留运行状态与 Markdown 预览；配置详情与写接口必须经过独立部署管理员会话。生产反向代理需设置 `WEB_CONSOLE_TRUSTED_PROXY_IPS`，HTTPS 生产应显式开启 `WEB_CONSOLE_SECURE_COOKIES`。
* **天气配置改为可选**：`QWEATHER_API_KEY` 留空时只关闭天气命令与 Agent 天气 Tool，不再阻止其他能力启动。

### Compatibility

* 根包 `qq-maid-bot` 版本号提升到 `0.20.0`，内部 crate 版本不统一提升。包含 `config_secret` 与 `admin_auth` SQLite migration；程序启动时会升级已有数据库，升级前应备份 `APP_DB_FILE`。
* 已有 `.env` / `agent.toml` 部署可继续使用；首次通过控制台保存后，对应已登记字段以受管值为准，升级前应分开备份 SQLite 与 `config/secrets/master.key`。
* `qbot update` 会先备份 `config/.env`，再移除已迁入 `agent.toml`、会导致新版本拒绝启动的废弃 Agent / Todo / 成员映射变量；其他配置（包括允许为空以关闭天气能力的 `QWEATHER_API_KEY`）保持不变。systemd、Docker 或宿主环境直接注入的同名变量仍需人工移除。
* `WEB_CONSOLE_ENABLED` 新实例默认开启但仍只绑定回环地址；不要把 8787 裸露到公网。Windows 下 `bootstrap.token` / `master.key` 当前依赖安装目录 ACL，尚未主动收紧（见 [#522](https://github.com/kuliantnt/qq-maid-bot/issues/522)）。
* 相关跟踪（尚未入主线实现）：[#515](https://github.com/kuliantnt/qq-maid-bot/issues/515) QQ 官方内置 ASR 语音转文字；[#518](https://github.com/kuliantnt/qq-maid-bot/issues/518) 可配置聊天命令前缀。

## [v0.19.0] - 2026-07-17

### Release Focus

* **记忆增强、多实体搜索与白名单运维版本**：本版本在 v0.18.x 分域记忆基础上补齐可选的确定性记忆整理与 Session Dream 自动沉淀，增强多实体联网搜索与待办组合筛选；同时新增默认关闭的 `/ops` 白名单运维命令，并统一 OpenAI 基础地址、搜索超时和 Markdown 展示边界，让慢链路搜索、超时反馈和日常运维更稳。

### Added

* **确定性记忆整理**（PR #500）：新增默认关闭的后台整理任务，按完整 target 检查点推进，只归档同一作用域下语义键与正文完全相同的重复项；召回侧先按 target / visibility 取候选，再按问题相关度、来源、置顶和新鲜度排序，仅 `SystemDerived` 使用 30 天半衰期。新增 Memory v4 migration `memory_consolidation_state`。
* **Session Dream 自动记忆**（PR #501）：新增默认关闭的 `MEMORY_DREAM_ENABLED`，与确定性整理可分别启用。聊天成功写入 Session 后异步提取结构化候选，只写当前用户 Personal 或当前成员 GroupProfile，不写群公共记忆，也不覆盖 `UserConfirmed`；基于稳定 `SessionMessage.message_id` 扫描活跃和归档用户消息，不读取 Session Summary。
* **多实体搜索编排**（PR #498）：`web_search` 支持单实体快查和多实体调研；服务端限并发独立搜索，逐项保留成功、失败、超时、来源、模型和耗时，部分成功时继续汇总。Agent 最终回答默认预留可配置预算，搜索统一使用首活动 / 静默 / 绝对超时，`/查` 与自然语言搜索共用同一实现。
* **待办列表组合筛选**（PR #505）：`/todo list`、确定性日期查询和自然语言 `list_todos` 复用同一 `TodoQuery`；支持今天、明天、本周、逾期、无截止时间与关键词（AND）组合筛选，默认展示上限从 5 调整为 10，超量时返回真实总数。
* **白名单运维命令 `/ops`**（PR #506）：新增默认关闭的 `/ops <command> [args...]` 确定性命令，在 session、pending、LLM 和 Tool Loop 之前收口。独立 `config/ops.toml` 配置管理员、允许群、固定绝对程序路径、参数数量 / 枚举 / 正则、超时和输出上限；校验通过后立即受理，结果经 Notification Outbox 异步投递，重试不会重新执行程序。私聊 `/ping` 可展示当前完整稳定 `user_id` 便于填白名单。

### Changed

* **OpenAI 基础地址只认 `OPENAI_BASE_URLS`**（PR #502）：删除 Core 对 `OPENAI_BASE_URL` 的回退读取，部署侧只需维护逗号分隔的多地址变量，并取第一个非空地址。
* **联网搜索超时与结束帧兼容**（PR #503）：LLM 默认超时调整为 180 秒，最终回答预留 45 秒；Web Search 首活动 / 静默 / 绝对超时调整为 60 / 30 / 120 秒，并忽略 `response.completed` 后的 `data: [DONE]` 结束哨兵，减少国内兼容供应商误超时。
* **Markdown 渲染统一与 RSS 地址展示**（PR #504）：动态文本转义、HTTP(S) 链接、QQ 渲染和纯文本 fallback 收敛到 `qq-maid-common::markdown`；RSS 列表在 Markdown 中改为“打开订阅源”短链接，纯文本 fallback 仍保留完整 URL。

### Fixed

* **QQ 流式超时失败提示**（PR #496）：C2C 首帧前失败和群聊 stream `Failed` 会发送 Core 安全失败文案并回填 ref index；已开始的 C2C stream 仍只结束原 stream，不补发第二条普通全文。
* **微信 XML 实体解析适配**（PR #495）：适配 gateway 依赖升级，并在微信 XML 解析中正确处理 `Event::GeneralRef` 及 Text / CDATA / 实体混合片段顺序拼接，避免实体被忽略或字段被后续片段覆盖。

### Compatibility

* 本版本包含 Memory v4 migration（整理检查点与 Dream 状态）。程序启动时会升级已有 SQLite；升级前应备份 `APP_DB_FILE`。
* `MEMORY_CONSOLIDATION_ENABLED` 与 `MEMORY_DREAM_ENABLED` 默认均为 `false`，升级后不会自动改变已有长期记忆或开始自动沉淀。Dream 只写 Personal / 当前成员 GroupProfile，不写 Group 公共记忆，也不覆盖 `UserConfirmed`。
* 若环境仍配置旧的 `OPENAI_BASE_URL`，需迁移到 `OPENAI_BASE_URLS`；旧变量不再被读取。
* `/ops` 默认关闭；需要时从 `runtime/config/ops.example.toml` 复制为未跟踪的 `config/ops.toml` 并填写管理员 / 允许群 / 固定程序。命令只执行配置中的绝对路径与参数规则，不调用 Shell。首期后台任务只存在于当前机器人进程，进程重启时不保证继续收集或推送进行中的结果。
* 根包 `qq-maid-bot` 版本号提升到 `0.19.0`，内部 crate 版本不统一提升。
* 记忆整理 / Dream、多实体搜索、Todo 筛选、`/ops`、超时与 Markdown 展示均有自动化测试覆盖；真实 Provider、真实 `/ops` 脚本权限和线上慢链路仍需部署后验证，日志与诊断继续默认脱敏。

## [v0.18.1] - 2026-07-16

### Release Focus

* **微信安全模式与群聊斜杠命令收口版本**：本版本在 v0.18.0 分域记忆线上继续补齐微信服务号安全模式加解密、统一群聊斜杠命令处理和搜索主体补全，让微信入口可以走平台推荐的安全模式，群聊中的未知 `/` 命令不再稀释聊天上下文，联网搜索也更少因为省略主体变成跑题查询。

### Added

* **微信服务号安全模式（AES）**（PR #492）：微信入口支持微信公众平台「安全模式」的 GET URL 验证、`msg_signature` 验签、AES-CBC 解密、AppID 校验和加密回包；模式不匹配会明确拒绝，不会把密文当明文处理。新增 `WECHAT_SERVICE_ENCRYPTION_MODE`（`plaintext` / `aes`，默认 `plaintext`）和 `WECHAT_SERVICE_ENCODING_AES_KEY`；`aes` 模式下 AppID 为必填并用于校验解密消息尾部的公众平台身份。验签与加解密全部停留在 Gateway 平台边界，不进入 Core 或日志。
* **群聊斜杠命令候选统一交给 Core**（PR #493）：群聊入口不再在 Gateway 维护命令白名单，所有 `/` 或全角 `／` 开头的候选都绕过普通聊天冷却直接交给 Core。Core 依次走现有确定性命令解析器，保留原参数；所有解析器均未命中的未知群命令进入明确的静默结果，不再作为普通聊天交给模型，避免污染会话上下文。

### Changed

* **记忆与 RSS 列表展示美化**（PR #493）：`/memory` 和 `/rss` 列表、空列表和详情统一改为带标题、操作分区和可复制命令的卡片式格式；群公共记忆按权限身份提示添加方式，RSS 订阅用状态图标区分启用/停用并在有错误时补出注意事项。展示文本同步进行 Markdown 转义，标题、地址和错误内容不会被特殊字符破坏渲染。
* **Core 静默响应跨层契约**（PR #493）：新增明确「Core 已判定无需回复」的静默响应类型，Gateway 通过 `diagnostics.suppressed=true` 显式识别后终止发送，不再因 `handled` 或正文为空就补发兜底文案；普通响应状态不会误命中静默判断。
* **联网搜索主体补全规则**（PR #491）：`web_search` 工具说明要求在调用前结合会话、引用消息、机器人身份和本地记忆补全省略的搜索主体，搜索请求在脱离聊天上下文后仍可独立理解，能确定具体对象时不再先搜索泛化问题。

### Compatibility

* 微信服务号 `plaintext` 模式行为与 v0.18.0 一致，未配置新选项时默认保持明文模式。升级到 `aes` 安全模式的部署需在公众平台后台同步切换「安全模式」并填写 43 位 EncodingAESKey；`WECHAT_SERVICE_ENCODING_AES_KEY` 不要提交到公开仓库。
* 群聊斜杠命令的可用集合和权限仍由 Core 现有命令注册表决定，本版本只把未知群命令从「进入普通聊天」改为「静默收口」，不会收到额外的命令也不改变已注册命令的参数、角色和 scope 校验。私聊未知斜杠文本维持原普通聊天兼容行为。
* 本版本无数据库 migration、必填环境变量重命名或 Provider 协议变更。新增微信安全模式选项均为未配置时保持原行为的可选项。根包 `qq-maid-bot` 版本号提升到 `0.18.1`，内部 crate 版本不统一提升。
* 微信安全模式的验签、解密、加密回包和未知群命令静默收口均有自动化测试覆盖；真实公众平台回调、微信客户端展示和真实群聊环境仍需部署后验证，但 Token、AppSecret、EncodingAESKey 不会进入日志或 `/ping all` 摘要。

## [v0.18.0] - 2026-07-15

### Release Focus

* **分域记忆与共享群聊身份隔离版本**：本版本建立 Memory v3 的个人记忆、群内个人画像和群公共记忆边界，按场景与可见性安全召回；明确的个人记忆和群内画像请求可在服务端校验后直接保存，群公共记忆继续要求显式命令。同时为共享群聊历史补充发言人归属，降低不同成员的昵称、偏好、身份声明和操作结果发生串线的风险。

### Added

* **Memory v3 分域模型**（PR #480 #483 #484）：分离访问范围、记忆类型、画像主体、关系主体、可见性和生命周期；私聊与群聊使用不同召回层级，群聊 SQL 查询直接排除其他成员的 Private 记忆，ContextOnly 内容只用于理解当前发言人，不得主动披露。
* **自然语言记忆写入与分域管理**（PR #484 #487 #490）：新增服务端白名单 `save_memory` Tool；用户明确要求“记住”时，可在范围、权限和敏感信息校验通过后直接保存个人记忆或当前群画像。`/memory` 支持个人记忆、当前群画像和群公共记忆的列表、详情、编辑、删除、清空及画像停用/启用；群公共写入只允许群主或管理员显式使用 `/memory group add 内容`。
* **共享会话发言人归属**（PR #488）：群聊与频道 Session 为每轮 user/assistant 消息持久化脱敏 `turn_actor` 快照，并向模型对齐当前与历史 `actor_ref`；会话压缩和上下文裁剪继续保留成员归属，私聊格式保持不变。

### Changed

* **Pending 动作生命周期统一**（PR #481）：Todo 与 Memory 确认流程复用带 schema version、状态、scope、owner、actor、过期时间和 revision 的 `PreparedAction` envelope；执行前通过 SQLite 事务原子领取，阻止重复或并发确认产生第二次副作用，执行失败保留真实失败状态。
* **内部辅助模型路由统一**（PR #469）：会话标题、Memory 整理、会话压缩和翻译统一按“显式专项模型 > 当前场景 `aux_route` > 当前场景 `main_route`”选择模型；RSS 模型翻译改由默认关闭的 `RSS_TRANSLATION_ENABLED` 控制。
* **README 信息层级精简**（PR #460）：根 README 收敛为项目定位、能力、平台、快速开始、配置、安全和文档导航入口，部署细节继续以 runtime 文档为准。

### Fixed

* **自然语言撤销待办完成状态**（PR #467）：支持“刚才那条还没做完”等状态纠正表达按需暴露 `restore_todos`，同时保持普通查询、疑问句和无关表达不会开放逆向操作工具。
* **群聊成员信息防串线**（PR #488）：共享 Session 中不同成员即使使用相同昵称，也不再仅凭展示名被视为同一人；历史机器人回复同步标记当时面向的成员，压缩后仍保留归属约束。
* **群聊 Memory Pending 与正文归一化**（PR #490）：群聊中的 `@`、召唤词及兼容 mention 前后缀会先按平台身份安全归一化，Pending 确认、取消和范围澄清不再被普通聊天冷却静默拦截；自然语言群公共记忆写入及遗留的同类 Pending 会被拒绝并引导使用 `/memory`。

### Maintenance

* **大型模块与测试拆分**（PR #461 #462 #463 #464）：拆分 Gateway 响应与媒体处理、Markdown 渲染、Todo/Core/LLM 测试以及 OpenAI Responses Tool Loop 协议适配模块，保持公开接口和运行行为不变。
* **测试稳定性**（PR #485）：日期查询与 Tool Loop 联动测试改用固定日期，避免北京时间零点附近出现偶发失败。

### Compatibility

* 本版本包含 Memory v3 和 Session V4 数据库 migration。程序启动时会升级已有 SQLite；升级前应备份 `APP_DB_FILE`。旧 personal/group 记忆会按确定规则迁移，无法安全归属的记录进入 `legacy_unassigned` 隔离区，不会被普通召回。
* 升级前遗留的旧格式 Pending 属于短期会话中间状态，读取时会清理并要求用户重新发起操作；已完成的 Todo、Memory 等持久化业务数据不受此规则影响。
* 新增个人记忆和群内画像不再二次确认；群公共记忆只能通过 `/memory group add 内容` 写入，并继续校验群主或管理员权限。清空记忆、停用群画像以及现有编辑/删除等破坏性管理操作仍按服务端流程确认。普通聊天不会自动写长期记忆，模型文案也不能代替真实持久化结果。
* `TODO_MODEL` 已移除，旧配置会返回明确升级提示；如需单独控制内部辅助任务，应使用 `agent.toml` 的场景 `aux_route` 或仍受支持的专项模型配置。`RSS_TRANSLATION_ENABLED` 为新增可选项，默认关闭。
* 根包 `qq-maid-bot` 版本号提升到 `0.18.0`，内部 crate 版本不统一提升。真实 Provider 下的模型措辞遵循程度仍需部署后观察，但权限、范围、身份和持久化结果均由服务端校验。

## [v0.17.2] - 2026-07-14

### Release Focus

* **跨平台发布包与 Windows 原生运维版本**：本版本将 Release 产物按 Linux、macOS 和 Windows 的实际运行环境拆分，补齐 Windows 原生安装、配置和进程控制入口，并统一发布包、源码安装与远程部署的应用目录结构。

### Added

* **Windows 原生控制脚本**（PR #458）：新增 `botctl.ps1` / `botctl.cmd`，支持启动、前台运行、停止、重启、状态、健康检查、控制台检查和日志跟随；源码构建可通过 `scripts/install-windows.ps1` 安装 release 二进制和控制文件，不再依赖 Git Bash、MSYS2 或 Cygwin。
* **Windows 一键安装与配置入口**：新增 `qbot.ps1` / `qbot.cmd`，支持下载或更新指定版本、SHA-256 校验、版本查看、服务控制和脱敏配置管理；安装和更新会保留 `config/.env`、SQLite、日志、PID 目录及已有 Agent 配置。

### Changed

* **Release 包按平台拆分**：GitHub Release 继续生成 Linux x86_64 / aarch64、macOS x86_64 / aarch64 和 Windows x86_64 五个平台包；Unix 包只包含 Shell 控制与诊断脚本，Windows 包只包含 PowerShell / CMD 控制和安装脚本，避免分发当前平台无法使用的文件。
* **发布目录结构统一**：公开环境变量模板统一位于 `config/.env.example`，不再在发布包根目录重复放置 `.env.example`；打包和发布校验同步检查平台文件白名单、必需文件、SHA-256 及敏感运行数据排除规则。
* **安装器按平台分工**：Unix Shell 安装入口收敛到 `scripts/qbot.sh`，Windows 使用 `scripts/qbot.ps1` / `scripts/qbot.cmd`；根 README 的 Linux 快速安装入口和各平台手动解压步骤已按新路径更新。
* **远程部署应用根目录明确化**：`scripts/deploy-remote.sh` 支持通过 `REMOTE_APP_DIR` 使用独立应用目录，未配置时继续兼容历史 `${REMOTE_PROJECT_DIR}/runtime` 路径；知识库同步默认路径随应用根目录保持一致。

### Compatibility

* 从旧 Release 包手动升级时，应从 `config/.env.example` 创建或比对配置；旧包根目录的 `.env.example` 不再分发。已有 `config/.env`、私有 prompt、知识资料、SQLite、日志和 PID 不应被发布包覆盖。
* Windows 原生 Release 当前仅提供 x86_64；Windows ARM64 可使用 WSL 中对应的 Linux Release。Linux 和 macOS 均提供 x86_64 与 aarch64 原生包。
* 本版本无数据库 migration、必填环境变量、Provider 协议或机器人业务行为变更。根包版本号将在正式 release 提交中同步提升，内部 crate 版本不随本次小版本统一提升。

## [v0.17.1] - 2026-07-14

### Release Focus

* **OneBot 11 一期完整收口版本**：本版本补齐进程内机器人引用续聊、图片与文件消息段映射、控制台能力展示和发布文档，完成 OneBot 11 单账号最小可用接入（#289）的发布收口。

### Added

* **OneBot 引用与媒体适配**（PR #456）：群聊引用当前进程内可命中的机器人消息时可免 `@` 继续追问，并保持 ref_index 跨账号、跨会话隔离；安全远程图片可进入现有图片理解链路，文件、不可读媒体和未知消息段降级为明确摘要。

### Changed

* **OneBot 能力状态与诊断同步**：Web 控制台和 `/ping` 按实际实现展示文本、图片、文件摘要、图文混合入站及纯文本出站能力，不再将 OneBot 入站整体标记为不可用。
* **部署与兼容说明收口**：根 README、Gateway README、runtime 配置说明和 NapCat 指南统一说明反向 WebSocket、Array 消息格式、单账号、OneBot-only、入口并存、引用缓存、媒体降级和纯文本出站边界。
* **NapCat WebUI 指引完善**（PR #455）：补充从 NapCat 启动日志进入 WebUI 的方式，减少首次配置时对默认入口的误解。

### Compatibility

* OneBot 11 仍只支持单账号、反向 WebSocket 和 Array 消息格式；不支持 OneBot 12、正向 WebSocket、HTTP 上报、CQ 字符串消息格式或同进程多账号。
* 出站仍只有纯文本，不支持图片、文件、Markdown、平台原生引用、`@` 消息段、流式输出或其他富媒体消息段。文件入站只提供安全元数据和摘要，不解析正文。
* NapCat 标记为维护者实机验证，fake OneBot 由自动化集成测试覆盖；其他 OneBot 11 实现未经实机验证，不声明完整兼容。
* 本版本无数据库 migration、必填环境变量或公开配置语义变更。根包 `qq-maid-bot` 版本号提升到 `0.17.1`，内部 crate 版本不同步提升。

## [v0.17.0] - 2026-07-13

### Release Focus

* **多入口平台化与 OneBot 11 / NapCat 接入版本**：本版本完成 OneBot 11 反向 WebSocket 到现有 Core 的文本交互主链路，可通过 NapCat 等兼容实现完成私聊、群聊 `@`、命令、普通聊天、纯文本回复和主动推送；同时补充面向 NapCat WebUI 的配置与排障指南，并支持不绑定 QQ 官方机器人凭证的 OneBot-only 部署。

### Added

* **OneBot 11 反向 WebSocket 接入底座**（PR #445）：新增默认关闭的单账号监听器、Bearer Token 鉴权、`self_id` 锁定、心跳与请求超时、连接替换、API `echo` 关联和优雅退出。协议 ID 同时兼容 JSON 数字与字符串，并以无精度损失的字符串形式进入平台边界。
* **OneBot 11 文本收发与 Core 聊天闭环**（PR #448 #450）：支持私聊和群聊 `@` 文本事件进入现有去重、会话、命令、聊天和 Tool 调用链；出站复用 OneBot `send_private_msg` / `send_group_msg`，严格校验平台返回，并支持 Todo、RSS 等主动推送按平台与账号精确投递。
* **NapCat 接入指南**（PR #453）：新增反向 WebSocket 客户端配置、Token 与消息格式填写方式、同机及跨主机连接、安全建议、验证步骤和常见故障排查说明。

### Changed

* **入口渠道可独立装配**（PR #436 #445 #450）：QQ 官方机器人凭证改为可选绑定，QQ、OneBot 11 与微信入口由统一监督逻辑按启用组合运行；OneBot-only 部署不再要求 QQ AppID / AppSecret，任一已启用渠道异常退出时会触发受控清理并返回真实错误。
* **多平台投递与会话边界扩展**（PR #448 #450）：OneBot 复用平台无关的 Core 请求和业务 scope，Gateway 保留平台原始账号与私聊/群聊投递目标；QQ 官方与 OneBot sender 相互隔离，不进行跨平台或跨账号降级。

### Fixed

* **机器人称呼配置统一**（PR #451）：程序生成的状态、会话提示和空回复兜底统一复用群聊主动关键词中的首个有效称呼，避免改名后部分路径仍固定自称“小女仆”。
* **RSS recent 帮助补齐**（PR #452）：在分层帮助、订阅列表提示和维护文档中补充 `/rss recent [数量]`，明确默认 5 条、最多 20 条。

### Compatibility

* OneBot 11 默认关闭；启用时必须配置非空 `ONEBOT11_ACCESS_TOKEN`。默认仅监听 `127.0.0.1:8789`，不应直接将未受保护的 WebSocket 端口暴露到公网。
* 当前支持范围为单账号、反向 WebSocket 和 text-only：群聊需要 `@` 触发，不向 OneBot 平台流式发送，也暂不支持 CQ 字符串内部格式、入站/出站富媒体、正向 WebSocket、HTTP webhook 或同进程多账号。
* 旧 QQ 官方机器人完整凭证继续默认启用；两项凭证均缺失表示未绑定，只缺一项会返回明确配置错误。新增 OneBot 配置均为可选项，不改变已有 QQ、微信和业务数据语义。
* 本版本无数据库 migration 或必填的全局配置格式变更。根包 `qq-maid-bot` 版本号提升到 `0.17.0`，内部 crate 版本不同步提升。
* OneBot 协议、连接、收发、Core 闭环和渠道隔离已有自动化测试覆盖；真实 NapCat 登录、跨主机网络与客户端展示仍需在实际部署环境验证。

## [v0.16.0] - 2026-07-12

### Release Focus

* **Agent Runtime 可靠性与本地可观测性版本**：本版本统一 Agent 跨候选模型的终止结果、执行轮次和工具轨迹，收紧超时、取消和工具副作用边界；同时重构 8787 本地只读管理面板，并完善 RSS 推送、Windows Release 安装与 QQ Gateway 连接恢复能力。

### Added

* **8787 本地只读管理面板一期**（PR #427）：新增 `/console/` 和 `/api/v1/console/status`，展示运行状态、平台收发能力与在线状态、存储安全摘要，并保留服务端 Markdown 预览。前端产物嵌入 Rust 二进制，Release 包不再携带独立 `static/` 目录；控制台默认关闭，仅适合本机或受控内网。
* **Windows Release 脚本安装**（PR #420）：`qbot.sh` 可在 Git Bash、MSYS2 和 Cygwin 中识别并安装 `windows-x86_64.zip`，`botctl.sh` 同步兼容 `.exe` 二进制和可配置停止超时。

### Changed

* **Agent Runtime 终止语义统一**（PR #415 #429）：成功与失败路径共享结构化 diagnostics，模型轮次、已发出工具、已启动工具、可信结果和结果未知状态按整次请求累计；超时或取消后不再启动新的工具副作用，已开始工具使用受控清理预算收尾。
* **Tool 路由与权威上下文收口**（PR #417 #421 #423）：自然语言搜索由 Agent 根据上下文决定是否调用 `web_search`，显式 `/查` 继续使用确定性搜索入口；状态分类不再决定 Tool Schema 或执行路径，Todo 高风险恢复工具仅在明确恢复意图下暴露，工具身份和作用域继续仅信任服务端 `ToolContext`。
* **业务工具目录边界收敛**（PR #423 #426）：RSS、火车和天气的运行实现、存储与测试进一步收敛到 `qq-maid-core/src/runtime/tools/<domain>/`，Respond 层保持跨域调度职责。
* **Agent / Todo 集成测试按职责拆分**（PR #424 #425）：将过大的 Respond 测试按 Agent turn、Todo 查询、写入、pending 和成功守卫拆分，便于后续维护回归边界。

### Fixed

* **Agent 多轮工具超时收尾**（PR #429）：工具启动前检查整体预算，工具失败进入收尾预算时立即终止后续轮次；Todo 写工具已有可信结果但最终模型回复失败时，可按真实工具结果生成确定性回执，不重跑写操作。
* **RSS 推送和空列表交互**（PR #416 #431）：RSS Markdown 经过安全重渲染，修复标题、链接、引用和缩进边界；订阅列表为空时补充可直接使用的添加引导。
* **QQ Gateway 地址获取恢复**：Gateway 地址获取失败时使用带随机抖动的退避策略重试，避免短暂网络或 QQ API 故障直接终止进程，并保持日志脱敏。

### Compatibility

* 控制台仍由 `WEB_CONSOLE_ENABLED=false` 默认关闭，无登录和写操作；启用后不应将 8787 端口裸露到公网，跨域访问需通过 `WEB_CONSOLE_ALLOWED_ORIGINS` 显式配置。
* Windows 自动安装目前仅支持 x86_64 Release；ARM64 Windows 会在下载前明确报错，可改用 WSL 的 Linux Release。
* 本版本无新的数据库 migration、必填环境变量或配置格式变更。根包 `qq-maid-bot` 版本号提升到 `0.16.0`，内部 crate 版本不同步提升。
* 控制台与 Windows 安装链路已有本地检查入口；QQ 真实网络下的 Gateway 重试节奏、平台在线状态和客户端展示仍需部署后验证。

## [v0.15.2] - 2026-07-10

### Release Focus

* **Agent Chat 与交互可靠性修复版本**：本版本将符合能力和场景约束的普通纯文本消息统一接入原生 Agent Chat，让模型在同一次响应中决定直接回答或调用服务端白名单 Tool；同时修复 Agent 流式回退、QQ 私聊正在输入通知载荷，以及 Todo 详情清除和成功验真问题。

### Changed

* **普通消息统一 Agent Chat 入口**（PR #411）：通过场景、Provider 能力、群聊开关和工具白名单约束的普通纯文本消息统一规划为 `AgentChat`。模型可直接流式回答、请求澄清或发出 Tool Call；slash、pending、固定 Web Search、确定性 Todo 查询和多模态降级等现有边界保持不变。
* **Agent 事件与诊断语义收口**（PR #411）：统一 Agent 运行状态事件和诊断字段，区分工具调用能力是否可用、模型是否实际调用工具、实际执行工具和本轮结果；普通直答不会被记录为工具执行。
* **README 与开发文档同步**（PR #408 #411）：精简 README 中易过期的版本细节，并同步 Agent Chat、Tool 二开和响应事件流文档。

### Fixed

* **Agent 流式请求与回退修复**（PR #412）：OpenAI Responses Agent Tool Loop 显式启用流式请求；流式超时改为只约束首个有效事件，开始出流后交由 Core 整体请求预算管理；流式回退日志增加脱敏后的计数与原因，不记录正文、工具参数或鉴权信息。
* **Agent 状态提示准确性修复**（PR #412）：仅在明确工具意图或真实工具活动发生后发送工具进度状态，普通 Agent 直答不再展示不准确的工具处理提示。
* **QQ 私聊正在输入通知修复**（PR #410）：为 C2C `msg_type=6` 通知补齐 `input_notify`、`input_type` 和 `input_second` 协议字段，修复接口返回 `50059 input type err` 的问题。
* **Todo 详情清除与成功验真修复**（PR #409）：`edit_todo.detail` 支持以空字符串或纯空白明确清除详情，并补充自然语言路由和成功守卫；工具未调用或执行失败时，不再允许最终回复声称详情已清除。

### Compatibility

* 现有 slash 命令、pending 确认、固定查询入口、群聊 Tool Calling 开关、Provider 能力降级和服务端 Tool 白名单边界保持兼容。
* 本版本无数据库 migration 或配置格式变更。根包 `qq-maid-bot` 版本号提升到 `0.15.2`，内部 crate 版本不同步提升。
* QQ 私聊正在输入通知的协议载荷已有单元测试覆盖，真实客户端展示效果仍需部署后验证。

## [v0.15.1] - 2026-07-10

### Release Focus

* **Luna 路线与普通消息收口版本**：本版本将默认 Agent 模型路线统一到 OpenAI GPT-5.6 Luna；在 v0.15.0 Tool / Todo 边界收敛的基础上，让普通消息的路由规划只生成一次，并由后续执行、流式输出、状态提示和诊断共享；同时提供 Linux `qbot` 一键脚本，降低首次安装和后续运维成本。

### Added

* **`qbot` 一键安装与管理脚本**（PR #402）：新增 `qbot.sh`，支持不克隆仓库直接安装 Release、配置 QQ Bot 和 AI 渠道、启停服务、查看状态与日志、运行健康检查和执行后续升级；README 快速开始已补充对应入口。

### Changed

* **GPT-5.6 Luna 统一默认主路线**：`runtime/config/agent.toml` 的私聊、群聊和辅助任务统一以 OpenAI GPT-5.6 Luna 为第一候选，并保留 Gemini、MiMo 和 DeepSeek 降级候选；搜索路线默认使用 Luna，同时保留 Gemini Search 切换示例。优先使用 Luna 需要 `OPENAI_API_KEY` 且账号具备 `gpt-5.6-luna` API 访问权限。

* **普通消息统一路由**（PR #406）：将普通消息统一为 `PlainChat` / `ToolAgent` 路由，并由不可变的规划结果同时承载顶层响应计划和单次 Tool 路由决策。Core streaming、dispatcher、chat flow、状态提示和诊断不再在执行阶段重复计算路由。

* **Tool / Todo 开发文档同步**（PR #406）：根据当前源码更新开发目录、通用 `agent_turn`、Todo domain adapter、pending、回执、可见实体和跨存储副作用边界，并明确 Tool 身份与作用域只能来自服务端 `ToolContext`。

### Compatibility

* 现有普通聊天、Tool Agent、slash 命令、pending 确认和确定性 handler 的用户可见语义保持兼容；本版本主要减少同一请求在不同阶段发生路由漂移的风险。
* 根包 `qq-maid-bot` 版本号提升到 `0.15.1`；内部 crate 版本本次不同步提升。配置格式和数据库 schema 未变更；默认模型路线已切换到 GPT-5.6 Luna，升级时需确认 API 访问权限。

## [v0.15.0] - 2026-07-10

### Release Focus

* **Tool / Todo 架构边界收敛版本**：本版本在 v0.14.x 事件流和群聊稳定性版本线上继续收口 Tool Loop、Todo、Reminder、RSS/Search/Weather/Train 等业务工具边界。重点是把 Todo / Reminder 路由、pending、可见实体快照、成功验真和查询新鲜度判断下沉到 `runtime/tools/todo/`，并把 Tool Loop 整轮后处理收敛到 `runtime/tools/agent_turn.rs`，让 Respond 层回到跨域调度、会话维护和响应拼装职责。

### Added

* **重复提醒与周期管理**（PR #389 #391 #394）：支持分钟级提醒、周期性纯提醒和重复提醒周期管理；Todo / Reminder 的周期解析、下一次提醒计算和通知 outbox 维护继续收敛到 Todo 工具域。

* **Gemini Provider 与搜索入口**（PR #397）：新增 Gemini Provider 和 Gemini Google Search 路线支持，`agent.toml` 示例补充 `gemini:` 前缀；OpenAI Web Search 与 Gemini Search 继续通过搜索路线配置选择。

* **自定义 Tool 二开指南**（PR #396）：新增 [docs/development/custom-tools.md](./docs/development/custom-tools.md)，说明服务端白名单 Tool 注册、场景白名单、自然语言路由、确定性展示、成功验真、可见实体和查询新鲜度等接入边界。

### Changed

* **Todo 工具域迁移与边界收敛**（PR #393 #398 #399）：Todo 运行实现、Todo / Reminder 路由、pending 状态机、确认/澄清恢复、可见实体快照、成功守卫、查询新鲜度和周期提醒规则集中到 `qq-maid-core/src/runtime/tools/todo/`；`runtime/respond/` 不再承载零散 Todo 业务规则。

* **普通消息 Tool Loop 路由边界收敛**（PR #399）：普通聊天弱意图判断与 Tool Loop 前置路由重新整理。纯聊天、解释、创作、文本整理和 Codex prompt 整理类请求默认保持普通聊天；明确 Todo / Reminder、天气、火车、RSS、Web Search 等工具意图才进入对应工具路径。

* **Tool Loop 后处理收敛到 tools/agent_turn**（PR #399）：整轮工具结果投影、确定性展示、失败/部分成功诊断、Todo 成功验真和可见实体快照写入统一由 tools 层处理；Respond / chat_flow 只负责发起 Tool Loop、保存会话和拼装最终响应。

* **非 Todo 工具路由保持独立**（PR #399）：天气、火车、RSS 和 Web Search 路由从 Todo 判断中拆开，避免非 Todo 工具命中后回退到普通聊天或 Todo 规则。

* **Todo 查询新鲜度与可见实体快照整理**（PR #399）：列表后的“完成第一条”“刚才那条”等续指继续依赖用户实际看到的快照；群聊中不同 actor 的 Todo 交互状态保持隔离，避免串用他人列表。

* **Todo 每日摘要开关恢复**（PR #395）：恢复 Todo 每日摘要开关配置和调度入口，让主动提醒能力保持可控。

### Fixed

* **Todo 成功守卫加强**（PR #398 #399）：工具失败、需要澄清或需要确认时，最终回复不得声称已经完成；Todo 写操作继续以真实工具执行结果和持久化结果为准。

* **RSS 标题污染修复**（PR #385）：修复 RSS 标题污染问题，降低订阅内容展示噪声。

* **群聊冷却提示修复**（PR #387）：群聊冷却命中且消息明确指向机器人时返回轻量提示，避免用户以为消息被无声丢弃。

### Compatibility

* 用户可见行为原则上保持兼容：已有 Todo 增删改查、提醒、重复提醒、天气、火车、RSS、Web Search、普通聊天和文本整理入口继续使用原有表达方式；本版本主要收紧误路由和成功验真边界。
* 版本号入口仍为根包 `qq-maid-bot` 的 `Cargo.toml`。各内部 crate 版本没有在本次发布中作为统一发布号同步提升。

### Follow-up

* Codex-like Agent Runtime 仍是后续规划，已由 issue #400 跟踪；本版本没有实现 #400，也不把它作为 v0.15.0 已发布能力。

## [v0.14.2] - 2026-07-09

### Release Focus

* **事件流、Tool Loop 流式与运维体验收敛版本**：本版本在 v0.14.x 版本线上继续推进统一响应事件流、Tool Loop 进度事件和最终回答流式转发，让私聊工具链回复更接近普通聊天的连续体验；同时修复 Web Search 长查询和文本整理误触发联网查询问题，补充开机自启动配置，并继续清理 Core 旧兼容路径。

### Added

* **统一响应事件流设计基线**（PR #368）：补充 Core 响应事件流基线，为普通聊天、Tool Loop 进度、最终回答流式推进和后续多入口发送能力统一打底。

* **Tool Loop 进度事件与最终回答流式推进**（PR #369 #372）：Tool Loop 新增可观测进度事件，并接入最终回答流式推进，降低长工具链请求只能等待完整结果的体验落差。

* **RSS 最近更新与批量管理增强**（PR #365）：增强 RSS 最近更新查看和批量管理能力，让订阅排查与日常整理更直接。

* **开机自启动配置**（PR #381）：新增发布包和运行目录相关的自启动配置支持，补充 systemd / Windows 启动模板链路，降低升级后手工配置成本。

### Changed

* **Slash command 事件化试点**（PR #376）：开始将 slash command 处理向事件化模型收敛，为后续命令、响应计划和多入口分发统一做准备。

* **Core 旧兼容路径清理**（PR #377）：删除一批 Core 旧兼容路径，让当前 Respond、Tool Runtime 和服务入口的调用链更清楚，减少后续维护时误走历史分支的风险。

* **Gateway 消费策略命名收敛**（PR #375）：收敛 Gateway 消费策略相关命名，使事件消费、发送策略和运行期语义更一致。

* **Todo 写操作回执简化**（PR #382）：简化 Todo 新增、修改、完成等写操作后的用户可见回执，减少重复状态噪音，同时保留真实执行结果。

### Fixed

* **文本整理误触发联网查询**（PR #367）：修复“整理 / 改写 / 总结”类文本处理请求被误判为联网查询的问题，降低普通文本任务进入搜索链路的概率。

* **Web Search 长查询预处理**（PR #378）：修复长查询在进入 Web Search 前的预处理问题，避免过长或结构复杂的搜索请求在工具链入口被错误截断或路由。

### Internal

* 更新 GitHub Project 管理 workflow（PR #383），继续整理维护自动化。

## [v0.14.1] - 2026-07-08

### Release Focus

* **联网查询与结构化出站修复版本**：本版本在 v0.14.0 群聊体验优化版本线上继续收敛 Core→Gateway 出站契约、搜索入口和 Gateway 运行期缓存，重点修复 `/查` 与自然语言搜索意图的响应路由，降低长联网查询、群聊引用和结构化输出渲染路径中的回归风险。

### Added

* **助手结构化出站模型**（PR #350 #351）：新增平台无关的助手输出内容模型，Gateway 根据平台能力渲染 Markdown、纯文本 fallback 和媒体占位，为后续更多出站内容类型打基础。

* **搜索 Tool 入口**（PR #359）：将 `/查`、`/查询` 和明确搜索意图收敛到服务端白名单 `web_search` Tool，复用现有 QueryExecutor，并补充流式搜索结果转发与查询上下文处理。

### Changed

* **Core→Gateway 出站边界收敛**（PR #354 #355 #356）：删除旧兼容出站接口，将出站正文读取统一到结构化 output 访问器，并把可复用输出模型下沉到 common，避免 Core、Gateway 和后续 Tool 路径各自拼接正文。

* **统一通知接入边界收敛**（PR #349）：继续整理通知、推送和运行期响应的接入边界，减少跨层理解运行细节的情况。

### Fixed

* **联网查询响应路由**（PR #362）：修复显式 `/查` 和自然语言搜索意图在响应计划中的路由问题；流式可用时走真实查询流，非流式时正确聚合回退，避免搜索回复被普通聊天或工具循环路径误处理。

* **Gateway 运行期缓存限制**（PR #360）：限制引用索引等 Gateway 运行期缓存容量和 TTL，避免长期运行时缓存无界增长，同时保留按会话 scope 的最近引用回填能力。

* **中文意图与模糊时段解析**（PR #357）：收敛中文工具意图判断和常见模糊时段解析，降低“下午发烧了怎么办”等非待办语句误入 Tool Loop 的概率。

### Internal

* 精简 Todo 测试重复构造（PR #348），并补充搜索、结构化输出、引用索引和 Gateway 缓存相关回归测试。

## [v0.14.0] - 2026-07-07

### Release Focus

* **群聊优化与 SQLite 连接池版本线**：本版本重点收敛群聊中的 pending、引用快照、Tool Loop Todo 聚合状态、入站身份上下文、群成员详情和展示名链路，降低 A/B 用户串上下文、引用旧消息错误 fallback、后续 Todo 操作选错对象的风险；同时引入 SQLite 连接池，改善长期运行时的数据库访问边界。Core runtime、Respond 编排、Visible Entity Snapshot、Interaction State / Tool Projection 的收敛作为本轮稳定性支撑继续推进。

### Added

* **SQLite 连接池**（PR #334）：引入统一 SQLite pool，并新增独立连接池配置。Session、Todo、Memory、RSS 等模块继续使用同一业务数据库，但运行期连接获取、并发访问和后台任务调度边界更清晰。

* **群成员详情查询与身份补全**（PR #323 #324）：新增群成员详情查询接口，并在入站身份上下文链路中补全成员资料；成员详情接口不可用时走明确降级路径，不把平台资料缺失误判为业务身份变化。手动展示名改为通过 `set` 管理（PR #329），减少身份显示与业务 owner 混用。

* **botmon 监控采样脚本**（PR #311 #313）：新增 `botmon.sh` 并集成到部署流程，Uptime 展示改为年月天小时分秒的可读格式。

### Changed

* **Core runtime 装配边界收敛**（PR #336）：收敛 runtime state、stores、executors、workers 的装配职责，减少业务模块直接理解启动细节或跨层持有状态的情况。

* **Respond 编排职责拆分**（PR #337）：拆分 Respond router、command dispatcher、tool runtime、interaction state 和 conversation session 等职责，让普通聊天、slash 命令、Tool Loop、pending 确认与会话状态的边界更明确。

* **Memory 直接操作与原子替换**（PR #338）：记忆新增、编辑、删除等操作改为更直接的服务端执行路径，编辑场景使用原子替换，避免模型文案替代真实持久化结果。

* **Visible Entity Snapshot 通用化**（PR #339）：将 Todo 可见列表快照向通用 Visible Entity Snapshot 收敛，继续支持“第一条 / 第二条 / 刚才那个”等基于用户可见编号的后续操作，同时为后续更多实体类型复用打底。

* **Tool Projection / Interaction State 边界初步收敛**（PR #341）：将工具投影和交互状态快照进一步收敛，减少 Todo 专属快照逻辑向通用交互层泄漏。剩余抽象优化已拆到 #342。

* **入站身份上下文链路增强**（PR #318 #321 #324）：打通并增强入站身份上下文，收敛身份字段，把平台身份、业务 owner/scope 与显示资料的职责继续拆开，降低群聊、多入口和引用场景中串上下文的风险。

* **业务 owner 与 scope 设计文档收敛**（PR #314 #316）：补充 owner 策略、群共享入口和 scope key 历史迁移评估，为后续多平台、多账号和群共享语义提供依据。

### Fixed

* **群聊 pending 交互隔离**（PR #308）：修复群聊 pending 交互状态可能跨用户复用的问题，并补齐 A/B 用户隔离回归测试。

* **群聊引用快照绑定收紧**（PR #309 #310）：收敛引用快照中的 Todo 编号链路，补齐 owner/scope 不匹配时不得 fallback 的回归测试，降低引用旧消息或他人消息时误操作 Todo 的风险。

* **群聊 Tool Loop Todo 聚合状态隔离**（PR #312）：修复群聊 Tool Loop Todo 聚合状态隔离问题，避免不同群成员的工具上下文互相影响。

* **QQ 群关键词触发与 scope/identity 边界说明**（PR #304 #305）：明确 QQ 群关键词触发边界，并澄清 scope 与 identity 的职责分离，减少群聊入口行为误解。

### Removed

* **无效 project-status-sync workflow**（PR #322）：移除从未生效的项目状态同步 workflow，降低维护噪音。

### Known Limitations

* **Todo 快照内部抽象仍会继续优化**：`last_todo_query` / `last_todo_action` 仍是 Todo 专属字段，`tool_projection.rs` 仍保留 Todo 可见列表判断。当前判断是不影响用户可见正确性的架构债，后续由 #342 跟踪，不阻塞本次发布。

### Internal

* 根包 `qq-maid-bot`：`0.13.2` → `0.14.0`

## [v0.13.2] - 2026-07-06

### Fixed

* **QQ 群引用索引覆盖**（PR #292）：补齐 QQ 官方群消息的 `ref_index` 登记路径。Gateway 观察到且带 `current_msg_idx/msg_idx` 的群消息会提前登记，后续引用只通过 `ref_msg_idx` 精确查找目标；不使用 `message_id` 伪造引用索引，也不重新引入 `message_id` fallback。

* **QQ quoted payload 兜底**（PR #292）：当本地 `ref_index` miss 但当前引用事件携带 `msg_elements[0]` quoted payload 时，会使用 payload 中的文本和附件元信息作为一次性引用上下文，缓解历史消息、未登记消息或重启前消息引用时只能看到 `REFIDX_*` 的问题。

* **QQ 官方群回复展示边界**（PR #292）：纯文本群回复和本地错误 fallback 不再拼显式 `<@openid>`，避免 QQ 文本消息原样展示 openid；Markdown 出站仍保留显式 mention，用于 `/rss` 等 Markdown 响应保持正常 at 展示。

## [v0.13.1] - 2026-07-06

### Added

* **QQ 图片与多模态输入链路**（PR #280 #285）：QQ 官方入口新增图片附件取回和本地媒体缓存，图片会作为结构化 input part 进入 Core / LLM 调用链；OpenAI-compatible provider 可按多模态 payload 发送图片，非图片文件仍只保留元数据，不做 OCR 或内容解析。

* **雷达 Slash 命令**（PR #276）：新增 `/rader`、`/radar`、`/雷达` 命令，可查看 Codex Radar 和 Claude Code Radar 的公开摘要，也可分别查看单个雷达或反馈入口；该命令只读取公开数据源，不进入普通聊天 Tool Loop。

### Changed

* **Todo 重复规则与周期推进**（PR #272）：重构重复待办的规则解析、展示和下一次提醒推进逻辑，统一日 / 周 / 月等周期语义，减少周期任务完成、恢复、编辑后提醒时间不一致的情况。

* **雷达卡片展示优化**（PR #276）：雷达摘要展示改为更稳定的卡片式文本，能区分额度雷达、降智雷达、字段缺失、部分数据源失败和无真实数据等状态，减少外部数据异常时的误导性输出。

### Fixed

* **QQ 引用上下文链路修复**（PR #285 #286）：修复 C2C、群聊和流式回复中的引用索引传递，用户引用上一轮消息、图片或流式回复时，Core 能收到更完整的上下文，避免图片说明、追问和补充问题丢上下文。

* **多模态消息顺序修正**（PR #280）：将回复块作为独立 input part 放在图片前发送，降低模型处理“引用说明 + 图片”时把文本和图片关系读反的概率。

### Internal

* **测试资产与模块拆分**（PR #277 #278 #279）：整理 Respond 记忆、会话、Todo Tool Loop 等测试结构，建立 Rust 测试资产基线，方便后续扩展多模态、引用和工具链回归测试。

## [v0.13.0] - 2026-07-05

### Added

* **响应输出策略层**（PR #232）：Core 响应新增输出策略，Gateway 可按策略选择普通完整回复、先提示再完整回复、先提示再流式回复等投递方式，为 C2C 流式、Tool Loop 进度和跨平台发送能力统一打底。

* **Tool Loop 可见进度提示**（PR #243 #260）：新增可配置的 C2C Tool Loop 短提示能力，默认通过 `QQ_MAID_C2C_VISIBLE_PROGRESS_STATUS_ENABLED` 开启；提示内容由 Core 输出策略控制，不代替最终回复或工具执行结果。

* **平台无关入站模型**（PR #255）：Gateway 新增 `InboundMessage`、`Actor`、`Conversation`、`ReplyTarget` 等平台模型，QQ 官方、微信和后续 OneBot 入口先转换成统一入站语义，再调用 Core。

* **Gateway 出站能力抽象**（PR #257）：新增出站能力和渲染 profile，QQ 文本、Markdown、图片、C2C 流式和微信文本等发送分支按 `ReplyCapability` 选择，不再把 QQ 官方发送细节塞进 Core。

* **微信服务号文本入口**（PR #258 #259）：新增可选微信服务号明文 text-only HTTP 回调，支持 GET URL 验证、POST 文本 XML 解析、同步 XML 文本回复、Markdown 降级为 text、签名校验和 `/ping all` 诊断摘要。

* **微信服务号长任务异步回复**：微信文本入口在 Core 处理超过同步安全预算时，会先返回 `success` 结束微信 HTTP 响应，避免平台超时重试导致同一消息重复执行；后台继续完成 Core 调用，并在配置 AppID / AppSecret 后通过客服文本消息补发最终结果。

* **微信客服消息令牌管理**：新增客服文本消息子模块，按需获取并缓存 `access_token`，遇到令牌失效错误时自动刷新后重试一次；新增 `WECHAT_SERVICE_REPLY_TIMEOUT_MS` 和 `WECHAT_SERVICE_API_BASE`，用于控制同步回复预算和测试替身 API Base URL。

* **可配置 OpenAI-compatible Provider 与 MiMo 支持**（PR #262）：新增 OpenAI-compatible provider 类型和 route 配置，`runtime/config/agent.toml` 默认注册 MiMo provider 元数据；模型候选链支持 `mimo:` 等 provider 前缀，并保留环境变量继承行为。

* **业务归属键与发送目标分离**（PR #261）：新增身份归一化和 rebaseline 支持，把 session / memory / todo 的业务 owner/scope 与真实投递目标拆开，为多平台入口和主动推送保留原始发送地址。

### Changed

* **CoreService 模块拆分**（PR #243）：Core 服务实现拆分为 `handle`、`streaming`、`errors`、`types` 等模块，降低单文件复杂度，并补齐流式事件和错误类型测试。

* **C2C 流式发送模块拆分**（PR #243）：Gateway C2C 流式逻辑拆为 delivery、event_stream、send、types 等子模块，保留首帧成功后不补发普通全文的边界。

* **Tool Loop 模糊路由收紧**（PR #244）：优化提醒、天气、列车、RSS、Todo 等自然语言路由边界，减少普通聊天误入工具链或工具链误接管聊天的情况。

* **RSS / Todo 统一通知投递目标**（PR #264）：RSS 更新和 Todo 提醒改为携带平台原始 delivery target，经 Notification Outbox 和 Gateway PushSink 投递，避免从业务 scope 反推出发送地址。

* **微信服务号回复边界**：入口能力从“最小同步 text-only 回调”扩展为“同步快路径 + 慢请求客服补发”；Markdown 仍降级为纯文本，仍不支持加密 XML、模板消息、图片语音视频、菜单事件、主动推送或流式输出。

* **微信服务号入口拆分**：原单文件回调入口拆为 `wechat_service/mod.rs` 和 `wechat_service/customer.rs`，同步回调、慢任务补发、客服消息 API、token 缓存和测试职责更清晰。

### Fixed

* **群聊提及判定修正**（PR #230）：普通群消息只信任官方结构化 mention / reply 语义识别机器人，移除基于旧 AppID、openid、member_openid、CQ 文本和 `<@...>` 文本的误判路径，并同步修正记忆 / RSS 等场景的群聊测试。

* **微信回调重试去重**（PR #259）：微信 POST 明文 `text` 消息按平台消息 ID 进入 Gateway 去重，平台重试不会重复进入 Core，也不会重复创建后台慢任务。

* **微信令牌失效恢复**：客服消息发送遇到 `40001`、`40014`、`42001` 等 access token 失效类错误时，会清理缓存、重新获取 token，并仅重试一次，避免无限放大真实业务错误。

* **客服消息返回校验**：客服消息 API 响应必须显式包含 `errcode`，避免把缺失关键字段的异常响应误判为发送成功。

* **敏感信息脱敏规则**：通用脱敏逻辑补充 JSON / 文本中的 `access_token`、`app_secret` 类字段，减少微信 API 错误体或诊断摘要误带敏感值的风险。

### Documentation

* **项目对外称呼统一**（PR #246）：README、贡献文档和开发文档统一使用“小女仆机器人”项目称呼。

* **多平台入口架构说明**（PR #263）：README、Gateway README 和开发文档补充平台原始 ID、业务 scope / owner、delivery target、ReplyCapability 和 adapter / sender 边界。

* **微信服务号配置文档**：README、Gateway README、runtime README 和 `.env.example` 同步更新微信文本入口、慢请求行为、客服补发前提、诊断摘要和安全注意事项。

* **OneBot 状态说明**：README 和 Gateway README 明确 OneBot 11 目前只有平台模型和边界预留，尚未实现反向 WebSocket server、事件 adapter、API sender 或主动推送路由。

### Internal

* 根包 `qq-maid-bot`：`0.12.1` → `0.13.0`

## [v0.12.1] - 2026-07-04

### Fixed

* **群聊空 @ 触发与 Bot 身份识别**（PR #224）：补齐 Bot 官方 App ID、稳定 ID 和额外 ID 的统一识别，修复群聊 `active` / `mention` 模式下空 @ 或结构化 mention 识别不稳定的问题。

* **工具路由与 Todo 查询边界**（PR #224）：收紧私聊 / 群聊 Tool Loop 路由，避免日期型待办查询误入普通工具链，同时保持群聊默认不扩大 Tool Loop 行为。

* **Clippy 告警**（PR #224）：按 `manual_pattern_char_comparison` 建议改用字符数组 pattern，修复 CI 中 `cargo clippy --workspace --all-targets --all-features -- -D warnings` 失败。

### Internal

* 根包 `qq-maid-bot`：`0.12.0` → `0.12.1`

## [v0.12.0] - 2026-07-04

### Added

* **Agent 场景策略配置**（#206, PR #207 #210）：新增 `runtime/config/agent.toml`，统一描述私聊 / 群聊场景、profile、Tool Loop 轮数、输出预算、reasoning effort 和 `/查` 搜索路线。默认模板作为非敏感策略随 runtime / Release 包分发。

* **Todo 日期查询**（#200, PR #207）：支持“查看今天待办”“查看明天待办”“明天有哪些未完成待办”等自然语言查询。日期筛选按北京时间本地自然日匹配，优先使用 `due_at`，缺失时回退 `due_date`，无时间待办不会混入日期结果。

* **Notification Outbox / Todo 单次提醒一阶段**（#174, PR #203）：新增统一通知 Outbox 存储和后台投递 Worker。Todo 创建、编辑、完成、取消、恢复和删除路径会同步维护单次提醒任务，由通知层负责领取、投递、失败重试和状态回写。

### Changed

* **模型路线继承与 fallback**（#206, PR #207 #210）：默认 `agent.toml` 不绑定具体 Provider 或模型路线；`private_main`、`group_main`、`aux` 继续继承 `.env` 中的 `PRIVATE_LLM_MODEL`、`GROUP_LLM_MODEL` 和 `LLM_MODEL`。`LLM_PROVIDER=auto` 可跳过缺少 API Key 的候选 Provider，并在 Tool Loop 可恢复错误时继续尝试后续候选。

* **Todo 可见编号快照**（PR #207）：日期查询、普通查询和 Tool 展示统一写入用户实际看到的可见列表快照，后续“完成第一条”“刚才那条”等指代继续绑定最近展示结果，不暴露数据库内部 ID。

* **Todo 列表折叠与回执展示**（PR #207 #211 #216）：普通待办列表超过 5 项时折叠并提示“查看完整结果”；全部看板保留少量隐藏项展开策略。Todo 时间、提醒和详情展示改为更紧凑的纯文本格式，减少 Markdown 行内 code / 引用块对 QQ 渲染的影响。

* **Release runtime 校验**（PR #207 #210）：打包脚本和运行目录校验要求 `config/agent.toml` 随 Release 包分发，并继续阻止真实 `.env`、数据库、日志、私有 prompt 和知识资料进入归档。

### Fixed

* **群聊 Active 触发修复**（PR #207 #210）：修复 `active` 模式下直接 @ 机器人不触发的问题，并修复中文 active keyword 前缀裁剪可能触发错误字节边界的问题。

* **Todo 展示回归修复**（PR #216）：修正 Todo 列表和回执中时间 chip、提醒时间和详情行的展示断言，补齐完整 CI 中遗漏的旧格式测试。

### Upgrade Notes

* 新增默认 `runtime/config/agent.toml`。已有部署建议对比新版 Release 包中的模板；不自定义 `AGENT_CONFIG_FILE` 时，默认文件缺失仍会回退旧环境变量兼容路径。
* `agent.toml` 不保存 API Key、Access Token、Base URL、真实 prompt、用户资料、聊天记录、SQLite 路径或日志路径；这些仍应放在 `.env` 或外部私有目录。
* 如果旧部署只配置 DeepSeek / BigModel / OpenAI 兼容网关，不需要因为默认 `agent.toml` 改成 OpenAI；普通聊天模型路线默认继续继承 `.env`。
* Todo 单次提醒属于一阶段能力，当前面向明确时间的个人待办提醒和状态回写，不等同于完整通知平台；RSS 迁移到统一通知链路仍是后续任务。

### Internal

* 根包 `qq-maid-bot`：`0.11.1` → `0.12.0`

## [v0.11.1] - 2026-07-03

### Changed

* **移除成员编号旧链路**（#166, PR #192）：删除旧 `MEMBER_ID_MAPPING_FILE` / `member_id_mapping.json` 成员编号识别链路，包括配置解析、Prompt 注入、聊天前处理、示例文件和相关测试。普通聊天中的三位数字不再触发身份切换或未知编号拦截，会作为正常文本进入聊天流程。

* **清理旧 session 启发式状态**（PR #192）：普通聊天不再写入 `active_scene`、`expected_mode`、`recent_session_focus`、`last_user_correction` 等早期便利状态。新增 Session V3 migration，从历史 `state_json` 中一次性删除已废弃聊天状态键，同时保留 `current_topic` 和其他扩展状态键。

* **多平台 Release 矩阵**（PR #195）：Release workflow 改为 Linux x86_64、Linux ARM64、macOS Intel、macOS Apple Silicon、Windows x86_64 五个平台原生 runner 构建，并为 release profile 开启符号剥离。

### Fixed

* **非流式回复发送链路**（PR #197）：CompleteToolLoop、Todo 和非流式 Provider 路径的合成最终 `TextDelta` 不再走 QQ C2C stream 首帧，改为 `Completed` 后走普通 C2C 回复。真实 Provider 增量流仍继续使用 QQ C2C stream；Active 后仍不补发普通全文。

### Documentation

* 精简根 `AGENTS.md`，并在 `CONTRIBUTING.md`、`docs/DEVELOPMENT.md` 中补齐 LLM 职责、测试入口和 Gateway/Core/LLM 边界（PR #196）。
* README 和 runtime 文档补充多平台 Release 包说明（PR #195）。

### Upgrade Notes

* `MEMBER_ID_MAPPING_FILE` 已移除。旧 `.env` 中如果仍保留非空 `MEMBER_ID_MAPPING_FILE`，新版本会返回明确配置错误；升级前需要删除该环境变量。
* 旧部署目录中的 `config/member_id_mapping.json` 不再读取，可以手动删除。
* 历史 session 状态由 V3 migration 清理；升级前建议备份 `APP_DB_FILE` 指向的 SQLite 数据库。
* 私有 Speaker/Profile 能力不在本版本内置重做，后续按 #170 独立设计。

### Internal

* `qq-maid-core`：`0.1.15` → `0.1.16`
* `qq-maid-gateway-rs`：`0.1.9` → `0.1.10`

## [v0.11.0] - 2026-07-02

### Added

* **C2C 最终回复流默认启用**（#141, PR #171）：私聊最终回复默认使用 QQ 原生文本流发送，首帧失败或未拿到 stream id 时自动降级为普通完整回复，不再重新运行 Agent Loop。可通过 `QQ_MAID_C2C_FINAL_REPLY_STREAM_ENABLED=false` 快速回滚。

* **QQ 原生 typing 支持**（PR #171）：新增 `msg_type=6` typing 模块，仅对私聊普通 Agent 请求延迟发送（默认 1000ms），首个流式首帧成功、完整回复、失败或取消时自动清理。新增 `C2cTypingSender` 抽象与 `C2cTypingStatusGuard` 生命周期管理，修复 typing stop 竞态。

* **Core 响应层通用工具结果编排**（#168, PR #167）：新增 `agent_outcome` 通用编排模块，统一计算 `ToolExecutionOutcome`、`AgentTurnStatus` 和可信响应块。整轮多个工具结果均进入最终响应，不再只展示最后一个。整轮状态按 Succeeded / Failed / PartialSuccess / PendingConfirmation / RequiresClarification 聚合。

* **工具结果展示策略三层分发**（#168, PR #167）：新增 `OutcomePresentation::{Trusted, Internal, Unhandled}` 三层分发，替换 `blocks.is_empty()` 推断语义。只在存在可信块且无 Unhandled 时允许覆盖模型最终回复。未适配工具默认 Unhandled，记录 diagnostics 并返回确定性兼容提示，不静默丢弃。

* **Weather / list_todos 确定性展示适配器**（PR #167）：新增 `tool_presenters` 模块，`get_weather` 转 `ResponseBlock::FactCard`，`list_todos` 适配为可信 `RelatedList` 并写入真实可见快照。

* **Provider 无关的统一 Agent Loop 状态机**（#137 #138, PR #159 #160）：新增 `tool_loop.rs`，收敛 Responses / Chat Completions 工具执行语义到统一 helper，Tool Loop 支持同轮多调用串行执行。

* **Tool Loop 最终可信回复 Core 事件流**（#140, PR #163）：建立 `FinalToolResponse` 事件，让 Tool Loop 执行完成后由 Core 生成确定性回复，而非依赖 LLM 自由发挥。

* **统一 Todo 语义澄清、Pending 与受限任务恢复**（#139, PR #161）：统一 Todo 语义澄清流程，支持受限任务恢复，Pending 操作生命周期完整。

* **Todo 写操作统一确认策略与确定性回执**（#164, PR #162 #167）：所有 Todo 写操作（新增、完成、恢复、修改、删除）统一走确定性回执，不再依赖模型文案替代执行结果。Todo 验真只看 `domain=="todo"` 且 `effect!=ReadOnly` 的写 outcome。

### Changed

* **Tool Calling 默认轮数**（PR #160）：由 3 调整为 5，给 Agent 更多空间完成多步操作。



### Fixed

* **typing 生命周期**：修复 stop 竞态导致 typing 任务泄漏；新增请求级原子 flag 保证 stop 幂等、Drop 兜底清理。

* **关闭流式异常处理**：Completed 走普通完整回复，Failed 发送 Core 返回的安全失败文案，channel 提前关闭发送固定本地失败提示，不重跑 Core、不伪造回复。

* **timeout stop reason 统一**：`SearchTimeout` / `LlmTimeout` 统一映射为 `timeout`。

* **Todo 写操作守卫**（#147, PR #158 #165）：修复 Todo 验真提示词绕过、混合文案绕过验真（PR #158）、已取消待办自然语言查询误入进行中快照（PR #165）等问题。

* **Todo 待确认操作快照**（PR #150）：合并 `last_todo_action` / `last_todo_query` 快照，防止确认阶段信息丢失。

* **待办列表展示与记忆快照**（PR #148 #146）：单状态待办列表统一看板式展示、补全已取消列表入口（PR #148）；修复记忆列表操作后快照丢失（PR #146）。

* **Tool 输出截断**：超过限制时统一包装为合法 JSON（`truncated` / `original_chars` / `preview`），不再产生非法 JSON 污染模型上下文。

### Documentation

* 添加鸣谢模块（`docs/acknowledgments.md`）。
* 精简根 `AGENTS.md` 并修正过期文档路径与描述。

### Internal

* `qq-maid-llm`：`0.1.6` → `0.1.7`

  * 新增 Agent Loop 统一状态机、Tool Loop 协议改进、工具注册。

* `qq-maid-core`：`0.1.14` → `0.1.15`

  * 新增 `agent_outcome` 通用工具结果编排模块。
  * 新增 `tool_presenters` 工具确定性展示适配器。
  * Todo 写操作确定性回执与多重守卫修复。

* `qq-maid-gateway-rs`：`0.1.8` → `0.1.9`

  * 新增 C2C 最终回复流式发送。
  * 新增 `typing` 模块与生命周期管理。
  * 关闭流式异常降级处理。

## [v0.10.1] - 2026-07-01

### Added

* **消息分段发送**：为 QQ 普通消息实现 Markdown/fallback 成对分段，移除 Core 侧聊天截断，由 Gateway 按 QQ 平台长消息限制进行分段发送。

* **上下文预算管理**：新增基于字符限制的上下文预算机制，支持消息保留/淘汰策略和 Tool Loop 超限保护，防止上下文溢出。

* **Tool Calling 扩展**

  * 为 DeepSeek 和 BigModel 接入工具调用能力，复用统一 Chat Completions Tool Calling 协议。
  * Todo 接入私聊 Agent Tools，支持自然语言待办操作。
  * Tool Loop 支持同轮多调用串行执行与 Todo 编号预绑定，减少多轮往返。
  * Todo Tool 支持独立 session 最近对象状态与 `reference="last"` 语义。

### Changed

* **代码重构**

  * 拆分 Core storage session/memory、Memory respond flow、chat_flow Todo 守卫等长文件为职责子模块。
  * 拆分 Gateway dispatcher 会话 worker 状态机。
  * 拆分 LLM provider 候选链路由实现。
  * 外移 LLM/Common、Gateway、Core 的内联测试模块到独立测试文件。
  * 拆分 storage/todo 纯 helper 与行映射。
  * 拆分 tools/todo 为职责子模块，合并 TodoEditPatch 并收敛 todo 门面与状态映射。
  * 收敛 Tool Loop 路由计划。
  * 移除废弃的 Todo slash 写入口。

* 看板拖动 Status 可联动 Issue 开关状态。

### Fixed

* 修复 Markdown 消息分段中的空代码块和群 @ fallback 前缀问题。
* 对齐官方长消息分段基线，迁移 markdown_strip 逻辑。
* 上下文预算估算错误必须传播，不能按 0 字符放行。
* 修复 Todo 写操作守卫误判，简化私聊 Todo 路由。
* 统一 Todo 查询最近可见列表快照读写，恢复确定性与同义词路由。
* 修复普通聊天流式路由恢复。
* 修正 todo 工具错误透传与依赖跳过问题，补全多工具预处理失败兜底。
* 修复 Todo Tool 恢复和 pending 覆盖，保留 Tool Loop 写入的 pending。

## [v0.10.0] - 2026-06-30

### Added

* **QQ 官方 C2C 流式输出**

  * Chat 接入 QQ 原生文本流式协议。
  * `/查` 接入 QQ 原生 Markdown 流式渲染。
  * Gateway 新增流式发送引擎，并拆分 C2C、群聊及消息渲染模块。

* **消息聚合与可靠投递**

  * 新增消息聚合器，可按时间窗口将多条回复合并为单条消息，降低 QQ 平台消息发送频率。
  * 新增基于 `reservation / commit / rollback` 的两阶段物理消息去重机制。
  * C2C flush 失败时支持 deferred 消息保存。
  * 程序关闭时可优雅回滚未提交的 reservation。

* **模型原生 Tool Calling 首期能力**

  * `qq-maid-llm` 新增工具注册、工具超时和输出大小限制。
  * 支持 OpenAI Responses API 的 `function_call / function_call_output` 串行 Tool Loop。
  * `qq-maid-core` 新增运行时工具模块。
  * 首期接入 `WeatherTool`。
  * 支持根据 Provider 能力自动选择 Tool Calling 路径。

* 天气逐日预报会根据请求天数自动选择和风天气 3 日或 7 日接口，并在本地按 `forecast_days` 截断结果。

* 新增 `qq-maid-healthcheck.sh` 进程级健康诊断脚本，支持进程、端口和连接状态检查。

* 部署与知识库同步脚本统一使用共享远程配置 `scripts/deploy.conf`。

* 新增知识库同步脚本 `sync_knowledge.sh` 及对应测试。

* 新增 Issue 和 PR 自动加入 GitHub 项目看板的 Workflow。

* `/ping` 异常摘要支持同时展示多条 note，不再遗漏 LLM 降级或 Gateway 重连信息。

### Changed

* **Gateway 模块重构**

  * 将 C2C、群聊、缓存、流式发送、渲染和消息聚合逻辑拆分为独立模块。
  * 精简 Gateway 主模块。
  * 重构去重模块以支持两阶段提交。

* `MAX_CONCURRENT_RESPONSES` 默认值由 `4` 调整为 `8`。

* Tool 输出超过限制时，统一包装为合法 JSON：

  * `truncated`
  * `original_chars`
  * `preview`

* 普通流式消息的成功日志由 `info` 降级为 `debug`，减少日志噪声。

* 扩大日志脱敏覆盖范围。

* Tool Loop 路径接入统一限流和健康观测能力。

### Fixed

#### QQ C2C 流式输出

* 修复流式回复只显示首个分片的问题。
* 修复流式请求缺少 `msg_id` 时被 QQ 平台降级为普通回复的问题。
* 修复中间帧重复发送完整正文，导致 QQ 客户端重复追加内容的问题。
* 修复结束帧再次追加完整正文的问题；结束帧现在只发送尚未发送的尾部内容。
* 修复完成帧缺少连续 `index` 和 `reset` 字段的问题。
* 修复错误使用中间帧返回的 `stream_id` 续接流的问题；现在仅使用首帧返回的 `stream_id`。

#### Tool Calling

* 修复 Tool 输出被直接截断后产生非法 JSON、污染模型上下文的问题。
* 修复 `forecast_days` 参数被静默纠正的问题。

#### 消息聚合与去重

* 修复消息聚合关闭过程中的竞态条件。
* 修复消息分类失败时可能丢失消息的问题。
* 修复连续聚合边界下的消息顺序和重复问题。
* 修复跨批次重试时丢失消息的问题。
* 修复 Barrier 生命周期管理问题。
* 修复 C2C reservation 失败后错误压制 QQ 平台重试的问题。

#### 部署与健康检查

* 修复知识库同步目标冲突问题。
* 增加缺失源目录保护，避免误删除远端知识库。
* 修复健康诊断脚本发布、退出码、连接过滤和配置读取问题。

### Documentation

* 新增 `docs/analysis/openclaw-qqbot-api-integration.md`

  * OpenClaw QQ Bot API 接入实现分析报告。

* 新增 `docs/design/tool-calling-qq-delivery.md`

  * Tool Calling 与 QQ 消息发送衔接设计说明。

* 新增 `docs/tasks/scope-aware-model-routing.md`

  * 群聊和私聊按场景配置聊天模型及查询模型的任务文档。

* 将 `message-aggregation.md` 归档至 `docs/tasks/done/`。

* 移除子 Agent 使用规则文档。

* 更新 `AGENTS.md` 中的 CI 行为和版本发布说明。

* 更新 `qq-maid-llm/README.md`，补充 Tool Loop 和模块结构说明。

### CI

* 重构 `release.yml`：

  * 新增独立的 preflight 前置校验 Job。
  * 仅当 Tag 对应提交位于 `master` 历史中，并且包含有效代码变更时，才启动矩阵构建。
  * 修正无效的 `paths-ignore` 配置。
  * 增加 `master` 祖先关系校验。

* preflight 拉取逻辑由 `--depth=1` 浅克隆改为完整 `refspec`。

* 动态值统一通过环境变量传递。

### Internal

* `qq-maid-llm`：`0.1.4` → `0.1.5`

  * 新增 `tool.rs` 和 `tool_loop.rs`。
  * `LlmProvider` 扩展 `chat_with_tools` 和 `tool_calling_protocol`。
  * `LimitingLlmProvider`、`ObservedProvider` 适配 Tool Calling。

* `qq-maid-core`：`0.1.12` → `0.1.13`

  * 新增 `runtime/tools/` 和 `WeatherTool`。
  * 新增 Tool Calling 配置。
  * `respond/` 适配 Tool Loop 路径。

* `qq-maid-gateway-rs`：`0.1.6` → `0.1.7`

  * 新增 C2C、群聊、缓存、流式发送、渲染和消息聚合模块。
  * 重构消息去重机制。
  * 精简 Gateway 主模块。

* `qq-maid-common`：`0.1.0` → `0.1.1`

  * 增强日志脱敏能力。


## [v0.9.1] - 2026-06-29

### Changed

- 升级依赖：`sha2` 0.10 → 0.11（qq-maid-core），`tokio-tungstenite` 0.28 → 0.29（qq-maid-gateway-rs）
- 适配 `sha2` 0.11 digest 输出格式，保持哈希字符串不变
- 刷新 Cargo.lock 同步补丁依赖版本

### Internal

- `qq-maid-core` 0.1.11 → 0.1.12
- `qq-maid-gateway-rs` 0.1.5 → 0.1.6

## [v0.9.0] - 2026-06-29

### Added

- 消息并发调度改造：Gateway 新增 `MessageDispatcher`，按 scope 粒度的消息串行调度与 Worker 生命周期管理，避免多 Worker 抢发回复
- LLM / Web Search 全局并发限制：`qq-maid-llm` 新增 `limiter.rs`，LLM 和 Web Search 共享同一 `Semaphore`，支持 `MAX_CONCURRENT_RESPONSES` 环境变量（默认 4）
- 优雅关闭：Gateway 接入 `CancellationToken`，WebSocket 主循环和 Dispatcher 在收到信号后正常退出
- Reply cache scope 隔离：缓存 key 增加 scope_key 维度，防止跨用户/跨群串数据

### Changed

- Gateway protocol 层不再直接处理消息，改为通过 Dispatcher 统一入队调度
- `truncate_chars` 和 `redact_sensitive_text` 收敛到 `qq-maid-common`，删除各模块重复实现

### Internal

- `qq-maid-common` 0.1.0（无变化）
- `qq-maid-core` 0.1.10 → 0.1.11（`max_concurrent_responses` 配置、limiter 包装、工具函数收敛）
- `qq-maid-llm` 0.1.3 → 0.1.4（新增 `limiter` 模块）
- `qq-maid-gateway-rs` 0.1.4 → 0.1.5（新增 `MessageDispatcher`、scope-keyed ReplyCache、CancellationToken 优雅关闭）

## [v0.8.0] - 2026-06-28

### Added

- Provider 真实增量流贯通：OpenAI Responses、Chat Completions、DeepSeek、BigModel 全部支持真实 SSE delta 向上传递到 Core 和 Gateway
- `/查` 改为使用 `query_stream()` 真实流式搜索，不再人工切片伪装流式
- 后台异步自动标题，不再阻塞主聊天回复
- `SessionStore::update_title_if_current()` 条件更新接口，防止后台标题覆盖会话数据

### Changed

- 普通聊天改走 `LlmChatService::stream_respond()` 真实 Provider 流，不再等待完整结果后才返回
- ModelRoute 流式候选链：首个非空 delta 后不再静默切换候选模型
- 异常 EOF 正确识别为流失败，不再被吞为成功完成
- 自动标题异步化：生成不阻塞 Completed，失败不影响本轮聊天，通过 `health_observation=ignore` 避免覆盖主模型健康状态

### Fixed

- 修复后台标题旧 SessionRecord 快照覆盖新会话数据的并发问题：后台标题改为条件 SQL 更新，仅当前标题仍为默认值时才写入

### Internal

- `qq-maid-llm` 0.1.2 → 0.1.3（Provider 流式改造、SSE 跨 chunk 拼接修复、Web Search 真实流）
- `qq-maid-core` 0.1.9 → 0.1.10（CoreResponseStream TextDelta 真实传递、chat 流式路径、异步标题、会话条件更新）
- `qq-maid-gateway-rs` 0.1.3 → 0.1.4（持续消费 TextDelta，仍只发送最终 Completed）
- LlmStreamEvent / CoreResponseEvent 统一标准流事件
- ObservedProvider 接入真实流式 metrics 和健康观测
- stream=false 兼容：非流请求包装为单 TextDelta + Completed，维持进程内流边界一致
- 取消传播改进：receiver 关闭后 producer 及早停止转发，释放 Provider 流

## [v0.7.0] - 2026-06-28

### Added

- 长期记忆增加个人/群聊作用域，修复跨用户记忆泄露
- 接入 BigModel（大模型 API）provider，扩展 LLM 供应商支持

### Fixed

- 修复知识库 frontmatter 检索污染：frontmatter 属性值被 BM25 误命中问题
- 修复群聊 pending 操作发起人校验：防止跨用户操作待办和记忆
- 修复已完成待办删除语义：改为永久删除（原软删除导致残留）
- 修复已取消待办删除确认文案区分
- 修复待办序号快照逻辑和已取消待办清理
- 修复记忆管理改用列表序号显示
- 修复记忆目标解析 Clippy 告警

### Internal

- `qq-maid-core` 0.1.8 → 0.1.9（长期记忆作用域隔离、待办/记忆多项修复）
- `qq-maid-gateway-rs` 0.1.2 → 0.1.3（群聊 pending 校验修复）
- `qq-maid-llm` 0.1.1 → 0.1.2（接入 BigModel provider）

### Documentation

- 更新 README Rust 行数描述

## [v0.6.2] - 2026-06-27

### Fixed

- 修复未闭合 fenced code block 到 EOF 时最后一行代码被误当 closer 复制进每个切片，污染检索索引

### Internal

- `qq-maid-gateway-rs` 0.1.1 → 0.1.2（v0.6.1 群聊 active 关键词配置、安全拦截文案遗漏提升）
- `qq-maid-llm` 0.1.0 → 0.1.1（v0.6.1 safety_blocked 错误码与健康摘要脱敏遗漏提升）
- `qq-maid-core` 0.1.7 → 0.1.8（未闭合代码围栏分块修复）

### Documentation

- RAG V2 任务文档从 tasks/ 移入 done/

## [v0.6.1] - 2026-06-27

### Added

- 重构知识检索切片机制：升级切片版本到 V2，按目标/软/硬字符上限和段落类型（文本/列表/引用/代码/表格）分段；代码片保留语言标签和行数上限；headings 感知标题路径
- 知识模块拆分为 chunking、scan、search、text 四个子模块，增强 FTS5 多 rank 策略和评分调试诊断

### Fixed

- 修复知识库 CJK 查询 1-gram 单字噪声挤占 BM25 相关结果的问题：存在 2-gram 或 3-gram 时自动过滤单字
- 修复短 CJK 查询（如"D区""站"）因单字过滤导致无结果的问题：短查询保留 1-gram 作为唯一检索信号
- 修复知识目录无源文件时 DB 已有索引被清空的问题：保留已有索引，支持从生产环境拷贝 app.db 到新部署环境
- 修复 LLM 安全拦截（prompt_blocked）错误提示不友好：新增 `safety_blocked` 专用错误码，gateway 返回固定用户文案而不回显敏感原因
- 修复群聊 active 模式缺少可配置关键词：新增 `QQ_MAID_GROUP_ACTIVE_KEYWORDS` 配置项，默认关键词 `小女仆`；Core 端群聊 prompt 增加唤醒关键词提示

### Changed

- Prompt 示例模板去项目特定用语，改通用表述
- maid_system 示例模板新增知识库查阅规则：优先使用已有资料、不用保守套话、被指出查到了就切换整理模式

### Documentation

- README 重写：增加项目个性与趣味内容、运行快照、使用示例、快速开始指引和 badge；移除未实现的 OneBot 标识；补充本地知识检索能力说明
- 新增 RAG 切片检索 V2 任务规划文档（docs/tasks/rag-chunking-retrieval-v2.md，688 行），收窄存储与邻接方案
- DEVELOPMENT.md 与 tasks/ 目录移动至 docs/ 下，CONTRIBUTING.md 更新文档链接

### CI

- 恢复 CI 全量触发：本轮尝试了 CI paths 过滤（仅生产代码变更触发）后因 pull_request 事件匹配不稳定而撤销，恢复为所有 PR 和 push 均触发

## [v0.6.0] - 2026-06-26

### Added

- 新增 Markdown 知识 FTS5 全文检索，替换旧世界观和上下文模块，支持中文 ngram 分词、标题感知分段、slug 去重和 chunk_id 防碰撞
- 新增 `*.example.md` 模板文件跳过知识扫描机制，避免示例文档污染检索结果
- 新增 RSS 抓取开始水位记录，以阻塞同一 URL 的竞态更新，减少重复推送

### Changed

- 扩大知识搜索候选集：单文件命中后不再垄断结果，保留其他文件的相关片段
- 整理 `.env.example`，删除死变量和旧兼容变量，精简注释
- 清理 `.gitignore` 失效规则并补全部署产物忽略

### Fixed

- 修复 RSS 历史回写条目不再误触发补发，仅保留真实 incident 更新入队
- 修复 RSS 延迟更新入队判断逻辑
- 修复知识索引评论问题
- 修复群聊默认 mention 策略缺失
- 补齐私有配置忽略规则

### Documentation

- AGENTS.md 新增分支与 PR 策略章节
- 修正和收紧 opencode-go 任务描述

## [v0.5.0] - 2026-06-25

### Added

- 新增独立 `qq-maid-llm` library crate，承载模型调用协议、Provider 路由、fallback、SSE、usage、健康观测和 OpenAI Web Search 协议；不依赖 `qq-maid-core`
- 新增 `qq-maid-llm/README.md`，说明 crate 职责边界、统一入口、模块结构、配置边界和调用链
- 依赖方向固定为 `qq-maid-gateway-rs → qq-maid-core → qq-maid-llm → qq-maid-common`，禁止反向依赖

### Changed

- 将 OpenAI Responses、Chat Completions、DeepSeek、模型候选链、SSE 解析、LLM metrics、健康观测和 `/查` Web Search 协议从 `qq-maid-core` 迁入 `qq-maid-llm`
- OpenAI Chat Completions 改为基于 `reqwest` 的自研实现，支持流式与非流式、`[DONE]`、usage 与 cached token 提取、空流补非流、401/403/429/timeout/5xx 与非标准错误正文分类
- DeepSeek 复用 OpenAI 兼容 Chat Completions adapter，不再维护独立 SDK 封装，只保留 base URL、认证和模型规则差异
- 聊天 Responses 与 `/查` Web Search 共用同一套 SSE frame 解析（`qq-maid-llm/src/sse.rs`），消除重复实现
- `qq-maid-core` 改为通过 `LlmService::chat` / `web_search` / `web_search_stream` / `upstream_status` 公开入口调用模型；core 侧 `provider/` 仅作为兼容 re-export 入口
- `LlmError` 收敛为仅模型调用相关错误（Provider 配置、网络/超时、HTTP 上游、SSE/协议、空回复、候选全部失败）；core 在 LLM 调用边界完成错误转换，保持用户侧错误格式和文案兼容
- 普通聊天、标题、Todo、记忆、Compact、翻译和 `/查` 全部切换到新的 `qq-maid-llm` 调用链，Prompt 组装位置、模型选择、fallback 顺序、用户侧回复内容和格式、健康检查数据保持不变
- README 文档导航补充 `qq-maid-llm/README.md` 链接
- AGENTS.md 同步更新项目定位、依赖方向、代码修改边界和常用验证规则

### Removed

- 从 `Cargo.toml`、`Cargo.lock` 和源码中完全移除 `rig-core` 依赖
- 删除 `qq-maid-core` 中已迁移的 Provider 实现（`provider/deepseek.rs`、`provider/openai/`、`provider/status.rs`、`util/sse.rs` 等）
- 删除 `/查` 中已迁移的模型协议代码

### Fixed

- 修复聊天 Responses 与 `/查` 各自维护一套 SSE frame 解析导致的重复实现问题

## [v0.4.5] - 2026-06-25

### Changed

- 调整普通聊天消息排列：稳定 system prompt 前置，请求时间上下文移到稳定前缀之后、记忆与会话上下文之前，避免每轮请求时间变化破坏 Prompt Cache 前缀命中
- `TokenUsage` 新增 `cached_input_tokens` 字段，采集上游 `input_tokens_details.cached_tokens` / `prompt_tokens_details.cached_tokens`，字段缺失时记 `None`，不伪造 0
- `provider/openai/chat.rs` 在流式与非流式补全中从原始响应提取缓存命中 token 数，rig-core `Usage` 已提供该字段时优先复用
- `provider/openai/extract.rs::extract_response_usage` 解析 `input_tokens_details.cached_tokens`
- `LlmChatService` 在每次请求完成后输出脱敏结构化日志 `llm request completed`，记录 provider、model、purpose、input/output/cached tokens、fallback_used

### Fixed

- 修复请求时间上下文注入头部导致稳定 prompt 前缀每轮被顶位、Prompt Cache 无法命中稳定前缀的问题

## [v0.4.4] - 2026-06-25

### Added

- 新增可配置上下文模块：支持通过 `CONTEXT_MODULES_FILE` 指向 TOML 索引文件，按关键词动态注入普通聊天的 system prompt 模块
- 模块支持 `always` 常驻、`keywords` 关键词命中、`priority` 优先级排序、`max_dynamic_modules` / `max_total_chars` 预算控制、路径逃逸校验
- 新增 `context_modules.example.toml` 公开模板，新增 `context/deploy.example.md` / `context/ops.example.md` 示例模块

### Changed

- 重构 `runtime::prompt` 模块，拆分为 `prompt_files`（固定 prompt 加载）、`member_mapping`（成员编号映射）、`context_modules`（可配置上下文模块）三个子模块
- 世界观不再强绑定 `innerworld_lore.md`，改为通过 `WORLD_FILE` 环境变量独立指定

## [v0.4.3] - 2026-06-25

### Fixed

- 修复 QQ 群事件兼容期内同时下发 `group_openid` 和旧字段 `group_id` 时，serde alias 导致 duplicate field 报错的问题：改为手动合并两个字段，优先使用 `group_openid`

## [v0.4.2] - 2026-06-25

### Changed

- 优化 `/火车` 时刻表渲染输出：始发站到达列显示 `--`、停留显示「始发」；终到站出发列显示 `--`、停留显示「终到」；中间站保持原来到发时间和停留分钟数逻辑；仅一站的异常数据保留原始到发、停留显示 `--`，不同时硬标为始发和终到
- `/火车` 时刻表新增展示 12306 `trainDetail` 可选字段：完整车次（`stationTrainCodeAll`）、担当客运段（`jiaolu_corporation_code`）、车型信息（`jiaolu_train_style`）、配属（`jiaolu_dept_train`）；字段缺失或为空时省略对应行，不推测、不补造
- 时刻表底部提示调整为「当前展示为当日计划时刻，不含实时正晚点、余票及临时停运信息，请以铁路12306或车站公告为准。」
- 空到发时间占位由 `--:--` 改为 `--`

## [v0.4.1] - 2026-06-25

### Fixed

- 修复 `/help` 首页"常用功能"区块在 QQ 纯文本渲染下命令被反引号吞掉不显示的问题：将待办、RSS、天气、查询、记忆、会话、状态等条目从 `bullet` 改为 `push_pair`，纯文本侧去掉反引号，Markdown 侧保留行内代码标记

## [v0.4.0] - 2026-06-24

### ⚠️ 破坏性变更：从双进程合并为单进程

**从此版本开始，项目不再分别运行 Gateway 和 LLM 两个独立程序，改为运行一个统一程序 `qq-maid-bot`。**

如果你从旧版（≤ v0.3.4）升级，请务必按以下步骤操作，否则会出现端口冲突导致新程序无法启动：

**升级步骤：**

```bash
# 1. 先停掉旧版两个独立进程
kill $(ps aux | grep qq-maid-gateway-rs | grep -v grep | awk '{print $2}')
kill $(ps aux | grep qq-maid-llm | grep -v grep | awk '{print $2}')
# 或如果旧版有 llmctl.sh / gatewayctl.sh：
# bash llmctl.sh stop
# bash gatewayctl.sh stop

# 2. 确认旧进程已全部退出
ps aux | grep -E 'qq-maid-(gateway-rs|llm)' | grep -v grep

# 3. 清理旧的独立二进制和脚本（新版部署时会自动清理）
rm -f runtime/qq-maid-gateway-rs runtime/qq-maid-llm
rm -f runtime/llmctl.sh runtime/gatewayctl.sh

# 4. 按新版方式构建和部署
bash scripts/deploy-local.sh
```

**为什么必须这样做：**
- 旧版 Gateway 和 LLM 各占一个端口独立运行；
- 新版 `qq-maid-bot` 单进程内部串联两模块，复用相同端口；
- 如果旧进程未退出，新版启动时端口被占用，会直接失败。

### Changed

- 将 Gateway (`qq-maid-gateway-rs`) 和 Core（原 `qq-maid-llm`）合并为一个统一可执行程序 `qq-maid-bot`
- `qq-maid-llm` crate 重命名为 `qq-maid-core`，定位更清晰
- Gateway 和 Core 改为 library crate，仅作为根包的依赖使用
- 统一入口 `src/main.rs`：先启动 Core HTTP，等待 `/healthz` 就绪后再启动 Gateway
- 所有部署、启停、诊断脚本切换为只操作 `qq-maid-bot` 统一程序
- `botctl.sh` 替代旧的 `llmctl.sh` / `gatewayctl.sh`
- Gateway 仍通过本机 HTTP 调用 Core `/v1/respond`，业务边界不变

### Fixed

- 修复 `todo_reminder` 测试在非上海时区（如 CI 的 UTC）下跨天失败：测试改用上海时区取当前日期，与调度器内部 `next_retry_after` 时区语义一致

### Removed

- 移除 `qq-maid-llm/src/main.rs`、`qq-maid-gateway-rs/src/main.rs` 两个独立入口
- 移除 `scripts/llmctl.sh`、`scripts/gatewayctl.sh` 双进程控制脚本
- 清理 Makefile 中旧的 `run-llm`、`run-gateway` 等双服务目标

## [v0.3.4] - 2026-06-24

### Added
- `/todo add` 支持火车行程识别与 12306 时刻校验：
  - LLM 解析车次/站点/日期后调用 12306 接口校验站点存在性与站序
  - 支持跨日行程时间计算（`dayDifference` 字段）
  - 校验失败或接口异常时不创建 Todo，返回针对性提示
  - 支持纯数字车次（如 1461）与字母前缀车次（G/D/C/Z/T/K）
  - `NotTrain` 识别结果自动回退普通 Todo 解析
- Todo 每日提醒后台调度（`runtime/todo_reminder.rs`）：
  - 按 Asia/Shanghai 每日定时扫描 pending 个人待办
  - 只推送可验证 private target 的 owner，群待办不主动推送
  - 同 owner 多 private scope 合并，冲突 target 脱敏跳过
  - 每日每 owner 仅发送一次，当天失败会自动补跑重试
- 通用 GatewayPushClient（`runtime/push.rs`），供 RSS 与 Todo 提醒共用
- 12306 接口 `stationNo`/`dayDifference` 兼容兜底，缺失时不阻塞 `/火车` 查询
- `day_difference_reliable` 字段：Provider 层可为展示兜底，校验层拒绝不可信跨日数据
- 火车 Todo 回看候选 `startDay` 逻辑：首个候选报站点错误时继续查询，避免误杀跨日行程
- 配置项 `TODO_DAILY_REMINDER_ENABLED` / `TODO_DAILY_REMINDER_TIME`

### Changed
- README 补充 Todo 每日提醒能力说明
- `.env.example` 补充推送入口说明

## [v0.3.3] - 2026-06-21

### Added
- Web 控制台路由安全头中间件（X-Content-Type-Options、X-Frame-Options、CSP）
- `scripts/validate-release-runtime.sh` — 待发布 runtime 目录完整性校验脚本
- `scripts/botctl.sh` — 统一启停控制脚本（start/stop/restart/status/logs/health/console）
- 群消息 `group_message_mode` 配置项，支持 `off` / `command` / `mention` / `active`
- OpenAI 兼容 GLM provider 支持
- `qq-maid-gateway-rs` 推送到 `qq-maid-llm` 内部 `/internal/push` 端点，支持群 @ 消息透传

### Changed
- `deploy.sh` 增加构建产物校验和缺失检测
- `Makefile install` target 正确拷贝 release 二进制和控制脚本到 `runtime/`
- `scripts/llmctl.sh` 增加 `LINES` 日志行数配置支持与 `console` 子命令
- `runtime/.env.example` 更新配置项
- `qq-maid-llm` Web 控制台功能：配置开关、CORS 管理、Markdown 渲染接口

### Fixed
- 移除 unused import 警告
- `cargo fmt` 格式化修复
- 修复 console 测试断言与 HTML 标题不一致

## [v0.3.2] - 2026-06-20

### Added
- 运行时目录校验与发布包校验脚本
- OpenAI `chat_only` 模式、可选群聊处理与运维脚本
- `git clone` 后本地部署快速开始指南与 `scripts/deploy-local.sh`
- todo ID 隐藏，统一使用列表序号和关键词匹配
- 命令回复独立的 Markdown 与纯文本双通道
- LLM 上游调用健康状态观测，支持 `/ping check` 诊断
- install 目标将构建产物安装到 `runtime/`

### Changed
- 天气模块拆分为 `types/qweather` 子模块，回复格式改为 Markdown
- 抽取分层帮助模块替换内联 `/help` 回复
- 简化 todo target 解析分支
- `README-dev.md` 重命名为 `DEVELOPMENT.md`

### Fixed
- 群 push 返回的 message_id 写入共享 BotOutboundCache
- 修复群聊作用域与部署控制台回归
- 安全增强、部署加固及脚本一致性优化

## [v0.3.1] - 2026-06-19

### Fixed
- 为 Windows 构建添加 zip 安装步骤

## [v0.3.0] - 2026-06-19

### Added
- 扩展多平台发布矩阵，支持 Linux/Windows/macOS/Android 六平台构建

### Changed
- `/ping` 模块拆分为子模块
- 超长文件拆分与 `markdown_cell` 换行逻辑精简

## [v0.2.0] - 2026-06-18

### Added
- `/ping` 添加摘要/详情双视图和 Markdown 支持
- GitHub Actions 依赖版本升级

## [v0.1.0] - 2026-06-18

首个公开可用版本，从私有仓库迁移而来。

### 项目基础设施
- Rust 双服务架构：Gateway 接收 QQ 事件，LLM 承载业务逻辑
- Cargo Workspace 统一管理 `qq-maid-gateway-rs`、`qq-maid-llm`、`qq-maid-common`
- QQ 官方机器人接入，处理 C2C 私聊和群聊 @ 消息
- SQLite 统一持久化 Session、Todo、Memory、RSS 状态
- OpenAI / DeepSeek 多 Provider 支持，候选链 fallback
- LLM 流式回复、空回复重试、verbose trace
- 服务控制脚本、make 诊断、部署脚本

### 会话管理
- 新建、重命名、恢复、清空会话
- 会话上下文自动压缩与标题自动生成
- Session 存储从 JSON 文件迁移至 SQLite

### 长期记忆
- `/memory` 指令生成草稿，用户确认后写入
- 记忆编辑、删除、查看，按序号管理
- 记忆存储从 JSONL 迁移至 SQLite

### Todo
- 新增、查询、完成、恢复、修改、删除待办
- 按截止时间排序，软删除语义
- `/todo done` 无参列出已完成，`/todo all` 列出全部状态
- Todo 存储从 JSON 文件迁移至 SQLite

### RSS / Atom
- 订阅管理、后台轮询、去重
- 通过 Gateway `/internal/push` 主动推送
- 外语标题/摘要自动翻译为简体中文
- RSS 专用 SQLite 迁移为通用数据库模块

### 查询与命令
- `/查`、`/查询`、`/search` 联网查询
- `/火车` 列车时刻查询
- `/天气` 和风天气查询（含预警、空气质量、生活指数）
- `/翻译` 多语言翻译
- 命令回复支持 Markdown 渲染

### 配置与运维
- 环境变量统一配置入口
- Prompt 目录外部配置与内置回退
- 成员 ID 映射、世界观文件支持
- 日志时间固定上海时区、默认脱敏
- Gateway 运行时诊断与状态快照

### 代码质量
- todo_flow、openai、respond 等模块持续拆分为子模块
- SSE 解析工具、公共 chat primitives 抽取复用
- 移除已废弃的 Python 接入层和旧 Provider
- rig-core 升级至 0.38.2

[v0.16.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.15.2...v0.16.0
[v0.15.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.15.1...v0.15.2
[v0.15.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.15.0...v0.15.1
[v0.15.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.14.2...v0.15.0
[v0.14.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.14.1...v0.14.2
[v0.14.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.14.0...v0.14.1
[v0.14.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.13.2...v0.14.0
[v0.13.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.13.1...v0.13.2
[v0.13.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.13.0...v0.13.1
[v0.13.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.12.1...v0.13.0
[v0.12.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.12.0...v0.12.1
[v0.12.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.11.1...v0.12.0
[v0.11.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.11.0...v0.11.1
[v0.11.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.10.1...v0.11.0
[v0.10.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.10.0...v0.10.1
[v0.10.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.9.1...v0.10.0
[v0.9.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.9.0...v0.9.1
[v0.9.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.8.0...v0.9.0
[v0.8.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.7.0...v0.8.0
[v0.7.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.2...v0.7.0
[v0.6.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.1...v0.6.2
[v0.6.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.6.0...v0.6.1
[v0.6.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.5.0...v0.6.0
[v0.5.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.5...v0.5.0
[v0.4.5]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.4...v0.4.5
[v0.4.4]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.3...v0.4.4
[v0.4.3]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.2...v0.4.3
[v0.4.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.1...v0.4.2
[v0.4.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.4.0...v0.4.1
[v0.4.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.4...v0.4.0
[v0.3.4]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.3...v0.3.4
[v0.3.3]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.2...v0.3.3
[v0.3.2]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.2
[v0.3.1]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.3.0...v0.3.1
[v0.3.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.2.0...v0.3.0
[v0.2.0]: https://github.com/kuliantnt/qq-maid-bot/compare/v0.1.0...v0.2.0
[v0.1.0]: https://github.com/kuliantnt/qq-maid-bot/commits/v0.1.0
