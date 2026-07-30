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
const backgroundSources = [
  "04d8ef15-e20e-4881-96e5-68c776daa603.png",
  "17dfd18a-1090-4560-9f82-10b982ac1aac.png",
  "脸脸 (27).png",
  "露露 (1).png",
  "小雨表情包 (10).png",
  "雪雪表情包 (36).png",
  "雅轩表情包.png",
  "file_000000000b0c71f5a0a3242a9ec6a3e7.png",
  "file_00000000647c71fdb3bdad19df3c7101.png",
];
const backgroundDir = resolve(dist, "background");
await mkdir(backgroundDir, { recursive: true });
await Promise.all([
  copyFile(resolve(root, "src", "index.html"), resolve(dist, "index.html")),
  copyFile(resolve(root, "src", "styles.css"), resolve(dist, "styles.css")),
  copyFile(resolve(root, "..", "assets", "757576FFCEA8D39E6665C762DF3D24FC.png"), resolve(backgroundDir, "default.png")),
  ...backgroundSources.map((source, index) => copyFile(
    resolve(root, "..", "assets", source),
    resolve(backgroundDir, `${String(index + 1).padStart(2, "0")}.png`),
  )),
]);
