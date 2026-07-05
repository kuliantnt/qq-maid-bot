# Rust 测试资产基线与治理清单

> 对应 Issue：#191。统计时间：2026-07-05。统计对象为 `HEAD` 已跟踪 Rust 文件，避免混入当前工作区其他未提交改动。

## 统计方法

可复现命令：

```bash
python3 scripts/audit-rust-tests.py --ref HEAD
```

脚本只读取 Git 已跟踪源码和 Markdown 文档，不读取运行时私有配置、日志、SQLite、真实 prompt、知识资料或本地 `.env`。统计项包括：

- `#[test]`、`#[tokio::test]`、`#[cfg(test)]`、`#[ignore]`；
- `src/**/tests.rs`、`src/**/tests/*.rs` 等独立测试文件；
- 文档中的 Rust fenced code block，作为 doctest 候选提示；
- fixture、mock、fake、stub、builder、snapshot、support 等测试基础设施热点；
- 数据库、网络协议、时间并发、Provider / Tool Loop、Pending / Session 等高风险关键字命中。

注意：`估算测试行` 是治理入口线索，不等同于冗余程度。独立测试文件按整文件计入，内联测试按 `#[cfg(test)]` 块估算。

## 分 crate 概览

| crate | Rust 文件 | Rust 行 | `#[test]` | `#[tokio::test]` | `#[cfg(test)]` | 估算测试行 | 独立测试文件 | ignored |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `qq-maid-common` | 9 | 2509 | 25 | 0 | 3 | 647 | 1 | 0 |
| `qq-maid-llm` | 38 | 13965 | 83 | 87 | 27 | 6334 | 2 | 0 |
| `qq-maid-core` | 151 | 65848 | 349 | 315 | 57 | 28827 | 23 | 0 |
| `qq-maid-gateway-rs` | 58 | 21960 | 142 | 116 | 54 | 9211 | 7 | 0 |
| `qq-maid-bot` | 1 | 126 | 0 | 0 | 0 | 0 | 0 | 0 |

补充：脚本未发现 `#[ignore]` 测试。Markdown 文档中有 35 个 Rust 代码块，但当前未发现 doc comment doctest。

## 热点模块

| 优先级 | 模块 | 测试属性 | 估算测试行 | 判断 |
| --- | --- | ---: | ---: | --- |
| P1 | `qq-maid-core/runtime/respond` | 308 | 14303 | 覆盖普通聊天、Tool Loop、Pending、Todo 编号快照、Memory、Session、RSS、Weather、Train 等用户入口，回归价值高，不适合直接删。 |
| P1 | `qq-maid-core/runtime/tools` | 60 | 3592 | Todo Tool 执行语义、参数校验、状态转换和真实写入验证集中区，适合抽公共构造和断言。 |
| P1 | `qq-maid-llm/provider/openai` | 63 | 2767 | SSE、Responses、Chat Completions、Tool Loop、fallback 协议测试，属于协议边界高风险区。 |
| P1 | `qq-maid-gateway-rs/gateway/aggregator` | 43 | 1353 | 聚合、并发、封口和调度风险集中，不建议瘦身断言。 |
| P2 | `qq-maid-core/storage/todo` | 34 | 1921 | SQLite 持久化、查询、排序、重复规则和时间语义，适合抽数据构造，不建议合并事务边界测试。 |
| P2 | `qq-maid-core/service` | 33 | 1294 | CoreService 主入口测试，跨业务链路价值高，可检查是否有重复 provider / store 初始化。 |
| P2 | `qq-maid-gateway-rs/gateway/stream` | 22 | 1037 | QQ C2C 流式发送、首帧和失败语义，平台兼容风险高。 |
| P3 | `qq-maid-core/config` | 51 | 1188 | 配置组合多，重复构造明显；可参数化，但需保留旧配置错误提示覆盖。 |
| P3 | `qq-maid-common/time_context` | 16 | 526 | 日期时间解析基础库，数量不高，保持稳定即可。 |

## 热点文件与分类

| 文件 | 测试属性 | 估算测试行 | 分类 | 建议 |
| --- | ---: | ---: | --- | --- |
| `qq-maid-core/src/runtime/respond/tests/chat.rs` | 90 | 4900 | 高风险 / 高价值 | 保留场景名和关键断言；优先抽 Todo / Tool Loop 场景构造、可见列表快照断言和 MockProvider 期望检查。 |
| `qq-maid-core/src/runtime/tools/todo/tests.rs` | 47 | 3057 | 中高风险 | 抽 Todo Tool 输入、scope、结果断言 helper；不要删除写入、取消、恢复、删除的真实执行验证。 |
| `qq-maid-gateway-rs/src/gateway/aggregator/tests.rs` | 42 | 1108 | 高风险 | 保留并发和封口行为；如治理，仅整理时间推进和消息构造 helper。 |
| `qq-maid-core/src/config/tests.rs` | 37 | 709 | 低到中风险 | 配置组合适合参数化；保留升级错误、默认值和兼容提示。 |
| `qq-maid-llm/src/provider/tests.rs` | 36 | 1047 | 高风险 | Provider 路由和 fallback 属协议核心，不建议压缩断言。 |
| `qq-maid-core/src/service/tests.rs` | 33 | 1292 | 高风险 | CoreService 跨层入口，保留主流程；可减少重复 app state 构造。 |
| `qq-maid-core/src/runtime/respond/tests/todo.rs` | 28 | 1516 | 中风险 | Slash / deterministic Todo flow 与 Tool Loop 互补，优先抽 fixture，不直接合并到 `chat.rs`。 |
| `qq-maid-core/src/storage/todo/tests.rs` | 20 | 1450 | 高风险 | 数据库和排序时间语义，保持细粒度测试。 |
| `qq-maid-gateway-rs/src/gateway/stream/tests.rs` | 22 | 1012 | 高风险 | QQ stream id、首帧、失败降级兼容测试，保留。 |

## fixture / mock / helper 观察

- `qq-maid-core/src/runtime/respond/tests/support.rs` 是 Respond 测试基础设施中心，约 2140 行，支撑聊天、Todo、Memory、Weather、Train、Radar 等流程。它不是失效资产，但继续膨胀会增加维护成本。
- `qq-maid-core/src/runtime/respond/tests/chat.rs` 同时是最大场景测试和最大 mock / snapshot 命中点，说明后续治理应围绕“用户入口场景构造”做小 helper，而不是先删用例。
- `qq-maid-core/src/config/tests.rs` 和 `qq-maid-core/src/http/routes.rs` 的 builder / mock 命中较高，适合参数化或抽统一请求构造。
- `qq-maid-llm/src/provider/test_support.rs` 已存在轻量 stub，应优先复用，避免各 provider 测试继续复制 mock tool context。

## 风险分类

低风险，可直接拆 helper：

- Respond / Todo 测试中重复创建服务、用户消息、Todo item、可见列表快照和 provider 调用断言；
- Config 测试中重复环境变量矩阵和默认配置构造；
- HTTP routes 测试中的请求 builder、accept/origin/header 组合；
- Gateway ping 渲染 / 评估测试中的固定 runtime snapshot 构造。

中风险，可参数化或分层收敛：

- `runtime/respond/tests/todo.rs` 与 `runtime/respond/tests/chat.rs` 中针对同一 Todo 操作的 slash、deterministic flow、Tool Loop 入口测试；
- `runtime/tools/todo/tests.rs` 与 `storage/todo/tests.rs` 对同一状态变化的高层 / 底层重复断言；
- Provider 路由中同类失败场景的响应结构断言。

高风险，不建议直接删除：

- Tool Calling 必须以真实工具结果为准的假成功拦截测试；
- Todo 最近可见列表快照、编号解析、删除 / 取消 / 恢复语义；
- SQLite migration、事务、排序和时间边界；
- QQ C2C stream 首帧、stream id、失败后不补发全文的兼容测试；
- Gateway 聚合、并发、去重、封口、超时和恢复；
- LLM SSE、fallback、Provider route、Tool Loop 串行执行和依赖失败跳过；
- 长期记忆确认流程和 session scope 隔离。

失效资产：

- 本次脚本未发现 `#[ignore]` 测试。
- 当前未发现可仅凭统计确认的无引用 fixture；需在 #188 中结合 `rg` 引用关系、测试执行和历史行为逐项核实。

结构问题：

- `runtime/respond/tests/chat.rs` 同时承载普通聊天、Todo Tool Loop、假成功拦截、列表快照恢复等多条职责，测试难写部分反映的是 Respond 入口层职责较重。建议在 #190 明确 Tool / Respond / Pending / Storage 分层后再瘦身高层细节断言。
- `respond/tests/support.rs` 作为万能测试基础设施已经接近独立测试框架，后续可以按业务 executor / mock reply 生成拆分，但不应引入复杂宏。

## 与现有任务重叠

- #108、#109、#110 已关闭，范围是 Core、Gateway、LLM/Common 的测试模块机械外移。本基线不重复建议“外移测试模块”，只把这些文件作为当前有效资产统计。
- #166 是历史兼容层与冗余实现长期清理，已明确不一次性删除过期测试。本基线只标出疑似失效资产的核实方向，不把 #166 已移除的成员映射测试计入主要收益。

## 建议后续拆分

1. P1：抽 `qq-maid-core/src/runtime/respond/tests/chat.rs` 中 Todo / Tool Loop 场景构造与可见列表快照断言 helper。
2. P1：收敛 `qq-maid-core/src/runtime/tools/todo/tests.rs` 的输入构造、scope 构造和执行结果断言。
3. P1：整理 Gateway aggregator 测试的时间推进、消息构造和封口断言 helper，保留并发行为覆盖。
4. P2：参数化 `qq-maid-core/src/config/tests.rs` 的配置矩阵，保留旧配置错误提示。
5. P2：复核 Respond Todo slash / deterministic / Tool Loop 三层测试职责，形成“底层写入、业务 flow、用户入口”覆盖边界。
6. P2：盘点 provider/openai 和 provider/tests 的 mock server 构造，复用 `provider/test_support.rs` 中的轻量 stub。
7. P3：在 #188 中继续用引用搜索核实无引用 helper、长期 unused fixture 和文档残留，不凭统计直接删除。

## 验收结论

- 统计方法可复现：已新增 `scripts/audit-rust-tests.py`。
- 已覆盖 workspace 主要 crate、内联测试、独立测试文件、ignored 测试和文档 Rust 代码块。
- 已区分测试规模、维护成本和回归价值。
- 已标出 #108、#109、#110、#166 的重叠范围。
- 未为减少统计数字而修改或删除任何测试。
