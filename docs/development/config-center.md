# 配置中心设计与字段清单

配置中心把现有文件和环境变量逐步收敛到明确的来源模型，不删除高级部署能力，也不把普通配置复制到 SQLite。实际字段解析仍以源码与 [`runtime/config/.env.example`](../../runtime/config/.env.example) 为准。

## 权威存储与优先级

```text
进程环境变量 / dotenv 字段级强制覆盖
                    ↓
普通值：config/runtime.toml    敏感值：SQLite 认证加密密文
                    ↓
                 安全默认值
```

- `config/runtime.toml` 是程序专用受管文件，WebUI 和人工编辑的是同一文件，不存在“网页值”和“文件值”两份事实来源。程序写回会规范化格式，需保留复杂注释和结构的 Agent/ops 策略继续使用各自文件。
- 进程环境先于 `config/.env` 和 `.env`，dotenv 仅补缺失项；dotenv 文件不存在是正常输入。外部同名字段存在时强制覆盖受管值，安全快照会标记 `source=environment` 与 `overridden=true`。
- `agent.toml` 的模型路线、scene 和 profile 继续按其专用 resolver 处理；`ops.toml` 继续是禁止通用 WebUI 编辑的高风险部署配置。
- 当前纳入配置中心的字段都标记为重启生效。受管文件写入使用内容 SHA-256 revision、整份校验、同目录临时文件、同步和原子替换；人工并发修改会返回 `config_conflict`。

## 敏感值与主密钥

API Key、Token、AppSecret、EncodingAESKey 等敏感值使用 XChaCha20-Poly1305 认证加密后写入统一 SQLite。记录包含算法、版本、24 字节 nonce、认证密文和更新时间；字段稳定 key 作为附加认证数据。普通读取接口只返回是否已配置，不返回原文。

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
| Provider | `provider.mode`、`provider.main_model`、各内置 Provider 的 base URL/model/API mode | OpenAI、DeepSeek、BigModel、Gemini、MiMo API Key |
| Core 功能 | RSS、Memory、Todo、Tool Calling 开关与 Todo 提醒时间 | `weather.qweather.api_key` |
| 控制台 | `console.enabled`、`console.allowed_origins` | 无 |
| QQ 官方 | `platform.qq_official.enabled` | AppID、AppSecret |
| OneBot 11 | enabled、bind host/port、WebSocket path | Access Token |
| 微信服务号 | enabled、encryption mode、bind host/port、callback path | Token、AppID、AppSecret、EncodingAESKey |

监听地址/端口、数据库路径、受管文件路径、主密钥路径、Agent/ops 文件路径和 `/ops` 执行规则属于 Bootstrap 或高风险部署项，只允许通过明确的文件/环境配置管理。尚未登记的历史环境变量继续由原 resolver 读取，行为不变；新增迁移字段时必须同步字段注册表、`.env.example` 和测试。

## 管理接口边界

启用控制台后，`GET /api/v1/console/configuration` 返回安全配置快照，包括 revision、来源、覆盖状态、可编辑性、生效方式和脱敏配置状态。#512 管理员认证接入前不注册配置写路由；后端领域方法已区分普通值 set/remove 与 secret replace/clear，不能把脱敏占位符当作真实 secret 保存。
