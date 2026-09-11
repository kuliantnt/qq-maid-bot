import { createFileRoute } from "@tanstack/react-router";
import { MigrationPlaceholder } from "../components/layout/migration-placeholder.js";

export const Route = createFileRoute("/knowledge")({
  component: () => <MigrationPlaceholder title="知识库" />,
});
