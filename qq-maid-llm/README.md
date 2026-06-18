# qq-maid-llm — Rust LLM 服务

`qq-maid-llm/` 是 QQ Maid Bot 的业务服务，负责 `/v1/respond`、普通聊天、联网查询、天气、翻译、会话、长期记忆、Todo、RSS / Atom 订阅和模型 provider 调用。

QQ 平台事件解析、白名单、`/ping` 本地诊断和消息回发不在本模块处理，相关实现见 [qq-maid-gateway-rs/README.md](../qq-maid-gateway-rs/README.md)。运行目录、私有配置、部署产物和数据文件说明见 [runtime/README.md](../runtime/README.md)。

## 当前范围

- HTTP 层只公开 `GET /healthz` 和 `POST /v1/respond`。
- 普通聊天、查询、天气、翻译、会话命令、长期记忆、Todo 和 RSS 指令都通过 `/v1/respond` 内部分发。
- Session、Todo、长期记忆、RSS / Atom 订阅和 RSS 去重状态统一写入 `APP_DB_FILE` 指向的 SQLite。
- 长期记忆只能通过明确 `/memory`、`/记忆`、`/记` 指令生成草稿，用户确认后写入；普通聊天不会自动写长期记忆。
- RSS 后台轮询由本服务调度，主动推送通过 gateway 的本机 `/internal/push` 出口发送。
- OpenAI / DeepSeek 以及模型候选链 fallback 逻辑保留在 provider 层。

旧 HTTP `/query`、HTTP `/memory`、`/v1/chat` 等入口不再公开，也不要重新引入 Python LLM、Python 查询、Python 记忆或 Python fallback 入口。

## 模块结构

```text
qq-maid-llm/src/
├── app/                 # 启动、dotenv 加载、日志、组件装配
├── config.rs            # 环境变量解析和默认值
├── http/                # /healthz 与 /v1/respond facade
├── provider/            # OpenAI、DeepSeek、fallback 和 provider-facing 类型
├── runtime/
│   ├── respond/         # /v1/respond 分发和 chat/search/weather/todo/memory/session flow
│   ├── pending/         # 待确认操作类型和确认分类
│   ├── query/           # 联网查询执行器
│   ├── rss/             # RSS / Atom 拉取、存储封装、调度和 push client
│   ├── prompt/          # prompt 文件、世界观和成员映射加载
│   ├── session.rs       # 会话领域逻辑
│   ├── memory.rs        # 长期记忆领域逻辑
│   ├── todo.rs          # Todo 领域逻辑
│   └── weather/         # 天气执行器
├── storage/             # SQLite、migration、session/memory/todo/rss 持久化
└── util/                # SSE、指标，以及 time_context 兼容 re-export
```

`runtime/respond.rs` 是 `/v1/respond` facade 后的统一业务入口；具体 flow 在 `runtime/respond/` 下维护。通用日期、时间和时区语义优先复用 `qq-maid-common/src/time_context.rs`；LLM 内部可继续通过 `util/time_context.rs` 兼容入口引用，不要在具体命令里重复实现。

## HTTP 接口

### `GET /healthz`

返回服务健康状态、当前 provider、模型和流式配置，供 gateway `/ping`、控制脚本和诊断脚本探测。

### `POST /v1/respond`

gateway 调用的唯一业务入口。请求体只接受当前 schema，未知字段会被拒绝：

```json
{
  "scope_key": "qq:c2c:xxx",
  "content": "/todo add 明天下午检查日志",
  "platform": "qq_official",
  "event_type": "C2C_MESSAGE_CREATE",
  "user_id": "optional",
  "group_id": "optional",
  "guild_id": "optional",
  "channel_id": "optional",
  "message_id": "optional",
  "timestamp": "optional"
}
```

默认返回 JSON `RespondResponse`。当 `LLM_SEND_MODE=streaming` 且调用方接受 SSE 时，目前只有 `/查` 联网查询路径会返回流式事件；普通聊天仍按现有 flow 处理。

## 指令能力

- 会话：`/new`、`/rename`、`/resume`、`/clear`、`/state`、`/compact`、`/help`。`/list` 仅作为 deprecated 兼容别名保留，推荐 `/resume` 或 `/恢复`。
- 记忆：`/memory`、`/memory 内容`、`/memory show 1`、`/memory edit 1 新内容`、`/memory delete 1`；中文别名 `/记忆`、`/记`。
- 待办：`/todo`、`/todo add 内容`、`/todo done 1`、`/todo undo 1`、`/todo edit 1 新内容`、`/todo delete 1`；中文别名 `/待办`、`/任务`。
- RSS：`/rss`、`/rss add RSS地址 [名称]`、`/rss delete 1`、`/rss test RSS地址`；中文别名 `/订阅`。
- 查询：`/查 关键词`、`/查询 关键词`、`/search 关键词`。
- 天气：`/天气杭州`、`/天气 杭州`、`/杭州天气`、`/weather 杭州`。
- 翻译：`/翻译 文本`、`/翻译日语 文本`、`/翻译成英语 文本`。

待确认操作会优先于普通命令处理；若修改 pending、确认分类或 todo / memory 的状态转换，优先复用 `runtime/pending/` 和 `runtime/respond/pending.rs` 中的既有逻辑。

## 配置和数据

本模块从进程环境变量读取配置。`make run-llm` 和部署控制脚本都会以 `runtime/` 为工作目录启动服务，因此默认会依次尝试加载：

```text
runtime/config/.env
runtime/.env
```

`dotenvy` 默认不覆盖已存在的环境变量：进程环境变量优先，先加载的 dotenv 文件会保留同名变量，后续文件只补充缺失项。

常用配置项：

- `LLM_PROVIDER`：`openai` / `deepseek` / `auto`。
- `LLM_MODEL`、`TITLE_MODEL`、`TODO_MODEL`、`MEMORY_MODEL`、`COMPACT_MODEL`、`TRANSLATION_MODEL`：主模型和内部任务模型；`TRANSLATION_MODEL` 供 `/翻译` 和 RSS 推送前翻译共用，留空时沿用主模型。
- `OPENAI_API_KEY`、`OPENAI_BASE_URL`、`OPENAI_BASE_URLS`、`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL`：provider 配置；`OPENAI_BASE_URLS` 为逗号分隔时取第一个非空地址，优先于 `OPENAI_BASE_URL`。
- `LLM_SERVER_HOST`、`LLM_SERVER_PORT`、`LLM_REQUEST_TIMEOUT_SECONDS`、`LLM_SEND_MODE`：HTTP 服务和请求行为。
- `APP_DB_FILE`：统一 SQLite 文件。
- `PROMPT_DIR`、`WORLD_FILE`、`MEMBER_ID_MAPPING_FILE`：prompt、世界观和成员映射。
- `RSS_*`：RSS / Atom 轮询、去重、推送和 SSRF 防护相关配置。
- `OPENAI_SEARCH_MODEL`：联网查询模型配置。`SEARCH_CONTEXT_SIZE`、`SEARCH_MAX_RESULTS` 目前只保留在模板中，当前 `/查` flow 仍使用查询模块默认值。
- `QWEATHER_API_KEY`、`QWEATHER_API_HOST`、`QWEATHER_GEO_HOST`：天气配置；当前 `QWEATHER_API_KEY` 为必需项。

模型配置支持单模型和候选链两种写法：

```env
LLM_MODEL=openai:gpt-5.4-mini
LLM_MODEL=openai:gpt-5.4-mini,deepseek:deepseek-chat
```

候选项按从左到右的优先级执行，候选项前后的空格会被忽略。当前 provider 层会在超时、HTTP/网络错误、provider 协议错误、上游空响应等可恢复失败后尝试下一个候选；配置错误、本地请求构造错误和业务参数错误不会继续请求其他模型。OpenAI provider 内部仍先完成 Responses API、空流补非流和 Chat Completions 兼容 fallback，只有该候选整体失败后才进入下一个候选。当前普通聊天、标题、Todo/Memory 内部解析、会话压缩、翻译和 RSS 翻译走通用聊天 provider 候选链；RSS 翻译所有候选失败后仍按原业务规则展示原文。`/查` 联网查询仍使用 `OPENAI_SEARCH_MODEL` 和 OpenAI Responses web_search 直连，暂不复用聊天候选链。

完整字段以 [runtime/.env.example](../runtime/.env.example) 为准。真实 `.env`、API Key、Prompt、世界观、成员映射、SQLite、日志和聊天记录不要提交到仓库。

## 运行和检查

从仓库根目录执行：

```bash
cp runtime/.env.example runtime/config/.env
make run-llm
```

只构建 LLM release 二进制：

```bash
make build-llm
```

修改 LLM 代码后至少执行：

```bash
make test-llm
```

`make test-llm` 会同时检查 `qq-maid-common/`，因为 LLM 的时间上下文等工具通过 common 复用。

跨 LLM / gateway、提交前或涉及 workspace 依赖时执行：

```bash
make test
```

只修改本文档时，至少执行 `git diff --check` 并人工核对链接、命令和敏感信息。
