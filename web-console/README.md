# Web Console

8787 部署管理控制台的原生 TypeScript 源码。这套 Console 是仓库原有前端的替代实现，页面复用 Rust 侧部署管理员服务端会话、CSRF、配置中心和业务管理 API，仍只适合本机或受控内网，不应将端口裸露到公网。

```bash
npm ci
npm run check
npm run build
```

`src/` 是唯一人工维护的源码，`dist/` 由构建脚本完整清理并生成，禁止直接编辑。`dist/` 会提交到 Git，Rust 使用 `include_str!` 直接嵌入这些产物，因此普通 Cargo 构建、测试、发布和机器人运行均不依赖 Node.js。

## 增量修改流程

前端采用“源码增量修改、产物完整重建”的方式，不直接修改服务器上的静态文件，也不手工编辑 `dist/`。

1. 从仓库根目录确认当前分支和工作区状态：

   ```bash
   git status --short --branch
   ```

2. 只修改对应领域的 `src/` 文件：页面结构改 `src/index.html`，页面行为改 `src/views/<page>.ts`，API 边界改 `src/api.ts` 和 `src/types.ts`，主题/背景/导航改各自模块。后端 DTO 或路由发生变化时，先确认 Rust API 契约，再同步前端解析器。

3. 在 `web-console/` 运行检查和构建：

   ```bash
   npm ci
   npm run check
   npm run build
   npm test
   ```

   构建脚本会清理并重新生成 `dist/`。生成的 JS、CSS、HTML 和资源必须一并提交，因为 Rust 通过 `include_str!` / `include_bytes!` 嵌入它们。

4. 回到仓库根目录确认没有漏同步的产物：

   ```bash
   git diff --exit-code -- web-console/dist
   cargo fmt --all -- --check
   cargo test -p qq-maid-core console_routes::tests
   ```

   如果 `dist` 与源码构建结果不一致，先重新运行 `npm run build`，不要手工修补生成文件。

5. 涉及 Rust 静态资源登记、响应头或 API DTO 时，再运行对应 Rust 测试；新增前端模块必须同步更新 `qq-maid-core/src/http/console_routes.rs` 的资源 allowlist。涉及缓存策略时，HTML 使用可重新验证的缓存，带有当前构建内容的静态 JS/CSS/图片才使用长期缓存。

6. 提交时按功能拆分：页面/API/后端契约/文档分别保持可独立审查；不要提交 `scripts/deploy.conf`、密钥、`.omo/`、demo 临时目录或本地构建缓存。

7. 部署前先在本地完成上述检查，再由部署脚本或服务器兼容环境构建。部署重启会使内存中的管理员 session 失效，需要重新登录；管理员数据库、配置和 secrets 不应被部署流程覆盖。

浏览器不把管理员会话、Bootstrap token、secret 或 CSRF 写入持久存储。secret 加载时只显示配置状态，输入留空表示不修改，清除必须使用显式动作；所有保存结果以服务端返回的 revision 和真实持久化状态为准。

首次初始化页面明确显示运行目录下的 `config/secrets/bootstrap.token`，并通过独立的 PreAuth Cookie 完成流程；Bootstrap 状态 GET 不签发 Cookie，也不会覆盖已经登录的管理员会话。初始化和密码重置 token 只在新生成时写入权限受限文件并向控制台输出一次，不会通过页面 API 回传；使用成功后立即失效。登录页可生成密码重置 token，新密码提交成功后撤销全部旧 Admin 会话。

修改源码并构建后，可在仓库根目录执行以下命令校验产物可复现：

```bash
git diff --exit-code -- web-console/dist
```

## Documentation Index

| 文档 | 用途 |
|---|---|
| [DESIGN.md](DESIGN.md) | 生产设计系统、token、组件层和目标页面领域 |
| [COMPONENT_REGISTRY.md](docs/COMPONENT_REGISTRY.md) | 可复用组件的结构、状态和扩展规则 |
| [THEME.md](docs/THEME.md) | 三色主题预设和 localStorage-only 持久化协议 |
| [API_CONTRACTS.md](docs/API_CONTRACTS.md) | 当前 API、认证边界和未来接口槽位 |
| [INTERACTION_CONTRACTS.md](docs/INTERACTION_CONTRACTS.md) | 配置保存、修改、冲突、密钥和重启交互协议 |
| [ADDING_A_PAGE.md](ADDING_A_PAGE.md) | 新增页面、组件、主题和 API 消费者的步骤 |

当前目标信息架构包含 Overview、Platforms、Agent、Configuration、Storage、Tools 六个产品领域。现有生产源码仍是单页分区，Agent 和 Tools 在迁移完成前分别作为配置策略和工具区块，不创建假导航页面。
