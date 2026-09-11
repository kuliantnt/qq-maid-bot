import { createFileRoute } from "@tanstack/react-router";
import { KnowledgePage } from "../features/knowledge/knowledge-page.js";

export const Route = createFileRoute("/knowledge")({
  component: KnowledgePage,
});
