# src 代码模块结构

Web 控制台前端源码按路由、业务页面和通用组件划分；`dist/` 由 `npm run build` 从 `web-console/` 生成并提交，Rust 直接嵌入构建产物。

## 顶层模块

| 文件 | 职责 |
|---|---|
| `main.tsx`、`app-providers.tsx` | React 入口、Router、Query 与 Jotai Provider |
| `routes/` | TanStack Router 页面路由与根布局 |
| `features/` | 按认证、配置、Todo、Memory、知识库等业务域组织页面与状态 |
| `components/` | 复用布局和 UI 组件 |
| `api.ts` | 后端 API 客户端（认证/状态/配置/Todo/用户偏好/文件/Markdown），统一经 `transport` 发送 |
| `api-routes.ts` | **路由集中配置**：所有 API 路径常量，改路径只动此文件 |
| `types.ts` | 共享 DTO 类型 |
| `theme.ts` | 主题预设与控制器（localStorage 兜底、服务端偏好 hydration） |
| `background.ts` | 背景控制器（内置默认/特殊/自定义、服务端偏好迁移、object URL 生命周期） |
| `file-cache.ts` | 用户文件 Blob 的浏览器 Cache API 缓存 |

## 依赖方向

```
main.tsx ──> app-providers.tsx ──> routes/* ──> features/*
features/* ──> api.ts / api-routes.ts / types.ts / components/*
```

页面业务代码优先留在对应 `features/<domain>/`；路由文件只负责装配页面。
