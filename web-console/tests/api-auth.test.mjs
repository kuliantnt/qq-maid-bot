import test, { afterEach } from "node:test";
import assert from "node:assert/strict";

import {
  ConsoleApiError,
  fetchConsoleStatus,
  loginAdmin,
  setCsrfToken,
  setUnauthorizedHandler,
} from "../dist/api.js";

function jsonResponse(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  setUnauthorizedHandler(null);
  setCsrfToken("");
  delete globalThis.fetch;
});

test("旧会话 401 晚于重新登录返回时不会清理新会话", async () => {
  let resolveOldRequest;
  const oldResponse = new Promise((resolve) => {
    resolveOldRequest = resolve;
  });
  globalThis.fetch = async (input) => {
    if (String(input) === "/api/v1/console/status") return oldResponse;
    return jsonResponse({
      session: {
        username: "admin",
        csrf_token: "csrf-new",
        expires_at: 123,
      },
    });
  };

  let unauthorizedCalls = 0;
  setUnauthorizedHandler(() => {
    unauthorizedCalls += 1;
  });
  setCsrfToken("csrf-old");
  const oldRequest = fetchConsoleStatus();

  await loginAdmin("admin", "password");
  resolveOldRequest(jsonResponse({ error: { code: "unauthorized", message: "会话已过期" } }, 401));

  await assert.rejects(oldRequest, (error) => (
    error instanceof ConsoleApiError && error.status === 401
  ));
  assert.equal(unauthorizedCalls, 0);
});

test("当前会话 401 仍然通知会话失效", async () => {
  globalThis.fetch = async () => jsonResponse({ error: { code: "unauthorized", message: "会话已过期" } }, 401);
  let unauthorizedCalls = 0;
  setUnauthorizedHandler(() => {
    unauthorizedCalls += 1;
  });
  setCsrfToken("csrf-current");

  await assert.rejects(fetchConsoleStatus(), (error) => (
    error instanceof ConsoleApiError && error.status === 401
  ));
  assert.equal(unauthorizedCalls, 1);
});
