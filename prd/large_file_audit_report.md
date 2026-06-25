# 大文件审计报告：1000 行以上 Rust 文件

> 本报告基于当前仓库真实代码统计，用于指导后续渐进式结构整理。
> 统计口径：`find ... -name "*.rs" -not -path "*/target/*"`，排除 `target/` 与 `.git/`。
> 行数构成为人工按 `mod tests {` 边界拆分的“生产 / 测试”估算，仅供参考。

## 背景

项目此前已陆续完成若干结构整理，Gateway 侧已拆出 `ping/`、`event.rs`、`protocol.rs`、`logging.rs`、`outbound.rs`、`push.rs`、`dedupe.rs` 等子模块。随着功能持续迭代，部分文件重新增长到 1000 行以上。本报告重新梳理当前所有 1000 行以上的 Rust 文件，作为后续低风险整理的依据，不要求一次性重写。

## 当前清单

全仓库 Rust 文件约 46419 行，1000 行以上的文件共 11 个：

| # | 文件 | 总行数 | 生产 | 测试 | 所属 crate |
|---|------|-------:|-----:|-----:|-----------|
| 1 | `qq-maid-core/src/storage/todo.rs` | 1919 | 1303 | 616 | core |
| 2 | `qq-maid-core/src/runtime/respond/tests/support.rs` | 1573 | 1573 | 0 | core（测试支持） |
| 3 | `qq-maid-core/src/runtime/respond/tests/todo.rs` | 1459 | 1459 | 0 | core（测试） |
| 4 | `qq-maid-core/src/http/routes.rs` | 1343 | 560 | 783 | core |
| 5 | `qq-maid-core/src/storage/rss.rs` | 1312 | 869 | 443 | core |
| 6 | `qq-maid-core/src/runtime/train/mod.rs` | 1235 | 679 | 556 | core |
| 7 | `qq-maid-gateway-rs/src/respond.rs` | 1214 | 690 | 524 | gateway |
| 8 | `qq-maid-core/src/storage/session.rs` | 1153 | 935 | 218 | core |
| 9 | `qq-maid-common/src/time_context.rs` | 1144 | 826 | 318 | common |
| 10 | `qq-maid-core/src/runtime/respond/llm_service.rs` | 1130 | 907 | 223 | core |
| 11 | `qq-maid-gateway-rs/src/gateway/mod.rs` | 1121 | 916 | 205 | gateway |

说明：

- 第 2、3 项为纯测试文件，行数大但拆分价值低，单独在“不建议拆分”一节说明。
- 第 4 项 `http/routes.rs` 总行数 1343，但生产代码仅 560 行，测试占 783 行；实际生产规模并不超标。
- 真正“生产代码超过 800 行”的文件为：`storage/todo.rs`（1303）、`storage/session.rs`（935）、`runtime/respond/llm_service.rs`（907）、`gateway/mod.rs`（916）、`storage/rss.rs`（869）、`time_context.rs`（826）。

## 逐文件分析

### 1. `qq-maid-core/src/storage/todo.rs` — 1919 行

职责：Todo 持久化层，SQLite schema、迁移、CRUD、排序、归一化、搜索评分。

内部结构：

- schema 与迁移常量（`TODO_SCHEMA_V1`、`TODO_MIGRATIONS`）
- 类型定义：`TodoStatus`、`TodoTimePrecision`、`TodoItem`、`TodoItemDraft`、`TodoOwner`、`TodoStore` 及若干 bulk 操作结果类型、提醒相关类型、`TodoError`
- `impl TodoStore`（约 210–763 行）：CRUD、批量完成 / 取消 / 恢复、提醒候选查询
- `impl TodoError` / `TodoStatus` / `TodoTimePrecision` / `TodoItemDraft`
- 查询函数：`query_items`、`query_items_by_status`、`query_items_by_owner_scopes_and_status`、`get_by_id_*`
- 行映射与错误辅助：`todo_item_from_row`、`from_sql_text_error`、`collect_rows`
- 时间推断与展示：`enrich_draft_time_from_text`、`infer_due_date_from_text`、`display_*`
- 归一化：`normalize_draft`
- 搜索与排序：`search_score`、`sort_todos`、`sort_completed_todos*`、`compare_todo_order`、`completed_todo_sort_key`、`todo_due_sort_key` 等
- ID 工具：`clean_todo_id`、`parse_todo_db_id`、`compare_todo_id`
- `mod tests`（616 行）

潜在拆分边界：

- 排序与比较逻辑（`sort_todos` 系列、`compare_todo_order`、`*_sort_key`、`compare_todo_id`）是纯函数，不依赖连接或事务，可提取为 `storage/todo/sort.rs`。
- 时间推断与展示（`enrich_draft_time_from_text`、`infer_due_date_from_text`、`display_*`）与 `qq-maid-common/src/time_context.rs` 已有同名函数存在重复，需先确认调用关系再决定是否复用，不要盲目合并。
- `TodoStore` 本身的 CRUD 与 bulk 操作内聚度高，不建议强行拆分。

风险：todo 软删除语义、批量操作状态转换、持久化格式必须保持兼容。排序逻辑被 `runtime/respond/todo_flow/` 多处依赖，移动后需保持函数签名不变。

### 2. `qq-maid-core/src/runtime/respond/tests/support.rs` — 1573 行

职责：respond 测试公共支持，Mock provider、Mock query/weather/train executor、mock LLM 回复生成函数。

内部结构：

- `MockProvider`、`MockWeatherExecutor`、`MockTrainExecutor` 及其 `impl LlmProvider` / `impl QueryExecutor` / `impl WeatherExecutor` / `impl TrainExecutor`
- mock 数据构造：`mock_weather_alerts`、`mock_air_quality`、`mock_life_indices`
- 失败型 executor：`FailingWeatherExecutor`、`FailingQueryExecutor`、`FailingTrainExecutor`
- `SeededTrainExecutor`
- mock 回复生成：`mock_revision_input`、`mock_current_memory_content`、`mock_todo_draft`、`mock_todo_parse_reply`、`mock_todo_revise_reply`、`mock_train_todo_parse_reply` 等
- `test_service_with_provider_base_title_query_weather_and_models` 测试服务构造
- `completed_at_for` 时间辅助

结论：纯测试基础设施，行数大是因为覆盖了 todo / memory / train / weather 多个流程的 mock。不建议拆分，除非 mock 回复生成函数进一步按流程分文件，但收益有限。如需控制增长，可考虑按流程拆为 `tests/support/todo_mock.rs`、`tests/support/train_mock.rs`，但优先级最低。

### 3. `qq-maid-core/src/runtime/respond/tests/todo.rs` — 1459 行

职责：todo 命令流程的集成测试用例集合，覆盖 add / list / done / delete / undo / pending edit / bulk delete / completed time query 等场景。

结论：纯测试用例，行数大是业务覆盖面广的正常结果。不建议拆分。如未来继续增长，可按子主题拆为 `tests/todo_add.rs`、`tests/todo_pending.rs`、`tests/todo_delete.rs`，但当前无必要。

### 4. `qq-maid-core/src/http/routes.rs` — 1343 行（生产 560）

职责：Core HTTP 路由层，`/healthz`、console、markdown render、`/v1/respond`、streaming。

内部结构：

- `AppState`、`HttpRespondRequest`、`HttpDiagnosticAction`、`RespondRequest` 转换
- `build_router` 路由装配
- `healthz`、`console_index`、`markdown_render` + preflight、`render_markdown_html`
- console 安全与 CORS：`with_console_security`、`with_console_cors`、`with_console_preflight_cors`、`allowed_console_origin`
- `respond` handler（约 320–450 行）：请求构造、上游调用、流式 / 非流式分支
- `run_upstream_check`、`accepts_streaming`、`stream_response`、`warn_respond_error`、`error_respond_error`
- `mod tests`（783 行）

潜在拆分边界：

- console 与 markdown render 相关 handler（`console_index`、`markdown_render`、`render_markdown_html`、CORS 辅助）与 `/v1/respond` 业务路由职责不同，可考虑提取为 `http/console.rs` 或 `http/markdown.rs`。
- `respond` handler 本身较长，但与 streaming、error 警告逻辑紧密耦合，不建议强行拆。

风险：console 安全、CORS 白名单、streaming 协议必须保持现有行为。生产代码仅 560 行，拆分收益有限，优先级中低。

### 5. `qq-maid-core/src/storage/rss.rs` — 1312 行

职责：RSS 订阅与推送状态持久化，schema、迁移、订阅 CRUD、item state upsert、legacy 兼容。

内部结构：

- 多个 schema 与迁移常量（含 legacy seen items、pending rebaseline 迁移）
- 类型：`RssTargetType`、`RssTarget`、`RssSubscription`、`RssFeedItem`、`RssPendingItem`、`FeedItemChange`、`RssStore`、`RssStoreError`
- `impl RssStore`（约 251–567 行）：订阅 CRUD、item 写入、state 更新、seen 维护
- `impl RssStoreError`
- 内部辅助：`insert_items_unlocked`、`upsert_item_state_unlocked`、`update_item_seen_unlocked`、`trim_seen_unlocked`
- legacy 判定：`is_pushed_legacy_state`、`is_same_entry_time`
- 行映射与工具：`subscription_from_row`、`collect_rows`、`clean_required`、`clean_optional`、`truncate_text`
- `mod tests`（443 行）

潜在拆分边界：

- legacy 兼容逻辑（`LEGACY_REVISION_PREFIX`、`is_pushed_legacy_state`、`is_same_entry_time` 及相关迁移）可考虑集中到 `storage/rss/legacy.rs`，但需确认迁移常量是否被外部引用。
- 其余 CRUD 与 item state 逻辑内聚度高，不建议拆。

风险：RSS 持久化格式、legacy 迁移、seen 状态语义必须保持兼容。优先级中低。

### 6. `qq-maid-core/src/runtime/train/mod.rs` — 1235 行

职责：12306 列车时刻查询，API 请求、响应反序列化、字段解析、站点查找、行程校验。

内部结构：

- 常量 `TRAIN_QUERY_URL`
- 请求 / 响应类型：`TrainScheduleRequest`、`TrainStop`、`TrainSchedule`、`TrainApiResponse`、`TrainApiData`、`TrainApiDetail`、`TrainApiStop`、`StringishValue`
- `TrainExecutor` trait 与 `Train12306Executor` 实现
- 反序列化辅助：`deserialize_optional_stringish`
- `impl TrainApiResponse`（约 239–323 行）：从 API 响应构造 `TrainSchedule`
- 字段解析辅助：`required_train_field`、`trim_optional_field`、`parse_station_no_field`、`parse_day_difference_field`、`normalize_train_time`、`parse_u32_field`、`parse_i32_field`
- 错误构造：`map_train_request_error`、`train_status_error`、`no_schedule_error`
- 站点工具：`normalize_station_name`、`find_stop_by_name`
- Todo 草稿与行程校验：`TrainTodoDraft`、`TrainTripValidation`、`TrainTripError`、`validate_train_trip`、`ensure_reliable_day_difference`、`compose_train_datetime`、`parse_train_time`
- `mod tests`（556 行）

潜在拆分边界：

- 12306 API 响应反序列化与字段解析（`TrainApiResponse` 及 `required_train_field` 等辅助）可提取为 `runtime/train/api.rs`。
- 行程校验逻辑（`TrainTripValidation`、`TrainTripError`、`validate_train_trip`、`ensure_reliable_day_difference`、`compose_train_datetime`）可提取为 `runtime/train/validation.rs`。
- `TrainTodoDraft` 与 todo 流程耦合，需确认 `runtime/respond/todo_flow/train_todo.rs` 的引用关系后再决定是否移动。

风险：12306 字段解析近期有改动（见近期 commit），拆分需基于最新字段语义。优先级中。

### 7. `qq-maid-gateway-rs/src/respond.rs` — 1214 行

职责：Gateway 侧 Respond HTTP 客户端，请求构造、SSE 流式解析、错误转换、回复内容构建。

内部结构：

- 类型：`RespondClient`、`RespondRequest`、`UpstreamCheckRequest`、`RespondResponse`、`RespondTransport`、`RespondStreamEvent`、`RespondStream`、`RespondErrorInfo`、`RespondError`
- `impl RespondError` 及错误转 QQ 文本函数：`respond_error_to_qq_text`、`respond_response_error_to_qq_text`、`respond_not_ok_to_qq_text`、`respond_response_error_summary`
- `impl RespondClient`（约 152–427 行）：`request`、`request_stream`、upstream check
- SSE 解析：`ParsedSseEvent`、`is_stream_response`、`take_sse_frame`、`find_sse_delimiter`、`parse_sse_frame`、`send_stream_final_error`
- 请求内容构建：`build_respond_content`、`build_group_respond_content`、`build_respond_content_parts`、`append_attachment_notes`
- error_info 辅助：`error_info_from_*`、`respond_error_info_to_qq_text`、`upstream_check_error_summary`、`sanitize_visible_error_message`、`truncate_visible_message`
- `mod tests`（524 行）

潜在拆分边界：

- SSE 解析（`ParsedSseEvent`、`take_sse_frame`、`find_sse_delimiter`、`parse_sse_frame`、`send_stream_final_error`、`is_stream_response`）是纯协议逻辑，可提取为 `respond/sse.rs`。
- 错误转换与脱敏（`respond_error_*`、`error_info_from_*`、`sanitize_visible_error_message`、`truncate_visible_message`）可提取为 `respond/error_text.rs`。
- `RespondClient` 请求执行与请求内容构建内聚度较高，保留在 `respond.rs`。

风险：SSE 事件顺序、错误文案、脱敏逻辑必须保持现有行为。`respond.rs` 是 gateway 与 core 的边界，公开类型（`RespondRequest`、`RespondResponse`、`RespondStreamEvent` 等）被 `gateway/mod.rs` 直接使用，拆分时不要扩大可见性。优先级中。

### 8. `qq-maid-core/src/storage/session.rs` — 1153 行

职责：Session 持久化层，schema、迁移、session CRUD、消息读写、敏感词脱敏、active session 管理。

内部结构：

- schema 与迁移常量、`DEFAULT_SESSION_TITLE`
- 敏感词正则 `SENSITIVE_PATTERNS`
- 类型：`SessionRecord`、`SessionMessage`、`LastTodoQuery`、`LastMemoryQuery`、`SessionMeta`、`SessionStore`、`StoredSessionRow`、`SessionError`
- `impl SessionStore`（约 239–450 行）：load / save / active session / 消息替换
- `impl SessionRecord` / `SessionMeta` / `SessionError` / `StoredSessionRow`
- 归一化与 ID 构造：`normalize_session`、`infer_scope`、`initial_session_state`、`normalize_session_title`、`build_session_id`、`safe_id_part`
- 事务辅助：`load_session_unlocked`、`load_messages_unlocked`、`upsert_session_tx`、`replace_messages_tx`、`set_active_session_id_*`
- JSON 编解码：`encode_json`、`encode_optional_json`、`decode_json`、`decode_optional_json`、`collect_sql_rows`
- 时间与脱敏：`now_iso_cn`、`redact_sensitive_text`
- `mod tests`（218 行）

潜在拆分边界：

- 敏感词脱敏（`SENSITIVE_PATTERNS`、`redact_sensitive_text`）可考虑独立，但需确认是否被 `storage/memory.rs` 或其他模块复用。
- JSON 编解码辅助（`encode_json` 系列）是通用工具，但与 `SessionError` 绑定，强行提取会引入泛型或新错误类型，收益有限。
- session CRUD 与事务逻辑内聚度高，不建议拆。

风险：session 作用域、active session 切换、持久化格式、敏感词脱敏必须保持兼容。优先级低。

### 9. `qq-maid-common/src/time_context.rs` — 1144 行

职责：全局时间上下文与日期 / 时间 / 时区工具，`Asia/Shanghai` 时区、日期表达式解析、多种格式化函数。

内部结构：

- 常量与正则：`REQUEST_TIMEZONE`、`SHANGHAI_OFFSET_SECONDS`、多个日期表达式正则
- 类型：`RequestTimeContext`、`ResolvedTimeExpression`、`DateBoundaryKind`、`DateBoundaryExpression`、`DateInferencePrecision`、`InferredDateExpression`
- 上下文构造：`request_time_context`、`now_iso_cn`
- 日期推断：`infer_due_date_from_text`、`is_valid_ymd_date`、`has_valid_ymd_date_prefix`
- 时间戳与格式化：`local_date_from_timestamp`、`format_local_date_*`、`format_local_time_for_display`、`format_diagnostic_*`、`format_duration_for_display`、`format_unix_seconds_*`
- RSS / Todo 时间格式化：`format_rss_time_for_display`、`format_todo_time_for_display`
- `impl RequestTimeContext` / `ResolvedTimeExpression` / `InferredDateExpression`
- 日期边界解析：`parse_date_boundary_expression`、`parse_boundary_date`、`parse_ymd_date`
- 底层解析辅助：`parse_naive_local_datetime`、`parse_rss_datetime`、`parse_small_number`、`parse_weekday`
- 时区与格式化内部函数：`shanghai_offset`、`format_date`、`format_datetime_with_offset` 等
- `mod tests`（318 行）

结论：`AGENTS.md` 明确要求“日期、时间和时区语义优先复用 `qq-maid-common/src/time_context.rs`”，该文件是共享基础工具，内聚度高。虽然 826 行生产代码偏大，但拆分可能破坏单一入口语义。如需整理，仅可考虑将“诊断时间格式化”与“业务时间格式化”分组，但不要引入跨模块依赖。优先级低，谨慎处理。

### 10. `qq-maid-core/src/runtime/respond/llm_service.rs` — 1130 行

职责：LLM 聊天服务核心，消息构建、provider 调用、回复截断、Markdown 剥离、trace 日志。

内部结构：

- 常量：`MAX_REPLY_LENGTH`、`MAX_MEMORY_DRAFT_LENGTH`、`CHAT_TRACE_TEXT_LIMIT`
- `ChatService` trait、`RespondOutput`、`LlmChatService` 及 `impl ChatService`
- trace 函数族：`trace_chat_messages`、`trace_chat_raw_reply`、`trace_chat_final_reply`、`trace_chat_*_enabled`、`format_chat_message_trace`、`chat_role_name`、`respond_purpose_name`、`trace_session_id`、`trace_scope_key`、`trace_text`
- 消息构建：`build_respond_messages`、`with_request_time_context`、`llm_time_context_prompt`、`has_request_time_context`、`build_chat_messages`、`build_memory_draft_messages`、`build_legacy_memory_draft_messages`、`build_memory_create_messages`、`build_memory_revise_messages`、`build_todo_parse_messages`、`build_compact_messages`
- 回复处理：`truncate_reply`、`strip_markdown_for_chat`、`flatten_markdown_tables`、`strip_markdown_line`、`strip_*_prefix`、`strip_inline_markdown`、`count_run`、`find_backtick_run`、`parse_markdown_link`、`find_closing_*`、`strip_emphasis_markers`、`protect_inline_literal`、`restore_inline_literals`
- `clean_memory_draft_output`、`response_from_output`、`format_chat_reply_channels`
- `mod tests`（223 行）

潜在拆分边界：

- Markdown 剥离逻辑（`strip_markdown_for_chat` 及其全部辅助函数 `flatten_markdown_tables`、`strip_markdown_line`、`strip_*_prefix`、`strip_inline_markdown`、`count_run`、`find_backtick_run`、`parse_markdown_link`、`strip_emphasis_markers`、`protect_inline_literal` 等）是自包含的纯文本处理，约 300+ 行，可提取为 `runtime/respond/markdown_strip.rs`。这是本文件最明确的自然边界。
- trace 函数族可考虑分组，但与 `RespondRequest` 紧密耦合，且涉及脱敏开关，不建议强行移动。
- 消息构建函数族按 purpose 分散，但共享 `RespondRequest` 上下文，内聚度尚可。

风险：Markdown 剥离策略直接影响用户可见回复格式，必须保持现有行为；`strip_markdown_for_chat` 被 `chat_flow` 等调用，移动后签名不变。优先级中高（Markdown 剥离提取收益明确、风险低）。

### 11. `qq-maid-gateway-rs/src/gateway/mod.rs` — 1121 行

职责：Gateway 主循环与消息处理编排，WebSocket 连接生命周期、C2C / Group 消息处理、流式响应消费、群聊冷却。

内部结构（已拆出 `dedupe` / `event` / `logging` / `protocol` / `outbound` / `ping` / `push` 子模块）：

- 模块声明、常量（`DEDUPE_TTL`、`GROUP_COOLDOWN`、`GROUP_USER_COOLDOWN`）、`BotOutboundCache`、`GroupCooldowns`
- `resolve_signals`
- `run`（约 129–208 行）：主循环、建连、事件分发顶层编排
- `handle_group_message`（约 209–320 行）
- `send_group_respond_response`（约 321–371 行）
- `collect_streaming_final_response`（约 372–395 行）
- `handle_c2c_message`（约 396–607 行）
- `render_local_ping_reply`（约 608–619 行）
- `handle_streaming_respond_response`（约 620–771 行）
- 群聊判定 helper：`should_ignore_group_message`、`should_process_group_message`、`is_group_command`、`contains_bot_mention`、`is_reply_to_bot`、`group_user_key`
- `build_streaming_buffered_response`
- 日志：`log_c2c_message_received`、`log_group_message_received`
- `mod tests`（205 行）

潜在拆分边界：

- 群聊判定 helper（`should_ignore_group_message`、`should_process_group_message`、`is_group_command`、`contains_bot_mention`、`is_reply_to_bot`、`group_user_key`）是纯判定逻辑，可提取为 `gateway/group_filter.rs` 或并入现有 `event.rs`。
- 流式响应消费（`handle_streaming_respond_response`、`collect_streaming_final_response`、`build_streaming_buffered_response`）可提取为 `gateway/streaming.rs`，C2C 与 Group 共用。
- `handle_c2c_message` 与 `handle_group_message` 是各自流程的编排，保留在 `mod.rs` 或提取为 `gateway/c2c.rs` / `gateway/group.rs`，但需先确认两者是否真有可复用的流式 / 发送逻辑，不要为复用创建万能 handler。
- 日志函数可并入现有 `logging.rs`。

风险：C2C 与 Group 在引用方式、目标标识、权限上存在差异，必须显式保留。群聊冷却、消息去重、`/ping` 本地处理、发送状态记录语义不变。`run` 主循环的连接生命周期与事件分发顶层编排应保留在 `mod.rs`。优先级中高（群聊判定 helper 与流式消费提取收益明确）。

## 拆分优先级建议

按“收益明确、风险低”排序：

1. **高**：`llm_service.rs` 提取 Markdown 剥离逻辑为独立模块（自包含纯文本处理，约 300+ 行，调用方少）。
2. **高**：`gateway/mod.rs` 提取群聊判定 helper 与流式响应消费（纯判定 / 共用消费逻辑，边界清晰）。
3. **中**：`respond.rs` 提取 SSE 解析与错误转换（纯协议 / 纯文本，但涉及 gateway-core 边界类型）。
4. **中**：`runtime/train/mod.rs` 提取 API 反序列化与行程校验（近期有字段改动，需基于最新语义）。
5. **中低**：`storage/todo.rs` 提取排序逻辑（纯函数，但被 todo_flow 多处依赖）。
6. **中低**：`http/routes.rs` 提取 console / markdown render（生产代码仅 560 行，收益有限）。
7. **低**：`storage/rss.rs` legacy 兼容集中、`storage/session.rs` 脱敏独立（内聚度高，风险高于收益）。
8. **低 / 谨慎**：`time_context.rs`（共享基础工具，`AGENTS.md` 明确要求复用，不建议轻易拆分）。

## 不建议拆分的文件

- `runtime/respond/tests/support.rs`（1573 行）：纯测试基础设施，行数大是覆盖面广的正常结果。
- `runtime/respond/tests/todo.rs`（1459 行）：纯测试用例，按业务场景覆盖，拆分无实际收益。
- `http/routes.rs` 的测试部分（783 行）：保持与生产代码同文件便于维护。

## 整理原则

- 优先做代码移动与可见性调整，不改变行为。
- 不为减少行数增加无意义转发层。
- 不大量扩大 `pub` / `pub(crate)` 可见性。
- 保持 C2C / Group、todo 软删除、session 作用域、记忆确认、RSS legacy 兼容等业务差异。
- 每次真实 QQ 发送只记录一次结果，发送状态记录语义不变。
- 移动代码时同步移动相关注释；修改逻辑时同步更新注释。
- 不修改 `qq-maid-core` 服务实现的业务语义、公开 HTTP 接口、QQ 命令触发条件、配置项名称。

## 验证要求

每次拆分后至少运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release --all-features
```

改动涉及 Gateway 启动、QQ 事件或发送逻辑时，需本地启动验证。

## 提交建议

- 纯代码移动使用 `refactor` 类型，单独提交。
- 涉及结构调整或去重使用独立提交，不与纯移动混入同一 commit。
- commit message 使用简洁中文，例如：
  - `refactor: 提取 llm_service Markdown 剥离逻辑`
  - `refactor: 提取 gateway 群聊判定与流式消费`

## 备注

- 本报告行数与结构基于当前 master 真实代码，后续若 codex 或其他改动导致结构变化，需重新统计。
- `storage/todo.rs` 的 `infer_due_date_from_text` 与 `qq-maid-common/src/time_context.rs` 同名函数存在重复，整理前需先确认调用关系，避免盲目合并破坏 todo 时间推断语义。
