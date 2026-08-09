# 任务文档与归档

`docs/tasks/` 只保留尚未完成的可执行任务文档和本索引。截至 2026-07-30，当前没有仍以仓库 Markdown 维护的开放任务；原目录下的任务已完成、已被新方案取代或已转为历史审计，统一移入 [`archive/`](./archive/)。

归档文档用于追溯背景和设计取舍，不是当前实现的权威说明。如果归档内的文件名、路径、环境变量或验收清单与当前仓库冲突，以源码、测试、[`AGENTS.md`](../../AGENTS.md)、[开发维护文档](../DEVELOPMENT.md) 和各 crate README 为准。

## 归档索引

### 平台接入与消息链路

- [OneBot 11 接入任务](./archive/onebot11-connect.md)
- [QQ 群机器人回复能力](./archive/qq-group.md)
- [QQ OpenID 身份检查](./archive/qq-openid-identity-check.md)
- [Gateway → Core 进程内调用与流式边界](./archive/gateway-core-inprocess-call-analysis-report.md)
- [Gateway → Core 第一阶段流式改造](./archive/gateway-core-inprocess-streaming-phase1-completion-report.md)
- [QQ 流式消息 / Tool Calling 投递审计](./archive/stream-tool-delivery-audit.md)
- [连续消息聚合](./archive/message-aggregation.md)
- [消息并发调度](./archive/message-concurrency-scheduling.md)

### LLM、Agent 与 Tool

- [`qq-maid-llm` 调用链重构](./archive/llm-pipeline-v2.md)
- [私聊 Agent Loop 语义基线](./archive/agent-loop-baseline.md)
- [私聊轻量 Agent / Harness 早期方案](./archive/private-agent-harness-tools.md)
- [场景感知模型路由旧方案](./archive/scope-aware-model-routing.md)
- [OpenCode Go / GLM-5.2 接入](./archive/opencode-go-add.md)
- [12306 列车查询 Tool](./archive/12306-train-query.md)

### Memory、知识与上下文

- [Memory 作用域隔离](./archive/add-scope-to-memory-fix-leakage.md)
- [RAG V1](./archive/rag-v1.md)
- [RAG 切片与检索 V2](./archive/rag-chunking-retrieval-v2.md)
- [可配置上下文模块 V1](./archive/configurable-context-modules.md)
- [上下文与请求修整层 V1](./archive/context-request-patch-v1.md)

### Todo / Reminder

- [第一阶段现状审计](./archive/todo-reminder-phase1-audit.md)
- [业务逻辑收口](./archive/todo-reminder-phase2.md)
- [引用定位、重复提醒与取消语义收尾](./archive/todo-reminder-phase3.md)

### 工程治理与问题修复

- [Gateway / LLM 单进程合并](./archive/merge-gateway-core.md)
- [`qq-maid-llm` / `qq-maid-core` 改名风险](./archive/rename-core-risk.md)
- [大文件审计](./archive/large-file-audit-report.md)
- [Rust 测试资产基线](./archive/rust-test-asset-baseline.md)
- [Issue #190 Todo / Tool Loop 分层测试审计](./archive/issue-190-test-layer-audit.md)
- [`/ping` 多异常摘要修复](./archive/ping-summary-missing-fallback-and-reconnect.md)

## 新任务文档约定

1. 文首写明来源、状态、目标和非目标，不用文件所在目录暗示已实现。
2. 实施前重新搜索当前源码和测试，不按历史文件名或旧路径推测调用链。
3. 任务完成、取消或被新方案取代后，移入 `archive/` 并在文首标注归档原因和当前权威入口。
4. 持续约束当前实现的内容应收口到 `docs/design/`、`docs/development/`、各 crate README 或 `AGENTS.md`，不长期留在任务拆解里。
