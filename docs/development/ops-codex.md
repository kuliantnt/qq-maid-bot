# `/ops codex` 使用指南

`/ops codex <任务描述>` 允许受信任的机器人管理员向服务器上的 Codex CLI 提交长任务。任务在固定工作目录和固定沙箱中异步执行，受理后立即返回任务 ID，最终结果通过 Notification Outbox 推送回原会话。

该入口权限较高，默认关闭。它适合检查部署目录、阅读知识资料、排查构建或修改固定工作区，不是面向普通用户的聊天功能。通用 `/ops` 权限、固定命令和安全模型见 [`/ops` 白名单运维命令使用指南](./ops-command.md)。

## 准备 Codex CLI

先使用机器人实际运行账号确认 Codex 可执行：

```bash
command -v codex
codex --version
codex exec --help
```

把 `command -v codex` 返回的绝对路径用于后续 `program` 配置。NVM 安装通常类似：

```text
/home/bot/.nvm/versions/node/v24.0.0/bin/codex
```

NVM 版 Codex 的脚本通常通过 `/usr/bin/env node` 启动，而 systemd 服务的 `PATH` 往往不包含 NVM 目录。`/ops codex` 会把固定 `program` 所在目录放到 Codex 子进程的 `PATH` 最前面，因此 `node` 和 `codex` 位于同一 `bin` 目录时，无需修改机器人进程的全局 `PATH`。该行为不影响普通 `/ops` 固定命令。

## 创建配置

在部署运行目录中复制公开模板：

```bash
cp config/ops.example.toml config/ops.toml
chmod 600 config/ops.toml
```

先启用总开关和需要的会话范围。私聊示例：

```toml
enabled = true

[private]
enabled = true
allowed_user_ids = ["从私聊 /ping 获取的稳定 user_id"]

[group]
enabled = false
allowed_group_ids = []
```

再填写 Codex 配置：

```toml
[codex]
enabled = true
program = "/absolute/path/to/codex"
working_directory = "/absolute/path/to/workspace"
timeout_seconds = 1800
max_prompt_bytes = 8000
max_stdout_bytes = 32768
max_stderr_bytes = 16384
profile = "qq-maid-ops"
sandbox = "workspace-write"
cancellable = true
max_concurrent_tasks = 1
```

关键字段：

| 字段 | 说明 |
| --- | --- |
| `program` | `codex` CLI 的现有文件绝对路径，不能由聊天消息覆盖 |
| `working_directory` | Codex 唯一固定工作根目录；可以是发布目录，也可以不是 Git 工作树 |
| `profile` | 固定 Codex profile 名称，建议为机器人运维单独配置 |
| `sandbox` | 只接受 `read-only` 或 `workspace-write` |
| `timeout_seconds` | 单任务最长执行时间 |
| `max_prompt_bytes` | 完整任务描述的 UTF-8 字节上限 |
| `max_stdout_bytes` / `max_stderr_bytes` | 最终回执保留的输出上限 |
| `cancellable` | 是否允许管理员取消运行中的任务 |
| `max_concurrent_tasks` | Codex 并发上限，建议从 `1` 开始 |

发布运行目录通常不带 `.git`。入口固定使用 `codex exec --skip-git-repo-check`，因此这种目录可以正常执行；该参数只跳过 Git 仓库检查，不会放宽 `working_directory`、profile 或 sandbox。

## 重启与检查

配置修改后重启服务：

```bash
./botctl.sh restart
./botctl.sh status
```

从机器人会话依次检查：

```text
/ops
/ops codex 只读取当前工作目录并概括它的用途，不修改文件
/ops list
```

受理回执示例：

```text
Codex 任务已受理
任务 ID：ops-a82f31c4
取消：/ops cancel ops-a82f31c4
```

任务结束后会收到成功、执行失败、超时、取消或启动失败的真实状态。模型文字不能替代进程退出状态。Codex 正常执行时会把进度、插件告警和工具日志写入 stderr；成功任务只向聊天推送最终 stdout。失败、超时或取消时，stderr 会写入权限受限的 `logs/ops/<任务ID>.log`，聊天只提示服务器日志路径。日志写入前会脱敏任务描述。普通 `/ops` 固定命令仍按原样返回 stdout 和 stderr。

Codex 最终输出会经过项目现有的 QQ 安全 Markdown 渲染。`http://` 和 `https://` 链接可以保留；指向 `/root/...` 等服务器本地文件的 Markdown 链接不可在 QQ 中打开，因此只展示链接标签，不发送内部绝对路径，避免 QQ 把中文路径、下划线和括号重复解析。

## 查询与取消

```text
/ops list
/ops cancel ops-a82f31c4
```

`/ops list` 只展示当前机器人进程中仍在运行的任务。首期任务注册表不跨进程恢复；机器人重启后，重启前仍在执行的任务不会继续收集结果。

## 常见报错

### `Not inside a trusted directory`

原因是 `working_directory` 不是 Git 工作树，且旧版本入口没有传 `--skip-git-repo-check`。部署包含该参数的新版本后重试。不要为了绕过报错在发布目录中临时执行 `git init`。

### `/usr/bin/env: 'node': No such file or directory`

原因通常是 NVM 版 `codex` 依赖同目录的 `node`，但机器人服务没有加载交互式 Shell 的 NVM 环境。部署会把固定 `program` 所在目录加入 Codex 子进程 `PATH` 的版本后重试，并确认以下两个文件确实位于同一目录：

```bash
ls -l "$(dirname "$(command -v codex)")/codex"
ls -l "$(dirname "$(command -v codex)")/node"
```

### 状态为“启动失败”

检查 `program` 是否为现有文件绝对路径、机器人账号是否有执行权限、`working_directory` 是否为现有目录。启动失败不会回显内部路径或系统错误详情，必要时在服务器上使用同一账号运行 `codex --version`。

### 状态为“执行失败”

查看回执中的退出码，再按提示到服务器读取 `logs/ops/<任务ID>.log`。常见原因包括 Codex profile 不存在、认证不可用、模型服务失败或任务内命令返回非零。框架不会把 stdout 中的“成功”字样当作成功状态。错误日志不会自动上传或提交，需要由部署者按本机日志策略清理。

### 服务器日志出现插件或 MCP 告警

Codex 的可选插件、系统 keyring 或 MCP Server 初始化失败，不一定会导致主任务失败。以任务退出状态和最终 stdout 为准；例如某个 MCP Server 报 Python `ModuleNotFoundError`，但任务仍能直接读取本地文件并成功退出。成功回执不会把这些进度日志推送到聊天，仍可在服务器侧按需修复对应插件或 MCP 环境。

### 收到受理回执但没有最终通知

先发送 `/ops list` 判断任务是否仍在运行，再检查 Notification Worker、平台连接和发送权限。Outbox 重试只重发已保存的结果，不会重新执行 Codex。

## 安全边界

- 只向完全受信任的管理员开放，并优先使用私聊。
- `working_directory` 只指向允许 Codex 读取或修改的目录。
- `sandbox` 不允许配置为 `danger-full-access`。
- 不在任务描述中发送 token、secret、API Key、真实用户 ID、群 ID、聊天记录或其他敏感资料。
- 任务描述始终作为 `--` 后的单个 argv 传递，不经过 Shell；精确的 `-` 会被拒绝，避免触发 stdin 读取语义。
- 超时或取消在 Unix 上会终止独立进程组；Windows 当前只保证终止直接子进程。

## 部署后验收

代码更新后按项目部署流程发布，并至少完成以下检查：

```bash
make deploy-remote
```

1. 服务重启成功，`botctl.sh status` 显示进程存活。
2. `/ops codex` 能返回任务 ID，不再出现 Git 信任目录或 Node PATH 报错。
3. 只读测试任务能收到真实成功结果。
4. `/ops list` 不再显示已结束任务。
5. `/ops cancel <任务ID>` 能取消一个明确允许取消的测试任务。
