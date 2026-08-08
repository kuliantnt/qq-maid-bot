# Docker 部署 · 人话版

> 这份教程是给「不想折腾、只想把机器人跑起来」的人看的，**面向 Linux 服务器**。
> 每句都说大白话，每条命令复制粘贴就能用。想看细节（安全加固、多实例、自动发布、
> 回滚）再看[完整版](./docker.md)。

## 装之前，先知道三件事

1. 本教程**只面向 Linux 服务器**：你需要一台能联网的 Linux 服务器或电脑
   （Debian / Ubuntu 这类最常见）。没有的话，租一台便宜的云服务器就行。
   下面所有命令都在**这台 Linux 服务器的终端**里执行。
2. Docker 你不用深究，先把它当成「帮你运行程序的小盒子」。我们要做的是：把做好的
   程序（叫"镜像"）下载下来、丢进小盒子跑起来，然后打开网页填设置。
3. 整个过程**不用写代码、不用编译**，照下面一步步来就行。

## 第 0 步：装 Docker（一条命令搞定）

在终端里（租的服务器就先 SSH 登上去）粘贴下面这条，回车，它会自动把 Docker 装好：

```bash
curl -fsSL https://get.docker.com | sudo sh
```

> 中途可能要你输密码（你登录服务器的密码），正常，输进去回车就行。

装完再执行下面这条，让当前用户以后用 docker 不用每次加 `sudo`：

```bash
sudo usermod -aG docker $USER
```

**然后退出 SSH、重新登录一次**（或者把终端关掉重开），这步才生效。

确认装好没有——下面两条命令都执行一下，能刷出一大段版本信息（不用看懂）就行：

```bash
docker version
docker compose version
```

> Windows / macOS 用户：你的电脑上**不用装 Docker**。先在自己电脑的终端里 SSH 登录
> Linux 服务器（`ssh 用户名@服务器IP`），登录后按本文执行即可，所有命令都在
> **服务器的 Linux 终端**里运行。

## 第 1 步：把程序文件下载到服务器

```bash
git clone https://github.com/kuliantnt/qq-maid-bot.git
cd qq-maid-bot
```

第一条是把程序文件下载到当前文件夹，第二条是进入这个文件夹。
（如果提示找不到 `git`，先执行 `sudo apt install -y git`，再重新来。）

## 第 2 步：准备两个配置文件

先复制下面这一整段，粘贴进终端回车。它负责建好文件夹、写好 `.env` 文件、设好权限，
都是一次性动作，以后不用再跑：

```bash
cp docker/compose.env.example compose.env
mkdir -p runtime/config/secrets runtime/data/storage runtime/media/inbound
printf '%s\n' 'LLM_SERVER_HOST=0.0.0.0' > runtime/config/.env
chmod 700 runtime/config/secrets
chmod 600 runtime/config/.env
sudo chown -R 10001:10001 runtime/config runtime/data runtime/media
```

然后改一个小地方：打开 `compose.env` 文件（终端里执行 `nano compose.env` 就能编辑，
改完按 `Ctrl+O` 保存、`Ctrl+X` 退出；没有 nano 就先 `sudo apt install -y nano`），
找到 `QQ_MAID_IMAGE=` 那一行，把后面的内容换成：

```text
QQ_MAID_IMAGE=docker.io/kuliantnt/qq-maid-bot:latest
```

> `latest` 就是「用最新版」的意思，最省事。想固定某个版本，就去
> [Releases 页面](https://github.com/kuliantnt/qq-maid-bot/releases) 抄一个版本号，
> 把 `latest` 换成它，例如 `:v0.23.9`。

`runtime/config/.env` 不用动，里面已经写好了一行网页要用到的设置。

## 第 3 步：启动！

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml up -d
```

第一次会自动下载镜像（程序包），可能要等几分钟，网速决定。想看看进度：

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml logs -f bot
```

看到日志一直往下刷、没有反复报错，就是跑起来了。按 `Ctrl+C` 退出日志（程序不会停）。

> 如果一直卡在下载、报 `timeout` / `429`，说明网络连 Docker 仓库不顺畅，去下面
> 「下载不动怎么办」配个加速。

## 第 4 步：打开网页，把设置填完

在你电脑的浏览器打开：

```text
http://127.0.0.1:8787
```

> 服务器在远处的话：先在你电脑的终端执行
> `ssh -L 8787:127.0.0.1:8787 用户名@服务器IP`，再打开上面的地址。

第一次打开会让你输入一个「临时密码」（网页上写的是 Bootstrap Token）。去服务器上
执行：

```bash
sudo cat runtime/config/secrets/bootstrap.token
```

把显示出来的那串字符复制到网页里，然后设置你自己的管理员密码。

登录进去之后，**QQ 机器人、AI 模型、联网搜索……全部在网页里填**，不用再碰命令行。

网页里保存完设置，回服务器执行一次重启让设置生效：

```bash
docker compose --project-directory . --env-file compose.env \
  -f docker/compose.yaml -f docker/compose.console.yaml restart bot
```

## 以后常用的命令

下面四条都是完整命令，直接复制执行：

```bash
docker compose --project-directory . --env-file compose.env -f docker/compose.yaml -f docker/compose.console.yaml ps
docker compose --project-directory . --env-file compose.env -f docker/compose.yaml -f docker/compose.console.yaml logs -f bot
docker compose --project-directory . --env-file compose.env -f docker/compose.yaml -f docker/compose.console.yaml restart bot
docker compose --project-directory . --env-file compose.env -f docker/compose.yaml -f docker/compose.console.yaml down
```

- `ps`：看现在跑没跑
- `logs -f bot`：看日志（`Ctrl+C` 退出）
- `restart bot`：重启
- `down`：停止并删除容器（数据都在，不会丢）

**升级**：改 `compose.env` 里的 `QQ_MAID_IMAGE` 版本号，再执行一次第 3 步的启动命令。

## 下载不动怎么办（给 Docker 配个加速）

如果你在国内，下载镜像经常超时失败。给 Docker 配一个「加速通道」：

在服务器上执行 `sudo nano /etc/docker/daemon.json`，把下面的内容粘进去
（文件原本有内容就保留原来的，只把 `registry-mirrors` 这段加进去或替换掉）：

```json
{
  "registry-mirrors": [
    "https://docker.m.daocloud.io",
    "https://docker.mirrors.ustc.edu.cn"
  ]
}
```

保存后重启 Docker，再重新执行一次第 3 步的启动命令：

```bash
sudo systemctl restart docker
```

> - 加速只对 Docker Hub（`docker.io`）有效，对 `ghcr.io` 没用；拉不到 `ghcr.io` 时，
>   把 `QQ_MAID_IMAGE` 换成 `docker.io/kuliantnt/qq-maid-bot:latest` 就行，两边都有官方镜像。
> - 公共加速地址随时可能失效，失效就换一个；云服务器厂商（阿里云 / 腾讯云）控制台里
>   一般有专属加速地址，最稳。

## 机器人没起来（容器显示 unhealthy）怎么查

网页打不开或者容器状态一直显示 `unhealthy`，别慌，这只是在说「健康检查没通过」，
不一定是你配错了。按顺序看：

1. **看日志**：执行
   `docker compose --project-directory . --env-file compose.env -f docker/compose.yaml -f docker/compose.console.yaml logs bot`，
   有没有报错、有没有说哪个文件解析失败？
2. **看权限**：第 2 步那句
   `sudo chown -R 10001:10001 runtime/config runtime/data runtime/media` 执行过没有？
3. **看配置文件**：日志里有没有提示哪个文件写错了？
4. **看端口**：健康检查访问的是 `LLM_SERVER_PORT`（默认 8787），`.env` 里别把它改乱了。

> 一个小知识：没填模型 API Key 时，程序只是「没设置好」（网页里会显示 setup_required），
> 但健康检查照样通过，容器是 healthy 的。所以「没配模型」不会导致 unhealthy，
> 去网页里补上设置就行。

## 常见问题

| 遇到 | 怎么办 |
| --- | --- |
| 启动报 `permission denied`（没有权限） | 第 2 步的 `sudo chown -R 10001:10001 runtime/config runtime/data runtime/media` 执行过没有？ |
| 下载镜像超时 / `429` | 看上面「下载不动怎么办」；或把 `QQ_MAID_IMAGE` 换成 Docker Hub 地址 |
| 网页打不开 | `runtime/config/.env` 里有没有 `LLM_SERVER_HOST=0.0.0.0`？启动命令有没有带 `-f docker/compose.console.yaml`？ |
| 容器一直 `unhealthy` | 看上面「机器人没起来怎么查」 |
| 想接微信 / OneBot 11 | 要额外加载对应配置文件，详见[完整版](./docker.md) |
| 想要更稳的版本 | 固定版本号（如 `:v0.23.9`），或去 GHCR 页面复制 `@sha256:...` 完整 digest 替换 `QQ_MAID_IMAGE` |

其他问题、备份、安全加固请看[完整版 Docker 部署](./docker.md)。
