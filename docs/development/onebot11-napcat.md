# 用 NapCat 接入小女仆

这篇教你把 [NapCat](https://napneko.github.io/) 连到小女仆，让她通过 QQ 私聊和群聊陪你聊天、跑命令、收待办提醒。

我们先说清楚一个前提：NapCat 是你在自己机器上跑的 QQ 协议端，负责登录 QQ、收发消息；小女仆是“大脑”，负责理解你说什么并生成回复。两者之间用 **OneBot 11 反向 WebSocket** 连起来——也就是 NapCat 主动连过来，小女仆被动等连接。

> 这篇是**使用手册**，只讲怎么配、怎么用、出问题怎么查。想了解内部实现边界的开发者，请看 [开发边界](../DEVELOPMENT.md) 和 [Gateway 模块说明](../../qq-maid-gateway-rs/README.md)。

## 当前能做什么、不能做什么

动手前先对齐预期，免得配完发现不是想的那样。

**能：**

- 通过 NapCat 走 QQ 私聊和群聊跟小女仆聊天、跑 `/todo`、`/help` 等命令
- 群里要 `@` 她她才回；私聊直接说就行
- 收到的是纯文本回复
- 待办每日提醒、RSS 推送能发到 QQ 私聊或群

**暂不能：**

- 一次连多个 QQ 账号（一台机器一个账号）
- 发图片、引用、`@` 别人等富媒体回复（收到只会以纯文本发出）
- 群里不 `@` 也响应（哪怕你发 `/todo`，群里也得先 `@`）
- 正向 WebSocket、HTTP 上报这些别的连接方式

## 准备工作

在开始之前，确认这几点都好了：

1. **小女仆已经能跑**。按 [部署说明](../../runtime/README.md) 构建出 `runtime/qq-maid-bot`，并且能 `./botctl.sh start` 启动。
2. **至少配好一个 AI 模型**。在 `runtime/config/.env` 里填好 `OPENAI_API_KEY`（或其它你用的 Provider Key）和模型路由，否则就算连上了她也“说不出话”。
3. **NapCat 已经装好并登录 QQ**。NapCat 的安装和登录请看它的官方文档：
   - [基础配置](https://doc.napneko.icu/config/basic)
   - [WebSocket 配置](https://napneko-napcatqq.mintlify.app/api/network/websocket)（含反向 WebSocket 客户端说明）
4. **想一个好记又够长的连接口令**。这就是下面的“Token”，两边要填一模一样的值，相当于它俩对暗号。随便拿个密码生成器生成一串，比如 32 位随机字符。**别用你 QQ 密码、别复用别的服务的密钥。**

## 第一步：让小女仆打开 OneBot 11 入口

打开 `runtime/config/.env`，找到 OneBot 11 那一段，按下面的样子改：

```env
ONEBOT11_ENABLED=true
ONEBOT11_BIND_HOST=127.0.0.1
ONEBOT11_BIND_PORT=8789
ONEBOT11_WEBSOCKET_PATH=/onebot/v11/ws
ONEBOT11_ACCESS_TOKEN=把你的那串口令填这里
ONEBOT11_REQUEST_TIMEOUT_MS=10000
ONEBOT11_MAX_MESSAGE_BYTES=1048576
```

几条要注意的：

- **口令必须填**。`ONEBOT11_ENABLED=true` 但口令空着，小女仆会直接报错 refusing to start，OneBot 入口根本起不来。
- **默认只在本机监听**（`127.0.0.1`）。也就是说，默认情况下只有和女仆同一台机器的 NapCat 能连过来。想跨机器连？看后面“跨主机连接”那一节，**不要**直接把这里改成 `0.0.0.0`。
- 其它默认值一般不用动。如果你以后遇到超大消息被切，再回头调 `ONEBOT11_MAX_MESSAGE_BYTES`。

如果你只想用 QQ 聊天入口、不接 QQ 官方机器人，把 `QQ_BOT_APP_ID` / `QQ_BOT_APP_SECRET` 留空、`QQ_BOT_ENABLED=false` 就行，官方那套不会启动。

改完保存，启动（或重启）小女仆：

```bash
cd runtime
./botctl.sh restart
./botctl.sh status
```

看到日志里有这么一行，就说明入口已经开了：

```text
OneBot 11 reverse WebSocket listening local_addr=127.0.0.1:8789 path=/onebot/v11/ws
```

## 第二步：在 NapCat 里加一条反向 WebSocket 连接

打开 NapCat 的 WebUI，进到**网络配置**，新建一条 **反向 WebSocket 客户端**（也叫 Reverse WebSocket Client）。按下表逐项填：

| NapCat 里的字段 | 填什么 | 说明 |
| --- | --- | --- |
| 启用 | 先关着，填完再开 | 这个开关一打开 NapCat 就会去连女仆，所以先把别的填好再开 |
| 名称 | 随便起，比如 `女仆` | 只是给你自己看的标签，不影响连接 |
| URL | `ws://127.0.0.1:8789/onebot/v11/ws` | **只有一个 URL 输入框**，不用分别填地址/端口/路径。地址、端口、路径要拼进这一个 URL 里 |
| 上报自身消息 | **关** | 关掉它，避免女仆自己的回复又被当成新消息处理一遍、自己跟自己聊起来 |
| 消息格式 | **Array** | 一定选 Array（数组）。选成 string（CQ 码）女仆会直接忽略 |
| Token | 跟小女仆里那串口令**一字不差** | **只贴口令本身，前面不要加 `Bearer `**。NapCat 会自己加好前缀再发 |
| 心跳间隔 | 保持默认（30000ms）就行 | NapCat 按这个节奏给女仆发“我还活着”的信号，保持默认就好 |
| 重连间隔 | 保持默认（30000ms）就行 | 断线后 NapCat 自动按这个间隔重连，女仆这边不用配 |
| SSL 证书验证 | 用正常 `wss://` 时保持**开**；只有自签名测试证书才考虑关 | 这个只对 `wss://`（加密连接）有意义。生产环境千万保持开 |

关于 URL 怎么拼：

- 和女仆同一台机器：`ws://127.0.0.1:8789/onebot/v11/ws`
- NapCat 在另一台机器、但内网能直连：`ws://女仆那台机器的IP:8789/onebot/v11/ws`
- 经反代加了 HTTPS：`wss://你的域名/onebot/v11/ws`

不用单独填“自身 QQ 号 / SelfID”这类字段——NapCat 会自动把你登录的 QQ 号告诉女仆。

填完**保存**，然后把“启用”开关打开。NapCat 就会去连小女仆。

## 第三步：确认连上了

两边都配好后，这样验证：

- 小女仆的日志里出现 `OneBot 11 client connected`，就说明握手成功了。
- 浏览器访问 `http://127.0.0.1:8787/ping`（或者 `./botctl.sh status`），能看到 OneBot 状态是已连接、还显示了（脱敏后的）QQ 号。
- NapCat 那边一般也有“已连接”的提示。

接着用另一个 QQ 给这个机器人发一句私聊“你好”，看女仆是不是回话。群里则要 `@` 她再说话。

## 群里怎么才会回

- **私聊**：直接发，她就会处理并回复。
- **群聊**：只有消息里 `@` 了她（@ 的是这个登录的 QQ 号本身），她才会接。不 `@` 的话哪怕你发 `/todo` 她也不会动。这条跟 QQ 官方入口不一样，特意说一下。
- `@` 她的这一下只用来“敲门”，不会把 `@女仆` 这几个字混进她真正看到的正文里；你消息里 `@` 了别人，那个 `@` 也只会作为“提到了谁”的结构信息保留，不会污染正文。

## 几个常见场景

### 跨主机连接

女仆跑在 A 机器，NapCat 在 B 机器，两台同处可信内网：

- 女仆那台保持 `ONEBOT11_BIND_HOST=127.0.0.1` 不变（**不要**改 `0.0.0.0` 暴露公网）。
- 在 NapCat 那台机器上建一条 SSH 隧道把本地端口转到女仆机器：

  ```bash
  # 在 B 机器上跑，把本地 8789 转发到女仆所在 A 机器
  ssh -L 8789:127.0.0.1:8789 你的用户名@女仆机器 -N
  ```

- NapCat 的 URL 还填 `ws://127.0.0.1:8789/onebot/v11/ws`，就走本地隧道过去了。

如果要真正对外，建议前面挂个反代做 HTTPS，URL 用 `wss://你的域名/onebot/v11/ws`，并在反代层限制来源 IP。

### 同时用 QQ 官方入口

可以同时开。`QQ_BOT_ENABLED=true` 和 `ONEBOT11_ENABLED=true` 一起开没问题，两套是独立的，一边出问题不会拖垮另一边。各自的聊天记录、待办、记忆也互不串。

### 多个 QQ 账号

当前**一台小女仆进程只认一个账号**：第一个连上的 QQ 号会“占座”，之后别的 QQ 号再连会被直接拒绝。要多账号，目前只能给每个账号各跑一个小女仆进程，监听不同端口（比如一个 8789、一个 8790）。

## 出问题怎么办

| 你看到的现象 | 可能的原因和处理 |
| --- | --- |
| NapCat 连不上，女仆日志里写 `rejected unauthorized OneBot 11 WebSocket connection` | 两边口令不一致。检查 NapCat 的 Token 是不是和 `ONEBOT11_ACCESS_TOKEN` 一字不差——**前面不要带 `Bearer `**（NapCat 会自己加），也别带多余空格或换行 |
| NapCat 显示连上了，但 `/ping` 里一直是未连接 | 检查 `ONEBOT11_ENABLED=true` 且口令非空；女仆日志如果没有 `OneBot 11 reverse WebSocket listening` 那行，说明入口没起来，去看启动错误信息里是不是写了 `access token is required when enabled` |
| 刚连上很快就被踢，日志说 `self_id report timed out` | NapCat 登录态可能有问题，没发任何事件；或者把 `ONEBOT11_REQUEST_TIMEOUT_MS` 调大一点再试 |
| 日志说 `different self_id is not allowed` | 这个进程已经被另一个 QQ 号占座了。要么重启女仆进程重新认号，要么确认是不是有多个 NapCat 账号连到了同一个端口 |
| 群里 `@` 了也不回 | 确认你 `@` 的就是这个登录的 QQ 号、不是别人；确认模型可用（`/ping` 看健康）；看日志有没有 `group_not_triggered`（说明那条消息没被判定为 `@` 她） |
| 群里发 `/todo` 没反应 | 群里必须先 `@` 她。私聊则可以直接发命令 |
| 私聊有反应但她不回话 | 大概率是模型没配好。查 `/ping` 和运行日志里的 Core/LLM 报错，常见是 `OPENAI_API_KEY` 没填、或模型路由指到了没配 Key 的 provider |
| 机器人好像在自言自语无限循环 | 正常不会发生——她自己的消息和 `message_sent` 事件都会被过滤掉。真出现了，先确认 NapCat 是不是正常上报了 `post_type=message_sent`，以及“上报自身消息”是不是误开了 |
| 待办/RSS 推送提示账号未连接 | 推送目标里指定的女仆账号和你连上的 QQ 号对不上，或者 NapCat 已经断开。`/ping` 能看到最近的断开原因 |

## 安全上提醒一句

- **不要把监听地址改成 `0.0.0.0` 直接裸奔公网**。默认 `127.0.0.1` 是故意的。要远程连，用 SSH 隧道或反代，并在反代上做 TLS 和来源 IP 限制。
- **口令单独生成**，别复用 QQ 密码、别复用微信服务号 Token、别复用 QQ AppSecret。
- 日志默认是脱过敏的——你不会在日志里看到完整 QQ 号、完整口令或聊天正文。这是设计如此，不是哪儿坏了。

## 想加更多功能的看这里

以下能力这篇**不**覆盖，是后续会逐步补的，现在配了也没有：

- 正向 WebSocket、HTTP 上报等其它连接方式
- 回复时带引用、`@`、图片
- 群里不 `@` 也响应、特定群免 `@`
- 一个女仆同时连多个 QQ 账号
- 群成员变动等通知类事件做业务

想了解这些后续方向的开发者，可以从 [设计边界](../tasks/onebot11-connect.md) 看起。
