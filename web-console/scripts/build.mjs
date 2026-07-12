import { copyFile, mkdir, rename, rm } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = resolve(root, "dist");
const tsc = resolve(root, "node_modules", ".bin", process.platform === "win32" ? "tsc.cmd" : "tsc");

await rm(dist, { recursive: true, force: true });
await mkdir(dist, { recursive: true });
execFileSync(tsc, ["--project", resolve(root, "tsconfig.json")], {
  cwd: root,
  stdio: "inherit",
});
await rename(resolve(dist, "main.js"), resolve(dist, "app.js"));
await Promise.all([
  copyFile(resolve(root, "src", "index.html"), resolve(dist, "index.html")),
  copyFile(resolve(root, "src", "styles.css"), resolve(dist, "styles.css")),
]);
