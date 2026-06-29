# BUG：/ping 异常摘要漏报 LLM fallback 与 Gateway 重连

> 来源：GitHub Issue #68「bug: /ping 异常摘要漏报 LLM fallback 和 Gateway 重连日志」
>
> 状态：待实现（小 PR，单 crate：`qq-maid-gateway-rs`）
>
> 本文只做问题分析、实施边界和验收定义，不表示能力已经完成。

---

## 一、背景

`/ping` 诊断回复由 `qq-maid-gateway-rs/src/gateway/ping/` 模块负责。回复结构如下：

```text
# 🟢/🟡/🔴 服务运行正常 / 可用，但存在警告 / 异常

> {assessment.summary}        ← 顶部「异常摘要」行

## 核心链路
| 模块 | 状态 | 详情 |       ← 每个链路一行状态
...

## 最近事件
- <事件列表>                  ← 按采集字段聚合的全部异常/恢复事件
...

## 当前消息
...
```

判定与渲染分工：

* 状态采集：`status.rs` 中 `GatewayRuntimeSnapshot` 记录连接 / READY / RESUMED / 重连 / invalid session / 心跳 / 收发尝试。
* 健康评估：`assess.rs::assess_ping_status` 汇总 gateway、QQ 连接、心跳、LLM 服务、LLM 上游、消息收发等行，计算 `overall`、`notes`、`events`。
* 顶部摘要：`assess.rs::summary_text(overall, &notes)`。
* 事件列表：`assess.rs::recent_events`。
* 渲染：`render.rs::render_c2c_ping_reply_at`。

LLM 上游与 Gateway 重连相关来源：

* LLM fallback：`qq-maid-llm` 的 Provider 候选链在 `provider/status.rs` 与 `provider/mod.rs` 中维护 `fallback_used`，经 `qq-maid-core::service::UpstreamStatusSnapshot` 透传到 `healthz.rs::parse_upstream`，落到 `LlmUpstreamSnapshot::Available { fallback_used, .. }`。
* Gateway 重连：`protocol.rs` 在 `OP_RECONNECT` 处 `runtime.record_reconnect()`，`OP_INVALID_SESSION` 处 `record_invalid_session`，恢复由 `READY` / `RESUMED` 触发 `record_ready` / `record_resumed`。

---

## 二、问题分析

### 2.1 顶部摘要只渲染一条 note

`assess.rs::summary_text` 的当前实现：

```rust
fn summary_text(overall: PingSeverity, notes: &[String]) -> String {
    match overall {
        PingSeverity::Normal => {
            "Gateway、QQ WebSocket、LLM 服务和上游模型均正常，未发现未恢复异常。".to_owned()
        }
        PingSeverity::Warning => {
            let detail = notes
                .first()
                .cloned()
                .unwrap_or_else(|| "存在需要关注的状态".to_owned());
            format!("服务当前可用，但需要关注：{detail}。")
        }
        PingSeverity::Error => {
            let detail = notes
                .first()
                .cloned()
                .unwrap_or_else(|| "存在影响服务的异常".to_owned());
            format!("检测到影响服务的异常：{detail}。")
        }
    }
}
```

Warning / Error 两种严重度都只取 `notes.first()`，后续 note 不会出现在顶部摘要行。

### 2.2 notes 的写入顺序固定为「LLM 上游在前、重连在后」

`assess_ping_status` 中 notes 推入顺序：

1. `state_error`（Error，运行时状态读取失败）。
2. LLM 上游分支：
   * `Unverified` → "LLM 上游尚未验证"。
   * `Available { fallback_used: true }` → "LLM 上游最近一次调用发生过降级"。
   * `Error { .. }` → "LLM 上游最近一次调用失败"。
3. `collect_reconnect_note`：
   * 重连已恢复 → "最近发生过重连并已恢复"（Warning）。
   * 重连未恢复 → "最近重连尚未发现恢复记录"（Error）。
4. invalid session 同理。
5. token 即将刷新。
6. QQ 发送失败 / 恢复。
7. LLM respond 失败 / 恢复。

因此当「LLM fallback 已降级 + Gateway 重连已恢复」同时出现时：

```text
overall = Warning
notes   = [
    "LLM 上游最近一次调用发生过降级",
    "最近发生过重连并已恢复",
]
```

顶部摘要渲染为：

```text
> 服务当前可用，但需要关注：LLM 上游最近一次调用发生过降级。
```

"Gateway 重连已恢复"被漏报。反之，当 LLM 上游状态在前面、重连靠后时，重连类条目永远在 LLM 上游之后，只要两者并存，重连摘要必然被丢弃。

更一般地，只要同时有任意两条及以上 note，顶部摘要只保留第一条，其余全部漏报。**Issue #68 点名的 "LLM fallback" 和 "Gateway 重连" 就是这一类最常见的并发条目**。

### 2.3 与 `## 最近事件` 区块不一致

`recent_events` 会把 LLM 降级、重连、invalid session、收发失败等全部列出，且最末有「未发现发送、LLM 或 Session 异常」兜底汇总，因此事件区块信息完整；但顶部摘要因为只取首条 note，与事件区块语义不一致，给运维造成"似乎只有一个问题"的误导。

### 2.4 不属于本次 bug 的范围

* `/ping check` 直接失败覆盖旧 healthz 快照：已在 `mod.rs::build_c2c_ping_reply_with_check_failure` 处理，`tests.rs::ping_check_direct_failure_overrides_stale_healthz_status` 覆盖，不在本次范围。
* `/ping all` 调试详情泄露脱敏：`render.rs` 已脱敏 `unix:`、openid、message_id，`tests.rs::renders_ping_all_with_debug_details_without_secrets` 覆盖，不在本次范围。
* LLM 上游错误摘要脱敏：`healthz.rs::safe_upstream_error_summary` 已做凭据过滤，保持现状。

---

## 三、目标

1. 顶部「异常摘要」行同时呈现同一严重度下的全部 note，不再只保留 `notes.first()`。
2. 至少覆盖 Issue #68 点名的两类并发条目：LLM fallback（已降级）+ Gateway 重连（已恢复 / 未恢复），以及 invalid session、token 即将刷新、QQ 发送失败、LLM respond 失败等其余 note。
3. 保持现有严重度语义不变：`overall` 仍按行严重度聚合，`Normal` 时仍输出固定绿色摘要。
4. 保持脱敏约束不变：摘要行只复用 `notes` 文本，不引入新的 raw 时间戳 / openid / 凭据。
5. 不改动 `recent_events`（事件区块已经完整）与 `/ping all` 调试详情。

---

## 四、实施边界

### 4.1 允许改动

* `qq-maid-gateway-rs/src/gateway/ping/assess.rs::summary_text`：
  * `Warning` / `Error` 分支改为遍历 `notes` 而非 `notes.first()`。
  * 多条 note 用稳定分隔（如中文顿号「、」或换行 + 引导符），保证 Markdown 引用块内一行可读；条目顺序沿用 notes 原序。
  * `Normal` 分支保持现有固定文案。
* `qq-maid-gateway-rs/src/gateway/ping/tests.rs`：
  * 新增多 note 并发场景的单测，断言顶部摘要同时包含 LLM fallback 与重连两条信息。
  * 复用现有 `LlmUpstreamSnapshot::Available { fallback_used: true, .. }`、`runtime.update_state` 设置 `last_reconnect_at` + `last_resumed_at` 的测试手法。

### 4.2 禁止改动

* 不改动 `status.rs` 的 `GatewayRuntimeSnapshot` 字段与 `record_*` 写入语义。
* 不改动 `protocol.rs` 中重连 / 恢复记录点。
* 不改动 `healthz.rs`、`render.rs` 的脱敏与调试详情输出。
* 不改动 `recent_events`、`## 最近事件` 结构与兜底文案。
* 不改动 `core` / `llm` 侧 `fallback_used` 维护逻辑。
* 不引入新的 HTTP 入口或运行时路径。

### 4.3 备注

* summary 行仍嵌入在 Markdown 引用块 `> {summary}` 中。多条 note 渲染需注意：使用顿号或分号内联即可，避免在引用块中产生多段 `>`，以免破坏当前 `/ping` 回复的固定结构（QQ 富文本 / Markdown 渲染对多段引用支持不一致）。
* 若条目过多导致摘要过长，可保留 `compact_summary` 风格的截断（120 字符），但截断不得单独丢弃重连类 note；优先保证 Issue #68 点名的两类条目至少各出现一次。

---

## 五、验收标准

1. `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features`、`cargo build --workspace --release --all-features` 全部通过。
2. 构造「LLM 上游 Available + fallback_used=true」与「`last_reconnect_at` + 之后 `last_resumed_at`」并发的 fixture，`/ping`（Summary 模式）回复的顶部引用摘要行同时包含：
   * "LLM 上游最近一次调用发生过降级"（或等价提法）；
   * "最近发生过重连并已恢复"（或等价提法）。
3. 构造「LLM 上游 Available + fallback_used=true」与「重连后未发现恢复记录」并发场景，`overall = Error`，摘要同时呈现降级与未恢复重连两条信息。
4. 单一 note 场景回归通过：`renders_fallback_success_as_available_but_degraded`、`renders_unverified_upstream_without_all_green`、`renders_failed_upstream_with_defensively_redacted_summary` 等既有测试不受新增多 note 汇总逻辑影响而失败。
5. 摘要行不出现 `unix:`、原始 openid / message_id、`Authorization` / `Bearer` / `sk-` 等敏感标记。
6. `## 最近事件` 区块输出与改动前一致（事件条目集不变）。

---

## 六、验证命令

```bash
# 1. 格式化检查
cargo fmt --all -- --check

# 2. Clippy（警告视为错误）
cargo clippy --workspace --all-targets --all-features -- -D warnings

# 3. 全 workspace 测试（重点关注 qq-maid-gateway-rs ping 模块）
cargo test --workspace --all-features

# 4. release 构建
cargo build --workspace --release --all-features
```

最低要求：改动在 `qq-maid-gateway-rs/src/gateway/ping/` 内，至少跑完 1–3 步；涉及子模块编译或结构变更时再跑第 4 步。