# QQ 流式消息 / Tool Calling 发送链路排查

## 背景

基于 `docs/analysis/openclaw-qqbot-api-integration.md` 第二十二章的四阶段排查顺序，结合 `docs/design/tool-calling-qq-delivery.md` 对当前项目链路的完整梳理，逐阶段检查现有实现，确认已正确的部分，标记需要改进的风险点。

---

## 第一阶段：确认 QQ API 字段

> 确保 C2C 流式消息的每个平台字段在首帧/中间帧/结束帧中都正确。

### ✅ 已正确

| 检查项 | 实现位置 | 状态 |
|--------|---------|------|
| `stream_msg_id` 首包后保存 | `C2cStreamState` 管理，`send_stream_chunk` 首帧成功后保存，`send_stream_end` 复用 | ✅ |
| `msg_seq` 同一流固定 | `begin_msg_seq_attempt` / `commit_msg_seq_attempt` 管理重试复用 | ✅ |
| `index` 成功后递增 | 每次成功发送后递增 | ✅ |
| `input_state=10` 只发一次 | `send_stream_end` 仅 `Completed` 触发，状态机保证终态不可再写入 | ✅ |
| `content_raw` 累计全文 | 每次发送累计全文，非增量 | ✅ |
| `event_id` / `msg_id` 来源正确 | 入站事件解析后传入 `C2cReplyTarget` | ✅ |

### ⚠️ 待真机确认

- `stream.state=1/10`、`reset=false` 语义：代码和测试已对齐参考实现，建议真机复核
- `extract_c2c_text_stream_id` 兼容性：确认覆盖所有真实 QQ 返回包格式

---

## 第二阶段：确认发送所有权

> 一次 Agent 回复只有一个最终出口，避免重复发送。

### ✅ 已正确

| 检查项 | 实现位置 |
|--------|---------|
| 第一次流式发送 | `stream_respond_c2c` 中 `send_stream_chunk` 首帧 |
| 中间更新 | 同一 `stream_respond_c2c` 循环内 `send_stream_chunk` |
| 最终 `state=10` | `send_stream_end` 仅由 `Completed` 事件触发 |
| 流式失败 fallback | 首帧失败回退到普通发送 |
| 流式成功后不再补发 | `Active` 状态后不走普通发送路径 |
| Tool Calling 走独立路径 | `chat_flow::handle_chat` → `respond_with_tools` → Complete |
| 群聊统一出口 | `consume_respond_stream` 收敛为一次性发送 |

### ⚠️ 需关注

- [ ] 未来 Tool Calling 若也要走流式，需确保 Complete 路径不会和 CoreResponseStream 同时触发发送
- [ ] 确认主动推送（RSS/Todo 提醒）不会和被动回复的 `msg_id` 重叠

---

## 第三阶段：确认回调来源

> partial reply、final reply、tool result、error fallback 不会同时触发 QQ 出站。

### ✅ 已正确

| 检查项 | 实现位置 |
|--------|---------|
| 私聊聊天入口唯一 | `chat_flow::handle_chat` 统一入口 |
| Tool Calling 内部闭环 | `openai_responses_tool_loop` 内部控制完整生命周期 |
| 流式事件唯一消费者 | `stream_respond_c2c` 独占消费 `CoreResponseStream` |
| Dispatcher 串行 | 同 scope 消息串行调度，不会两个 worker 抢发 |

### ⚠️ 风险点

1. **Tool Loop 无可见中间态**：工具调用是「内部完成 → 直接产出最终答案」。若后续加「正在调用工具」提示，需新增 `ToolStarted` / `ToolProgress` 等 Core 事件，并确保不触发额外 QQ 发送出口。

2. **`task_id` 复用 `message_id`**：`tool_context_from_request` 优先取消息 ID。多轮多工具场景不够稳，后续需要独立任务 ID。

---

## 第四阶段：确认文本边界

> 通过日志确认第二次输出是累计文本还是新回复段。

### ✅ 已完成

`stream.rs` 与 `api.rs::post_c2c_stream_message` 已落地 openclaw-qqbot 建议的结构化日志字段：

**每次 QQ 流请求**（`post_c2c_stream_message`）：

- `phase`（`first_chunk` / `middle_chunk` / `final_chunk` / `broken_active_final_chunk` / `completed_flush_final_chunk` / `failed_final_chunk` 等）
- `msg_seq`、`previous_success_msg_seq`、`state` / `stream_state_value`
- `index_present`、`reset_present`、`stream_index`、`reset`、`previous_success_index`、`next_index`
- `has_stream_id`、`content_chars`、`index_committed`、`msg_seq_committed`
- 失败时附带 `http_status`、`qq_code`、`qq_message`、`error`

**流式状态机层**（`stream_respond_c2c_with_sender`）：

- `phase`、`stream_state`、`stream_state_value`、`reset`、`index`
- `has_stream_id_before_send` / `has_stream_id_after_send`
- `content_chars`、`accumulated_chars`、`final_chars`、`chunk_chars`、`sent_len`

**最终结束**：

- `final_owner` 由 `Completed` / `BrokenActive` / `Pending` 分支出口日志体现（`final_chunk` / `broken_active_final_chunk` / `ordinary_fallback_on_completed`）
- `fallback_used` 通过 `ordinary_fallback_on_completed` 日志体现
- `stream_completed` / `normal_send_skipped` 由状态机终态与是否触发普通 fallback 隐含

### ⚠️ 待真机确认

- `prefix_match`（本次 content 是否为上一帧前缀）未单独记录字段；当前设计为增量发送，默认不重叠，真机联调若发现重复内容再补
- `callback_source` 未单独命名；当前由 `phase` 隐含区分 partial / final / fallback

---

## 附加：tool-calling-qq-delivery.md 风险项

1. **群聊无边流式/状态消息能力**：群聊即使 Core 返回 Stream，也被 Gateway 收敛为一次性发送
2. **QQ 字段所有权文档化**：实现已正确，但建议写入 `AGENTS.md` 防止后续越界
3. **`keyboard` 未实现**：后续接入需官方文档

---

## 行动项

### 立即可做

- [x] `stream_respond_c2c_with_sender` 增加结构化调试日志（第四阶段字段）— 已落地，见 `stream.rs`
- [x] `send_stream_end` 增加最终出口日志 — 已落地，见 `stream.rs` 各 final 分支
- [x] `tool_context_from_request` 增加注释说明 `task_id` 复用 `message_id` 的局限 — 已补充注释

### 短期

- [ ] 字段所有权表纳入 `AGENTS.md` 或设计文档
- [x] Tool Loop 成功/失败增加 `tool_loop_used` / `tool_loop_rounds` 日志 — 已落地，见 `tool_loop.rs`

### 中期（需设计）

- [ ] Core 级工具状态事件（`ToolStarted` / `ToolFinished` 等）
- [ ] 独立 `task_id` 生成与生命周期管理
- [ ] 群聊工具状态提示策略
