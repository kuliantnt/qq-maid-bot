import { createFileRoute } from "@tanstack/react-router";
import { MigrationPlaceholder } from "../components/layout/migration-placeholder.js";

/** Configuration 页正在迁移到 React（最大页面，最后迁移）。 */
export const Route = createFileRoute("/configuration")({
  component: () => <MigrationPlaceholder title="配置" />,
});
