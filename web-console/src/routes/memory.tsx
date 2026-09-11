import { createFileRoute } from "@tanstack/react-router";
import { MigrationPlaceholder } from "../components/layout/migration-placeholder.js";

export const Route = createFileRoute("/memory")({
  component: () => <MigrationPlaceholder title="Memory" />,
});
