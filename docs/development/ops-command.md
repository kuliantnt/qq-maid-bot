# `/ops` 白名单运维命令使用指南

`/ops` 让管理员从 QQ 私聊或指定群聊触发少量预先配置好的本地程序。例如查看机器人状态、执行备份、刷新缓存。它默认关闭，也不是远程终端：QQ 消息不能指定程序路径，不能执行任意 Shell，不能解释管道、重定向或命令替换。

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
| `max_stdout_bytes` | stdout 独立保留上限，最大 1 MiB |
| `max_stderr_bytes` | stderr 独立保留上限，最大 1 MiB |
| `min_args` / `max_args` | 参数数量范围，最多 16 个 |
| `commands.<name>.args.<n>.allowed_values` | 第 `n` 个参数的允许值，位置从 0 开始 |
| `commands.<name>.args.<n>.pattern` | 第 `n` 个参数的完整匹配正则 |

默认读取 `config/ops.toml`。默认路径不存在时功能保持关闭，机器人可正常启动。需要使用外部私有文件时，在 `config/.env` 设置：

```env
OPS_CONFIG_FILE=/opt/qq-maid-private/ops.toml
```

显式设置的文件不存在时，机器人会拒绝启动。

## 执行与通知边界

流程固定为：

```text
/ops 命令
  -> Gateway 提交结构化身份和会话目标
  -> Core 确定性识别，不进入 LLM / Tool Loop
  -> 检查开关、用户/群权限、命令和参数
  -> 立即返回受理回执
  -> 后台直接启动固定程序并收集真实退出状态
  -> 写入 source_type=ops 的 Notification Outbox 结果快照
  -> 现有 Push Worker 投递和重试
```

通知重试只重发已经保存的结果快照，不会再次执行脚本。stdout 和 stderr 会持续排空，但只分别保留配置上限；超出时结果中会明确标记截断。日志只记录命令名、会话类型、执行状态、退出码、耗时和截断状态，不记录参数、完整 ID、程序路径或输出正文。

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

这是首期明确限制。机器人进程退出后，正在执行的后台任务和尚未写入的结果会丢失。需要可靠重启自身时，应让外部 systemd、容器编排或其他守护进程负责，并接受本轮可能只有受理回执；不要依赖脚本在杀掉机器人后还能由同一进程推送结果。

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

框架不会调用 `cmd /C` 或 PowerShell，因此不要直接把 `.cmd`、`.bat`、`.ps1` 当作跨环境可靠入口。复杂操作可封装为受控的小型 `.exe`，仍由配置固定程序路径和参数规则。

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
