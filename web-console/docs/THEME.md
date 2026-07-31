# Local Theme Contract

## Boundary

主题是浏览器偏好，不是机器人运行配置。主题状态只存在当前浏览器 origin 的 `localStorage`，不发送到后端，不写入 `runtime.toml`、SQLite 或 secret store，也不进入 session、cookie、CSRF header 或 API body。

## Presets

首版预设固定为三种源色：

| ID | 名称 | dark | light | contrast |
|---|---|---|---|---|
| `night-shift` | Night Shift | `#07130f` | `#e9f4e7` | `#78e3ad` |
| `ember-grid` | Ember Grid | `#17100d` | `#f3e2c7` | `#ff704d` |
| `tide-signal` | Tide Signal | `#061519` | `#dcf1ed` | `#e85f68` |

`dark` 是组件材质，`light` 是页面画布，`contrast` 是文字、图标、边框和活动信号。衍生 token 由 CSS 变量和 `color-mix()` 计算。

## Storage format

```text
key: console-theme
value: {"preset":"night-shift","version":1}
```

规则：

1. 页面加载时读取并解析；缺失、损坏或未知 preset 回退 `night-shift`。
2. 回退不立即写入，只有用户主动选择时才保存。
3. 用户选择新 preset 时同步写入并立即更新 root 的 `data-theme`。
4. “恢复默认”删除该 key，并应用默认主题。
5. 不存放用户名、session、CSRF、secret、自由文本或后端响应。
6. 读取 localStorage 失败时只使用内存默认值，不阻塞页面。

## CSS contract

生产 root 使用 `data-theme`：

```css
:root {
  --console-dark: ...;
  --console-light: ...;
  --console-contrast: ...;
}

:root[data-theme="ember-grid"] { ... }
:root[data-theme="tide-signal"] { ... }
```

主题选择入口位于 `Configuration -> Interface -> Color`，不是总览、平台、存储或工具页面。首版不做自定义颜色编辑；以后增加自定义颜色必须先做对比度校验。

## Adding a preset

1. 扩展 `ConsoleTheme` 类型。
2. 在 preset 表中加入 dark、light、contrast 三色。
3. 增加衍生 token 规则和预览 swatch。
4. 将 preset 加入主题选择器。
5. 保持未知旧值回退默认，不需要后端迁移。
6. 运行 `npm run check`、`npm run build` 和现有测试。
