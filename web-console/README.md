# Web Console

8787 只读管理面板的原生 TypeScript 源码。页面只适合本机或受控内网排障，不应将端口裸露到公网。

```bash
npm ci
npm run check
npm run build
```

`src/` 是唯一人工维护的源码，`dist/` 由构建脚本完整清理并生成，禁止直接编辑。`dist/` 会提交到 Git，Rust 使用 `include_str!` 直接嵌入这些产物，因此普通 Cargo 构建、测试、发布和机器人运行均不依赖 Node.js。

修改源码并构建后，可在仓库根目录执行以下命令校验产物可复现：

```bash
git diff --exit-code -- web-console/dist
```
