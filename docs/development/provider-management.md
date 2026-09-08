# 供应商管理

对应 [#684 阶段 1 / 2](https://github.com/kuliantnt/qq-maid-bot/issues/684)。存储优先级、管理员认证与备份规则沿用[配置中心](./config-center.md)。

## 使用流程

在“模型与供应商”点击“＋ 新建供应商”，在弹窗中选择受信预设或自定义连接，填写不可变 Connection ID、显示名称、协议与 Base URL。创建后可新增或替换 API Key，再填写真实模型 ID 执行服务端测试。模型路线继续填写 `provider:model-id`，保存后重启生效。

所有自定义供应商（包括 OpenCode 预设）使用同一表单，可编辑、启停和删除。删除或停用前须迁移全部模型及搜索路线引用；错误会列出引用位置，服务端不会自动改写路线。手工配置与启动预检遵守相同约束。旧配置未写 `enabled` 时保持启用。新建 Connection 必须使用小写 canonical ID；历史非小写 key 只能在原始 key 精确匹配时编辑或删除，不能改成 canonical 小写，也不能与另一个仅大小写不同的 key 共存。

内置 OpenAI、DeepSeek、BigModel、Gemini 的字段在同一页面展示，继续使用原 runtime/Secret 存储和自动保存；内置定义不可删除，但可停用并显式清除凭证。新增 `provider.<id>.enabled` 对应公开模板中的 `<PROVIDER>_ENABLED`，默认启用，停用同样拒绝所有显式路线引用。内置 OpenAI `auto` 测试采用 Responses，`chat_only` 测试采用 Chat Completions；不自动测试跨协议 fallback。Gemini 在此测试其现有 Chat Adapter，不代表原生搜索已经验证。

## 安全与兼容

- 受信预设在 Core `config/center/provider_presets.rs` 随版本发布，当前包含 OpenCode 三个原有 ID 和已有 Adapter 支持的 OpenAI、DeepSeek、BigModel、Gemini 模板。前端不维护品牌协议或连接安全配置。
- 新连接的 Credential Slot 由服务端随机生成；浏览器不能指定任意环境变量或 Secret 路径。一个新连接对应一个加密 Slot，API 只返回配置状态和 opaque revision。
- 既有 `api_key_env` 保持不变；指向已登记 Secret 字段时继续复用原 CAS 接口，其他历史环境变量只读展示，仍由部署环境管理。旧版 OpenCode 共用 `OPENCODE_API_KEY` 的请求保持兼容。
- 删除连接会归档其专属加密密文：不再有连接引用，重新创建同名连接分配全新 Slot，不复用旧密文。当前不提供归档密文的自动清理，避免跨 TOML 与 SQLite 的非原子删除。共享历史凭证不随连接删除。
- API Key 不写入 Agent 文件、不回传页面、不进入诊断结果；供应商凭证保存成功后清空输入及已保存明文状态。配置保存继续使用独立 revision、原子写入及 `pending_restart`。
- 没有数据库 schema 变更，也不在启动时迁移已有配置。

## 管理 API

现有 `GET /api/v1/console/configuration` 增加 `configuration.providers`，提供受信预设、已注册 Adapter 和自定义连接 Credential 状态；内置字段仍在 `fields`，自定义保存值/运行值仍在 `agent`，不复制平行配置源。

- `PATCH .../configuration/agent`：复用 `set_provider` / `remove_provider`；新增字段 `display_name`、`enabled`。新建时省略 `api_key_env`，既有连接省略时保留原引用，不能重新绑定。
- `PATCH .../configuration/providers/credential`：`id`、`expected_agent_revision`、`expected_revision`、`value`；字符串替换，`null` 清除。两个 revision 均须匹配。
- `POST .../configuration/providers/test`：`id`、`expected_revision`、`model`；自定义连接使用 Agent revision，内置使用 runtime revision。测试的是已保存配置，浏览器不提交目标 URL 或凭证。

测试由 LLM Adapter 层构造最小真实请求，固定文本、无工具、最多 32 个输出 token；可能产生供应商计费。连接超时 5 秒、总超时 15 秒、响应上限 64 KiB、最多两个并发测试，禁止重定向。不回传上游响应正文和原始错误 URL。结果包含真实耗时、HTTP 状态、稳定分类和 `network/authentication/adapter/model_call` 各阶段的 `success/failed/unknown/not_tested`。HTTP 200 或模型列表均不能单独证明模型可用；缺少有效协议输出时明确标记失败或未知。网络成功表示收到了 HTTP 响应，不能单独证明认证或模型可用。

最近测试结果仅保留在当前页面会话，连接或凭证 revision 变化后失效；重新登录/初始化后清空，不代替运行时健康状态。

## Connection 模型发现

`POST /api/v1/console/configuration/providers/models` 接收 `id` 和 `expected_revision`，只读取已保存连接，不写配置或路线。自定义连接使用 Agent revision，内置连接使用 runtime revision。接口仍要求管理员 Session、Origin 与 CSRF；浏览器不能提交 URL、认证或 Credential。响应包含 `connection_id`、请求的 `revision` 和 `discovery`。结果仅属于请求开始时的保存配置，不是运行值，也不缓存或热加载。

模型发现依据 Connection 实际使用的协议 Adapter：内置 OpenAI Responses 使用 `OpenAiResponses`，OpenAI Chat、DeepSeek、Gemini、BigModel 使用 `OpenAiCompatible`。这些内置连接与自定义 `openai_compatible` / `openai_responses` 均从连接自身 Base URL 追加 `/models`，沿用 Header、Scheme 和当前解析出的 Credential。内置 OpenAI 多 Base URL 使用首个非空地址，不跨地址重试；端点返回 404 / 405 / 501 时由 discovery 层返回 `unsupported / endpoint_unsupported`，不按品牌预判支持情况。协议参考 [OpenAI 模型列表](https://platform.openai.com/docs/api-reference/models/list)和 [DeepSeek 模型列表](https://api-docs.deepseek.com/api/list-models)。

发现沿用自定义 `request_timeout_seconds`，缺省用 `LLM_REQUEST_TIMEOUT_SECONDS`，再限制总等待不超过 15 秒、连接不超过 5 秒。最多两个并发，忙时立即返回 `unknown / busy`；禁止重定向、自动重试与分页跳转。响应上限 1 MiB、最多 10000 个条目；model id 去重排序，仅保留 ID 及 `source=connection_discovery`，不透传额外上游字段、错误正文、URL 或凭证。

| state | models | 含义 |
| --- | --- | --- |
| `success` | 数组（允许空） | 上游返回结构有效的模型列表；不等于真实调用成功或能力已验证 |
| `unsupported` | `null` | Adapter 未实现发现，或端点返回 404 / 405 / 501 |
| `failed` | `null` | 认证、限流、传输、超时、响应超限或其他 HTTP 错误 |
| `unknown` | `null` | 并发繁忙、响应无法识别、无效条目或声明存在未读取分页 |

结果另有 `category`、`http_status` 和 `elapsed_ms`；未启用、未配置凭证、连接不存在或 revision 冲突走现有配置 API 错误。发现失败不影响历史 Route，发现未知模型也保留其 ID。Web 页面在该原始发现结果之上补充以下模型管理；发现接口本身继续只返回 ID，不改变模型请求参数或能力。


## Web 模型管理与本地覆盖

供应商卡片的“管理模型”打开模型列表，点击“获取模型”访问该 Connection 自身的 discovery。发现结果优先展示；目录参考条目与本地条目分别标明 `connection discovery`、`catalog`、`local override` 来源。成功空列表、不支持、失败、未知有独立提示，不将目录条目当作 Connection 已暴露的模型。支持 ID / 显示名称搜索、启停与生命周期状态筛选，每次展示最多 50 条，可继续展开。

模型名称与原始模型 ID 分开显示；目录补充要求模型 ID 精确匹配。内置 Connection 使用明确身份映射：`openai → openai`、`deepseek → deepseek`、`gemini → google`、`bigmodel → zhipuai`。自定义 Connection 当前没有 Catalog Provider 关联配置，因此只补充 `models.json` 中以 canonical Connection ID 为 provider 的本地条目，不根据 Base URL、品牌、协议、模型名或同名 Catalog Provider 猜身份。代理返回的私有 ID、斜杠、冒号后缀均保持原样；没有可靠匹配时元数据保持未知，仍可用于 Route。

“模型信息”展示名称、上下文、最大输出、模态、每百万 token 美元参考价格、状态、能力声明以及逐字段来源和目录出处。Advertised 是声明，Verified 始终标记未知；discovery 或 Catalog 都不会自动启用 Tool Calling、改变 LLM 请求参数或业务工具权限。

“本地新增 / 编辑模型”使用普通表单，不要求管理员编辑 JSON。留空或选择继承会省略相应覆盖字段；已有元数据结构仍为 #692 的 `config/models.json`（`MODELS_CONFIG_FILE` 可指定部署路径），没有第二套配置格式。启停同样只更新该文件。配置中心复用 mutation lock、opaque revision、文件安全检查和原子替换，保存后重启生效，无 schema migration。一个本地条目以 provider / id 为键替换，其他条目不变；省略字段继续继承 Catalog。

“一键加入 Route”向选中的现有 `agent.model_routes` 追加 `provider:model-id`，复用 Agent revision/CAS 与保存队列，保留已有候选顺序。不以名称代替 ID、不自动选择或删除其他候选、不改写 Route。ID 中的冒号属于模型 ID 后缀；含逗号的 ID 无法用现有候选链语法无歧义表达，展示仍保留但拒绝添加。

新增管理 API 仍要求管理员 Session、Origin 与 CSRF：

- `POST .../configuration/providers/model-metadata`：接收 `id`，返回 `metadata`，含 models revision、明确 provider 身份、有效元数据、当前本地 overrides、目录出处、`verified_capabilities=unknown` 与 `apply_mode=restart`。不向上游请求。
- `PATCH .../configuration/providers/model-override`：接收 `id`、models 的 `expected_revision`、一个现有 `LocalModelEntry` 格式的 `model`。服务端校验 provider 身份、元数据、文件权限和全部 Route 引用后写入，并复用脱敏审计及 configuration 响应。未知字段、Credential / Adapter / 业务权限字段及伪造 verified 能力均拒绝。

## disabled 模型引用完整性

本地有效 `enabled=false` 的模型不能被任何保存的模型路线或搜索路线引用，包括未使用路线和候选链中的非首项。禁用已引用模型、给路线添加已禁用模型、手工编辑后的 runtime / Secret / Agent 保存与启动 preflight 均拒绝非法配置，首次设置模式也不放宽此约束。保存失败不写入候选文件；已有非法路线可通过显式启用模型或修改路线修复。

目录缺失、发现失败、生命周期 deprecated 或能力 unknown 不构成禁用。不会自动删除 Route、替换模型或用 fallback 隐藏显式禁用错误。本 PR 不包含 Models.dev 在线刷新或 cache、定时刷新、多 Key、Credential failover、新协议 Adapter 或运行时 capability 自动启用。
