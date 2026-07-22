# Docker 与 Compose 部署

Docker 是服务器推荐部署方式。GitHub Actions 在固定的 Debian 13/Rust 构建链路中生成镜像，
服务器只拉取并运行镜像，不安装 Rust、Cargo、Node.js，也不现场编译。现有 Release 包、
`qbot` / `botctl` 和源码部署继续保留。

## 运行模型

- `Dockerfile` 使用固定版本及 digest 的 Rust builder 和 Debian runtime；构建命令为
  `cargo build --workspace --release --all-features --locked`。
- runtime 只包含统一 `qq-maid-bot`、CA、时区、C++ 运行库和 `/healthz` 所需的 curl，
  不包含 Rust、Cargo 或 Node.js。
- `qq-maid-bot` 直接作为 PID 1，以 `10001:10001` 非 root 身份运行；`docker stop`
  发送 SIGTERM，并由现有 shutdown 链路在 Compose 的 20 秒宽限期内退出。
- 根文件系统只读，所有 Linux capabilities 均移除，不使用 privileged、host network、
  Docker socket 或 systemd。
- 应用日志只写 stdout/stderr，通过 `docker compose logs` 读取。
- OCI labels 记录仓库、构建 commit、构建时版本标识和构建时间；启动日志也会记录
  Cargo 版本与 commit。

基础镜像支持 `linux/amd64` 与 `linux/arm64`。发布工作流在对应架构的原生 GitHub runner
分别构建，只有两个架构都成功才生成多架构 manifest。具体 digest、压缩镜像字节数和
实际发布架构以 Container workflow 的 Summary 为准；不要把未成功发布的平台视为已验证。

## 安装前提

安装受支持版本的 [Docker Engine](https://docs.docker.com/engine/install/) 和
[Docker Compose plugin](https://docs.docker.com/compose/install/linux/)，然后确认：

```bash
docker version
docker compose version
```

Docker daemon 应由 systemd 或发行版服务管理。`docker` 组可以启动 privileged 容器、挂载
宿主机根目录，因此其权限事实上接近宿主机 root。把 SSH 登录用户设为非 root，只能减少
普通文件和 Shell 操作的默认权限，不能构成对宿主机的完整权限隔离；该账号必须只供部署使用，
使用专用 SSH key，不能与普通账号共用。

需要进一步隔离时，可以评估 [rootless Docker](https://docs.docker.com/engine/security/rootless/)，
或在 `authorized_keys` 为 CI key 配置 SSH `command="..."` forced-command，由受审计的入口只
允许上传指定部署文件和执行固定 digest 部署命令。forced-command 需要同时处理当前工作流的
SCP 与 SSH 调用，启用前应在独立测试账号验证；不要直接把任意远程参数拼接到 Shell。

## 本地首次启动

从仓库根目录准备实例目录：

```bash
cp compose.env.example compose.env
mkdir -p runtime/config/secrets runtime/data/storage runtime/media/inbound
touch runtime/config/.env
chmod 700 runtime/config/secrets
chmod 600 runtime/config/.env
sudo chown -R 10001:10001 runtime/config runtime/data runtime/media
```

编辑 `compose.env`，把 `QQ_MAID_IMAGE` 替换为版本标签或 digest。正式环境优先使用：

```text
QQ_MAID_IMAGE=ghcr.io/kuliantnt/qq-maid-bot@sha256:<64位digest>
```

应用配置字段继续以 [`runtime/config/.env.example`](../../runtime/config/.env.example) 和
[配置中心清单](../development/config-center.md) 为准，不在本文复制。空 `.env` 可以进入
`setup_required` 管理状态；程序只在缺失时创建默认 `agent.toml`、`runtime.toml`、
`secrets/master.key` 和 Bootstrap token，不覆盖已有文件。

校验并启动：

```bash
docker compose --env-file compose.env config --quiet
docker compose --env-file compose.env up -d --wait --wait-timeout 140
docker compose --env-file compose.env ps
docker compose --env-file compose.env logs -f bot
```

`/healthz` 返回 HTTP 200 表示进程可服务；未完成配置时 JSON 中会是
`state=setup_required`、`ready=false`。容器 healthy 不代表 QQ、LLM 或外部平台已经连通。

## 端口与监听地址

基础 `compose.yaml` 不映射任何宿主机端口。按实际入口叠加一个或多个小型 override：

```bash
# 本机控制台，默认映射到宿主机 127.0.0.1:8787
docker compose --env-file compose.env \
  -f compose.yaml -f compose.console.yaml up -d

# 微信回调
docker compose --env-file compose.env \
  -f compose.yaml -f compose.wechat.yaml up -d

# OneBot 反向 WebSocket
docker compose --env-file compose.env \
  -f compose.yaml -f compose.onebot.yaml up -d
```

映射端口时，容器内相应监听地址必须改为 `0.0.0.0`：控制台使用
`LLM_SERVER_HOST=0.0.0.0`，微信使用 `WECHAT_SERVICE_BIND_HOST=0.0.0.0`，OneBot 使用
`ONEBOT11_BIND_HOST=0.0.0.0`。宿主机绑定地址和端口由 `compose.env` 中对应的
`QQ_MAID_*_HOST` / `QQ_MAID_*_PORT` 控制。

控制台默认仍应绑定宿主机回环地址，可通过 SSH tunnel 使用。微信公网回调应由受信 TLS
反向代理接入；不要为了一个入站平台映射另外两个端口，也不要直接把控制台暴露到公网。

## 持久化、权限与备份

| 宿主机路径 | 容器路径 | 权限与内容 |
| --- | --- | --- |
| `QQ_MAID_CONFIG_DIR` | `/app/runtime/config` | UID/GID 10001 可写；`.env`、受管配置、Prompt、知识库、主密钥 |
| `QQ_MAID_DATA_DIR` | `/app/runtime/data` | UID/GID 10001 可写；SQLite 和本地模型缓存 |
| `QQ_MAID_MEDIA_DIR` | `/app/runtime/media` | UID/GID 10001 可写；入站媒体临时数据 |

至少同时备份 SQLite 和 `config/secrets/master.key`；只备份数据库无法恢复其中的加密配置。
一致性备份应先停止容器，或使用 SQLite 在线备份能力。真实 `.env`、主密钥、Prompt、知识
资料、SQLite 和日志不得进入镜像、Git、Artifact 或 Actions 日志。

重建容器不会修改 bind mount 中的已有文件。升级前检查目录仍由 `10001:10001` 读写；
不要用 root 容器或 `chmod 777` 掩盖权限错误。

## 多实例

每个实例使用独立的 Compose project、目录、账号、API Key、主密钥、数据库和宿主机端口。
不需要复制 Compose 模板，只需要独立变量文件：

```text
/opt/qq-maid-a/compose.env
/opt/qq-maid-a/runtime/{config,data,media}
/opt/qq-maid-b/compose.env
/opt/qq-maid-b/runtime/{config,data,media}
```

两个 `compose.env` 分别设置不同的 `COMPOSE_PROJECT_NAME` 和绝对 `QQ_MAID_*_DIR`。需要
映射入口时再设置不同宿主机端口，并复用仓库中的同一 `compose.yaml` 与入口 override。

## Registry 标签与发布流程

发布模型遵循 build once, deploy many：

| 触发 | 容器行为 |
| --- | --- |
| PR | 校验 Compose 和部署脚本，在 amd64/arm64 原生 runner 构建并加载镜像；amd64 执行完整容器运行契约，arm64 执行基础启动与健康检查；不推送、不部署 |
| `master` | 仅向 GHCR 发布 `sha-<完整commit>` 多架构镜像，输出 digest 并更新可变 `master`；只有仓库变量 `TEST_DEPLOY_ENABLED=true` 时才按 digest 部署测试服 |
| `vX.Y.Z` | 确认 tag 位于 `master`、该 commit 已成功部署到 test、GHCR `sha-<commit>` 存在且双架构 revision label 一致；不重新构建，把同一 manifest 的 `vX.Y.Z`、`X.Y`、`X`、`latest` 同步到 GHCR 与 Docker Hub，并校验两个 Registry 的实际 digest |

GHCR 镜像为 `ghcr.io/kuliantnt/qq-maid-bot`；Docker Hub 正式镜像为
`docker.io/kuliantnt/qq-maid-bot`。Docker Hub 不接收 `sha-*` 或 `master`。`master` 和
`latest` 都是可变标签，不能作为回滚依据。测试服和正式环境均应把完整
`仓库@sha256:digest` 记录为部署事实。tag 对应的 commit 镜像不存在时 Release workflow
明确失败，不会在 tag 阶段补建另一份镜像。任一 Registry 登录、复制、打标签或 digest
校验失败，正式发布 job 都会失败，不能视为双渠道发布成功。现有 GitHub Release 原生包仍由
同一个 tag 工作流生成；创建 tag 不会自动更新正式服务器。

commit 镜像在 `master` 阶段已经构建，tag 阶段无法在保持同一 digest 的同时修改 OCI label。
`org.opencontainers.image.revision` 始终是 40 位 Git commit；
`org.opencontainers.image.version` 保留构建时的 `sha-*` 标识，不代表后来追加的正式 tag。
正式 `vX.Y.Z` 应从部署命令记录的 release 或所选镜像 tag/digest 对照中读取。

GHCR 包公开时无需登录。私有包应使用只含 `read:packages` 的独立凭据：

```bash
printf '%s' "${GHCR_READ_TOKEN}" | docker login ghcr.io -u "${GHCR_USERNAME}" --password-stdin
```

不要把仓库写权限或个人高权限 Token 长期留在服务器。

正式发布还需要在仓库 Actions Secrets 配置 Docker Hub 的专用访问令牌：

- `DOCKERHUB_USERNAME`
- `DOCKERHUB_TOKEN`

令牌只用于 Release workflow 同步正式标签，不参与 PR/master 构建，也不会把 GHCR 的
`sha-*` 或 `master` 标签同步到 Docker Hub。

## 阿里云测试服初始化

服务器不克隆仓库、不安装构建工具。先按 Docker 官方步骤安装 Engine 和 Compose plugin，
把 `scripts/docker-host-init.sh` 与专用 SSH 公钥临时传到服务器，再以 root 执行一次：

```bash
sudo bash docker-host-init.sh \
  --app-dir /opt/qq-maid-bot-test \
  --project-name qq-maid-bot-test \
  --authorized-key-file /tmp/qq-maid-test-deploy.pub
```

脚本可重复执行，会检查 Docker，创建 UID/GID `10001` 的非 root `qqmaid` 用户、独立目录、
空 `.env` 和 `compose.env`；已有配置不会覆盖。完成后删除临时公钥文件，填写：

```text
/opt/qq-maid-bot-test/runtime/config/.env
```

如果 GHCR 包是私有的，以 `qqmaid` 用户执行一次只读 `docker login`。测试与正式环境必须
使用不同的 Bot 账号、数据库、API Key、主密钥、目录和端口。

GitHub Environment 创建 `test`，配置 Secrets：

- `TEST_DEPLOY_HOST`
- `TEST_DEPLOY_PORT`
- `TEST_DEPLOY_USER`（默认初始化脚本创建的是 `qqmaid`）
- `TEST_DEPLOY_SSH_KEY`
- `TEST_DEPLOY_KNOWN_HOSTS`

仓库 variable `TEST_DEPLOY_ENABLED` 默认视为未启用；只有明确设置为 `true`，master Container
workflow 才进入 `test` Environment。服务器和上述 Secrets 尚未初始化时应保持关闭，GHCR
commit 镜像仍会正常构建发布。不要用 Secret 是否为空作为部署开关。

可选 Environment variable `TEST_DEPLOY_DIR`，默认 `/opt/qq-maid-bot-test`。`KNOWN_HOSTS`
必须由可信通道预先取得，工作流不会运行 `ssh-keyscan` 临时信任未知主机。

## 自动部署、健康检查与回滚

`master` 镜像的两个架构发布成功且 `TEST_DEPLOY_ENABLED=true` 后，工作流：

1. 合成并记录不可变 commit manifest digest。
2. 通过专用 SSH key 上传 `compose.yaml` 与 `docker-deploy.sh`。
3. 把完整 `仓库@digest` 和 commit 交给服务器。
4. 校验仓库和 digest 格式，拉取镜像并检查 OCI revision label。
5. 原子更新 `.image.env`，执行 `docker compose up -d --pull never`。
6. 等待容器进入 healthy，记录 commit、可选正式 release、构建时 label、digest、容器 ID 和
   部署时间。
7. 启动或健康检查失败时恢复上一 `.image.env`，重新启动并确认旧版本 healthy。

重复部署同一 digest 且容器 healthy 时直接成功返回，不产生无意义重建。部署并发组会取消
旧的测试部署，避免旧 commit 最后覆盖新 commit。首次部署没有回滚点，失败时会明确报告。

服务器查看当前版本、digest、健康和资源占用：

```bash
cd /opt/qq-maid-bot-test
./docker-deploy.sh status
cat deployments/current.env
docker compose --env-file compose.env --env-file .image.env logs --tail 200 bot
```

正式环境不跟随 `master` 或 tag 自动更新。准备独立实例后，显式选择测试通过的版本 digest，
再使用同一部署脚本：

```bash
./docker-deploy.sh deploy \
  --image ghcr.io/kuliantnt/qq-maid-bot@sha256:<digest> \
  --commit <40位commit> \
  --release vX.Y.Z
```

`status` 中 `release` 来自这份部署记录；`build_version_label` 是镜像构建时不可变的 OCI
label。未传 `--release` 时会显示 `unrecorded` / `unreleased`，不会把 `sha-*` 误称为正式版本。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| `permission denied` | bind mount 的目录和文件是否允许 UID/GID 10001 读写；不要改成 root 运行 |
| 容器长期 `unhealthy` | `docker compose logs bot`；确认 `LLM_SERVER_PORT` 与 healthcheck 一致，进程未在启动前退出 |
| 控制台端口连不上 | 是否加载 `compose.console.yaml`，且 `.env` 中 `LLM_SERVER_HOST=0.0.0.0` |
| 微信/OneBot 连不上 | 是否只加载对应 override、设置容器内 bind host、配置防火墙/反向代理 |
| GHCR `unauthorized` | 包可见性与服务器只读 `docker login`；不要把 token 写进 Compose |
| 拉取后 revision 不一致 | 拒绝部署，检查 tag/digest 是否来自本仓库的 Container workflow |
| 新版本 unhealthy | `docker-deploy.sh` 会恢复上一 digest；检查新旧错误和回滚 healthy 结果 |
| 主密钥丢失 | 从与 SQLite 同期的备份恢复；不能重新生成密钥解密旧数据 |

源码/Release 部署以 `runtime/` 为宿主机工作目录，由 `botctl` 管理进程和日志文件；容器部署
以 `/app/runtime` 为工作目录，由 Compose 管理进程，日志走 stdout/stderr，配置通过 bind mount
和 env file 注入，端口默认不映射。三种方式共用同一 Rust 二进制、配置字段、SQLite schema
和 `/healthz` 语义。
