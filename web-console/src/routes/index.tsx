import { createFileRoute } from "@tanstack/react-router";
import { OverviewPage } from "../features/dashboard/overview-page.js";

export const Route = createFileRoute("/")({
  component: OverviewPage,
});
