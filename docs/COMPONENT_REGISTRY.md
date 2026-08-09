# Component Registry

这是生产前端的可复用组件索引。组件实现迁移期间可以暂时使用旧 class，但新代码不能复制旧 CSS 形成第二套组件。

## 分层

| 层 | 内容 | 不应知道 |
|---|---|---|
| Foundation | token、theme、icon、focus、motion、z-index | 业务数据和 API |
| Primitive | Frame、Button、Field、Status、Table、Feedback | 页面路由和后端 endpoint |
| Composite | StatusBar、BottomNav、PageShell、ConfigField、ProviderCard | 认证细节和 secret 原文 |
| Page | Overview、Platforms、Agent、Configuration、Storage、Tools | 其他页面的 DOM 内部结构 |

## Primitive contracts

### Frame

结构：`article.console-frame`，外层 1px 边框，`::before` 或专用内层元素形成 1px 空隙和 1px 内框。变体使用 modifier，例如 `console-frame--warning`。不能添加圆角。

状态：default、hover、focus-visible、warning、error、disabled。只有可交互 Frame 才可以获得键盘焦点。

### Button

结构：原生 `button`，使用 `console-button` 和一个 modifier。变体：`primary`、`secondary`、`danger`、`quiet`。禁止用 div 模拟按钮，禁止内联 style。

状态：default、hover、active、focus-visible、disabled、loading。loading 时保留按钮尺寸和可理解的文本。

### Icon

所有图标是内联 SVG 或受控 SVG 字符串，统一 `currentColor`、固定 stroke width 和 `aria-hidden`。图标名必须登记在 icon registry，禁止使用 emoji 或临时 Unicode 符号。

### StatusIndicator

结构：状态图标、可见文字和可选说明。状态：good、warn、error、neutral、unknown。颜色只是辅助，文字必须表达状态。

### Field controls

`TextField`、`Select`、`Toggle` 和 `PasswordField` 必须使用原生控件，label 与 input 绑定，错误文本通过 `aria-describedby` 关联。secret 字段只显示配置状态，不显示原文；清除必须是显式动作。

### DataTable

适合稳定列结构和只读数据。必须定义 loading、empty、error、long-content 状态。移动端如果表格不能自然重排，应提供同数据的 `DataList` 变体。

### Feedback

`Toast` 是全局单例，`InlineStatus` 用于页面内反馈，`ErrorState` 用于请求失败，`EmptyState` 用于合法空结果。错误消息不得吞掉 API error code，也不得把 secret、绝对路径或内部 ID展示给用户。

## Shell contracts

### PageShell

负责固定 `StatusBar`、可滚动 content region、固定 `BottomNav`。PageShell 不知道具体页面数据。内容区必须为当前页面提供独立的 heading、loading、empty 和 error 状态。

### StatusBar

全窗口固定在顶部，左右 10px 留白。左侧是品牌，右侧是可注册的状态项和账户操作。新增状态使用 `StatusBarItem` 注册，不修改布局核心。

### BottomNav

固定在底部，每项包含 SVG 图标、标签和 `aria-current`。它只消费页面 registry 的可见项，不直接导入业务 view。

### ConfigField / ConfigGroup

配置字段必须来自 typed snapshot，展示 source、editable、apply mode、pending restart 和 valid 状态。保存动作由页面 controller 负责，组件不直接调用 API。

### Configuration interaction composites

配置相关复合组件的行为契约见 [`INTERACTION_CONTRACTS.md`](./INTERACTION_CONTRACTS.md)：

- `BusinessConfigNav`：只负责业务域定位；runtime、Secret 与 Agent 保存来源在同一业务域内组合，不生成重复字段入口。
- `SaveBar`：按配置域展示 dirty 数量、保存中、成功、待重启和错误。
- `DirtyIndicator`：用文本和状态图标说明存在未保存修改。
- `SecretField`：只处理当前内存输入；空白不修改，显式清除才发送 clear，成功后清空原文。
- `ConflictPanel`：展示普通字段本地/服务器差异；secret 只展示配置状态，不能展示原文。
- `ConfirmDialog`：危险操作二次确认、焦点捕获和关闭后的焦点恢复。
- `SessionExpiredOverlay`：重新认证并保留当前页面内存草稿，不使用 localStorage 备份。

## Extension rules

1. 新状态、新颜色和新间距先添加到 `DESIGN.md`，再写 CSS。
2. 新组件使用稳定的 `console-*` 命名和 modifier，不使用 `!important`。
3. 不用 `:nth-child` 表达业务状态，不把状态写在 class 名以外的隐式 DOM 位置。
4. 组件只接受 typed props/context，不接受 `Record<string, unknown>` 作为长期接口。
5. 页面共享结构放 Composite，单页特殊内容留在 Page 模块。
6. 同一结构第二次出现时才抽取公共组件，避免提前制造空抽象。
7. 新可加载 JS/CSS 产物必须同步 `qq-maid-core/src/http/console_routes.rs` 的 `CONSOLE_ASSETS`。

## Page registry contract

目标实现使用页面注册表，路由只选择页面，页面只管理自己的内容：

```ts
export interface ConsolePage {
  readonly id: string;
  readonly label: string;
  readonly icon: IconName;
  readonly order: number;
  readonly capability?: string;
  mount(container: HTMLElement, context: PageContext): void | Promise<void>;
  unmount?(): void;
  refresh?(): Promise<void>;
}
```

当前代码尚未实现该 registry。迁移时先保留旧 view 的数据契约，再逐页接入，不同时重写所有 API parser。
