# Console Theme Contract

## Boundary

主题预设是浏览器界面偏好，不是机器人运行配置。预设 ID 只存在当前浏览器 origin 的 `localStorage`，不写入 `runtime.toml`、SQLite、secret store、session、cookie、CSRF header 或业务 API。

登录后的自定义颜色继续沿用管理员用户偏好 API；它只覆盖背景、主文字和强调色，不改变主题预设的卡片、输入、边框和状态语义。

## Presets

生产控制台固定提供三套语义主题：

| ID | 名称 | background | surface | card | input | border | text-primary | text-secondary | accent |
|---|---|---|---|---|---|---|---|---|---|
| `console-dark` | Console Dark | `#0D1117` | `#161B22` | `#21262D` | `#0D1117` | `#30363D` | `#E6EDF3` | `#8B949E` | `#3FB950` |
| `night-green` | Night Green | `#101714` | `#18251F` | `#22332B` | `#0C1411` | `#355044` | `#E6F0EA` | `#98A8A0` | `#6EE7A8` |
| `light` | Light | `#F6F8FA` | `#FFFFFF` | `#F0F3F6` | `#FFFFFF` | `#D0D7DE` | `#1F2328` | `#59636E` | `#1F883D` |

每套主题还必须定义 `surface-secondary`、`accent-hover`、按钮对比文字以及 `success`、`warning`、`error`。这些值集中在 `src/theme.ts`，组件只能消费 CSS 语义 token。

## Storage Format

```text
key: console-theme
value: {"preset":"console-dark","version":2}
```

规则：

1. 页面加载时读取并解析；缺失、损坏或未知 preset 回退 `console-dark`。
2. 回退不立即写入，只有用户主动选择时才保存。
3. 用户选择新 preset 时同步写入并立即更新 root 的 `data-theme` 与语义 token。
4. “恢复默认”删除该 key，并应用 `console-dark`。
5. v1 `night-shift` 迁移为 `night-green`；已移除的 `ember-grid`、`tide-signal` 回退 `console-dark`。
6. 不存放用户名、session、CSRF、secret、自由文本或后端响应。
7. 读取 localStorage 失败时只使用内存默认值，不阻塞页面。

## CSS Contract

基础语义 token：

```css
--console-background
--console-surface
--console-surface-secondary
--console-card
--console-input
--console-border
--console-text-primary
--console-text-secondary
--console-accent
--console-success
--console-warning
--console-error
```

主题控制器从 `CONSOLE_THEMES` 将完整预设写入 root。CSS 可以从基础 token 派生焦点环、弱强调背景和透明背景叠加层，但不能在页面或组件选择器里新增主题专属颜色。

主题选择入口位于 `Configuration -> Interface -> 主题`。预览色块固定展示背景、卡片和强调色。

## Adding A Preset

1. 扩展 `ConsoleThemePreset` 和 `CONSOLE_THEMES`。
2. 为所有语义角色提供值，并检查正文对比度至少 4.5:1。
3. 不新增按页面复制的主题 CSS。
4. 将 preset 加入主题选择器并补充存储、应用和刷新恢复测试。
5. 保持未知旧值回退默认，不新增后端运行配置。
6. 运行 `npm run check`、`npm run build` 和 `npm test`。
