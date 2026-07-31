export const OPEN_CODE_ROUTE_TEMPLATE_NOTICE = "以下内容是路线模板，保存前必须将尖括号中的占位模型名替换为 OpenCode 模型目录中的真实 ID；需要 /messages 的模型暂未支持。";
const PRESETS = [
    {
        id: "opencode_zen",
        label: "OpenCode Zen Responses",
        kind: "openai_responses",
        baseUrl: "https://opencode.ai/zen/v1",
        apiKeyEnv: "OPENCODE_API_KEY",
        authHeader: "Authorization",
        authScheme: "Bearer",
        requestTimeoutSeconds: null,
        enabled: false,
    },
    {
        id: "opencode_zen_chat",
        label: "OpenCode Zen Chat",
        kind: "openai_compatible",
        baseUrl: "https://opencode.ai/zen/v1",
        apiKeyEnv: "OPENCODE_API_KEY",
        authHeader: "Authorization",
        authScheme: "Bearer",
        requestTimeoutSeconds: null,
        enabled: false,
    },
    {
        id: "opencode_go",
        label: "OpenCode Go",
        kind: "openai_compatible",
        baseUrl: "https://opencode.ai/zen/go/v1",
        apiKeyEnv: "OPENCODE_API_KEY",
        authHeader: "Authorization",
        authScheme: "Bearer",
        requestTimeoutSeconds: null,
        enabled: false,
    },
];
export function openCodeProviderPresets() {
    return PRESETS.map((preset) => ({ ...preset }));
}
export function readOpenCodeProviders(documentValue) {
    const providers = record(record(documentValue).providers);
    return PRESETS.map((preset) => {
        const saved = record(providers[preset.id]);
        if (Object.keys(saved).length === 0)
            return { ...preset };
        const timeout = saved.request_timeout_seconds;
        return {
            ...preset,
            kind: preset.kind,
            baseUrl: string(saved.base_url) || preset.baseUrl,
            // 三张预设卡片始终使用同一受管 Key 与 Bearer 认证；历史值不能反向变成可编辑表单状态。
            apiKeyEnv: preset.apiKeyEnv,
            authHeader: preset.authHeader,
            authScheme: preset.authScheme,
            requestTimeoutSeconds: typeof timeout === "number" && Number.isInteger(timeout) ? timeout : null,
            enabled: true,
        };
    });
}
export function openCodeProviderChange(form) {
    const preset = PRESETS.find((value) => value.id === form.id);
    if (!preset || form.kind !== preset.kind)
        throw new Error("OpenCode Provider ID 或协议不受支持");
    if (!isHttpUrl(form.baseUrl))
        throw new Error(`${form.label} Base URL 必须是合法的 HTTP(S) 地址`);
    if (form.requestTimeoutSeconds !== null
        && (!Number.isInteger(form.requestTimeoutSeconds) || form.requestTimeoutSeconds < 1)) {
        throw new Error(`${form.label} 请求超时必须是大于 0 的整数秒数`);
    }
    const provider = {
        kind: form.kind,
        base_url: form.baseUrl.trim(),
        api_key_env: preset.apiKeyEnv,
        auth_header: preset.authHeader,
        auth_scheme: preset.authScheme,
        request_timeout_seconds: form.requestTimeoutSeconds,
    };
    // Responses 预设显式持久化 false；后端也会拒绝任何自定义 Provider 的 true。
    if (form.kind === "openai_responses")
        provider.chat_fallback = false;
    return { action: "set_provider", id: form.id, provider };
}
export function openCodeProviderWarning(enabled, keyConfigured) {
    if (!enabled && !keyConfigured)
        return "Provider 未添加，OpenCode API Key 也未配置；仍可先编辑模型路线。";
    if (!enabled)
        return "Provider 尚未添加；仍可先编辑模型路线。";
    if (!keyConfigured)
        return "Provider 已保存，但 OpenCode API Key 尚未配置。";
    return "";
}
export function renderOpenCodeProviders(snapshot, onSave, onRemove) {
    const saved = readOpenCodeProviders(snapshot.agent?.savedValue);
    const running = readOpenCodeProviders(snapshot.agent?.runningValue);
    const key = snapshot.fields.find((field) => field.key === "provider.opencode.api_key");
    const keyConfigured = key?.configured === true;
    const section = document.createElement("section");
    section.className = "config-field-group";
    const heading = document.createElement("h3");
    heading.textContent = "模型 Provider";
    const intro = document.createElement("p");
    intro.className = "hint";
    intro.textContent = "三个 OpenCode 预设固定共用 OPENCODE_API_KEY、Authorization 和 Bearer；可修改 Base URL 与请求超时，保存后需重启生效。";
    const grid = document.createElement("div");
    grid.className = "provider-card-grid";
    for (const form of saved) {
        const runningForm = running.find((value) => value.id === form.id);
        grid.append(providerCard(form, runningForm, keyConfigured, snapshot.agent?.editable === true, onSave, onRemove));
    }
    section.append(heading, intro, grid);
    return section;
}
export function renderOpenCodeRouteHints(disabled, enabledProviders, keyConfigured) {
    const section = document.createElement("section");
    section.className = "config-field-group route-hints";
    const heading = document.createElement("h3");
    heading.textContent = "OpenCode 模型路线提示";
    const note = document.createElement("p");
    note.className = "hint";
    note.textContent = OPEN_CODE_ROUTE_TEMPLATE_NOTICE;
    const examples = [
        ["opencode_zen:<responses-model>", "opencode_zen"],
        ["opencode_zen_chat:<chat-model>", "opencode_zen_chat"],
        ["opencode_go:<chat-model>", "opencode_go"],
    ];
    const list = document.createElement("div");
    list.className = "route-example-list";
    for (const [example, provider] of examples) {
        const row = document.createElement("div");
        const code = document.createElement("code");
        code.textContent = example;
        const buttons = document.createElement("span");
        for (const [route, label] of [["private_main", "插入私聊模板"], ["group_main", "插入群聊模板"], ["aux", "插入辅助模板"]]) {
            const button = document.createElement("button");
            button.type = "button";
            button.className = "secondary provider-action";
            button.textContent = label;
            button.disabled = disabled;
            button.onclick = () => appendRouteCandidate(`agent-route-${route}`, example);
            buttons.append(button);
        }
        const warning = openCodeProviderWarning(enabledProviders.includes(provider), keyConfigured);
        row.append(code, buttons);
        if (warning) {
            const warningText = document.createElement("small");
            warningText.className = "field-warning";
            warningText.textContent = warning;
            row.append(warningText);
        }
        list.append(row);
    }
    section.append(heading, note, list);
    return section;
}
function providerCard(form, running, keyConfigured, editable, onSave, onRemove) {
    const card = document.createElement("article");
    card.className = "provider-card";
    const title = document.createElement("h4");
    title.textContent = form.label;
    const state = document.createElement("p");
    state.className = "provider-state";
    const pending = JSON.stringify(form) !== JSON.stringify(running);
    state.textContent = form.enabled
        ? pending ? "已保存，等待重启" : "已保存，当前已生效"
        : running.enabled ? "已移除，等待重启" : "Provider 未添加";
    const warning = openCodeProviderWarning(form.enabled, keyConfigured);
    const keyState = document.createElement("p");
    keyState.className = warning ? "field-warning" : "hint";
    keyState.textContent = warning || "OpenCode API Key 已配置（明文不会回传页面）。";
    const id = textInput("Provider ID", `${form.id}-id`, form.id, true);
    const kind = textInput("协议", `${form.id}-kind`, form.kind, true);
    const baseUrl = textInput("Base URL", `${form.id}-base-url`, form.baseUrl, !editable);
    const advanced = document.createElement("details");
    const summary = document.createElement("summary");
    summary.textContent = "高级连接字段";
    const apiKeyEnv = textInput("API Key Env（固定）", `${form.id}-api-key-env`, form.apiKeyEnv, true);
    const authHeader = textInput("认证 Header（固定）", `${form.id}-auth-header`, form.authHeader, true);
    const authScheme = textInput("认证 Scheme（固定）", `${form.id}-auth-scheme`, form.authScheme, true);
    const timeout = textInput("请求超时（秒）", `${form.id}-timeout`, form.requestTimeoutSeconds?.toString() ?? "", !editable, "number");
    advanced.append(summary, apiKeyEnv.row, authHeader.row, authScheme.row, timeout.row);
    const actions = document.createElement("div");
    actions.className = "provider-card-actions";
    const save = document.createElement("button");
    save.type = "button";
    save.className = "provider-action";
    save.textContent = form.enabled ? "保存修改" : "添加 Provider";
    save.disabled = !editable;
    save.onclick = () => void onSave(readProviderForm(form, baseUrl.input, apiKeyEnv.input, authHeader.input, authScheme.input, timeout.input));
    actions.append(save);
    if (form.enabled) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "secondary provider-action";
        remove.textContent = "移除 Provider";
        remove.disabled = !editable;
        remove.onclick = () => {
            if (window.confirm(`确定移除 ${form.label} 吗？已有模型路线不会自动删除。`))
                void onRemove(form.id);
        };
        actions.append(remove);
    }
    card.append(title, state, keyState, id.row, kind.row, baseUrl.row, advanced, actions);
    return card;
}
function readProviderForm(source, baseUrl, apiKeyEnv, authHeader, authScheme, timeout) {
    return {
        ...source,
        baseUrl: baseUrl.value,
        apiKeyEnv: apiKeyEnv.value,
        authHeader: authHeader.value,
        authScheme: authScheme.value,
        requestTimeoutSeconds: timeout.value.trim() ? Number(timeout.value) : null,
        enabled: true,
    };
}
function textInput(labelText, id, value, disabled, type = "text") {
    const row = document.createElement("label");
    row.className = "provider-field";
    row.textContent = labelText;
    const input = document.createElement("input");
    input.id = id;
    input.type = type;
    input.value = value;
    input.disabled = disabled;
    const providerId = id.replace(/-(base-url|timeout)$/, "");
    if (!disabled && providerId.startsWith("opencode_"))
        input.dataset.autosaveProvider = providerId;
    if (type === "number")
        input.min = "1";
    row.append(input);
    return { row, input };
}
function appendRouteCandidate(id, candidate) {
    const input = document.getElementById(id);
    if (!(input instanceof HTMLInputElement))
        return;
    const current = input.value.split(",").map((value) => value.trim()).filter(Boolean);
    if (!current.includes(candidate))
        current.push(candidate);
    input.value = current.join(", ");
    input.focus();
}
function isHttpUrl(value) {
    try {
        const url = new URL(value.trim());
        return (url.protocol === "https:" || url.protocol === "http:")
            && Boolean(url.hostname) && !url.username && !url.password && !url.search && !url.hash;
    }
    catch {
        return false;
    }
}
function record(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value) ? value : {};
}
function string(value) { return typeof value === "string" ? value : ""; }
