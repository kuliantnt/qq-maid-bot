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
// 背景资产只保留两张图：favicon（default.png，64×64 压缩版）与特殊九宫格
// 拼图（special.webp，由原 9 张独立图按 3×3 拼合），显著减小嵌入产物体积。
const backgroundDir = resolve(dist, "background");
await mkdir(backgroundDir, { recursive: true });
await Promise.all([
  copyFile(resolve(root, "src", "index.html"), resolve(dist, "index.html")),
  copyFile(resolve(root, "src", "styles.css"), resolve(dist, "styles.css")),
  copyFile(resolve(root, "..", "assets", "favicon.png"), resolve(backgroundDir, "default.png")),
  copyFile(resolve(root, "..", "assets", "special-sprite.webp"), resolve(backgroundDir, "special.webp")),
]);
