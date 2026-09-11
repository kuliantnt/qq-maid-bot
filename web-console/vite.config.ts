import path from "node:path";
import { copyFile, mkdir } from "node:fs/promises";
import type { Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import { defineConfig } from "vitest/config";

const root = path.resolve(import.meta.dirname, ".");

/** 把背景资产拷贝进 dist/background，保持 Rust include_bytes! 的既有路径不变。 */
function copyBackgroundAssets(): Plugin {
  return {
    name: "copy-background-assets",
    apply: "build",
    closeBundle: async () => {
      const dist = path.resolve(root, "dist", "background");
      await mkdir(dist, { recursive: true });
      // 背景资产只保留两张图：favicon（default.png）与九宫格拼图（special.webp），
      // 由原 9 张独立图按 3×3 拼合压缩，控制嵌入产物体积。
      await Promise.all([
        copyFile(path.resolve(root, "..", "assets", "favicon.png"), path.resolve(dist, "default.png")),
        copyFile(path.resolve(root, "..", "assets", "special-sprite.webp"), path.resolve(dist, "special.webp")),
      ]);
    },
  };
}

export default defineConfig({
  // 控制台由 Rust 静态托管在 /console 前缀下，base 必须与之一致。
  base: "/console/",
  plugins: [
    react({
      babel: {
        // React Compiler 接管 memo/useMemo/useCallback 等细粒度性能优化；
        // 业务代码只需遵守 Rules of React，不再手写 memo 化。
        plugins: [["babel-plugin-react-compiler", {}]],
      },
    }),
    tailwindcss(),
    // TanStack Router 文件式路由：从 src/routes 自动生成 routeTree.gen.ts。
    tanstackRouter({
      routesDirectory: "./src/routes",
      generatedRouteTree: "./src/routeTree.gen.ts",
    }),
    copyBackgroundAssets(),
  ],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Rust include_str! 固定嵌入这些文件名；带哈希的版本化文件名留待缓存策略升级时再启用。
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        assetFileNames: (info) =>
          info.names.some((name) => name.endsWith(".css")) ? "styles.css" : "[name][extname]",
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./tests/setup.ts"],
    include: ["tests/**/*.test.{ts,tsx}"],
    css: false,
  },
});
