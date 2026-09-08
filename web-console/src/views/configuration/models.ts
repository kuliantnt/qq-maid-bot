/** Connection discovery 是实际暴露列表；目录与本地信息只补充展示，不验证运行能力。 */
import { discoverConnectionModels, fetchModelMetadata, updateAgentConfiguration, updateModelOverride } from "../../api.js";
import { array, record, string } from "./fields.js";
import { current, runSave } from "./state.js";
import { errorMessage } from "./ui.js";
import { createModelForm } from "./model-form.js";
import { mergeModels, filterModels, discoveryLabel } from "./model-data.js";
export { mergeModels, filterModels, discoveryLabel } from "./model-data.js";
import { addCandidate, normalizeCandidates } from "./model-route-editor.js";

export function appendModelManager(parent: HTMLElement, connection: string, revision: string, enabled: boolean): void {
  const open = node("button", "管理模型"); open.type = "button"; open.className = "secondary provider-action";
  // 停用 Connection 仍可管理本地元数据，只有获取模型与加入 Route 不可用。
  open.onclick = () => {
    const dialog = node("dialog", ""); dialog.className = "provider-create-dialog model-manager";
    dialog.setAttribute("aria-label", `${connection} 模型管理`);
    let metadata: Record<string, unknown> = {}, discovery: Record<string, unknown> | null = null;
    const title = node("h3", `${connection} · 管理模型`);
    const notice = node("p", "模型与价格仅供参考。Advertised 为声明；Verified 能力未知，获取模型成功不等于真实调用或能力验证。保存后重启生效。");
    const status = node("p", "正在读取本地模型与目录…"); status.setAttribute("role", "status");
    const error = node("p", ""); error.setAttribute("role", "alert");
    const search = field("搜索 model id / display name");
    const filter = node("select", ""); filter.setAttribute("aria-label", "模型状态筛选");
    for (const [value, label] of [["all", "全部状态"], ["enabled", "本地启用"], ["disabled", "本地禁用"], ["active", "Active"], ["beta", "Beta"], ["deprecated", "Deprecated"]]) {
      const option = node("option", label!); option.value = value!; filter.append(option);
    }
    const list = node("div", "");
    const form = createModelForm();
    const editor = form.element;
    let loaded = false;
    let visibleLimit = 50;
    let formRevision = "missing";
    const save = async (entry: Record<string, unknown>, expectedRevision = string(metadata.revision)): Promise<boolean> => {
      const result = await runSave(async () => {
        try { return await updateModelOverride(connection, expectedRevision, entry); }
        catch (cause) { error.textContent = errorMessage(cause); throw cause; }
      });
      if (!result) return false;
      metadata = await fetchModelMetadata(connection); render(); status.textContent = "本地模型已保存；重启后生效。";
      return true;
    };
    const render = (): void => {
      list.replaceChildren();
      const rows = filterModels(mergeModels(discovery, metadata), search.value, filter.value);
      if (!rows.length) list.append(node("p", "当前没有匹配的展示条目；Connection 获取状态见上方。"));
      if (rows.length > visibleLimit) {
        list.append(node("p", `当前显示 ${visibleLimit} / ${rows.length} 条，可搜索缩小范围。`));
        button(list, "显示更多模型", false, async () => { visibleLimit += 50; render(); }, error);
      }
      for (const row of rows.slice(0, visibleLimit)) {
        const card = node("article", ""); card.className = "provider-card";
        card.append(node("h4", string(row.metadata.display_name) || row.id), node("p", `模型 ID：${row.id}`), node("p", `来源：${row.sources.join(" / ")} · ${row.metadata.enabled === false ? "本地禁用" : "本地启用"} · Status：${row.metadata.status ?? "unknown"}`));
        card.append(node("p", `Context window：${row.metadata.context_window ?? "unknown"} · Max output：${row.metadata.max_output_tokens ?? "unknown"}`));
        const detail = node("details", ""); detail.append(node("summary", "模型信息 / Advertised / Provenance"));
        appendModelDetails(detail, row.metadata, row.sources.includes("catalog") ? record(metadata.catalog_source) : {});
        card.append(detail);
        button(card, "编辑模型信息", false, async () => {
          formRevision = string(metadata.revision); form.edit(row.id, row.override); editor.open = true; editor.scrollIntoView?.({ block: "nearest" });
        }, error);
        button(card, row.metadata.enabled === false ? "启用模型" : "禁用模型", false, async () => { await save({ ...row.override, provider: metadata.provider, id: row.id, enabled: row.metadata.enabled === false }); }, error);
        const routes = record(record(current?.agent?.savedValue).model_routes);
        const route = node("select", ""); route.setAttribute("aria-label", `为 ${row.id} 选择 Route`);
        for (const name of Object.keys(routes)) { const option = node("option", name); option.value = name; route.append(option); }
        card.append(route);
        button(card, "加入 Route", !enabled || row.metadata.enabled === false || !current?.agent?.editable || !Object.keys(routes).length, async () => {
          if (row.id.includes(",")) throw new Error("该模型 ID 含有路线分隔符，无法按现有语法加入 Route");
          const saved = await runSave(() => {
            const selected = record(record(record(current?.agent?.savedValue).model_routes)[route.value]);
            const result = addCandidate(normalizeCandidates(array(selected.candidates).flatMap(value => string(value).split(","))), `${connection.toLowerCase()}:${row.id}`);
            if (result.error) throw new Error(result.error);
            return updateAgentConfiguration(current!.agent!.revision, [{ action: "set_model_route", name: route.value, candidates: result.list }])
              .catch(cause => { error.textContent = errorMessage(cause); throw cause; });
          }, new Set([`id:agent-route-${route.value}`]));
          if (saved) status.textContent = `已加入 Route ${route.value}；重启后生效。`;
        }, error);
        list.append(card);
      }
    };
    button(editor, "保存本地模型", false, async () => {
      if (!loaded) throw new Error("模型信息尚未加载成功");
      if (await save({ ...form.read(), provider: metadata.provider }, formRevision)) formRevision = string(metadata.revision);
    }, error);
    button(editor, "新增另一模型", false, async () => { formRevision = string(metadata.revision); form.edit("", {}); }, error);
    const controls = node("div", "");
    button(controls, "获取模型", !enabled, async () => {
      status.textContent = "正在获取 Connection 模型…"; discovery = null; render();
      try { discovery = await discoverConnectionModels(connection, revision); }
      catch (cause) { discovery = { state: "failed", category: errorMessage(cause) }; }
      status.textContent = discoveryLabel(discovery); render();
    }, error);
    button(controls, "刷新模型信息", false, async () => { metadata = await fetchModelMetadata(connection); loaded = true; render(); }, error);
    button(controls, "关闭", false, async () => dialog.close(), error);
    search.oninput = () => { visibleLimit = 50; render(); }; filter.onchange = search.oninput;
    dialog.append(title, notice, controls, status, error, search, filter, list, editor);
    document.body.append(dialog); dialog.addEventListener("close", () => { dialog.remove(); open.focus(); }); dialog.showModal();
    void fetchModelMetadata(connection).then(value => { metadata = value; formRevision = string(value.revision); loaded = true; status.textContent = discoveryLabel(discovery); render(); }).catch(cause => { error.textContent = errorMessage(cause); status.textContent = "模型信息读取失败"; });
  };
  parent.append(open);
}
function node<K extends keyof HTMLElementTagNameMap>(tag: K, text: string): HTMLElementTagNameMap[K] { const element = document.createElement(tag); element.textContent = text; return element; }
function field(label: string): HTMLInputElement { const input = node("input", ""); input.placeholder = label; input.setAttribute("aria-label", label); return input; }
function button(parent: HTMLElement, label: string, disabled: boolean, action: () => Promise<void>, error: HTMLElement): void {
  const element = node("button", label); element.type = "button"; element.className = "secondary provider-action"; element.disabled = disabled; element.dataset.modelDisabled = String(disabled);
  element.onclick = async () => { element.disabled = true; error.textContent = ""; try { await action(); } catch (cause) { error.textContent = errorMessage(cause); } finally { element.disabled = disabled; } }; parent.append(element);
}

function appendModelDetails(parent: HTMLElement, model: Record<string, unknown>, source: Record<string, unknown>): void {
  const modalities = record(model.modalities), price = record(model.price), capabilities = record(model.capabilities), provenance = record(model.provenance);
  const labels: Record<string, string> = { catalog: "目录", official_patch: "官方补丁", local_override: "本地覆盖" };
  const row = (label: string, value: unknown, origin: unknown): void => {
    parent.append(node("p", `${label}：${value ?? "未知"}${typeof origin === "string" ? ` · 来源：${labels[origin] ?? origin}` : ""}`));
  };
  for (const [key, label] of [["display_name", "显示名称"], ["context_window", "上下文窗口"], ["max_output_tokens", "最大输出"], ["status", "状态"]]) row(label!, model[key!], provenance[key!]);
  row("输入模态", array(modalities.input).join("、") || "未知", provenance.modalities);
  row("输出模态", array(modalities.output).join("、") || "未知", provenance.modalities);
  row("输入价格（美元 / 百万 token）", price.input_per_million_usd, provenance.price);
  row("输出价格（美元 / 百万 token）", price.output_per_million_usd, provenance.price);
  for (const [key, label] of [["reasoning", "推理"], ["tool_calling", "工具调用"], ["vision", "视觉"]]) {
    const claim = record(capabilities[key!]).advertised;
    row(`${label} · 声明能力（Advertised）`, claim === true ? "声明支持" : claim === false ? "未声明支持" : "未知", record(provenance.capabilities)[key!]);
    row(`${label} · 已验证能力（Verified）`, "未知", null);
  }
  if (Object.keys(source).length) {
    const details = node("details", ""); details.append(node("summary", "目录出处"));
    for (const [key, label] of [["name", "数据源"], ["source_url", "上游地址"], ["source_version", "版本"], ["fetched_at", "快照时间"], ["upstream_license", "许可"], ["source_hash", "校验摘要"], ["converter_version", "转换版本"]]) details.append(node("p", `${label}：${source[key!] ?? "未知"}`));
    parent.append(details);
  }
}
