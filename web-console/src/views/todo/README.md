# Todo 页面模块

Todo 页面按职责拆分为以下模块，`todo.ts` 为页面入口，子模块通过显式导入协作。

| 文件 | 职责 |
|---|---|
| `todo.ts` | 页面主逻辑：初始化、筛选参数收集、列表刷新与分页回退、目标分页加载、列表渲染、删除/编辑、结果提示；re-export `todoRecurrenceKind` |
| `todo-card.ts` | 单条 Todo 卡片渲染与操作按钮（完成/恢复、查看编辑、删除——删除恒为最后） |
| `todo-form.ts` | 创建 Todo 弹窗表单：字段读取、提交、成功关窗刷新、失败保留内容；重复规则转换 `todoRecurrenceKind` |
| `todo-paging.ts` | 纯分页模型：目标按页累积、筛选重置页码、删除后页码回退 |

## 依赖

```
todo.ts ──> todo-card.ts / todo-form.ts / todo-paging.ts / ../../api.ts
todo-card.ts / todo-form.ts ──> todo.ts（共享 refreshTodos/valueOf/showResult 等）
```

## 契约

- 不直接调用后端：全部经 `../../api.ts`
- 创建/编辑/删除/筛选/分页的后端语义与校验不变
