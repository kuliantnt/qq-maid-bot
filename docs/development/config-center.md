# 配置中心设计与字段清单

配置中心把现有文件和环境变量逐步收敛到明确的来源模型，不删除高级部署能力，也不把普通配置复制到 SQLite。实际字段解析仍以源码与 [`runtime/config/.env.example`](../../runtime/config/.env.example) 为准。

## 权威存储与优先级

```text
进程环境 / dotenv
  ├─ Provider 凭证、平台配置、普通运行配置
  │    > config/runtime.toml / SQLite 认证加密密文
  │    > 默认值
  │
  └─ AGENT_CONFIG_FILE 等 Bootstrap 路径覆盖
       > config/agent.toml 中的 Agent 策略
```

- `config/runtime.toml` 是 Provider 连接参数、平台接入和普通业务配置的受管文件；普通值写入 TOML，API Key、Token、AppSecret 等敏感值写入 SQLite 认证密文。
- `config/agent.toml` 是模型路线、搜索路线、Profile、Scene、Tool Calling 和 Tool 白名单的唯一持久化事实来源。网页直接结构化编辑这个文件，不是比它更高的一层。
- 两个文件都允许人工维护，也都使用独立的 SHA-256 revision。程序写回会规范化 TOML 格式并删除注释/自定义排版，但会通过现有 Agent schema 保留全部合法配置语义和所有未修改条目。
- 进程环境先于 `config/.env` 和 `.env`，dotenv 仅补缺失项；dotenv 文件不存在是正常输入。外部同名字段存在时强制覆盖受管值，安全快照会标记 `source=environment` 与 `overridden=true`。
- `AGENT_CONFIG_FILE` 仅决定服务端受管目标，浏览器不能提交任意路径。统一程序要求该文件存在且通过完整校验；`ops.toml` 继续是禁止通用 WebUI 编辑的高风险部署配置。
- 当前纳入配置中心的字段都标记为重启生效。受管文件写入使用内容 SHA-256 revision、整份校验、同目录临时文件、同步和原子替换；同一进程内严格串行，并在修改开始和正式替换前核对同一个 expected revision。跨进程写入或人工编辑不共享进程内锁，替换前复核只能尽量缩短冲突窗口，仍存在极小的 TOCTOU 窗口，因此这里不是绝对 CAS 保证。

## 敏感值与主密钥

API Key、Token、AppSecret、EncodingAESKey 等敏感值使用 XChaCha20-Poly1305 认证加密后写入统一 SQLite。记录包含算法、版本、24 字节 nonce、认证密文和更新时间；字段稳定 key 作为附加认证数据。普通读取接口只返回是否已配置及不可逆的 opaque revision，不返回 nonce、密文或原文；不存在统一表示为 `missing`。replace/clear 必须携带 expected revision，关联 secret 可在一个 SQLite 事务中批量比较、校验和修改。

runtime 或 secret 保存前会构造候选最终环境视图：未修改的当前值、候选修改、进程环境/dotenv 外部覆盖以及解析器默认值共同参与；agent 保存则额外传入尚未落盘的候选 Agent 文档。统一根程序把候选交给 Core、LLM Provider 与 Gateway 的正式启动预检：Provider 路由复用 `build_provider` 的纯配置计划，逐条确认自定义 Provider 声明和至少一个可用凭证，但不创建上游请求；OneBot Token、微信服务号 Token/AES 字段、QQ AppID/AppSecret 等跨字段约束也不在配置中心复制第二套规则。候选无效时不写文件或 SQLite；快照中的 `valid` 同样来自该组合校验结果。进程环境或 dotenv 已覆盖的字段会显示 `editable=false`，对应 runtime/secret 领域写接口也会拒绝修改。

解密主密钥不在 SQLite、`.env`、受管 TOML、日志或诊断包中。默认路径是相对于受管配置目录的 `secrets/master.key`；首次不存在时从系统安全随机源生成，以原子方式安装，Unix 下目录和文件权限分别限制为 `0700`、`0600`。已有文件损坏、是符号链接、不是普通文件或向组/其他用户开放时拒绝启动。

部署和备份必须遵守：

- Docker/容器重建时持久化主密钥，不能在新容器层重新生成；
- 数据库和主密钥分别保护、分别备份；只备份数据库无法恢复敏感配置；
- 部署脚本只创建 `config/secrets/`，不上传、覆盖或生成 `master.key`；
- `MASTER_KEY_FILE` 可指向只读挂载、Docker Secret、systemd credential 落地文件或等价外部来源，但变量中只放路径，不放密钥原文。

## 已登记字段

字段元数据由 Core 与 Gateway 各自声明，根进程合并；通用层不理解平台协议细节。下列均为稳定 key，括号内为兼容环境变量。

| 模块 | 普通受管字段 | 加密敏感字段 |
| --- | --- | --- |
| Provider | 各内置 Provider 的 Base URL、API mode 等连接元数据 | OpenAI、DeepSeek、BigModel、Gemini、MiMo API Key |
| Core 功能 | RSS、Memory、Todo 与 Todo 提醒时间 | `weather.qweather.api_key` |
| 控制台 | `console.enabled`、`console.allowed_origins` | 无 |
| QQ 官方 | `platform.qq_official.enabled` | AppID、AppSecret |
| OneBot 11 | enabled、bind host/port、WebSocket path | Access Token |
| 微信服务号 | enabled、encryption mode、bind host/port、callback path | Token、AppID、AppSecret、EncodingAESKey |

`provider.main_model`、Provider 默认模型、私聊/群聊 Tool Calling 开关等 Agent 策略不登记到 `runtime.toml`；route/profile/scene 的结构化接口统一修改 `agent.toml`。监听地址/端口、数据库路径、受管文件路径、主密钥路径、Agent/ops 文件路径和 `/ops` 执行规则属于 Bootstrap 或高风险部署项，只允许通过明确的文件/环境配置管理。

## Agent 策略快照与写入边界

配置快照的 `agent` 节点返回独立的 `revision`、`source=agent_toml`、`saved_value`、`running_value`、`pending_restart`、`read_only` 与 `editable`。保存只更新文件值；当前进程继续使用启动时捕获的 `running_value`，两者不同时 `pending_restart=true`，重启重新加载后恢复一致。

领域写接口只接受 route、search route、profile 和 private/group scene 的结构化变更，不接受文件路径。每次保存都会先解析当前完整文档，应用局部变更，再调用 `AgentRuntimeConfig` 的同一 schema 与引用校验；非法 route/profile/scene/Tool 引用不会进入正式文件。符号链接、非普通文件、只读文件或组/其他用户可写的不安全权限均拒绝写入。

## 管理接口边界

启用控制台后，`GET /api/v1/console/configuration` 返回 runtime 与 agent 两个配置域的安全快照，但必须先通过独立部署管理员会话。HTTP 写接口分别接受 runtime 普通值 set/remove、agent 结构化变更和带 expected revision 的 secret replace/clear/批量修改，不能把脱敏占位符当作真实 secret 保存。所有认证与配置写操作要求同源 Origin、HttpOnly 服务端会话、轮换 CSRF、权限和脱敏审计；现有跨域 allowlist 只保留给只读状态与 Markdown 兼容接口，不授予管理 API 跨域凭据能力。

`setup_required` 降级态允许按向导分步保存“字段自身合法、整体启动候选尚缺其他域”的配置，以支持首次配置中断后继续；此放宽不跳过字段类型/语义、Agent schema、revision 冲突、文件权限、原子写入或 secret CAS。正常运行态仍要求每次变更通过完整启动预检。配置页将本地正式启动预检与外部连接测试分开：连接测试必须由管理员显式触发，只允许访问登记 Provider 的官方 HTTPS 主机和模型列表端点，使用 8 秒超时、禁止重定向，不发送聊天内容，也不覆盖现有配置；自定义兼容地址仅参加本地预检，避免引入任意 URL/内网 SSRF。
