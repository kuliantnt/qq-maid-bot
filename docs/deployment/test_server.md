# qq-maid-bot 测试服务器 Docker 部署教程

## 一、最终效果

部署完成后，流程是：

```text
代码合并到 master
    ↓
GitHub Actions 构建 amd64/arm64 镜像
    ↓
推送 GHCR 的 sha-<commit> 镜像
    ↓
阿里云按精确 digest 自动更新
    ↓
健康检查失败则自动恢复旧镜像
```

阿里云服务器只运行容器，不安装 Rust、Cargo、Node.js，也不在服务器编译项目。

建议先完成一次手动部署，确认镜像、配置和权限正确，再打开 GitHub 自动部署。

---

## 二、准备条件

服务器建议：

```text
Ubuntu 22.04 / 24.04
2 核 2 GB
amd64
至少 10 GB 可用磁盘
开放 SSH 端口
```

QQ 官方机器人使用主动 WebSocket 连接时，不需要开放应用端口。

需要控制台、微信回调或 OneBot 入站时，再单独配置端口。

---

## 三、安装 Docker

在阿里云服务器执行：

```bash
sudo apt update
sudo apt install -y ca-certificates curl

sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
  -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc

sudo tee /etc/apt/sources.list.d/docker.sources >/dev/null <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

sudo apt update
sudo apt install -y \
  docker-ce \
  docker-ce-cli \
  containerd.io \
  docker-buildx-plugin \
  docker-compose-plugin

sudo systemctl enable --now docker
```

验证：

```bash
sudo docker run --rm hello-world
sudo docker version
sudo docker compose version
```

这是 Docker 官方当前推荐的 Ubuntu 安装方式；Compose 使用 `docker compose` 插件，而不是旧的独立 `docker-compose` 命令。

---

## 四、生成专用部署密钥

在自己的电脑执行：

```bash
ssh-keygen \
  -t ed25519 \
  -f ~/.ssh/qq-maid-test-deploy \
  -C "qq-maid GitHub Actions test deploy"
```

生成：

```text
~/.ssh/qq-maid-test-deploy
~/.ssh/qq-maid-test-deploy.pub
```

私钥以后放到 GitHub Secret，公钥安装到阿里云。

不要复用日常登录私钥。

### 记录服务器 SSH 指纹

先在阿里云服务器执行：

```bash
sudo ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
```

在本机执行：

```bash
ssh-keyscan -p 22 -t ed25519 <阿里云IP> \
  > ~/.ssh/qq-maid-test-known_hosts

ssh-keygen -lf ~/.ssh/qq-maid-test-known_hosts
```

确认两边指纹一致。

不要让 GitHub Actions 在部署时临时执行 `ssh-keyscan` 并无条件信任结果。

---

## 五、初始化服务器目录和部署用户

在本地项目根目录执行：

```bash
scp scripts/docker-host-init.sh \
  ~/.ssh/qq-maid-test-deploy.pub \
  <你的管理员账号>@<阿里云IP>:/tmp/
```

登录服务器：

```bash
ssh <你的管理员账号>@<阿里云IP>
```

初始化：

```bash
sudo bash /tmp/docker-host-init.sh \
  --app-dir /opt/qq-maid-bot-test \
  --project-name qq-maid-bot-test \
  --authorized-key-file /tmp/qq-maid-test-deploy.pub
```

清理临时文件：

```bash
rm -f /tmp/docker-host-init.sh
rm -f /tmp/qq-maid-test-deploy.pub
```

脚本会创建：

```text
用户：qqmaid
UID/GID：10001:10001

/opt/qq-maid-bot-test/
├── compose.env
├── runtime/
│   ├── config/
│   │   ├── .env
│   │   └── secrets/
│   ├── data/
│   │   └── storage/
│   └── media/
│       └── inbound/
└── deployments/
```

部署用户会加入 `docker` 组。需要知道：Docker 组事实上拥有接近宿主机 root 的权限，因此该账号必须是专用部署账号，不能和日常登录账号混用。

---

## 六、填写测试环境配置

编辑：

```bash
sudo nano /opt/qq-maid-bot-test/runtime/config/.env
```

测试环境至少使用独立的：

```text
QQ Bot AppID 和 Secret
LLM API Key
数据库
主密钥
管理密码
平台回调配置
```

不要直接使用正式 Bot 账号和正式数据库。

QQ 官方机器人基础示意：

```dotenv
QQ_BOT_ENABLED=true
QQ_BOT_APP_ID=测试机器人AppID
QQ_BOT_APP_SECRET=测试机器人Secret

ONEBOT11_ENABLED=false
WECHAT_SERVICE_ENABLED=false

OPENAI_API_KEY=测试环境APIKey
OPENAI_BASE_URLS=https://你的接口地址
OPENAI_API_MODE=auto

LLM_SERVER_HOST=127.0.0.1
LLM_SERVER_PORT=8787

RUST_LOG=info,qq_maid_gateway_rs=debug
```

收紧权限：

```bash
sudo chown qqmaid:qqmaid \
  /opt/qq-maid-bot-test/runtime/config/.env

sudo chmod 600 \
  /opt/qq-maid-bot-test/runtime/config/.env
```

程序缺少配置文件时可以进入 `setup_required`，并创建默认配置和 Bootstrap 信息；已有文件不会被容器更新覆盖。

---

## 七、先手动部署一次

### 1. 等待 master 镜像生成

PR #562 合并后，下一次 `master` Container workflow 会发布：

```text
ghcr.io/kuliantnt/qq-maid-bot:sha-<40位commit>
```

打开 GitHub Actions 的 Container workflow，在 Summary 中复制：

```text
commit SHA
manifest digest
```

最终镜像引用类似：

```text
ghcr.io/kuliantnt/qq-maid-bot@sha256:abc123...
```

不要用 `master` 标签作为部署事实；应使用完整 digest。

### 2. 上传部署文件

在本地项目根目录执行：

```bash
scp \
  -i ~/.ssh/qq-maid-test-deploy \
  compose.yaml \
  scripts/docker-deploy.sh \
  qqmaid@<阿里云IP>:/opt/qq-maid-bot-test/
```

然后登录：

```bash
ssh \
  -i ~/.ssh/qq-maid-test-deploy \
  qqmaid@<阿里云IP>
```

设置权限：

```bash
cd /opt/qq-maid-bot-test
chmod 755 docker-deploy.sh
```

### 3. 私有 GHCR 镜像登录

GHCR 镜像公开时跳过本步骤。

私有镜像需要一个只有 `read:packages` 权限的 Token：

```bash
printf '%s' "$GHCR_READ_TOKEN" |
  docker login ghcr.io \
    -u <GitHub用户名> \
    --password-stdin
```

不要在服务器保存具有仓库写权限的高权限 Token。

### 4. 执行部署

```bash
cd /opt/qq-maid-bot-test

./docker-deploy.sh deploy \
  --image ghcr.io/kuliantnt/qq-maid-bot@sha256:<完整digest> \
  --commit <对应的40位commit>
```

成功时脚本会：

```text
校验镜像仓库和 digest
拉取镜像
检查 OCI revision 是否等于目标 commit
启动容器
等待 /healthz 进入 healthy
记录部署状态
```

查看状态：

```bash
./docker-deploy.sh status
cat deployments/current.env
```

查看日志：

```bash
docker compose \
  --env-file compose.env \
  --env-file .image.env \
  logs --tail 200 bot
```

持续查看：

```bash
docker compose \
  --env-file compose.env \
  --env-file .image.env \
  logs -f bot
```

查看容器：

```bash
docker compose \
  --env-file compose.env \
  --env-file .image.env \
  ps
```

---

## 八、配置 GitHub 自动部署

手动部署成功后，再开启自动部署。

### 1. 创建 GitHub Environment

进入仓库：

```text
Settings
→ Environments
→ New environment
→ test
```

Environment 可以隔离测试部署的 Secrets，并可配置审批和分支保护规则。

### 2. 添加 Environment Secrets

在 `test` Environment 添加：

```text
TEST_DEPLOY_HOST
TEST_DEPLOY_PORT
TEST_DEPLOY_USER
TEST_DEPLOY_SSH_KEY
TEST_DEPLOY_KNOWN_HOSTS
```

示例值：

```text
TEST_DEPLOY_HOST=服务器IP
TEST_DEPLOY_PORT=22
TEST_DEPLOY_USER=qqmaid
```

`TEST_DEPLOY_SSH_KEY` 填入私钥完整内容：

```bash
cat ~/.ssh/qq-maid-test-deploy
```

`TEST_DEPLOY_KNOWN_HOSTS` 填入：

```bash
cat ~/.ssh/qq-maid-test-known_hosts
```

Secrets 应放在 Environment 中，而不是提交到仓库或 Compose 文件。

### 3. 开启仓库变量

进入：

```text
Settings
→ Secrets and variables
→ Actions
→ Variables
→ New repository variable
```

添加：

```text
TEST_DEPLOY_ENABLED=true
```

这是非敏感开关，因此使用 Variable，不使用 Secret。GitHub Variables 适合保存非敏感工作流配置。

可选：

```text
TEST_DEPLOY_DIR=/opt/qq-maid-bot-test
```

默认就是该路径，可以不设置。

### 4. 触发自动部署

下一次代码合并到 `master` 后，Container workflow 会：

```text
构建两个架构镜像
→ 生成 commit manifest
→ 推送 GHCR
→ 上传 compose.yaml 和 docker-deploy.sh
→ SSH 登录测试服
→ 按精确 digest 部署
→ 等待 healthy
→ 失败时回滚
```

`workflow_dispatch` 当前主要用于构建验证，不要把它当成测试服部署入口。正常测试部署由 `master` push 触发。

---

## 九、访问 Web 控制台

基础 `compose.yaml` 默认不映射任何宿主机端口。

需要临时使用控制台时：

### 1. 修改容器内监听地址

编辑测试 `.env`：

```dotenv
LLM_SERVER_HOST=0.0.0.0
LLM_SERVER_PORT=8787
```

### 2. 上传控制台 override

```bash
scp \
  -i ~/.ssh/qq-maid-test-deploy \
  compose.console.yaml \
  qqmaid@<阿里云IP>:/opt/qq-maid-bot-test/
```

### 3. 使用 override 重建

在服务器执行：

```bash
cd /opt/qq-maid-bot-test

docker compose \
  --env-file compose.env \
  --env-file .image.env \
  -f compose.yaml \
  -f compose.console.yaml \
  up -d --wait
```

默认只映射到宿主机：

```text
127.0.0.1:8787
```

本机建立 SSH 隧道：

```bash
ssh \
  -i ~/.ssh/qq-maid-test-deploy \
  -L 8787:127.0.0.1:8787 \
  qqmaid@<阿里云IP>
```

然后浏览器访问：

```text
http://127.0.0.1:8787
```

不要直接把管理控制台暴露到公网。

注意：当前自动部署脚本只使用基础 `compose.yaml`。下一次自动部署可能移除手工加载的控制台端口 override。需要长期保留控制台时，应让部署脚本支持固定的 Compose override 列表。

---

## 十、回滚和排障

### 自动回滚

新镜像出现以下情况时会自动尝试恢复旧镜像：

```text
拉起失败
容器未进入 healthy
容器进入 unhealthy
```

首次部署没有旧镜像，因此第一次失败无法自动回滚。

### 手工回滚

记录上一版：

```text
旧 digest
旧 commit SHA
```

执行：

```bash
./docker-deploy.sh deploy \
  --image ghcr.io/kuliantnt/qq-maid-bot@sha256:<旧digest> \
  --commit <旧commit>
```

### 常用检查

```bash
cd /opt/qq-maid-bot-test

./docker-deploy.sh status

docker compose \
  --env-file compose.env \
  --env-file .image.env \
  ps

docker compose \
  --env-file compose.env \
  --env-file .image.env \
  logs --tail 300 bot

docker inspect \
  --format '{{.State.Health.Status}}' \
  "$(docker compose \
      --env-file compose.env \
      --env-file .image.env \
      ps -q bot)"
```

### 权限错误

```bash
sudo chown -R 10001:10001 \
  /opt/qq-maid-bot-test/runtime/config \
  /opt/qq-maid-bot-test/runtime/data \
  /opt/qq-maid-bot-test/runtime/media
```

不要使用：

```bash
chmod -R 777
```

### 备份

至少备份：

```text
runtime/data/
runtime/config/
```

特别是：

```text
runtime/config/secrets/master.key
SQLite 数据库
```

数据库中的敏感配置由主密钥加密，只备份数据库、不备份主密钥，恢复后可能无法解密旧配置。
