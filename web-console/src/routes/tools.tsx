import { createFileRoute } from "@tanstack/react-router";
import { MarkdownPage } from "../features/tools/markdown-page.js";

export const Route = createFileRoute("/tools")({
  component: MarkdownPage,
});
