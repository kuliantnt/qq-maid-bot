import { createFileRoute } from "@tanstack/react-router";
import { PlatformsPage } from "../features/platforms/platforms-page.js";

export const Route = createFileRoute("/platforms")({
  component: PlatformsPage,
});
