import { createFileRoute } from "@tanstack/react-router";
import { MigrationPlaceholder } from "../components/layout/migration-placeholder.js";

export const Route = createFileRoute("/tools")({
  component: () => <MigrationPlaceholder title="工具" />,
});
