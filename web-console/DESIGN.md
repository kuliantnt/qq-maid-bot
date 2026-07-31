# Web Console Design System

本文件是生产 Web Console 的视觉和交互契约。

## 1. Atmosphere & Identity

这是一个本地优先的运维控制台。它应该像安静、精确的控制室，而不是营销页面。默认使用低饱和黑灰背景，通过背景、面板、卡片和输入框的明暗关系建立层级；绿色只承担操作、焦点和成功信号。识别性来自方形双线框体：外部 1px 线、1px 空隙、内部 1px 线。

生产页面采用“信号先于说明”的构图规则：页面标题只负责定位，主模块负责判断，旁侧模块负责扫描，长解释退到紧邻控件的短提示或原生 disclosure。Overview 是运行信号面板，Platforms 是身份卡与能力矩阵，Storage 是资源健康清单，Configuration 是控制面工作台，Tools 是编辑器与可信预览的分栏工作区。

## 2. Color

每套主题定义完整语义色。基础色集中在 `src/theme.ts`，其他视觉状态只能从语义 token 推导，不在组件中散落硬编码颜色。

| 角色 | CSS token | 用途 |
|---|---|---|
| 页面背景 | `--console-background` | 页面基础画布 |
| 一级面板 | `--console-surface` | 状态栏、面板、导航容器 |
| 二级面板 | `--console-surface-secondary` | 次级分区和弱层级 |
| 卡片 | `--console-card` | Provider、Todo、指标和编辑区域 |
| 输入 | `--console-input` | 输入框、选择器和编辑器 |
| 边框 | `--console-border` | 外部 1px 边框 |
| 主文字 | `--console-text-primary` | 标题、正文和关键数据 |
| 次文字 | `--console-text-secondary` | 说明、元数据和非活动状态 |
| 强调 | `--console-accent` | 主操作、焦点和活动信号 |
| 内框线 | `--console-line-inner` | 内部 1px 边框 |
| 成功 | `--console-success` | 在线、可用、通过；同时使用方形信号 |
| 警告 | `--console-warning` | 待处理、未验证、未配置；同时使用菱形信号 |
| 错误 | `--console-error` | 离线、失败、危险操作；同时使用三角形信号 |

### 新增可复用 token

| 角色 | CSS token | 用途 |
|---|---|---|
| 方形几何 | `--console-radius` | 统一保持 0 圆角的框体语言 |
| 轻层次 | `--console-shadow` | 仅用于浮动导航、认证门和 toast，不用于普通业务行 |
| 背景网格 | `.console-background-grid` | 由单张 `special.webp` 拼图按 3×3 切片组成的全屏背景 |
| 透明玻璃 | `--console-glass` / `--console-glass-raised` / `--console-glass-muted` | 页面组件的半透明填充，让背景图透出但保留文字对比度 |
| 无背景（默认） | — | 未选择任何背景时只呈现主题底色，不显示背景图；favicon 使用压缩后的 `default.png` |
| 特殊背景 | `.console-background--special` | 通过浏览器控制台输入 `kuliantnt` 解锁的九宫格背景 |

### 规则

- 页面层级依次使用 `background -> surface -> card/input`，不能靠大面积强调色区分区域。
- 文字和图标优先使用主、次文字 token；强调色只用于操作、焦点、活动状态和成功信号。
- 主题切换必须检查正文对比度至少 4.5:1，较大文字和图标至少 3:1。
- 状态不能只靠颜色表达，必须同时有文本或可访问名称。
- 状态信号不能只靠红绿差异表达；成功、警告、错误必须同时使用不同几何形状，且保留文本标签。
- 状态徽章也必须保留几何信号：成功为方形、警告为菱形、中性为圆形，并保留文字标签。
- 主题只保存浏览器本地的预设 ID，见 `docs/THEME.md`。

## 3. Typography

- 正文：`ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`。
- 技术数据：`ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`。
- 正文最小字号 14px；状态数字、版本、Provider 和路径使用等宽字体。
- 小标签使用正字距，标题使用轻微负字距；不要让所有标题都使用全大写。
- 中文文本优先使用句式大小写，不使用没有语义的英文装饰文案。

## 4. Spacing & Layout

基础单位为 4px。生产 token 使用 `--console-space-1` 到 `--console-space-12`，分别对应 4px、8px、12px、16px、20px、24px、32px、40px、48px。

应用壳层固定为三层：全宽状态栏、填满剩余高度的动态内容区、悬浮在内容区上的居中底部导航。状态栏左右保留 10px 安全留白，内容区桌面左右约 16px，移动端至少 16px；底部导航宽度不超过 410px，并与视口边缘保留悬浮间距。内容区负责滚动，底部导航以更高层级覆盖内容，不额外切出独立底栏区域。

壳层使用 `--console-shell-top` 与 `--console-shell-bottom` 作为固定层安全 inset，窄屏通过媒体查询提高顶部 inset；层级使用 `--console-z-top` 与 `--console-z-bottom`，避免组件自行散落 z-index。

### 目标产品领域

这是目标信息架构，不代表当前源码已经存在同名独立页面：

1. Overview，总览。
2. Platforms，平台和能力。
3. Agent，模型、检索、搜索和工具策略。
4. Configuration，运行配置、凭据和界面设置。
5. Storage，数据库、缓存、附件和迁移状态。
6. Tools，Markdown 预览及以后可加入的本地工具。

当前生产代码仍是单页分区。现有 `Dashboard`、`Platforms`、`Capabilities`、`Storage`、`Configuration`、`Markdown` 是迁移来源；Agent 和 Tools 首先作为 Configuration 或工具分区，是否拆成独立导航项由后续实现阶段决定。不得为了满足目标名称而创建假页面。

## 5. Components

组件分三层，详细索引见 `docs/COMPONENT_REGISTRY.md`。

### Foundation

主题变量、间距、层级、焦点环、动效和 SVG 图标注册。Foundation 不知道业务 API。

### Primitives

`Frame`、`Button`、`Icon`、`StatusIndicator`、`Badge`、`TextField`、`Select`、`Toggle`、`SectionHeader`、`DataTable`、`Toast`、`LoadingState`、`EmptyState` 和 `ErrorState`。每个 primitive 必须有稳定 DOM 结构、modifier 状态和键盘行为。

### Shell and composites

`StatusBar`、`BottomNav`、`PageShell`、`MetricCard`、`StatusCard`、`ConfigField`、`ConfigGroup`、`ProviderCard`、`MarkdownEditor`。这些组件组合 primitives，但不复制基础 CSS。

### 页面组合

- **Overview signal board**：`StatusCard` 作为唯一视觉焦点；`MetricCard` 组成紧凑指标 rail；Provider 与 upstream 作为 edge readout。数据模块可 hover 提升 2px，表示可扫描的交互层次。
- **Platform identity / capability matrix**：平台状态使用动态 table body 保持 API 兼容，桌面端以身份行呈现，窄屏转为逐行 card；能力矩阵保留方向与能力语义，但视觉上与平台状态分离。
- **Storage resource list**：`#storage-body` 保持 table renderer 和移动 card fallback；路径使用 mono metadata，状态与 schema/migration 作为同一资源行的健康信号。
- **Configuration workbench**：顶部为校验、摘要和重启后的控制 strip；主体按模型与供应商、模型路由、联网与工具、记忆与知识库、回复与语音、平台接入、待办与通知、系统与安全切换。runtime、Secret、Agent 与 Interface 仍是内部保存来源，但不作为用户主导航；单次只显示当前业务域的配置卡片。
- **MarkdownEditor**：左侧编辑、右侧后端清理预览。安全预览标签与“后端清理结果”始终可见，编辑器获得主导宽度。

新页面只组合已有组件。新组件只有在至少两个页面需要相同结构时才进入 registry。

## 6. Motion & Interaction

- 微交互 120 到 180ms，页面或分区切换 240 到 320ms。
- 只动画 `transform`、`opacity` 和 `filter`，不动画尺寸、位置或布局属性。
- 每个交互控件必须有 default、hover、active、focus-visible、disabled 和 loading 状态。
- `prefers-reduced-motion: reduce` 下关闭非必要动效。
- 不能为非交互元素添加无意义动画。

页面进入使用一次 8px 的 `transform` + `opacity` + 极轻 `filter` 渐入；业务行、指标和主题选择只在 hover/focus 时以 transform 表达可操作或可扫描性。动画不改变布局，不使用背景装饰性循环动画。
导航切换使用一次性中心扩散过渡：从 300px 中心图开始，以主题浅色玻璃模糊遮蔽内容区，遮蔽完成后切换页面并淡出；默认（无背景）模式不显示中心图，特殊模式按 `special.webp` 拼图的 9 个切片循环中心图。初次加载不播放，减少认知噪音。

文案规则：主视觉只保留标签、值、状态和下一步操作；安全保证不删除，改用短标签、`hint`、`aria` 或 `details` 保留在 DOM 中。认证、secret、CSRF、本地预检和本地主题范围的提示必须继续可发现。

## 7. Depth & Surface

采用液态玻璃和双线框体的混合策略：半透明深色填充、背景模糊、轻微饱和度、上方内高光、下方内暗线。页面底层默认不显示背景图，只呈现主题底色；特殊模式使用 `.console-background-grid` 以固定的左上到右下顺序铺设单张 `special.webp` 拼图的 3×3 切片（原 9 张独立图合并压缩，减少包体积）。特殊模式由浏览器控制台输入 `kuliantnt` 解锁；认证后背景的权威状态来自服务端用户偏好（`background_file_ids`、`active_background_file_id`、`background_mode`、`kuliantnt`），`background_mode` 表达当前模式（`default` 无背景 / `special` 特殊九宫格），自定义背景继续由 `active_background_file_id` 表达，`kuliantnt` 只表示是否解锁。旧 Cookie 只允许在首次认证成功时一次性迁移（解锁状态与旧背景模式一起写入服务端成功后才清理），不再作为持久化状态。背景层不可交互、不承载信息，内容组件通过 `--console-glass*` token 透出背景。输入框、编辑器、状态按钮和危险操作可以使用更高不透明度以维持可读性。组件不使用圆角，不用重阴影制造层次。边框结构固定为 1px 外线、1px 空隙、1px 内线。

## 8. Accessibility Constraints & Accepted Debt

- 目标 WCAG 2.2 AA。
- 所有底部导航项可键盘访问并暴露 `aria-current`。
- 主题选择使用按钮或 radio-like 控件，并暴露当前选择。
- Configuration workbench 的业务选项使用真实按钮、`tablist`/`tab`/`tabpanel` 语义、`aria-selected`、`tabindex` 与左右/Home/End/Enter/Space 键盘行为；重新渲染后恢复仍然可用的选择。
- 表格在窄屏必须提供可读的替代布局，不能让主任务依赖横向滚动。
- 异步请求必须有加载、成功、空态和错误状态。
- secret、session、CSRF 和主题以外的用户数据不得写入 localStorage。
- 九宫格图片背景必须使用 `aria-hidden="true"` 的装饰层，不能替代任何信息、状态或可操作内容。

当前接受的债务：生产界面仍是旧的单页 HTML，组件抽取和六个目标领域的路由迁移尚未实施。该债务在后续每迁移一个页面时减少，不通过添加第二套平行 CSS 长期维持。

## 9. API Boundary

页面只能通过 `src/api.ts` 或明确拆分的 typed API module 访问后端。组件不能直接调用 `fetch`，不能读取 cookie，不能处理 CSRF token。当前 API 和未来扩展规则见 `docs/API_CONTRACTS.md`。

## 10. Interaction Contract

配置保存、revision 冲突、secret 添加/替换/清除、配置校验、Provider 测试和受控重启必须遵循 `docs/INTERACTION_CONTRACTS.md`。其中 dirty、saving、conflict、pending restart 和 session expired 是真实交互状态，不是装饰性标签。
