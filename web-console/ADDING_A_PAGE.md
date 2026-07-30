# Adding a Page or Feature

这份手册用于增量开发。不要复制一个旧页面再改 class，也不要为了未来接口创建假页面。

## 先判断是页面还是区块

- 新增独立用户任务、独立数据源或独立刷新周期，才考虑新页面。
- 只是在 Configuration 中增加一组字段，应新增 `ConfigGroup`，不要新增导航项。
- Agent、Tools 首版可以作为目标产品领域，但当前实现先分别落在配置策略和工具区块。
- Memory、日志、消息调试、附件管理没有真实 API 时，只保留 registry 扩展说明。

## 新增页面步骤

1. **写设计契约**：在 `DESIGN.md` 补充页面职责、数据状态、组件组合和无障碍要求。
2. **确认接口**：后端先提供稳定 DTO 和安全错误模型；没有 endpoint 不写真实页面。
3. **定义类型**：在 `types.ts` 添加响应类型，避免把 `unknown` 传入页面。
4. **添加 API module**：在 `api.ts` 或 `api/<domain>.ts` 中复用现有 transport、CSRF 和错误处理。
5. **创建 view module**：页面导出清晰的 `mount` 或 `render` 入口，接收 typed data，不直接读取其他页面 DOM。
6. **注册页面**：向 page registry 添加 `id`、label、order、SVG icon 和 capability。当前 registry 尚未实现时，使用现有 `index.html` 和 `main.ts` 的最小接入方式，不改变其他页面的 DOM 契约。
7. **组合组件**：优先使用 Component Registry 中的 Frame、Status、Field、Table、Feedback 和 PageShell。
8. **处理状态**：实现 loading、empty、error、success、disabled、long-content 和窄屏布局。
9. **添加测试**：为 parser、纯状态转换和页面关键行为添加 Node test；至少覆盖一个错误路径。
10. **同步静态资源**：新的 dist 文件必须登记到 `qq-maid-core/src/http/console_routes.rs` 的 `CONSOLE_ASSETS`。
11. **运行验证**：

```bash
npm run check
npm run build
npm test
git diff --check
```

如果修改了生产源码，确认构建后的 `web-console/dist` 是有意的差异，并运行相关 Rust console tests。

## 新增组件步骤

1. 先确认相同结构至少被两个页面需要。
2. 在 `DESIGN.md` 增加 token、结构、variants、states、a11y 和 motion。
3. 写独立 primitive/composite module，不把业务 API 放进组件。
4. 为 default、hover、active、focus、disabled、loading、empty、error 做 showcase 或测试。
5. 在 `docs/COMPONENT_REGISTRY.md`登记 DOM 契约和 modifier。

## 新增主题步骤

按 `docs/THEME.md` 添加三色 preset。只写 `console-theme` localStorage，绝不添加后端字段、请求 header 或其他持久化 key。

## 禁止事项

- 不迁移 React 或引入新的 UI 框架。
- 不在组件内直接 `fetch`、读 cookie 或保存 CSRF。
- 不用 emoji、圆角卡片或复制粘贴的页面专用 CSS。
- 不创建没有后端支持的假数据页面。
- 不把 secret、session、raw ID 或主题以外的偏好写入 localStorage。
