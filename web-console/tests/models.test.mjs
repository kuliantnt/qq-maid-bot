import test from "node:test";
import assert from "node:assert/strict";
import { mergeModels, filterModels, discoveryLabel } from "../dist/views/configuration/models.js";
import { discoverConnectionModels, fetchModelMetadata, updateModelOverride } from "../dist/api.js";
import { jsonResponse } from "./helpers/fake-dom.mjs";

test("发现模型优先，未知私有模型保留，只有精确 ID 补充目录和字段来源", () => {
  const metadata = { models: [
    { id: "known", display_name: "本地显示名称", enabled: false, status: "beta", provenance: { display_name: "local_override", context_window: "catalog" } },
    { id: "catalog-only", display_name: "参考条目", enabled: true, status: "deprecated", provenance: { display_name: "catalog" } },
  ], overrides: [{ provider: "openai", id: "known", display_name: "本地显示名称", enabled: false }] };
  const rows = mergeModels({ state: "success", models: [{ id: "private/unknown" }, { id: "known" }] }, metadata);
  assert.deepEqual(rows.map(row => row.id), ["known", "private/unknown", "catalog-only"]);
  assert.deepEqual(rows[0].sources, ["connection discovery", "catalog", "local override"]);
  assert.deepEqual(rows[1].metadata, {});
  assert.equal(filterModels(rows, "本地显示", "disabled")[0].id, "known");
  assert.equal(filterModels(rows, "PRIVATE/", "enabled")[0].id, "private/unknown");
  assert.equal(filterModels(rows, "", "deprecated")[0].id, "catalog-only");
  assert.equal(filterModels(rows, "", "beta")[0].id, "known");
});

test("unsupported、failed、unknown 与 success 空列表不混淆", () => {
  const states = ["unsupported", "failed", "unknown", "success"];
  const labels = states.map(state => discoveryLabel({ state, models: [], category: "test" }));
  assert.equal(new Set(labels).size, 4);
  assert.match(labels[3], /获取成功.*空列表/);
  for (const label of labels.slice(0, 3)) assert.doesNotMatch(label, /暂无模型|空列表/);
  assert.match(discoveryLabel(null), /尚未获取/);
  assert.deepEqual(mergeModels({ state: "failed", models: [{ id: "stale" }] }, {}), []);
});

test("模型 API 使用服务端 Connection 身份及独立 CAS，无 URL、凭证或能力启用参数", async () => {
  const original = globalThis.fetch;
  const requests = [];
  globalThis.fetch = async (url, init) => {
    requests.push({ url, method: init.method, body: JSON.parse(init.body) });
    if (url.endsWith("model-metadata")) return jsonResponse({ ok: true, metadata: { revision: "models-r1" } });
    if (url.endsWith("model-override")) return jsonResponse({ ok: true, configuration: { revision: "runtime-r", fields: [] } });
    return jsonResponse({ ok: true, discovery: { state: "unsupported", models: null } });
  };
  try {
    assert.equal((await discoverConnectionModels("proxy", "agent-r1")).state, "unsupported");
    assert.equal((await fetchModelMetadata("proxy")).revision, "models-r1");
    const model = { provider: "proxy", id: "private/model", enabled: false };
    await updateModelOverride("proxy", "models-r1", model);
    assert.deepEqual(requests.map(r => r.body), [
      { id: "proxy", expected_revision: "agent-r1" }, { id: "proxy" }, { id: "proxy", expected_revision: "models-r1", model },
    ]);
    assert.deepEqual(requests.map(r => r.method), ["POST", "POST", "PATCH"]);
  } finally { globalThis.fetch = original; }
});

import { createModelForm } from "../dist/views/configuration/model-form.js";
import { createFakeDom, installDomGlobals, clearDomGlobals } from "./helpers/fake-dom.mjs";

test("元数据表单直接编辑字段，保留启停和声明，继承不复制默认值", () => {
  installDomGlobals(createFakeDom());
  try {
    const form = createModelForm();
    form.edit("vendor/model:free", { display_name: "显示名", enabled: false, context_window: 128000,
      max_output_tokens: 4096, status: "beta", price: { input_per_million_usd: 0, output_per_million_usd: 2 },
      modalities: { input: ["text", "image"], output: ["text"] }, capabilities: { reasoning: { advertised: true }, tool_calling: { advertised: false } } });
    assert.deepEqual(form.read(), { id: "vendor/model:free", display_name: "显示名", enabled: false,
      context_window: 128000, max_output_tokens: 4096, status: "beta", price: { input_per_million_usd: 0, output_per_million_usd: 2 },
      modalities: { input: ["text", "image"], output: ["text"] }, capabilities: { reasoning: { advertised: true }, tool_calling: { advertised: false } } });
    form.edit("new-model", {});
    assert.deepEqual(form.read(), { id: "new-model" });
    assert.equal(form.element.querySelectorAll("textarea").length, 0);
    const context = form.element.querySelectorAll("input").find(field => field.getAttribute("aria-label").startsWith("上下文窗口"));
    context.value = "-1"; assert.throws(() => form.read(), /正整数/);
  } finally { clearDomGlobals(); }
});
