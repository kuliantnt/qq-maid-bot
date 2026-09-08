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

模型目录、本地模型覆盖和多 Key 轮换属于 #684 后续阶段，本次不引入远程目录或新的协议 Adapter。
