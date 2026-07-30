import { copyFile, mkdir, rm } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = resolve(root, "dist-demo");
const tsc = resolve(root, "node_modules", ".bin", process.platform === "win32" ? "tsc.cmd" : "tsc");

await rm(dist, { recursive: true, force: true });
await mkdir(dist, { recursive: true });
execFileSync(tsc, ["--project", resolve(root, "tsconfig.demo.json")], { cwd: root, stdio: "inherit" });
await copyFile(resolve(root, "src", "demo", "index.html"), resolve(dist, "index.html"));
await copyFile(resolve(root, "src", "demo", "styles.css"), resolve(dist, "styles.css"));
