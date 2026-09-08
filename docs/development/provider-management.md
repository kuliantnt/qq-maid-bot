# 供应商管理

对应 [#684 阶段 1](https://github.com/kuliantnt/qq-maid-bot/issues/684)。存储优先级、管理员认证与备份规则沿用[配置中心](./config-center.md)。

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

自定义 `openai_compatible` / `openai_responses` 及内置 OpenAI、DeepSeek 从连接自身 Base URL 追加 `/models`，沿用 Header、Scheme 和当前解析出的 Credential。内置 OpenAI 多 Base URL 使用首个非空地址，不跨地址重试；BigModel、Gemini 内置 Adapter 暂返回 `unsupported / adapter_unsupported`，不猜测其他原生接口。协议参考 [OpenAI 模型列表](https://platform.openai.com/docs/api-reference/models/list)和 [DeepSeek 模型列表](https://api-docs.deepseek.com/api/list-models)。

发现沿用自定义 `request_timeout_seconds`，缺省用 `LLM_REQUEST_TIMEOUT_SECONDS`，再限制总等待不超过 15 秒、连接不超过 5 秒。最多两个并发，忙时立即返回 `unknown / busy`；禁止重定向、自动重试与分页跳转。响应上限 1 MiB、最多 10000 个条目；model id 去重排序，仅保留 ID 及 `source=connection_discovery`，不透传额外上游字段、错误正文、URL 或凭证。

| state | models | 含义 |
| --- | --- | --- |
| `success` | 数组（允许空） | 上游返回结构有效的模型列表；不等于真实调用成功或能力已验证 |
| `unsupported` | `null` | Adapter 未实现发现，或端点返回 404 / 405 / 501 |
| `failed` | `null` | 认证、限流、传输、超时、响应超限或其他 HTTP 错误 |
| `unknown` | `null` | 并发繁忙、响应无法识别、无效条目或声明存在未读取分页 |

结果另有 `category`、`http_status` 和 `elapsed_ms`；未启用、未配置凭证、连接不存在或 revision 冲突走现有配置 API 错误。发现失败不影响历史 Route，发现未知模型也保留其 ID。此阶段不推断 Connection → Catalog Provider 映射，不附加 Catalog 元数据，也不读写 `models.json`；#692 的 Catalog 语义保持不变。Web UI、显式元数据关联、本地覆盖写入、disabled Route 校验、在线刷新和多 Key 仍由 #684 后续 PR 实现。
