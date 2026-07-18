# Web Console

8787 部署管理控制台的原生 TypeScript 源码。页面复用 Rust 侧部署管理员服务端会话、CSRF 和配置中心 API，仍只适合本机或受控内网，不应将端口裸露到公网。

```bash
npm ci
npm run check
npm run build
```

`src/` 是唯一人工维护的源码，`dist/` 由构建脚本完整清理并生成，禁止直接编辑。`dist/` 会提交到 Git，Rust 使用 `include_str!` 直接嵌入这些产物，因此普通 Cargo 构建、测试、发布和机器人运行均不依赖 Node.js。

浏览器不把管理员会话、Bootstrap token、secret 或 CSRF 写入持久存储。secret 加载时只显示配置状态，输入留空表示不修改，清除必须使用显式动作；所有保存结果以服务端返回的 revision 和真实持久化状态为准。

首次初始化页面明确显示运行目录下的 `config/secrets/bootstrap.token`，并通过独立的 PreAuth Cookie 完成流程；Bootstrap 状态 GET 不签发 Cookie，也不会覆盖已经登录的管理员会话。初始化和密码重置 token 只在新生成时写入权限受限文件并向控制台输出一次，不会通过页面 API 回传；使用成功后立即失效。登录页可生成密码重置 token，新密码提交成功后撤销全部旧 Admin 会话。

修改源码并构建后，可在仓库根目录执行以下命令校验产物可复现：

```bash
git diff --exit-code -- web-console/dist
```
