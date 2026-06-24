
# 修改任务：qq-maid-llm 改名为 qq-maid-core

当前项目已经采用统一单进程入口：

- 最终可执行文件：`qq-maid-bot`
- 控制脚本：`botctl.sh`
- `qq-maid-gateway-rs` 和 `qq-maid-llm` 均作为 library crate 被根包引用
- 运行时不再分别启动 gateway 和 LLM 二进制

本次只进行源码 crate 和文档语义改名，不恢复双服务运行方式。

## 必须修改

1. 使用 `git mv`：

```bash
git mv qq-maid-llm qq-maid-core
````

2. 修改根目录依赖：

```toml
qq-maid-core = { path = "qq-maid-core" }
```

3. 修改子 crate：

```toml
[package]
name = "qq-maid-core"
```

4. 全仓替换 crate 标识：

```text
qq_maid_llm → qq_maid_core
```

5. Workspace member：

```text
qq-maid-llm → qq-maid-core
```

6. Makefile 中：

```text
LLM_DIR → CORE_DIR
test-llm → test-core
llm-fmt → core-fmt
llm-test → core-test
llm-check → core-check
```

如果上述目标当前仍存在，则同步更名；不要重新添加已经删除的
`run-llm`、`build-llm` 或独立进程控制目标。

7. 文档中：

* 描述整个业务 crate 时，将“LLM 服务”改为“核心模块”或“Core”
* 文档链接改为 `qq-maid-core/README.md`
* 架构图中的 `qq-maid-llm` 改为 `qq-maid-core`
* 保留具体模型能力相关的“LLM”术语

## 不要修改

* 根包和最终二进制 `qq-maid-bot`
* `botctl.sh`
* HTTP 接口 `/v1/respond`
* 当前内部监听端口
* `LLM_PROVIDER`
* `LLM_MODEL`
* `LLM_API_KEY`
* `LLM_BASE_URL`
* 数据库和 migration
* `qq-maid-gateway-rs`

## 顺手清理旧架构残留

DeepSeek 扫描结果显示文档和 Makefile 中可能仍混有旧双服务描述，例如：

* `make run-llm`
* `make run-gateway`
* `make build-llm`
* `make build-gateway`
* `llmctl.sh`
* `gatewayctl.sh`
* “先启动 LLM，再启动 Gateway”
* “项目由两个 Rust 服务组成”

这些内容如果已经不符合当前实现，应删除，只保留：

* `make run`
* `make build`
* `qq-maid-bot`
* `botctl.sh`
* “单进程分层架构”

不要把旧命令改名后继续保留，除非当前 Makefile 确实仍提供对应的开发目标。

## 验证

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo build --release -p qq-maid-bot

rg -n \
  'qq-maid-llm|qq_maid_llm|LLM_DIR|test-llm|llm-(fmt|test|check)' \
  . \
  --glob '!target/**' \
  --glob '!.git/**'
```

最终残留只能是 changelog、历史迁移说明或明确的兼容注释。
