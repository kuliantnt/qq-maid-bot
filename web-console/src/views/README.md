# views 页面视图

按页面划分：`todo/` 与 `configuration/` 为多模块页面目录，各自带 README 说明模块划分；`dashboard.ts`、`platforms.ts`、`storage.ts`、`markdown.ts` 为单文件小页面（职责单一）。

| 路径 | 页面 |
|---|---|
| `dashboard.ts` | Overview 总览信号面板渲染 |
| `platforms.ts` | 平台状态表渲染 |
| `storage.ts` | 存储资源表渲染 |
| `markdown.ts` | Markdown 编辑器与预览绑定 |
| `todo/` | Todo 页面（列表/筛选/创建弹窗/分页），见 `todo/README.md` |
| `configuration/` | 配置中心页面（分组导航/表单/自动保存/模型路线/OpenCode/TTS/Web 搜索），见 `configuration/README.md` |
