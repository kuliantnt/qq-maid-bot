import type { TodoItem, TodoStatus } from "../../types.js";
import { Button } from "../../components/ui/button.js";
import { StatusBadge } from "../../components/ui/status-badge.js";

type TodoCardProps = {
  todo: TodoItem;
  busy: boolean;
  onChangeStatus: (todo: TodoItem, status: TodoStatus) => void;
  onEdit: (todo: TodoItem) => void;
  onDelete: (todo: TodoItem) => void;
};

/** Todo 卡片：标题 + 状态徽章 + 元信息 + 操作行；与旧版信息结构一致。 */
export function TodoCard({ todo, busy, onChangeStatus, onEdit, onDelete }: TodoCardProps) {
  const completed = todo.status === "completed";
  const deadline = todo.dueAt ? `截止 ${todo.dueAt}` : todo.dueDate ? `截止 ${todo.dueDate}` : "无截止日期";
  const reminder = todo.reminderAt ? `提醒 ${todo.reminderAt}` : "无提醒";
  return (
    <article className="console-frame flex flex-col gap-2 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="m-0 text-base font-bold text-ink">{todo.title}</h3>
        <StatusBadge tone={completed ? "success" : "warning"} label={completed ? "已完成" : "待处理"} />
      </div>
      <p className="m-0 font-mono text-xs text-muted">
        {[todo.target.platform, todo.target.scopeType, deadline, reminder].join(" · ")}
      </p>
      {todo.detail ? <p className="m-0 text-sm leading-relaxed text-ink">{todo.detail}</p> : null}
      <div className="flex flex-wrap gap-2">
        {completed ? (
          <Button variant="secondary" disabled={busy} onClick={() => onChangeStatus(todo, "pending")} className="px-3 py-1.5 text-xs">
            恢复待处理
          </Button>
        ) : (
          <Button variant="secondary" disabled={busy} onClick={() => onChangeStatus(todo, "completed")} className="px-3 py-1.5 text-xs">
            标记完成
          </Button>
        )}
        <Button variant="secondary" disabled={busy} onClick={() => onEdit(todo)} className="px-3 py-1.5 text-xs">
          查看 / 编辑
        </Button>
        <Button variant="danger" disabled={busy} onClick={() => onDelete(todo)} className="px-3 py-1.5 text-xs">
          删除
        </Button>
      </div>
    </article>
  );
}
