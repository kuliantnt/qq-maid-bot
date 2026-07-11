# qbot.sh 逻辑梳理

一份 2147 行、用来统管 qq-maid-bot 全生命周期的 Bash 管理脚本：Release 下载安装、启停控制、健康检查、配置向导、自我部署。目标是"一条 `qbot xxx` 覆盖运维日常"。

## 命令入口（`qbot.sh:2099-2147`）

| 命令 | 走向 |
|---|---|
| `start / stop / restart / status / log / health / console` | 转发给 `${APP_DIR}/botctl.sh` |
| `install [ver] / update / upgrade / patch` | `install_or_update` |
| `version` | 本地 VERSION + GitHub API latest |
| `config show/get/path/set/bot/ai` | `config_cmd` 分发 |
| `deploy / self-install` | `install -m 755` 自身到 `/usr/local/bin/qbot` |
| `help / -h / --help / 空` | `usage` |

默认 `APP_DIR=/root/qq-maid-bot`（Windows Shell 下切到 `$HOME/qq-maid-bot`），可用 `QBOT_APP_DIR` 覆盖。

## 六大功能模块

### 1. GitHub 下载 & 加速（`qbot.sh:147-299`）

- `QBOT_GITHUB_PROXY` / `QBOT_GITHUB_PROXIES` 定义镜像前缀，加上"直连"合并成候选源
- `probe_github_prefix_ms` 用 `curl --range 0-0` 拉 1 字节测速；`sorted_github_sources` 按毫秒排序，失败源排最后
- `download_github_file` 按顺序试，每次下完 `downloaded_file_is_valid` 校验（`gzip -t` / `unzip -tq` / sha256 正则），失败切下一源；全都挂了再兜底一次直连

### 2. 环境探测 & 依赖（`qbot.sh:301-403`）

- `detect_target` 输出 `linux-x86_64 / linux-aarch64 / macos-* / windows-x86_64`；显式拒 windows-aarch64
- `install_deps` 缺 `curl tar gzip sha256sum unzip` 时按平台调 apt/dnf/pacman；Cygwin 直接报错让用户装

### 3. 版本 & 进程状态（`qbot.sh:405-482`）

- `latest_version` 先走 GitHub API 解 `tag_name`，失败降级到 `/releases/latest` 重定向的 URL 尾部
- `local_version` 读 `${APP_DIR}/VERSION`
- `read_qbot_pid` 读 `run/qq-maid-bot.pid`，`is_qbot_running` = pid 文件存在 + `kill -0` 通过

### 4. 配置读写（`qbot.sh:514-681`）

- `.env` 位置固定：`${APP_DIR}/config/.env`；缺失时从 `config/.env.example`（或旧 `.env.example`）拷一份
- `set_env_var`：awk 就地重写（匹配 `^KEY=`，未命中追加），value 用单引号 escape（`'\''` 逃逸法），保留原 owner/group/mode；首次修改自动 `.bak.YYYYMMDD_HHMMSS`
- `get_env_var` → `decode_env_value` 反解单引号
- `mask_config_value` 对 `*_KEY / SECRET / TOKEN / PASSWORD / _KEY$` 显示 `xxxx...xxxx`（≤8 字符时全 `********`）
- `get_real_env_var` 把 `你的xxx / your... / YOUR...` 这类占位符视为"未配置"

### 5. 配置向导（`qbot.sh:939-1893`）

- **通用 prompt**：`prompt_read_value`（普通/掩码输入）、`prompt_choice_value`（枚举）、`prompt_model_value`（模型选择器）
- **哨兵值**：`__QBOT_PROMPT_KEEP__` 保留、`__QBOT_PROMPT_CLEAR__` 清空，`apply_prompted_env_var` 统一 dispatch
- **模型选择器**（`qbot.sh:1076-1398`）有两套模式：
  - TTY + Bash 4+：字符级读入，实时筛选、↑↓选中、Backspace/Ctrl+U 编辑、Enter 确认，默认 20 条/页
  - 非 TTY 兜底：行输入 + `/关键词` 筛选 + `/all` 显示全部 + `/` 重置
- **provider→env 映射**（`qbot.sh:827-888`）：`openai→OPENAI_API_KEY/OPENAI_BASE_URL`；`deepseek→DEEPSEEK_*`；`bigmodel→BIGMODEL_*`；`mimo→MIMO_API_KEY`（无 base_url 变量，因为默认写死）；`auto` 复用 OpenAI 系。**特殊**：`--provider mimo` 会把 `LLM_PROVIDER` 写成 `auto`（走 openai_compatible）
- **模型列表来源**：`fetch_provider_models` 一次性调 `{base_url}/models`，`sed/awk` 拉出所有 `"id"` 字段，之后本地筛选不再打接口
- `normalize_model_value`：若模型名没带 `provider:` 前缀且不是 auto，自动补 `${provider}:`；auto 无前缀且无逗号时按 openai: 补
- `normalize_base_url_value`：openai/auto 时若 URL 没 `/vN` 结尾自动补 `/v1`；其他 provider 只去尾斜杠
- `config bot`：`QQ_BOT_APP_ID/SECRET/SANDBOX`、显示名、群消息模式(`off/command/mention/active`)、active 关键词、mention IDs
- `config ai`：provider、API Key、Base URL、`LLM_MODEL / PRIVATE_LLM_MODEL / GROUP_LLM_MODEL / OPENAI_SEARCH_MODEL / OPENAI_API_MODE(auto|chat_only)`

### 6. Release 安装/升级（`qbot.sh:1895-2061`）

`install_or_update` 主流程：

1. `install_deps` → `detect_target` → `resolve_version`（含 `qbot update` 时"已是目标版本"短路）
2. `download_release`：拉 `qq-maid-bot-<ver>-<target>.<tar.gz|zip>` 及 `.sha256`，`sha256sum -c` 后解压到临时目录
3. 若 bot 在跑先 `botctl.sh stop`
4. `copy_release_into_app`：
   - **首次**（目录空）：直接 `mv` 整个包
   - **非首次**：白名单覆盖二进制（`qq-maid-bot[.exe]`）、脚本（`botctl/botmon/diagnose-network/validate-runtime/qq-maid-healthcheck/qq-maid-systemd`）、`README.md/VERSION/.env.example/windows-startup-example.bat`；`static/` 整体重写
   - `merge_config`：`agent.toml` 冲突则另存 `agent.toml.release-<ver>`（不覆盖用户改动）；`.example*` 强制覆盖；其他文件只在缺失时补齐；**从不动** `.env / data/ / logs/ / run/`
5. 若之前在跑，`botctl.sh start` 拉起来

## 值得注意的设计

- **零丢配置**：升级永远不覆盖 `.env` 和 `data/`；agent.toml 冲突走并列备份而不是覆盖
- **安全默认**：所有敏感字段自动脱敏；配置写入前 awk 就地替换 + 首次备份
- **秘钥输入交互**：终端里字符级 raw read，掩码显示 `*` 且支持 Backspace/Ctrl+U，非 TTY 走 `read -r`
- **网络策略明确**：直连 GitHub 为主 + 用户白名单镜像 fallback，绝不硬编码第三方加速器，脚本自身也在 `usage` 里明说
- **可测试性**：`qbot.sh:2088` 通过 `[[ "${BASH_SOURCE[0]}" != "$0" ]]` 判断 source vs 执行——`source qbot.sh` 只加载函数、不触发 dispatch，方便 shell 回归测试

## 一个可以留意的点

`copy_release_into_app` 里非首次分支的 `cp -a` 只发生在文件在 dst 不存在时（`merge_config` 里 `[[ ! -e "${dst}" ]]`），但 `agent.toml` 走的是自定义分支：改动后再升级永远只会看到 `agent.toml.release-<ver>` 副本，不会有任何自动提示"上游 agent.toml 有更新"。用户如果不主动 diff 副本，很容易漏掉上游新加的 provider/route 默认值。这是一个已知的产品权衡（用户数据为大），不算 bug。

## GitHub 镜像自动检测集成（`qbot.sh:2021-2079`）

`install_or_update` 在 `install_deps` 前会先调 `bootstrap_github_network`，把网络探测和镜像切换的活外包给同目录下的 `github_mirror_auto.sh`。目的：直连不通时自动挑一个可用镜像喂给本次 install，不让下载环节先崩再让用户手动配 `QBOT_GITHUB_PROXY`。

### 触发时机与短路条件

`bootstrap_github_network` 在两种情况下不动：

1. `GITHUB_ACCEL_PROXY` 或 `GITHUB_ACCEL_PROXIES` 已有值——用户显式配了 `QBOT_GITHUB_PROXY(IES)`，尊重用户选择
2. `QBOT_SKIP_MIRROR_AUTO=1`——离线安装 / 内网环境的显式关闭开关

外加一层软条件：同目录找不到可执行的 `github_mirror_auto.sh` 则静默跳过，向后兼容没装镜像脚本的部署。

### 主流程

```
bootstrap_github_network
  ├── 有 QBOT_GITHUB_PROXY(IES) 或 QBOT_SKIP_MIRROR_AUTO=1  → return 0
  ├── github_mirror_auto.sh 不存在或不可执行             → return 0
  ├── bash github_mirror_auto.sh --check  ── OK ──→ ui_note 直连正常, return 0
  │                                       └ FAIL ─┐
  │                                               ↓
  ├── bash github_mirror_auto.sh (完整流程) ── 失败 ──→ ui_warn 回退直连, return 0
  │                                          └ 成功 ─┐
  │                                                  ↓
  └── 读取 ${self_dir}/.github_mirror_env
        ├── 无 GITHUB_MIRROR                → ui_warn 忽略, return 0
        ├── type=proxy → 赋给 GITHUB_ACCEL_PROXY（本次 install 生效）
        ├── type=full  → ui_warn 只对 git clone 生效, Release 下载回退直连
        └── 其他       → ui_warn 未知类型
```

关键实现细节：

- 用 `readlink -f -- "${BASH_SOURCE[0]}"` 解引用当前脚本，得到真实目录后拼 `github_mirror_auto.sh`，兼容 symlink 部署（如 `deploy` 装到 `/usr/local/bin/qbot` 的场景，会追回真实的 `APP_DIR`）
- `.github_mirror_env` 用 `sed -nE "s/^export GITHUB_MIRROR='([^']+)'.*/\1/p"` 反解单引号包裹的值，避开 shell source 副作用
- 无论镜像脚本怎么失败，`bootstrap_github_network` 永远 `return 0`——它只做加速尝试，不阻断安装

### 为什么 full 型镜像不能直接喂给 qbot

qbot 的 `download_github_file` 走 prefix 拼接：`<QBOT_GITHUB_PROXY>https://github.com/xxx`。这匹配 `ghproxy.net`、`gh-proxy.com` 这类 proxy 型镜像的用法，但 `kkgithub.com`、`bgithub.xyz` 这些 full 型是"整站替代"，需要把 URL 里的 `github.com` 替换成镜像域名，跟 prefix 模型不兼容。

所以 `bootstrap_github_network` 遇到 full 型只记日志、不设 `GITHUB_ACCEL_PROXY`。full 镜像并非白装——`github_mirror_auto.sh` 在完整流程里已经写入 git 的 `insteadOf` 全局配置，git clone 依然会走加速；只是当前这一轮 Release tarball 下载走 qbot 自己的直连兜底。

### 与 `github_mirror_auto.sh` 的接口契约

qbot 依赖镜像脚本的三个稳定行为：

1. `--check` 退出码：0 表直连 OK，非 0 表需要切镜像
2. 无参完整流程执行成功后写出 `${self_dir}/.github_mirror_env`
3. 该文件包含 `export GITHUB_MIRROR='...'` 和 `export GITHUB_MIRROR_TYPE='proxy|full'` 两行

**一个已修的坑**：镜像脚本的 `log_write` / `log_separator` 早期把彩色控制台输出打到 stdout，导致内部 `mirror_info=$(find_working_mirror)` 把日志行捕获成 mirror URL。已改为写 stderr（日志文件里仍保留完整记录），这样任何 `$(func)` 捕获都拿到纯净返回值。若后续修改镜像脚本要注意保持这个约定。

### 用户可控开关

| 变量 | 效果 |
|---|---|
| `QBOT_GITHUB_PROXY=https://ghproxy.net/` | 显式指定单个 proxy 前缀，跳过自动检测 |
| `QBOT_GITHUB_PROXIES="url1 url2"` | 显式指定多个候选，跳过自动检测 |
| `QBOT_SKIP_MIRROR_AUTO=1` | 关闭自动检测（离线/内网） |
| 删除 `github_mirror_auto.sh` 可执行位 | 静默禁用（软关闭） |

### 一个可以留意的点

`github_mirror_auto.sh` 内置候选排序把 6 个 full 站排在 4 个 proxy 站之前。如果 full 站先响应，qbot 就拿不到能立即用的 proxy——本次 Release 下载仍会回退直连。适合 qbot 的顺序理论上应该反过来，但那要改镜像脚本自身的策略，会影响它单独跑（`git clone` 场景）时的偏好。当前的取舍是：不改镜像脚本，让 qbot 侧只做适配和日志提示；如果用户明确要为 Release 下载优化，手动 `export QBOT_GITHUB_PROXY=https://ghproxy.net/` 就够了。
