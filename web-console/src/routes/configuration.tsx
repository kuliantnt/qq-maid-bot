import { createFileRoute } from "@tanstack/react-router";
import { ConfigurationPage } from "../features/configuration/configuration-page.js";

export const Route = createFileRoute("/configuration")({
  component: ConfigurationPage,
});
