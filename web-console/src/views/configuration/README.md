# 配置中心页面模块

配置中心按功能拆分为以下模块，`configuration.ts` 为入口并 re-export 全部公开符号（测试与外部导入保持单入口）。

## 模块划分

| 文件 | 职责 |
|---|---|
| `configuration.ts` | 入口：`initializeConfiguration`、整体 `render`、重启/校验绑定；re-export 各子模块公开符号 |
| `state.ts` | 模块共享状态（当前快照、控制器、保存队列、输入捕获、Secret 已保存记录）与 setter |
| `fields.ts` | 表单控件工厂（textField/numberField/selectField/checkboxField/fieldGroup/badge/button）与 DOM 工具 |
| `ui.ts` | 状态提示（showResult/showToast/errorMessage）与按钮禁用反馈 |
| `autosave.ts` | 自动保存机制（focusout 触发、脏检查、显式保存按钮去重） |
| `public-fields.ts` | 普通配置（runtime.toml 字段）渲染与保存 |
| `secret-fields.ts` | 敏感配置渲染与保存（脱敏、CAS、已保存状态记录） |
| `agent-fields.ts` | Agent 文档区域：场景/工具白名单/知识/模型 Provider 卡片与模型路线渲染保存 |
| `web-search.ts` | 联网搜索配置：读取/变更校验/路由变更/Tavily 凭据状态 |
| `tts.ts` | TTS 配置：Provider 选项、数值范围校验、字段联动 |
| `navigation.ts` | 业务分组定义、配置键到分组的映射、tab 导航渲染与键盘行为 |
| `opencode-providers.ts` | OpenCode 三个预设 Provider 卡片与模板插入 |
| `model-route-editor.ts` | 模型候选路线 Chip 编辑器（增删/拖动排序/键盘操作） |
| `theme-selector.ts` | 主题预设/自定义颜色与背景选择器（属于 Interface 配置组） |

## 依赖分层

```
configuration.ts（入口）
  ├── state.ts（共享状态，无业务依赖）
  ├── fields.ts / ui.ts（基础工具）
  ├── navigation.ts（分组映射）
  ├── autosave.ts ──> public/secret/agent-fields（保存函数）
  ├── public-fields.ts / secret-fields.ts / agent-fields.ts
  │     ──> web-search.ts / tts.ts / opencode-providers.ts / model-route-editor.ts
  └── theme-selector.ts
```

## 契约

- 自动保存、revision 冲突、Secret CAS、配置校验与后端 API 语义不变
- 对外公开符号（测试使用）统一从 `configuration.ts` 导出
