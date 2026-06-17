# qq-maid-gateway-rs / qq-maid-llm 渐进式结构拆分路线

## 1. 文档目的

本文件记录 `qq-maid-gateway-rs` 与 `qq-maid-llm` 的后续结构治理计划。

目标是逐步降低超长 Rust 文件的维护成本，让人工和 Codex 都能更容易：

* 定位具体业务逻辑；
* 阅读调用链；
* 修改单一功能；
* 补充和维护测试；
* 审查 diff；
* 避免修改无关代码。

本计划只处理内部代码组织，不改变用户可见行为、HTTP 接口、命令语义、持久化格式或现有服务边界。

所有拆分按批次渐进执行。每一批均应独立完成、独立测试、独立审查，禁止一次性完成全部拆分。

---

## 2. 总体原则

### 2.1 只做结构拆分

每个拆分批次默认只允许：

* 移动类型、函数、实现和测试；
* 新增 Rust 子模块；
* 调整 `mod` 和 `use`；
* 调整必要的 re-export；
* 做最小可见性调整；
* 为保持编译通过进行机械性修改。

不得借拆分机会：

* 修改业务语义；
* 重写状态机；
* 修改用户可见回复；
* 修改命令别名；
* 修改 HTTP 路由；
* 修改 JSON 或 serde 字段语义；
* 修改认证策略；
* 修改日志和 diagnostics 语义；
* 修改错误分类；
* 引入新依赖；
* 做无关格式化或重命名；
* 顺手修复其他 Bug。

发现现有 Bug、重复代码或架构问题时，只记录在完成报告中，另开任务处理。

### 2.2 每次只拆一个批次

每个批次都应：

1. 修改前执行对应测试并记录结果；
2. 只处理当前指定模块；
3. 保持公开路径和行为兼容；
4. 修改后执行相同测试；
5. 核对测试数量没有减少；
6. 完成独立 review；
7. 通过后再开始下一批。

不得在一个提交中同时拆 Todo、Weather 和 Gateway。

### 2.3 目录方案允许按仓库实际调整

本文给出的文件结构是推荐方案，不是必须机械照搬。

Codex 阅读真实代码后可以：

* 合并耦合度过高的子模块；
* 调整文件名称；
* 保留部分小 helper 在主模块；
* 将纯类型和纯格式化代码优先移动。

但不得为了满足文件名而：

* 复制 helper；
* 制造循环依赖；
* 创建大量无意义转发函数；
* 扩大公开可见性；
* 引入新的全局状态；
* 将一个清晰的大文件拆成多个高度互相依赖的小文件。

### 2.4 最小可见性

拆分后使用满足调用范围的最小可见性：

1. 优先保持私有；
2. 其次使用 `pub(super)`；
3. 必要时使用 `pub(in path)`；
4. 只有确需跨 crate 内模块调用时才使用 `pub(crate)`；
5. 不新增不必要的 `pub`。

公开路径必须通过 facade 或 re-export 保持兼容。

---

## 3. 当前优先级判断

### P0：respond 测试拆分

目标文件：

```text
qq-maid-llm/src/runtime/respond/tests.rs
```

当前测试文件同时包含：

* LLM provider mock；
* Query executor mock；
* Weather executor mock；
* service builder；
* fixture；
* session 测试；
* chat 测试；
* search 测试；
* weather 测试；
* memory 测试；
* todo 测试；
* pending 测试；
* translation 测试。

这是风险最低、收益最高的第一批。

推荐结构：

```text
runtime/respond/tests/
├── mod.rs
├── support.rs
├── session.rs
├── chat.rs
├── search.rs
├── weather.rs
├── memory.rs
├── todo.rs
├── pending.rs
└── translation.rs
```

其中 `support.rs` 统一放置：

* MockProvider；
* MockQueryExecutor；
* MockWeatherExecutor；
* Failing executor；
* service builder；
* 临时目录创建；
* 通用 fixture；
* 通用断言 helper。

约束：

* 测试名称不变；
* 测试数量不减少；
* 断言语义不变；
* fixture 内容不变；
* mock 默认行为不变；
* 环境变量隔离方式不变；
* 不把测试辅助类型移动到生产代码；
* 不改生产代码以迁就测试拆分。

特别验收：

* 拆分前后记录测试数量；
* 确认所有新文件均由 `mod.rs` 引入；
* 防止测试文件遗漏后出现“测试全绿但实际少跑”的情况。

验证：

```bash
make test-llm
```

执行状态（2026-06-16）：已完成。

实际结果：

* 原 `qq-maid-llm/src/runtime/respond/tests.rs` 已拆分为 `runtime/respond/tests/` 目录；
* `mod.rs` 只负责引入子模块；
* `support.rs` 放置 mock、fixture、service builder、临时目录和通用请求辅助；
* 主题测试按 `session`、`chat`、`search`、`weather`、`memory`、`todo`、`pending`、`translation` 分文件维护；
* 测试辅助只使用 `pub(super)` 暴露给 `tests` 子模块内部，未移动到生产代码；
* 拆分前 `runtime::respond::tests` 为 84 个测试，拆分后仍为 84 个测试；
* 拆分前后 `make test-llm` 均通过，LLM 总测试数为 175。

---

## 4. 第一阶段：Todo 主流程拆分

目标文件：

```text
qq-maid-llm/src/runtime/respond/todo_flow.rs
```

拆分后目标目录：

```text
qq-maid-llm/src/runtime/respond/todo_flow/
```

当前文件同时承担：

* `/todo` 命令解析和别名归一化；
* 命令分派；
* Todo 目标 ID 和列表编号解析；
* 最近列表快照解析；
* 已完成时间条件查询；
* Todo 草稿 LLM 解析；
* 编辑 patch 解析；
* 时间补全；
* 所有 Todo 回复格式化；
* 新增、编辑、完成、恢复、删除、批量删除等 pending 状态处理；
* 候选选择和确认、取消、修订流程。

推荐结构：

```text
runtime/respond/todo_flow/
├── mod.rs
├── command.rs
├── target.rs
├── draft.rs
├── completed_query.rs
├── format.rs
└── pending.rs
```

### mod.rs

保留：

* `RustRespondService::handle_todo_flow`
* 高层命令分派；
* 各子模块声明；
* 对必要内部类型的最小 re-export。

主入口只负责：

1. 识别 Todo 命令；
2. 创建 owner；
3. 按 action 分派；
4. 将结果包装为 RespondResponse。

不要在第一批强行继续缩短主入口；主分派保持集中通常更容易阅读。

### command.rs

负责：

* `/todo` 指令解析；
* 中文别名；
* action 归一化；
* 参数切分；
* 帮助用法判断。

不得改变：

* 命令名；
* 别名；
* 参数优先级；
* 未知命令回复。

### target.rs

负责：

* `TodoTarget`；
* `TodoNumberResolution`；
* ID 解析；
* 最近列表编号解析；
* 已完成列表编号解析；
* 候选匹配；
* 缺失编号处理；
* target label。

不得改变：

* ID 精确匹配优先级；
* 列表快照语义；
* 编号越界回复；
* 搜索结果顺序；
* 候选数量限制。

### draft.rs

负责：

* `TodoEditPatch`；
* Todo 新增草稿解析；
* 编辑草稿解析；
* JSON 提取；
* 字段清洗；
* 时间补全；
* draft merge。

不得改变：

* LLM purpose；
* JSON 字段含义；
* 时间推断规则；
* 修订语义；
* 缺失字段处理；
* 用户输入作为 fallback 的逻辑。

### completed_query.rs

负责：

* `CompletedTodoTimeQuery`；
* 已完成时间条件解析；
* 最近已完成查询快照；
* TTL 校验；
* 批量删除条件复用。

不得改变：

* 北京时间语义；
* 截止日期是否含当天；
* TTL；
* 查询快照失效条件。

### format.rs

负责全部 Todo 用户可见文案，例如：

* 待办列表；
* 已完成列表；
* 搜索结果；
* 新增确认；
* 编辑确认；
* 删除确认；
* 批量删除确认；
* 候选选择；
* 无匹配；
* 编号越界；
* 操作成功和失败摘要。

第一轮只移动函数，不改任何文案、标点、排序和截断规则。

### pending.rs

负责：

* `handle_pending_todo_operation`；
* 新增确认；
* 编辑确认；
* 完成确认；
* 恢复确认；
* 删除确认；
* 批量删除；
* 候选选择；
* 确认、取消和修订分类。

这是 Todo 拆分风险最高的部分，应最后移动。

必须保持：

* 确认优先级；
* 取消优先级；
* 修订优先级；
* owner 校验；
* pending created_at；
* pending 清理时机；
* 候选选择流程；
* 删除仍为软删除；
* 失败时 pending 是否保留的现有语义。

回归测试重点：

* 新增待确认；
* 新增时修订草稿；
* 编辑确认和修订；
* 完成；
* 恢复；
* 删除确认；
* 批量删除；
* 多候选选择；
* 最近列表编号；
* 已完成列表编号；
* 已完成时间条件；
* TTL；
* owner 不匹配；
* 软删除。

验证：

```bash
make test-llm
```

执行状态（2026-06-16）：已完成。

实际结果：

* 原 `qq-maid-llm/src/runtime/respond/todo_flow.rs` 已拆分为 `runtime/respond/todo_flow/` 目录；
* `mod.rs` 保留 `RustRespondService::handle_todo_flow`、高层命令分派、TodoStore 调用、LLM 草稿解析调用、列表编号完成/恢复和批量删除准备逻辑；
* `command.rs` 放置 `/todo` 指令解析和子命令别名归一化；
* `target.rs` 放置 `TodoTarget`、`TodoNumberResolution`、最近列表快照、编号解析、目标 ID 清洗、编辑参数解析和候选编号解析；
* `completed_query.rs` 放置已完成时间条件解析和最近已完成查询 TTL 校验；
* `draft.rs` 放置 `TodoEditPatch`、LLM JSON 草稿解析、编辑补丁解析、字段校验和时间补全；
* `format.rs` 放置全部 Todo 用户可见回复格式化，未修改原文案、标点、排序和截断规则；
* `pending.rs` 放置 `handle_pending_todo_operation`，pending 类型定义仍保留在 `runtime/pending/`，总分发仍保留在 `runtime/respond/pending.rs`；
* `handle_pending_todo_operation` 的可见性调整为 `pub(in crate::runtime::respond)`，只允许 respond 模块内部的 pending 总分发调用，未扩大到 crate 级公开；
* 其他子模块 helper 使用 `pub(super)`，只在 `todo_flow` 目录内部共享；
* 未修改 `storage/todo.rs`，未改变 Todo 持久化格式、软删除语义、owner/session 边界、命令别名或用户可见行为；
* 新增了模块级中文注释，说明指令解析、草稿解析、格式化、目标解析和 pending 状态机的边界约束；
* 拆分后 `make test-llm` 通过，LLM 总测试数仍为 175。

---

## 5. 第二阶段：Weather 模块拆分

目标文件：

```text
qq-maid-llm/src/runtime/weather/mod.rs
```

当前文件同时包含：

* Weather 领域类型；
* supplement 状态；
* WeatherExecutor trait；
* QWeather executor；
* Geo lookup；
* URL 构建；
* v7 请求；
* v1 请求；
* DTO；
* 状态码解析；
* 城市偏好；
* 地名消歧；
* 天气转换；
* 预警转换；
* 空气质量转换；
* 生活指数转换；
* 模块测试。

推荐结构：

```text
runtime/weather/
├── mod.rs
├── types.rs
├── executor.rs
└── qweather/
    ├── mod.rs
    ├── client.rs
    ├── dto.rs
    ├── convert.rs
    ├── location.rs
    └── util.rs
```

### mod.rs

作为 facade，保持以下路径继续可用：

```rust
runtime::weather::WeatherExecutor
runtime::weather::DynWeatherExecutor
runtime::weather::WeatherRequest
runtime::weather::WeatherOutcome
runtime::weather::build_weather_executor
```

其他当前被外部模块使用的公开类型也必须继续通过 re-export 可用。

### types.rs

移动：

* WeatherRequest；
* WeatherLocation；
* CurrentWeather；
* DailyWeather；
* WeatherAlert；
* AirQualitySummary；
* WeatherLifeIndex；
* WeatherSupplementStatus；
* WeatherSupplement；
* WeatherOutcome。

不得更改：

* 字段含义；
* derive；
* PartialEq 行为；
* diagnostics 状态字符串；
* supplement 的状态语义。

### executor.rs

移动：

* WeatherExecutor；
* DynWeatherExecutor；
* build_weather_executor；
* 默认预报天数。

构造行为保持不变。

### qweather/client.rs

移动：

* QWeatherExecutor；
* HTTP client；
* Geo lookup 请求；
* 当前天气请求；
  -三天预报请求；
  -预警请求；
  -空气质量请求；
  -生活指数请求；
  -并发或顺序请求编排。

不得改变认证行为。

当前约束：

* v7 请求继续保持现有 query key 方式；
* v1 增强接口继续保持现有 `X-QW-Api-Key` header；
* 禁止生成 `Authorization: apikey`；
* 本次不引入 JWT；
* 同一个请求不要重复注入两套认证；
* 增强接口失败仍只降级；
* 基础天气失败仍按原错误路径返回。

注意：本次只是保持现状，不讨论是否统一认证方式。

### qweather/dto.rs

移动所有上游响应 DTO：

* Geo response；
* Weather now response；
* Weather daily response；
* Alert response；
* Air quality response；
* Indices response；
* v1 错误响应；
* v7 code 字段。

DTO 只表达上游 JSON，不混入用户回复格式。

### qweather/convert.rs

移动：

* Geo DTO 到 WeatherLocation；
* now DTO 到 CurrentWeather；
* daily DTO 到 DailyWeather；
* alert 转换；
* AQI index 选择；
* pollutants 提取；
* life indices 转换；
* 字符串到数值解析的业务转换。

保持：

* 当地 AQI 标准优先；
* QAQI 回退；
* 第一个可用 index 回退；
* `zeroResult=true` 表示无预警；
* 不自行枚举预警等级；
* 不根据颜色推断严重程度；
* 增强数据单项解析失败不影响基础天气。

### qweather/location.rs

移动：

* 城市查询覆盖；
* 行政区划偏好；
* 地名标准化；
* 候选排序；
* 同名地点消歧；
* 经纬度解析。

保持现有：

* 西湖偏好；
* 萧山偏好；
* 江北偏好；
* lookup override；
* 无城市参数和无结果错误。

### qweather/util.rs

移动：

* 默认 host；
* API 路径常量；
* URL 构建；
* HTTP success 检查；
* v7 code 错误；
* v1 错误解析；
* 通用字段清理；
* 数值解析；
* 安全错误摘要。

不要把所有 helper 都机械塞入 `util.rs`。只放真正跨多个 QWeather 子模块复用的函数。

回归重点：

* 城市查找；
* 同名地点排序；
* v7 query key；
* v1 API key header；
* 当前天气；
  -三天预报；
  -预警有数据；
  -预警 zeroResult；
  -空气质量当地标准；
  -空气质量回退；
  -生活指数；
  -增强接口失败降级；
  -核心接口失败；
  -错误日志脱敏。

验证：

```bash
make test-llm
```

---

## 6. 第三阶段：Gateway Respond 客户端拆分

目标文件：

```text
qq-maid-gateway-rs/src/respond.rs
```

该文件目前同时包含：

* Respond 请求和响应类型；
* RespondClient；
* HTTP 请求；
* Content-Type 判断；
* SSE frame 缓冲；
* SSE frame 解析；
* stream task；
* QQ 可见错误文案；
* C2C 内容构建；
* 单元测试。

推荐结构：

```text
qq-maid-gateway-rs/src/respond/
├── mod.rs
├── types.rs
├── client.rs
├── sse.rs
├── content.rs
└── error_text.rs
```

保持现有导出路径：

```rust
crate::respond::RespondClient
crate::respond::RespondRequest
crate::respond::RespondResponse
crate::respond::RespondTransport
crate::respond::RespondStream
crate::respond::RespondStreamEvent
crate::respond::build_respond_content
crate::respond::respond_error_to_qq_text
crate::respond::respond_not_ok_to_qq_text
crate::respond::respond_response_error_summary
```

实际公开符号以仓库使用情况为准。

### types.rs

移动：

* RespondRequest；
* RespondResponse；
* RespondTransport；
* RespondStream；
* RespondStreamEvent；
* RespondError。

### client.rs

移动：

* RespondClient；
* respond_c2c；
* HTTP 状态处理；
* Content-Type 分支；
* stream task 启动。

### sse.rs

移动：

* ParsedSseEvent；
* `is_stream_response`；
* frame delimiter 查找；
* frame 提取；
* SSE 多行 data 解析；
* `[DONE]`；
* final event 解析；
* stream 无 final 错误处理。

保持：

* CRLF；
* LF；
* 多行 data；
* comment 行；
* 未知 event；
* `[DONE]`；
* invalid UTF-8；
* invalid JSON；
* stream 提前结束；
* final 只发送一次；
* 不发送空 QQ 消息。

### content.rs

移动：

* C2CMessage 到 RespondRequest；
* reply、当前消息和上下文的内容拼装；
* `build_respond_content`。

不得改变发往 `/v1/respond` 的字符串格式。

### error_text.rs

移动：

* RespondErrorInfo；
* HTTP 错误安全摘要；
* response `ok: false` 错误提取；
* QQ 用户可见 fallback 文案。

不得输出：

* token；
* Authorization；
* secret；
  -完整 URL 查询参数；
* openid 原文；
  -后端敏感响应正文。

验证：

```bash
make test-gateway
```

---

## 7. 第四阶段：Gateway 主循环拆分

目标文件：

```text
qq-maid-gateway-rs/src/gateway/mod.rs
```

Gateway 已经有：

```text
dedupe.rs
event.rs
logging.rs
ping.rs
```

因此主循环拆分应尽量复用现有模块，而不是重新制造一套重复职责。

推荐结构：

```text
gateway/
├── mod.rs
├── connection.rs
├── session.rs
├── dispatch.rs
├── c2c.rs
├── streaming.rs
├── signal.rs
├── dedupe.rs
├── event.rs
├── logging.rs
└── ping.rs
```

### mod.rs

保留：

```rust
pub async fn run(config: AppConfig)
```

负责：

* 构造共享依赖；
* 持有 Gateway 生命周期；
* 重连循环；
* ResumeState 生命周期；
* reply cache 生命周期。

不要把共享状态拆成全局变量或单例。

### session.rs

移动：

* opcode 常量；
* ResumeState；
* identify payload；
* resume payload；
* heartbeat payload；
* identify/resume 发送。

保持：

* session_id；
* seq；
* INVALID_SESSION 可恢复语义；
  -不可恢复时清空 resume；
* RECONNECT 行为；
* READY 和 RESUMED 记录。

### connection.rs

移动：

* gateway URL 获取；
* WebSocket 建连；
* HELLO；
* heartbeat interval；
* read/write loop；
* Text/Binary/Ping/Pong/Close；
* envelope 反序列化。

### dispatch.rs

移动：

* `handle_envelope`；
* READY；
* RESUMED；
* RECONNECT；
* INVALID_SESSION；
* HEARTBEAT_ACK；
* C2C event 分派；
  -未知 opcode。

### signal.rs

移动：

* MessageCache；
* reply content 回填；
* signal resolution。

保持其为短时内存缓存，不引入持久化或业务语义。

### c2c.rs

移动：

* C2C 主编排；
* reply signal；
  -去重；
* `/ping` 本地处理；
* respond 请求；
* QQ 发送；
  -错误 fallback；
  -日志和 runtime 统计。

Gateway 仍然只负责接入和转发，不得在这里承载：

* LLM prompt；
* session 业务；
* Todo；
* Memory；
  -天气；
  -长期上下文。

### streaming.rs

移动 Gateway 侧对 RespondStream 的消费：

* delta 缓冲；
* final response；
* partial fallback；
  -无 final 处理；
  -避免空消息；
  -最终 QQ 消息发送。

注意不要与 `respond/sse.rs` 重复：

* `respond/sse.rs` 负责解析 HTTP SSE；
* `gateway/streaming.rs` 负责消费已经解析出的 stream event，并决定 QQ 回发行为。

共享状态要求：

* 优先通过参数传入引用；
* 不新增静态可变状态；
* 不为减少函数参数引入复杂 Context 大对象，除非现有参数已经严重失控；
* 如新增 context，只允许包装现有依赖，不改变生命周期和所有权语义。

回归重点：

* READY；
* RESUMED；
* INVALID_SESSION；
* RECONNECT；
* heartbeat；
* C2C 解析；
  -消息去重；
* `/ping` 不访问 LLM；
* Respond JSON；
* Respond SSE；
* Respond HTTP 错误；
* `ok: false`；
* partial fallback；
  -无 final；
* reply content 回填；
  -脱敏日志。

验证：

```bash
make test-gateway
```

---

## 8. 第五阶段：Todo 存储模块拆分

目标文件：

```text
qq-maid-llm/src/storage/todo.rs
```

优先级低于 Todo 流程，因为当前存储模块虽然较长，但公开接口集中且业务相对稳定。

推荐结构：

```text
storage/todo/
├── mod.rs
├── types.rs
├── store.rs
├── normalize.rs
├── sort.rs
├── search.rs
└── time.rs
```

### types.rs

移动：

* TodoStatus；
* TodoTimePrecision；
* TodoItem；
* TodoItemDraft；
* TodoOwner；
  -批量操作结果；
* TodoError；
* TodoFile。

serde 字段、默认值、flatten 扩展字段必须保持不变。

### store.rs

移动：

* TodoStore；
  -文件加载；
  -文件保存；
* create；
* list；
* edit；
* complete；
* restore；
* cancel；
  -批量操作。

不得改变：

* 每 owner 一个文件；
  -自增 ID；
* Mutex 行为；
  -原子写入方式；
  -软删除；
  -时间字段更新；
  -未知 JSON 字段保留；
  -历史数据兼容。

### normalize.rs

移动：

* draft normalize；
* item normalize；
* file normalize；
  -清理字符串；
  -兼容旧数据。

### sort.rs

移动：

* pending 排序；
* completed 排序；
* created_at 排序；
* compare helper。

### search.rs

移动：

* search score；
* ID 精确和前缀匹配；
  -全文匹配；
  -候选限制。

### time.rs

移动：

* due_date；
* due_at；
* precision；
  -完成日期筛选；
  -北京时间转换。

拆分时不得进行数据迁移。

验证：

```bash
make test-llm
```

并使用现有历史 Todo fixture 做兼容读取测试。

---

## 9. 暂缓模块

以下模块可以继续观察，暂时不主动拆：

```text
qq-maid-llm/src/runtime/query/mod.rs
qq-maid-llm/src/runtime/respond/llm_service.rs
```

只有满足以下任一条件时再建立拆分任务：

* 文件继续明显增长；
  -新增功能频繁修改多个不相关区域；
* Codex 定位逻辑困难；
  -测试和实现高度混杂；
  -出现多人修改冲突；
  -单次 review 无法清晰判断影响范围。

可能的后续结构：

```text
runtime/query/
├── mod.rs
├── types.rs
├── executor.rs
├── openai.rs
├── stream.rs
├── extract.rs
└── payload.rs
```

```text
runtime/respond/llm_service/
├── mod.rs
├── messages.rs
├── format.rs
└── trace.rs
```

在没有实际维护痛点前，不要仅因为行数而拆。

---

## 10. 公共接口和兼容要求

### qq-maid-llm

* HTTP 层公开路由保持仓库现状；
  -不新增、删除或修改现有路由；
  -不恢复 `/query`、`/memory`、`/v1/chat` 等旧入口；
* `runtime/respond.rs` 继续作为 `/v1/respond` facade；
* Todo、Memory、Session 的 serde 数据结构不变；
* `runtime/pending` 类型语义不变；
* pending 分发顺序不变；
  -用户可见文案不变。

### qq-maid-gateway-rs

-仍然只作为 QQ Gateway 接入层；
-不承载 LLM、Memory、Todo、Session 等业务逻辑；

* `pub async fn run(config: AppConfig)` 保持可用；
* RespondClient 公开路径保持兼容；
* `/ping` 继续在 Gateway 本地处理；
* QQ 发送和 fallback 语义不变。

### 安全要求

所有拆分批次继续保证：

-不打印 token；
-不打印 secret；
-不打印 Authorization；
-不打印 API KEY；
-不打印真实 `.env`；
-不打印 openid 原文；
-错误正文按现有脱敏规则处理；
-不因移动代码而降低日志脱敏范围。

---

## 11. 测试策略

### 每批执行前

记录：

-当前分支和 commit；
-对应测试命令；
-测试数量；
-测试结果；
-是否存在原有失败。

### 每批执行后

执行相同命令，并核对：

-编译通过；
-测试数量未减少；
-测试结果不退化；
-公开路径仍可编译；
-没有未使用 re-export；
-没有不必要的可见性扩大；
-没有重复 helper；
-没有遗漏模块声明。

### 最低验证命令

LLM 模块：

```bash
make test-llm
```

Gateway 模块：

```bash
make test-gateway
```

跨模块或阶段完成：

```bash
make test
```

如果仓库约定包含格式化和 Clippy，也应执行对应命令。

不得伪造未执行的测试结果。

---

## 12. 每批 Codex 完成报告要求

Codex 每次完成拆分后必须说明：

1. 本次处理的批次；
2. 原文件修改前后的职责；
3. 新增和移动了哪些文件；
4. 每个主要类型或函数移动到哪里；
5. 哪些公开路径通过 re-export 保持兼容；
6. 哪些可见性发生变化；
7. 为什么这些可见性是最小必要范围；
8. 是否调整了推荐目录结构；
9. 如有调整，原因是什么；
10. 拆分前执行了哪些测试；
11. 拆分后执行了哪些测试；
12. 测试数量是否一致；
13. 是否存在未执行的命令；
14. 是否发现现有 Bug 或后续优化点；
15. 是否确认本次没有业务语义变化。

---

## 13. 推荐执行顺序

按照以下顺序逐步实施：

1. `runtime/respond/tests.rs`
2. `runtime/respond/todo_flow.rs`
3. `runtime/weather/mod.rs`
4. `qq-maid-gateway-rs/src/respond.rs`
5. `qq-maid-gateway-rs/src/gateway/mod.rs`
6. `storage/todo.rs`
7. 根据实际维护痛点再判断 query 和 llm_service

其中：

* 第 1 批风险最低，应先做；
* 第 2、3 批收益最大；
* Gateway 两批必须分开；
* Storage Todo 等上层 Todo 流程稳定后再拆；
  -后续模块不设强制完成时间。

---

## 14. 停止条件

任一批次出现以下情况时，应停止继续拆分并先 review：

-需要修改业务行为才能编译；
-需要改变公开接口；
-需要修改 serde 持久化结构；
-出现大量 `pub(crate)`；
-出现循环依赖；
-需要复制已有 helper；
-测试数量减少；
-原有测试无法解释地失效；
-diff 大量包含无关格式化；
-新文件之间高度互相引用；
-主模块虽然变短，但理解成本增加。

此时应回退到更小的拆分范围，而不是继续扩大修改。

---

## 15. 总结

本项目确实存在需要拆分的超长文件，但目标不是追求更小的文件，而是让职责和修改边界更清晰。

近期只需要优先完成：

1. Respond 测试拆分（已完成，2026-06-16）；
2. Todo 主流程拆分；
3. Weather 模块拆分。

前三批完成后，应观察实际维护收益，再决定是否继续拆 Gateway 和存储模块。

宁可保留一个职责单一的长文件，也不要制造十个互相穿透的小文件。
