# Changelog

本文档基于 [keep a changelog](https://keepachangelog.com/zh-CN/1.0.0/) 格式，记录每个已发布版本的变更。

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
