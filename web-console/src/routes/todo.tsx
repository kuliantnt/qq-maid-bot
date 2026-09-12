import { createFileRoute } from "@tanstack/react-router";
import { TodoPage } from "../features/todo/todo-page.js";

export const Route = createFileRoute("/todo")({
  component: TodoPage,
});
