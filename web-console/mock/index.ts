/**
 * Web Console dev mock 插件：在 vite dev server 内直接接管 `/api/` 请求，
 * 让 `npm run dev` 无需启动 Rust 后端即可预览全部页面与交互。
 *
 * - 仅 dev 生效（`apply: "serve"`），`npm run build` 的 dist/ 产物不受任何影响；
 * - 设 `MOCK_API=off` 可关闭 mock（需要连真实后端时自行配代理）；
 * - 设 `MOCK_AUTH=gate` 可预览登录 / 首次初始化 / 密码重置表单流程（任意凭据可登录）。
 */
import type { Plugin } from "vite";
import { handleMockRequest } from "./handlers.js";

export function consoleMockApi(): Plugin {
  return {
    name: "console-mock-api",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url ?? "";
        if (!url.startsWith("/api/")) {
          next();
          return;
        }
        handleMockRequest(req, res).catch((cause: unknown) => {
          // mock 自身异常按 500 返回，避免悬挂请求阻塞页面加载。
          server.config.logger.error(`console-mock-api 处理失败：${cause instanceof Error ? cause.stack ?? cause.message : String(cause)}`);
          res.statusCode = 500;
          res.setHeader("Content-Type", "application/json; charset=utf-8");
          res.end(JSON.stringify({ error: { code: "mock_failed", message: "mock 处理失败，详见 dev server 日志" } }));
        });
      });
      server.config.logger.info(
        "console-mock-api 已启用：/api/* 由内存 mock 提供数据（MOCK_API=off 关闭，MOCK_AUTH=gate 预览登录页）",
        { timestamp: true },
      );
    },
  };
}
