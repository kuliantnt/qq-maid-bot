# 配置迁移、备份恢复与安全升级

统一二进制提供无浏览器也能使用的运维 CLI。源码、Release 与 Docker 使用相同的配置注册表、
SQLite migration 和备份格式；Web 控制台仍只是另一种交互入口。命令应在实例运行目录执行，
也就是能看到 `config/`、`data/` 的目录；Docker 中对应 `/app/runtime`。

## 运行方式与配置边界

| 方式 | 启动 | 配置与数据 | 日志、健康和重启 |
| --- | --- | --- | --- |
| 源码 | 仓库根目录 `make local` | 默认 `runtime/config`、`runtime/data` | 终端或 `runtime/logs`；`/healthz`；开发进程重启 |
| Release | 运行目录 `botctl` | 同目录 `config`、`data` | `botctl logs/health/restart` |
| 直接 CLI | `qq-maid-bot run` 或无参数 | 当前目录 `config`、`data` | stdout/stderr；`/healthz`；由宿主管理 |
| Docker | `docker compose up -d` | bind mount 到 `/app/runtime/config`、`data`、`media` | `docker compose logs/ps/restart` |
| Web 管理 | 只管理已登记字段，不是独立运行方式 | 普通值写受管 TOML，secret 加密写 SQLite | 显示来源与待重启状态，不控制 Docker socket |

环境变量与 dotenv、`runtime.toml`、`agent.toml`、`ops.toml` 的字段规则以
[配置中心清单](../development/config-center.md)为准。Web/Docker 都不是正常启动的硬依赖；
完整的旧文件配置仍可直接运行，配置不完整时则进入 `setup_required`。

新装 Release 时，`qbot install` 会在交互终端询问是否启用 Web；选择否会持久化
`WEB_CONSOLE_ENABLED=false`，之后使用 `qbot config` 和 `config/.env` 即可。自动化安装使用
`qbot install --web false` 或 `QBOT_INSTALL_WEB_CONSOLE=false`，不会等待输入；未显式设置的
非交互安装为兼容旧行为默认启用。重复安装不会改写已有选择，除非再次显式传入 `--web`。
部署侧显式关闭具有安全优先级：即使旧的配置中心数据曾保存为开启，启动后也不会注册
登录页、认证或配置 API，也不会生成 Bootstrap token。

## 只读预检和来源

```bash
./qq-maid-bot migration status
./qq-maid-bot config check
./qq-maid-bot config sources
```

- `migration status` 只读显示数据库是否存在、已应用/待应用/未知 migration 数和最后成功时间。
- `config check` 在数据库或配置中心尚未初始化时保持只读，不会为了检查创建文件或执行 migration；
  已完整初始化时复用真实 Core、Provider 与 Gateway 预检。
- `config sources` 只显示字段来源、配置/覆盖/有效/待重启状态，不输出 secret 或普通字段原值。
- 数据库包含当前二进制不认识的 migration 时，启动和 CLI 都返回
  `schema_incompatible`。这通常表示正在尝试用旧镜像读取新 schema，应使用兼容镜像或恢复同期备份。

## 旧 dotenv 保守迁移

默认只生成脱敏报告，不修改任何文件：

```bash
./qq-maid-bot config migrate
./qq-maid-bot config migrate \
  --env-file config/.env \
  --env-file .env
```

报告将登记字段分为：

- `import_managed`：可写入 `runtime.toml` 的普通值；
- `import_secret_redacted`：可认证加密写入 SQLite 的 secret，报告不显示原文；
- `conflict_keep_managed`：已有 Web/TOML/密文值，保留现值而不覆盖；
- `keep_external`：数据库路径、监听、`agent.toml`/`ops.toml` 路径等 Bootstrap 或受限项继续外部管理；
- `invalid_redacted`：值不合法，修正前拒绝实际导入。

确认后显式执行：

```bash
./qq-maid-bot config migrate --apply
```

导入只填补空缺，具有 revision/CAS 检查，可重复执行；原 `.env`、`agent.toml`、`ops.toml`
不会被修改、删除或强制停用。普通文件与 SQLite 无法共享同一个事务；若中途失败，命令明确报错，
再次执行只继续填补剩余空缺，不覆盖已经成功的部分。

## 数据库备份、配置恢复包与完整部署备份

CLI 使用 SQLite Online Backup API，可在有 WAL 写入时取得数据库一致快照，并复制配置目录内
允许纳入的文件。输出是包含 `manifest.toml` 与逐文件 SHA-256 的新目录：

```bash
./qq-maid-bot backup create --output /secure/backup-20260722
./qq-maid-bot backup verify --from /secure/backup-20260722
```

边界必须区分：

- `database/app.db` 是数据库备份，包含 Todo、Session、Memory、RSS、管理员认证以及受管 secret
  的密文；它不包含解密主密钥，不能独立读取加密受管配置。
- 默认 CLI 恢复包还复制当前配置目录，但排除 `.env`、整个 `secrets/` 和一次性 Bootstrap token。
- `--include-secrets` 允许复制当前配置目录内的 `.env` 与 `secrets/`（Bootstrap token 始终排除）：

```bash
./qq-maid-bot backup create \
  --output /secure/full-backup-20260722 \
  --include-secrets
```

该选项不把主密钥“塞进数据库”，只会复制配置目录内原本存在的密钥文件。若
`MASTER_KEY_FILE` 指向配置目录外，主密钥不会进入恢复包，必须通过独立安全通道保存和恢复。
含敏感材料的恢复包在 Unix 上限制为目录 `0700`、文件 `0600`，仍应使用受控加密介质离线保存。

上述两种 CLI 恢复包都不是完整部署备份：它们不包含二进制或容器镜像、Compose、
`compose.env`/`.image.env`、media、服务管理配置，也不包含配置目录外的 Agent/Ops/Prompt/知识文件
或外部 secret。完整部署灾备必须在恢复包之外另行保存这些部署材料和路径映射。不要把任何恢复包
放入 Git、普通 Artifact、公开对象存储或与实例相同的单一磁盘。

## 恢复

恢复先校验 manifest、所有摘要、SQLite `integrity_check` 和 migration 兼容性。默认 dry-run：

```bash
./qq-maid-bot backup restore \
  --from /secure/full-backup-20260722 \
  --target /opt/qq-maid-restored
```

停掉原实例并确认目标目录不存在或为空后：

```bash
./qq-maid-bot backup restore \
  --from /secure/full-backup-20260722 \
  --target /opt/qq-maid-restored \
  --apply
```

恢复固定写入新实例的 `data/storage/app.db` 与 `config/`，不会覆盖当前运行目录，也不会在打开的
SQLite inode 旁替换文件。数据库存在加密受管配置时，必须把同期主密钥恢复到默认
`config/secrets/master.key` 或 `MASTER_KEY_FILE` 指定位置：缺失时启动明确报错且不会生成新密钥，
密钥不匹配时认证解密明确失败。补齐外部 secret 和部署文件后，在新目录运行 `config check`，
再启动服务。原实例在新实例验证完成前应保持停止但不要删除。

建议至少做一次受控演练：创建 Todo、Session、Memory、RSS 数据，备份后修改原实例，恢复到
干净目录，再用当前版本打开数据库并验证四类数据仍可读取。没有真实平台凭据时，这不等于
QQ/OneBot/微信或 Provider 联调成功。

## Docker 升级与回滚

`scripts/docker-deploy.sh deploy` 只接受受信仓库的 digest。已有实例升级时的顺序为：

1. 拉取目标 digest，核对 OCI commit；
2. 使用目标镜像和当前持久化卷创建 `data/backups/pre-upgrade-*` 数据库与配置恢复包；
3. 原子切换镜像引用并重建容器；
4. 等待 `/healthz`；
5. 失败时自动恢复上一 digest，并让旧镜像继续使用当前数据卷；数据不会自动恢复；
6. 旧镜像若因新 schema 无法健康启动，脚本明确失败。保留旧 digest，手工把升级前恢复包恢复到
   另一个干净实例目录，补齐 Compose、镜像状态和包外部署文件后再启动旧镜像。

升级前恢复包允许包含配置目录内的 secret，且仍位于实例数据卷；成功后应转移到加密离线介质。
它不等于完整部署备份，配置目录外的主密钥或其他部署文件仍需独立保存。只有显式设置
`DEPLOY_BACKUP_BEFORE_UPGRADE=false` 才会跳过，脚本会输出风险警告。镜像回滚和 schema
恢复不是一件事：旧二进制能读取当前 schema 时才可直接回滚镜像，否则必须恢复与旧版本同期的
备份。多实例必须分别执行，不能复用数据目录、主密钥、Compose project 或备份目标。

完整容器端口、权限、digest 与测试服流程见 [Docker 与 Compose 部署](./docker.md)。
