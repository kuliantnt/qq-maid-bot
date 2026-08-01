import { changeTodoStatus, openEditor, removeTodo, actionButton } from "./todo.js";
export function todoCard(todo) {
    const card = document.createElement("article");
    card.className = `todo-card todo-card--${todo.status}`;
    const heading = document.createElement("div");
    heading.className = "todo-card-heading";
    const title = document.createElement("h3");
    title.textContent = todo.title;
    const status = document.createElement("span");
    status.className = `config-badge ${todo.status === "completed" ? "config-badge-ok" : "config-badge-warn"}`;
    status.textContent = todo.status === "completed" ? "已完成" : "待处理";
    heading.append(title, status);
    const meta = document.createElement("p");
    meta.className = "todo-card-meta";
    const deadline = todo.dueAt ? `截止 ${todo.dueAt}` : todo.dueDate ? `截止 ${todo.dueDate}` : "无截止日期";
    const reminder = todo.reminderAt ? `提醒 ${todo.reminderAt}` : "无提醒";
    meta.textContent = [todo.target.platform, todo.target.scopeType, deadline, reminder].join(" · ");
    card.append(heading, meta);
    if (todo.detail) {
        const detail = document.createElement("p");
        detail.className = "todo-card-detail";
        detail.textContent = todo.detail;
        card.append(detail);
    }
    const actions = document.createElement("div");
    actions.className = "todo-card-actions";
    if (todo.status === "pending")
        actions.append(actionButton("标记完成", () => void changeTodoStatus(todo, "completed")));
    else
        actions.append(actionButton("恢复待处理", () => void changeTodoStatus(todo, "pending")));
    actions.append(actionButton("查看 / 编辑", () => void openEditor(todo)));
    actions.append(actionButton("删除", () => void removeTodo(todo), "danger"));
    card.append(actions);
    return card;
}
