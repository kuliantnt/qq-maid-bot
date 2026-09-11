import { createFileRoute } from "@tanstack/react-router";
import { StoragePage } from "../features/storage/storage-page.js";

export const Route = createFileRoute("/storage")({
  component: StoragePage,
});
