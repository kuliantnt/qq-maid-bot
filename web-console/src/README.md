# src 代码模块结构

Web 控制台前端源码按职责与页面划分模块，`dist/` 由 `npm run build` 从本目录生成并提交（Rust 用 `include_str!` 嵌入）。

## 顶层模块

| 文件 | 职责 |
|---|---|
| `main.ts` | 入口：认证/初始化流程、登录后 hydration、状态自动刷新、全局事件绑定 |
| `api.ts` | 全部后端 API 客户端（认证/状态/配置/Todo/用户偏好/文件/Markdown），统一经 `transport` 发送 |
| `api-routes.ts` | **路由集中配置**：所有 API 路径常量，改路径只动此文件 |
| `types.ts` | 共享 DTO 类型 |
| `dom.ts` | DOM 工具（取元素、文本写入、状态标签） |
| `theme.ts` | 主题预设与控制器（localStorage 兜底、服务端偏好 hydration） |
| `background.ts` | 背景控制器（内置默认/特殊/自定义、服务端偏好迁移、object URL 生命周期） |
| `file-cache.ts` | 用户文件 Blob 的浏览器 Cache API 缓存 |
| `agent-tools.ts` | Agent 工具选项与白名单工具名工具 |
| `console-shell.ts` | 控制台壳层：底部导航、页面切换状态机、历史同步 |
| `views/` | 页面视图，按页面分子目录，详见 `views/README.md` |
| `views/dashboard.ts` `platforms.ts` `storage.ts` `markdown.ts` | 单文件小页面（职责单一，无需拆分） |

## 依赖方向

```
main.ts ──> api.ts / theme / background / console-shell / views/*
views/* ──> api.ts / api-routes.ts / types.ts / dom.ts / 兄弟模块
```

页面视图之间不互相依赖；`views/configuration/*` 内部按 state → fields → 业务模块分层。
