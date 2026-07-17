# `/ops` 白名单运维命令使用指南

`/ops` 让管理员从 QQ 私聊或指定群聊触发少量预先配置好的本地程序。例如查看机器人状态、执行备份、刷新缓存。它默认关闭，也不是远程终端：QQ 消息不能指定程序路径，不能执行任意 Shell，不能解释管道、重定向或命令替换。内置 `/ops codex` 是单独关闭的高权限长任务入口，不能由普通固定命令的开关顺带启用。

最简单的用法是先只开私聊，确认可用后再考虑群聊。

## 五分钟完成私聊配置

### 1. 取得自己的稳定用户 ID

私聊机器人发送：

```text
/ping
```

回复的“当前消息”区域会直接显示完整 `user_id`。把它用于后面的 `allowed_user_ids`。该值来自 Gateway 当前收到的私聊事件，不需要经过模型或手工映射。

### 2. 准备一个固定脚本

以下示例假设实际部署目录是 `/opt/qq-maid/runtime`。请按自己的目录调整，不要照抄不存在的路径。

```sh
#!/bin/sh
set -eu

exec /opt/qq-maid/runtime/botctl.sh status
```

将它保存为 `/opt/qq-maid/ops/status.sh`，并赋予机器人运行账号执行权限：

```bash
chmod 700 /opt/qq-maid/ops/status.sh
```

先在服务器上用机器人实际运行账号手工执行一次，确认退出码和输出正确。`/ops` 不会替你配置 sudo、systemd、Docker 权限或脚本文件权限。

### 3. 创建私有配置

在运行目录执行：

```bash
cd /opt/qq-maid/runtime
cp config/ops.example.toml config/ops.toml
```

仓库内的 `runtime/config/ops.toml` 已被 Git 忽略，真实用户 ID、群 ID 和脚本路径不会被误提交。最小私聊配置如下：

```toml
enabled = true

[private]
enabled = true
allowed_user_ids = ["填写 /ping 返回的完整 user_id"]

[group]
enabled = false
allowed_group_ids = []

[commands.status]
program = "/opt/qq-maid/ops/status.sh"
timeout_seconds = 10
max_stdout_bytes = 4096
max_stderr_bytes = 2048
min_args = 0
max_args = 0
```

`program` 必须是绝对路径。程序在启动时不要求已经存在；路径错误会被真实执行结果报告为“启动失败”。配置语法、范围或正则错误则会阻止机器人启动，避免带着错误权限配置继续运行。

### 4. 重启并验证

配置只在启动时读取：

```bash
/opt/qq-maid/runtime/botctl.sh restart
```

随后私聊发送：

```text
/ops status
```

校验成功时会立刻收到：

```text
运维任务 status 已受理，完成后会通知你。
```

脚本完成后，结果通过现有 Notification Outbox 和 Push Worker 发送回同一个私聊。结果以进程退出状态为准：退出码 `0` 为成功，非零退出为失败，超过配置时间为超时，无法创建进程为启动失败。

## 带参数的命令

用户参数始终作为独立 argv 传给固定程序，不会拼接成 Shell 字符串。可以限制参数数量、允许值和格式。

只允许 `gateway` 或 `core`：

```toml
[commands.restart]
program = "/opt/qq-maid/ops/restart-component.sh"
timeout_seconds = 60
max_stdout_bytes = 4096
max_stderr_bytes = 4096
min_args = 1
max_args = 1

[commands.restart.args.0]
allowed_values = ["gateway", "core"]
```

用法：

```text
/ops restart gateway
```

脚本仍应自行做一次参数校验，形成纵深防护：

```sh
#!/bin/sh
set -eu

case "${1-}" in
  gateway|core) ;;
  *) echo "unsupported component" >&2; exit 2 ;;
esac

# 在这里调用固定的受控操作，不要把参数重新交给 eval 或 sh -c。
```

也可以使用对整个参数完整匹配的 Rust 正则：

```toml
[commands.inspect.args.0]
pattern = "[a-z][a-z0-9-]{0,31}"
```

同一个位置同时配置 `allowed_values` 和 `pattern` 时，两项必须同时满足。每条命令最多允许 16 个参数，单个参数最多 1024 字节；控制字符会被拒绝。

## 开启群聊

建议在私聊验证完成后再开启：

```toml
[group]
enabled = true
allowed_group_ids = ["填写平台稳定群 ID"]
```

群聊请求需要同时满足：

1. 总开关和群聊开关都开启；
2. 当前原始群目标命中 `allowed_group_ids`；
3. Gateway 取得的服务端可信角色是群主 `owner` 或管理员 `admin`。

普通成员、角色缺失或未知都会拒绝。`allowed_group_ids = []` 表示不允许任何群，不表示允许全部群。群聊执行完成后结果回原群，不会私发给操作者。

不同平台或机器人账号的目标由 `platform + account_id + target_type + target_id` 隔离。当前公开模板不为不同命令配置不同权限；通过上述群聊权限后，可执行统一白名单里的所有命令。

## 内置 Codex 长任务

> `/ops codex` 相当于向服务器上的 Codex 代理提交远程开发任务。Codex 可能读取、修改固定工作目录内的文件，并运行 Cargo、Git、测试、编译器等开发命令。只应向完全受信任的管理员开放；默认关闭。

Codex 不需要也不允许注册为 `[commands.codex]`。`codex`、`list`、`cancel`、`stop`、`kill`、`close` 都是保留名称，出现在 `[commands.<name>]` 时会阻止启动。最小配置：

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

启用时 `program` 必须是现有文件，`working_directory` 必须是现有目录，且二者都必须是绝对路径。发布运行目录可以不是 Git 工作树，入口会固定加入 `--skip-git-repo-check`；该参数只跳过仓库检查，不会放宽工作目录或沙箱。`sandbox` 只接受 `read-only` 或 `workspace-write`，拒绝 `danger-full-access`。部署者应在固定 `profile` 中继续限制模型、审批和其他 Codex 行为；本入口不会加入 `--add-dir`、危险绕过审批/沙箱参数，也不会从用户消息读取任何 Codex 控制参数。

当前固定启动 argv 为：

```text
<program> exec --skip-git-repo-check --profile <profile> --sandbox <sandbox> --cd <working_directory> --color never -- <完整任务描述>
```

进程自身的 current directory 同时固定为 `working_directory`。`<完整任务描述>` 是最后一个独立 argv，保留其中的中文、空格、引号、`;`、`|`、`$()` 等字符，不经过 Shell，也不会按空白继续拆分。

受理、查询和取消：

```text
/ops codex 检查当前项目为什么构建失败，并修复相关问题
/ops list
/ops cancel ops-a82f31
```

`stop`、`kill`、`close` 只作为 `cancel` 的兼容别名；帮助文案始终优先展示 `/ops cancel <任务ID>`。受理后立即返回 `ops-` 加 8 位随机十六进制短 ID。SQLite 的唯一约束和进程内注册表共同排除冲突，不暴露数据库自增 ID。

进程内注册表只管理正在运行的 Codex 任务，并保留最多 64 条、最长 10 分钟的最近完成状态供重复取消查询。默认并发上限为 1，达到上限立即拒绝，不排入无限队列。私聊任务按 `platform + account_id + private target + 发起用户不可逆摘要` 隔离；群聊任务按 `platform + account_id + group target` 隔离，只有该群当前可信 owner/admin 可以查看或取消。

Unix 为每个 Ops 进程建立独立进程组。Codex 取消或超时时先向整组发送 `SIGTERM`，经过短暂 grace period 后再向整组发送 `SIGKILL`，覆盖 Codex 派生的 Cargo、Git、测试和编译器进程。Windows 首期尚未接入 Job Object，只能确认终止直接 Codex 子进程；结果会明确提示派生进程可能仍需人工检查，不能据此声称整棵进程树已停止。

## 配置字段

| 字段 | 含义 |
| --- | --- |
| `enabled` | `/ops` 总开关，默认 `false` |
| `private.enabled` | 是否允许私聊，默认 `false` |
| `private.allowed_user_ids` | 可执行的稳定用户 ID 精确列表 |
| `group.enabled` | 是否允许群聊，默认 `false` |
| `group.allowed_group_ids` | 可执行的稳定群 ID 精确列表，空列表拒绝全部群 |
| `commands.<name>.program` | 固定程序或脚本绝对路径 |
| `timeout_seconds` | 执行超时，范围 1 到 3600 秒 |
| `max_stdout_bytes` | stdout 独立内存保留上限，最大 64 KiB |
| `max_stderr_bytes` | stderr 独立内存保留上限，最大 64 KiB |
| `min_args` / `max_args` | 参数数量范围，最多 16 个 |
| `commands.<name>.args.<n>.allowed_values` | 第 `n` 个参数的允许值，位置从 0 开始 |
| `commands.<name>.args.<n>.pattern` | 第 `n` 个参数的完整匹配正则 |
| `codex.enabled` | Codex 独立开关，默认 `false` |
| `codex.program` | 固定 Codex CLI 现有文件绝对路径 |
| `codex.working_directory` | 固定且现有的工作目录绝对路径 |
| `codex.profile` / `codex.sandbox` | 固定 profile 与安全沙箱；sandbox 只允许 `read-only` / `workspace-write` |
| `codex.max_prompt_bytes` | 完整任务描述字节上限，最大 64 KiB |
| `codex.cancellable` | 是否允许通过内置 cancel 取消 |
| `codex.max_concurrent_tasks` | Codex 并发上限，默认 1，范围 1 到 8 |

默认读取 `config/ops.toml`。默认路径不存在时功能保持关闭，机器人可正常启动。需要使用外部私有文件时，在 `config/.env` 设置：

```env
OPS_CONFIG_FILE=/opt/qq-maid-private/ops.toml
```

显式设置的文件不存在时，机器人会拒绝启动。

## 执行与通知边界

流程固定为：

```text
/ops 命令
  -> Gateway 提交结构化身份、稳定平台消息 ID 和会话目标
  -> Core 确定性识别，不进入 LLM / Tool Loop
  -> 检查开关、用户/群权限、命令和参数
  -> 以 platform + account_id + 会话类型 + 规范化会话目标 + message_id 摘要原子领取执行
  -> 立即返回受理回执
  -> 后台直接启动固定程序并收集真实退出状态
  -> 写入 source_type=ops 的 Notification Outbox 结果快照
  -> 现有 Push Worker 投递和重试
```

通知重试只重发已经保存的结果快照，不会再次执行脚本或 Codex。执行领取表只保存稳定入站键的 SHA-256 摘要、短任务 ID、命令名和状态；同一 `platform + account_id + 会话类型 + 会话目标 + message_id` 的并发或后续重放不会再次 spawn，不同会话即使平台 message_id 碰撞也不会共享任务。入口没有可信消息 ID 时会拒绝高副作用执行，不用正文、时间戳或昵称降级拼接伪唯一键。

内置 Codex 通过直接 argv 启动固定的 `program`，不会经过 Shell。profile、sandbox、工作目录和其他启动参数全部来自配置；任务描述作为独立 argv 放在 `--` 之后，精确的 `-` 会被拒绝，避免触发 Codex 的 stdin 语义。中文、空格、换行、引号和 Shell 元字符都只作为任务内容传递。

主进程无论成功、非零退出、wait 失败、超时还是取消，stdout/stderr 收集都只有有限 drain 时间。正常退出允许较长排空；终止路径使用更短 grace。派生进程继续持有管道时，收集任务会被中止，并使用此前已捕获内容生成结果，真实主进程退出码不会被覆盖。普通固定程序正常退出后不会主动清理其自行后台化的派生进程，部署者仍应避免在短命令脚本中遗留后台任务。

结果按独立内容段写入同一个 Outbox payload，每段 Markdown 与 fallback 都来自同一段原始输出，单段最多 4000 字符。QQ 官方逐段发送 Markdown，失败时只回退当前段纯文本；OneBot 逐段发送同一段的纯文本 fallback。Worker 每确认一段发送成功就持久化 `delivered_parts`，失败重试从首个未确认段继续，不重发已落库确认的前置段。极端情况下平台已经成功但本地进度提交同时失败，系统无法获得跨平台原子确认，重试仍可能重复该最后一个未落库段。

## 常见问题

### 回复“运维命令未启用”

检查运行目录下是否存在 `config/ops.toml`，以及顶层 `enabled = true`。如果使用 `OPS_CONFIG_FILE`，确认它指向机器人进程实际可读的绝对路径。

### 回复“当前未开放私聊运维命令”

检查 `[private].enabled = true`。

### 回复“你没有执行运维命令的权限”

重新私聊发送 `/ping`，把完整 `user_id` 原样放进 `allowed_user_ids`。不要使用昵称、备注名或自行截断的 ID。

### 回复参数不合法

检查 `min_args`、`max_args`，以及对应位置的 `allowed_values` / `pattern`。命令按空白分隔参数，不解释引号；需要带空格的复杂输入不适合通过 `/ops` 传递，应改成配置中的短枚举值，由固定脚本内部映射。

### 收到“启动失败”

检查 `program` 是否是绝对路径、文件是否存在、机器人运行账号是否有执行权限，以及脚本 shebang 指向的解释器是否存在。

### 收到“执行失败”

查看结果中的退出码和 stderr。框架不会根据输出文本猜测成功；脚本打印“成功”但退出码非零，仍然判定失败。

### 收到受理回执但没有结果通知

检查 Notification Worker、目标平台连接和机器人发送权限。结果入队失败或平台投递失败不会伪造成成功。平台发送失败由现有 Outbox 策略重试，不会重跑脚本。

### 用脚本重启机器人自身后没有最终结果

这是首期明确限制。机器人进程退出后，内存注册表、正在执行的后台任务和尚未写入的结果会丢失。入站领取记录已经持久化，因此同一平台事件重放不会重跑命令，但也不能恢复丢失任务或补造最终结果。需要可靠重启自身时，应让外部 systemd、容器编排或其他守护进程负责，并接受本轮可能只有受理回执。

## Windows 注意事项

`program` 仍必须是绝对路径，并由操作系统直接启动。优先配置可直接运行的 `.exe`：

```toml
[commands.status]
program = 'C:\qq-maid\ops\status.exe'
timeout_seconds = 10
max_stdout_bytes = 4096
max_stderr_bytes = 2048
min_args = 0
max_args = 0
```

框架不会调用 `cmd /C` 或 PowerShell，因此不要直接把 `.cmd`、`.bat`、`.ps1` 当作跨环境可靠入口。复杂操作可封装为受控的小型 `.exe`，仍由配置固定程序路径和参数规则。Codex 取消/超时在 Windows 当前也只终止直接子进程，尚未提供 Job Object 级进程树保证。

## 上线前检查

- 总开关、私聊和群聊开关只开启实际需要的范围；
- 管理员和群列表使用稳定 ID，不使用昵称；
- 每个 `program` 都是部署者控制的绝对路径；
- 脚本及其父目录不可被机器人聊天用户或低权限账号修改；
- 参数尽量使用短枚举，必要时再使用完整匹配正则；
- 脚本不使用 `eval`，不把 argv 再拼接进 `sh -c`；
- 超时和输出上限按命令实际需要设置；
- 使用机器人运行账号手工验证成功、非零退出和权限不足路径；
- 先在私聊验证，再按需开放群聊；
- 备份、重启等命令已评估幂等性和机器人自身重启限制。
