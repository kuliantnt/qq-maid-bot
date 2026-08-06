# Docker 部署 · 小白版

> 想 5 分钟把机器人跑起来，看这篇就够了。
> 想看完整细节（安全加固、多实例、自动发布、回滚）再看[完整版](./docker.md)。

## 一句话说明

你的服务器**只需要装 Docker**，不用装 Rust、不用装 Node.js、不用现场编译。
我们把程序打包成了现成镜像，你拉下来、跑起来，然后在**网页里把配置填完**就完事了。

## 开始前：装好 Docker

先装 [Docker Engine](https://docs.docker.com/engine/install/) 和
[Docker Compose plugin](https://docs.docker.com/compose/install/linux/)，然后确认：

```bash
docker version
docker compose version
```

两条命令都不报错、能显示版本号，就可以继续。

## 第 1 步：下载仓库

```bash
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot
```

## 第 2 步：准备目录和两个小文件

先照着复制粘贴（都是一次性准备动作）：

```bash
cp docker/compose.env.example compose.env
mkdir -p runtime/config/secrets runtime/data/storage runtime/media/inbound
touch runtime/config/.env
chmod 700 runtime/config/secrets
chmod 600 runtime/config/.env
sudo chown -R 10001:10001 runtime/config runtime/data runtime/media
```

然后做两件小事：

1. 编辑 `compose.env`，把 `QQ_MAID_IMAGE=` 后面的内容换成最新版本号。
   去 [Releases 页面](https://github.com/kuliantnt/qq-maid-bot/releases) 看最新版，例如：

   ```text
   QQ_MAID_IMAGE=ghcr.io/kuliantnt/qq-maid-bot:v0.23.8
   ```

2. 编辑 `runtime/config/.env`，加这一行（不加的话网页控制台打不开）：

   ```text
   LLM_SERVER_HOST=0.0.0.0
   ```

## 第 3 步：启动

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml up -d
```

第一次会自动下载镜像，等一会儿。想看进度就看日志（看完按 `Ctrl+C` 退出）：

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml logs -f bot
```

没有报错退出、日志正常滚动，就说明跑起来了。

## 第 4 步：打开网页，把所有配置填完

浏览器打开：

```text
http://127.0.0.1:8787
```

> 服务器在远处的话，先在自己电脑上开 SSH 隧道，再打开上面的地址：
> `ssh -L 8787:127.0.0.1:8787 用户名@服务器IP`

第一次打开会让你输入 **Bootstrap Token**（一次性初始化凭据），到服务器上查看：

```bash
cat runtime/config/secrets/bootstrap.token
```

填进去、设置管理员密码。之后 **QQ 机器人、AI 模型、联网搜索……全都在网页里填**，
不用再碰配置文件。

网页里保存配置后，回到服务器重启一下让配置生效：

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml restart bot
```

## 常用命令

下面的 `...` 就是第 3 步里那一长串（`--project-directory . --env-file compose.env
-f docker/compose.yaml -f docker/compose.console.yaml`），照抄即可：

```bash
docker compose ... ps          # 看状态
docker compose ... logs -f bot # 看日志
docker compose ... restart bot # 重启
docker compose ... down        # 停止并删除容器（数据保留在 runtime/ 目录）
```

**升级**：改 `compose.env` 里的 `QQ_MAID_IMAGE` 版本号，再执行一次第 3 步的
`up -d` 即可。

## 常见问题

| 现象 | 怎么办 |
| --- | --- |
| 启动报 `permission denied` | 第 2 步的 `sudo chown -R 10001:10001 ...` 有没有执行？ |
| 网页打不开 | `runtime/config/.env` 里有没有 `LLM_SERVER_HOST=0.0.0.0`？启动命令有没有带 `-f docker/compose.console.yaml`？ |
| 容器一直 `unhealthy` | `docker compose ... logs bot` 看日志，按提示补配置（多半是模型 API Key 没填） |
| 只想接微信 / OneBot 11 | 要额外加载对应 override 并改容器内监听地址，详见[完整版](./docker.md) |
| 想要更稳的镜像 | 去 GHCR 包页面复制 `@sha256:...` 完整 digest，替换 `QQ_MAID_IMAGE` |

其他问题、备份、安全加固请看[完整版 Docker 部署](./docker.md)。
