import { AUTH_ROUTES, CONFIGURATION_ROUTES, MARKDOWN_RENDER_ROUTE, KNOWLEDGE_ROUTES, RESTART_ROUTE, STATUS_ROUTE, TODO_ROUTES, USER_DATA_ROUTES, } from "./api-routes.js";
export class ConsoleApiError extends Error {
    code;
    status;
    constructor(message, code = "request_failed", status = 0) {
        super(message);
        this.code = code;
        this.status = status;
        this.name = "ConsoleApiError";
    }
}
let csrfToken = "";
let unauthorizedHandler = null;
export function setCsrfToken(value) {
    csrfToken = value;
}
/** 统一通知页面会话失效，避免各个页面分别吞掉 401 后继续显示已认证状态。 */
export function setUnauthorizedHandler(handler) {
    unauthorizedHandler = handler;
}
function notifyUnauthorized(status) {
    if (status === 401)
        unauthorizedHandler?.();
}
export async function fetchSession() {
    const payload = record(await fetchJson(AUTH_ROUTES.session, {
        headers: { Accept: "application/json" },
    }));
    const session = parseSession(payload.session);
    setCsrfToken(session.csrfToken);
    return session;
}
export async function fetchBootstrap() {
    const payload = record(await fetchJson(AUTH_ROUTES.bootstrap, {
        headers: { Accept: "application/json" },
    }));
    return parseBootstrapStatus(payload.bootstrap);
}
export async function issuePreAuth() {
    const payload = record(await mutatingJson(AUTH_ROUTES.preauth, "POST"));
    const token = string(payload.csrf_token, "");
    if (!token)
        throw new ConsoleApiError("认证服务未返回 CSRF token", "invalid_response");
    setCsrfToken(token);
    return token;
}
export async function initializeAdmin(username, password, bootstrapToken) {
    const payload = record(await mutatingJson(AUTH_ROUTES.initialize, "POST", {
        username,
        password,
        bootstrap_token: bootstrapToken,
    }));
    const session = parseSession(payload.session);
    setCsrfToken(session.csrfToken);
    return session;
}
export async function requestPasswordReset() {
    const payload = record(await mutatingJson(AUTH_ROUTES.passwordResetBootstrap, "POST"));
    return parseBootstrapStatus(payload.bootstrap);
}
export async function resetAdminPassword(password, bootstrapToken) {
    const payload = record(await mutatingJson(AUTH_ROUTES.passwordReset, "POST", {
        password,
        bootstrap_token: bootstrapToken,
    }));
    const session = parseSession(payload.session);
    setCsrfToken(session.csrfToken);
    return session;
}
export async function loginAdmin(username, password) {
    const payload = record(await mutatingJson(AUTH_ROUTES.login, "POST", { username, password }));
    const session = parseSession(payload.session);
    setCsrfToken(session.csrfToken);
    return session;
}
export async function logoutAdmin() {
    await mutatingJson(AUTH_ROUTES.logout, "POST", undefined, true);
    setCsrfToken("");
}
export async function fetchUserPreferences() {
    const payload = record(await mutatingJson(USER_DATA_ROUTES.preferencesGet, "POST", {}));
    return parseUserPreferences(payload.data);
}
export async function updateUserPreferences(patch) {
    const payload = record(await mutatingJson(USER_DATA_ROUTES.preferencesUpdate, "POST", {
        ...(patch.customColors === undefined ? {} : { custom_colors: patch.customColors }),
        ...(patch.backgroundFileIds === undefined ? {} : { background_file_ids: patch.backgroundFileIds }),
        ...(patch.activeBackgroundFileId === undefined ? {} : { active_background_file_id: patch.activeBackgroundFileId }),
        ...(patch.backgroundMode === undefined ? {} : { background_mode: patch.backgroundMode }),
        ...(patch.kuliantnt === undefined ? {} : { kuliantnt: patch.kuliantnt }),
    }));
    return parseUserPreferences(payload.data);
}
/** 按文件列表分页元数据完整收集全部用户文件，避免假设用户文件最多一页（100 条）。 */
export async function collectAllUserFiles(fetchPage) {
    const collected = [];
    let page = 1;
    while (true) {
        const current = await fetchPage(page);
        collected.push(...current.items);
        const totalPages = Math.max(current.totalPages, Math.ceil(current.total / Math.max(current.pageSize, 1)));
        if (page >= totalPages)
            return collected;
        page += 1;
    }
}
export async function listUserFiles() {
    return collectAllUserFiles(async (page) => {
        const payload = record(await mutatingJson(USER_DATA_ROUTES.filesList, "POST", {
            page,
            page_size: 100,
        }));
        const data = record(payload.data);
        return {
            items: array(data.items).map(parseUserFile),
            page: finiteNumber(data.page) ?? 1,
            pageSize: finiteNumber(data.page_size) ?? 100,
            total: finiteNumber(data.total) ?? 0,
            totalPages: finiteNumber(data.total_pages) ?? 1,
        };
    });
}
export async function uploadUserFile(file) {
    const response = await fetch(USER_DATA_ROUTES.filesUpload, {
        method: "POST",
        credentials: "same-origin",
        headers: { Accept: "application/json", "X-CSRF-Token": csrfToken },
        body: (() => { const form = new FormData(); form.append("file", file); return form; })(),
    });
    if (!response.ok) {
        notifyUnauthorized(response.status);
        throw new ConsoleApiError(`文件上传失败（HTTP ${response.status}）`, "request_failed", response.status);
    }
    const payload = record(await response.json());
    return parseUserFile(payload.data);
}
export async function readUserFile(file) {
    const response = await fetch(file.url, {
        method: "POST",
        credentials: "same-origin",
        headers: { "X-CSRF-Token": csrfToken },
    });
    if (!response.ok) {
        notifyUnauthorized(response.status);
        throw new ConsoleApiError(`文件读取失败（HTTP ${response.status}）`, "request_failed", response.status);
    }
    return response.blob();
}
export async function deleteUserFile(fileId) {
    await mutatingJson(USER_DATA_ROUTES.filesDelete, "POST", { file_id: fileId });
}
export async function fetchKnowledgeCapabilities() {
    const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.capabilities, "POST", {}));
    const data = record(payload.data);
    return {
        supported_extensions: array(data.supported_extensions).map((value) => requiredString(value, "supported_extensions")),
        max_file_bytes: requiredFiniteNumber(data.max_file_bytes, "max_file_bytes"),
        max_filename_chars: requiredFiniteNumber(data.max_filename_chars, "max_filename_chars"),
    };
}
export async function listKnowledgeFiles(params) {
    const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.list, "POST", {
        page: params.page,
        page_size: params.page_size,
        search: params.search,
        ...(params.status === "all" ? {} : { status: params.status }),
        sort: params.sort,
        order: params.order,
    }));
    return parseKnowledgeFilePage(payload.data);
}
export async function uploadKnowledgeFile(file) {
    const form = new FormData();
    form.append("file", file);
    const response = await fetch(KNOWLEDGE_ROUTES.upload, {
        method: "POST",
        credentials: "same-origin",
        headers: { Accept: "application/json", "X-CSRF-Token": csrfToken },
        body: form,
    });
    if (!response.ok)
        throw await responseError(response);
    return parseKnowledgeFileItem(record(await response.json()).data);
}
export async function downloadKnowledgeFile(item) {
    if (item.file_id === null)
        throw new ConsoleApiError("知识库文件缺少标识", "invalid_response");
    const response = await fetch(KNOWLEDGE_ROUTES.get(item.file_id), {
        method: "POST",
        credentials: "same-origin",
        headers: { "X-CSRF-Token": csrfToken },
    });
    if (!response.ok)
        throw await responseError(response);
    return {
        blob: await response.blob(),
        filename: filenameFromContentDisposition(response.headers.get("Content-Disposition")) ?? item.filename,
    };
}
export function filenameFromContentDisposition(value) {
    if (value === null)
        return null;
    const encoded = /(?:^|;)\s*filename\*=UTF-8''([^;]+)/i.exec(value);
    if (encoded?.[1] !== undefined) {
        try {
            return decodeURIComponent(encoded[1]);
        }
        catch {
            return null;
        }
    }
    const plain = /(?:^|;)\s*filename="([^"]+)"/i.exec(value);
    return plain?.[1] ?? null;
}
export async function deleteKnowledgeFile(fileId) {
    await mutatingJson(KNOWLEDGE_ROUTES.delete, "POST", { file_id: fileId });
}
export async function retryKnowledgeFile(fileId) {
    const payload = record(await mutatingJson(KNOWLEDGE_ROUTES.retry, "POST", { file_id: fileId }));
    return parseKnowledgeFileItem(payload.data);
}
export async function fetchConfiguration() {
    const payload = record(await fetchJson(CONFIGURATION_ROUTES.get, {
        headers: { Accept: "application/json" },
    }));
    return parseConfigurationPayload(payload);
}
export async function updateRuntimeConfiguration(expectedRevision, changes) {
    const payload = record(await mutatingJson(CONFIGURATION_ROUTES.runtime, "PATCH", {
        expected_revision: expectedRevision,
        changes,
    }));
    return parseConfigurationPayload(payload);
}
export async function updateSecretConfiguration(changes) {
    const payload = record(await mutatingJson(CONFIGURATION_ROUTES.secrets, "PATCH", { changes }));
    return parseConfigurationPayload(payload);
}
export async function updateAgentConfiguration(expectedRevision, changes) {
    const payload = record(await mutatingJson(CONFIGURATION_ROUTES.agent, "PATCH", {
        expected_revision: expectedRevision,
        changes,
    }));
    return parseConfigurationPayload(payload);
}
export async function requestRestart() {
    const payload = record(await mutatingJson(RESTART_ROUTE, "POST", {}));
    return string(payload.message, "重启命令已提交");
}
export async function validateConfiguration() {
    const payload = record(await mutatingJson(CONFIGURATION_ROUTES.validate, "POST", {}));
    const validation = record(payload.validation);
    return { valid: validation.valid === true, message: string(validation.message, "配置校验已完成") };
}
export async function fetchConsoleStatus() {
    const value = await fetchJson(STATUS_ROUTE, { headers: { Accept: "application/json" } });
    const root = record(value);
    return {
        runtime: parseRuntime(root.runtime),
        provider: parseProvider(root.provider),
        platforms: array(root.platforms).map(parsePlatform),
        storage: array(root.storage).map(parseStorage),
        configuration: parseConfiguration(root.configuration),
    };
}
export async function renderMarkdown(markdown) {
    const value = await fetchJson(MARKDOWN_RENDER_ROUTE, {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ markdown }),
    });
    const payload = record(value);
    if (payload.ok !== true || typeof payload.html !== "string") {
        throw new ConsoleApiError("Markdown 渲染服务返回了无法识别的结果");
    }
    return payload.html;
}
export async function listTodos(filters = {}) {
    const payload = record(await mutatingJson(TODO_ROUTES.list, "POST", {
        page: 1,
        page_size: 50,
        ...filters,
    }));
    return parseTodoPage(payload.data);
}
export async function listTodoTargets(page = 1, pageSize = 100) {
    const payload = record(await mutatingJson(TODO_ROUTES.targets, "POST", {
        page,
        page_size: pageSize,
    }));
    return parseTodoTargetPage(payload.data);
}
export async function createTodo(input) {
    const payload = record(await mutatingJson(TODO_ROUTES.create, "POST", input));
    return parseTodoItem(payload.data);
}
export async function getTodo(id) {
    const payload = record(await mutatingJson(TODO_ROUTES.get, "POST", { id }));
    return parseTodoItem(payload.data);
}
export async function updateTodo(id, changes) {
    const payload = record(await mutatingJson(TODO_ROUTES.update, "POST", { id, ...changes }));
    return parseTodoItem(payload.data);
}
export async function deleteTodo(id) {
    await mutatingJson(TODO_ROUTES.delete, "POST", { id });
}
function parseTodoPage(value) {
    const data = record(value);
    return {
        items: array(data.items).map(parseTodoItem),
        page: finiteNumber(data.page) ?? 1,
        pageSize: finiteNumber(data.page_size) ?? 50,
        total: finiteNumber(data.total) ?? 0,
        totalPages: finiteNumber(data.total_pages) ?? 1,
    };
}
function parseKnowledgeFilePage(value) {
    const data = record(value);
    return {
        items: array(data.items).map(parseKnowledgeFileItem),
        page: finiteNumber(data.page) ?? 1,
        page_size: finiteNumber(data.page_size) ?? 20,
        total: finiteNumber(data.total) ?? 0,
        total_pages: finiteNumber(data.total_pages) ?? 1,
    };
}
export function parseKnowledgeFileItem(value) {
    const item = record(value);
    const source = item.source;
    const status = item.status;
    if (source !== "managed" && source !== "directory")
        throw new ConsoleApiError("知识库文件来源无效", "invalid_response");
    if (status !== "pending" && status !== "processing" && status !== "ready" && status !== "failed") {
        throw new ConsoleApiError("知识库文件状态无效", "invalid_response");
    }
    return {
        file_id: nullableString(item.file_id),
        filename: requiredString(item.filename, "filename"),
        content_type: requiredString(item.content_type, "content_type"),
        size: finiteNumber(item.size),
        source: source,
        source_label: requiredString(item.source_label, "source_label"),
        status: status,
        uploaded_at: nullableString(item.uploaded_at),
        processing_started_at: nullableString(item.processing_started_at),
        processed_at: nullableString(item.processed_at),
        updated_at: requiredString(item.updated_at, "updated_at"),
        error_code: nullableString(item.error_code),
        error_summary: nullableString(item.error_summary),
        chunk_count: finiteNumber(item.chunk_count),
        embedding_count: finiteNumber(item.embedding_count),
        downloadable: requiredBoolean(item.downloadable, "downloadable"),
        download_url: nullableString(item.download_url),
    };
}
function parseUserPreferences(value) {
    const item = record(value);
    return {
        customColors: array(item.custom_colors).filter((entry) => typeof entry === "string"),
        backgroundFileIds: array(item.background_file_ids).filter((entry) => typeof entry === "string"),
        activeBackgroundFileId: nullableString(item.active_background_file_id),
        backgroundMode: item.background_mode === "special" ? "special" : "default",
        kuliantnt: item.kuliantnt === true,
    };
}
function parseUserFile(value) {
    const item = record(value);
    return {
        fileId: string(item.file_id, ""),
        filename: string(item.filename, "未命名文件"),
        contentType: string(item.content_type, "application/octet-stream"),
        size: finiteNumber(item.size) ?? 0,
        createdAt: string(item.created_at, ""),
        url: string(item.url, ""),
    };
}
function parseTodoItem(value) {
    const item = record(value);
    const target = record(item.target);
    return {
        id: string(item.id, ""),
        title: string(item.title, "未命名 Todo"),
        detail: nullableString(item.detail),
        dueDate: nullableString(item.due_date),
        dueAt: nullableString(item.due_at),
        reminderAt: nullableString(item.reminder_at),
        timePrecision: string(item.time_precision, "none"),
        recurrenceKind: string(item.recurrence_kind, "none"),
        recurrenceIntervalDays: finiteNumber(item.recurrence_interval_days) ?? 0,
        recurrenceInterval: finiteNumber(item.recurrence_interval) ?? 0,
        recurrenceUnit: string(item.recurrence_unit, "day"),
        status: item.status === "completed" ? "completed" : "pending",
        createdAt: string(item.created_at, ""),
        updatedAt: string(item.updated_at, ""),
        completedAt: nullableString(item.completed_at),
        target: {
            targetRef: nullableString(target.target_ref),
            platform: string(target.platform, "unknown"),
            scopeType: string(target.scope_type, "unknown"),
            userId: nullableString(target.user_id),
            groupId: nullableString(target.group_id),
            accountId: nullableString(target.account_id),
            reminderSupported: target.reminder_supported === true,
            diagnostic: nullableString(target.diagnostic),
        },
    };
}
function parseTodoTargetOption(value) {
    const item = record(value);
    return {
        targetRef: string(item.target_ref, ""),
        platform: string(item.platform, "unknown"),
        accountId: nullableString(item.account_id),
        scopeType: string(item.scope_type, "unknown"),
        userId: nullableString(item.user_id),
        groupId: nullableString(item.group_id),
        reminderSupported: item.reminder_supported === true,
    };
}
function parseTodoTargetPage(value) {
    const data = record(value);
    return {
        items: array(data.items).map(parseTodoTargetOption),
        page: finiteNumber(data.page) ?? 1,
        pageSize: finiteNumber(data.page_size) ?? 100,
        total: finiteNumber(data.total) ?? 0,
        totalPages: finiteNumber(data.total_pages) ?? 1,
    };
}
async function fetchJson(input, init) {
    let response;
    try {
        response = await fetch(input, { credentials: "same-origin", ...init });
    }
    catch {
        throw new ConsoleApiError("无法连接本地管理接口，请检查服务是否仍在运行");
    }
    if (!response.ok) {
        let code = "request_failed";
        let message = `管理接口请求失败（HTTP ${response.status}）`;
        try {
            const payload = record(await response.json());
            const error = record(payload.error);
            code = string(error.code, code);
            message = string(error.message, message);
        }
        catch { /* 保留稳定的 HTTP 错误摘要。 */ }
        notifyUnauthorized(response.status);
        throw new ConsoleApiError(message, code, response.status);
    }
    try {
        return await response.json();
    }
    catch {
        throw new ConsoleApiError("管理接口返回了无效 JSON");
    }
}
async function mutatingJson(input, method, body, allowEmpty = false) {
    const response = await fetch(input, {
        method,
        credentials: "same-origin",
        headers: {
            "Content-Type": "application/json",
            Accept: "application/json",
            "X-CSRF-Token": csrfToken,
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    if (allowEmpty && response.status === 204)
        return {};
    if (!response.ok) {
        let code = "request_failed";
        let message = `管理接口请求失败（HTTP ${response.status}）`;
        try {
            const payload = record(await response.json());
            const error = record(payload.error);
            code = string(error.code, code);
            message = string(error.message, message);
        }
        catch { /* 保留稳定错误。 */ }
        notifyUnauthorized(response.status);
        throw new ConsoleApiError(message, code, response.status);
    }
    return await response.json();
}
async function responseError(response) {
    let code = "request_failed";
    let message = `管理接口请求失败（HTTP ${response.status}）`;
    try {
        const payload = record(await response.json());
        const error = record(payload.error);
        code = string(error.code, code);
        message = string(error.message, message);
    }
    catch { /* 保留稳定错误。 */ }
    notifyUnauthorized(response.status);
    return new ConsoleApiError(message, code, response.status);
}
function parseRuntime(value) {
    const item = record(value);
    return {
        ok: item.ok === true,
        ready: item.ready === true,
        state: item.state === "ready" || item.state === "setup_required" ? item.state : "unknown",
        version: string(item.version, "unknown"),
        startedAt: nullableString(item.started_at),
        uptimeSeconds: finiteNumber(item.uptime_seconds),
    };
}
function parseBootstrapStatus(value) {
    const item = record(value);
    return {
        initialized: item.initialized === true,
        setupRequired: item.setup_required === true,
        passwordResetPending: item.password_reset_pending === true,
        tokenFile: string(item.token_file, "config/secrets/bootstrap.token"),
        expiresAt: finiteNumber(item.expires_at),
    };
}
function parseSession(value) {
    const item = record(value);
    const token = string(item.csrf_token, "");
    if (!token)
        throw new ConsoleApiError("认证服务返回了无效会话", "invalid_response");
    return {
        username: string(item.username, "admin"),
        capabilities: array(item.capabilities).filter((value) => typeof value === "string"),
        csrfToken: token,
        expiresAt: finiteNumber(item.expires_at) ?? 0,
    };
}
function parseConfigurationPayload(value) {
    const payload = record(value);
    return parseConfigurationSnapshot(payload.configuration, payload.registered_tools, payload.restart);
}
function parseConfigurationSnapshot(value, toolsValue = [], restartValue = {}) {
    const item = record(value);
    const agent = record(item.agent);
    return {
        revision: string(item.revision, "missing"),
        fileExists: item.file_exists === true,
        fields: array(item.fields).map(parseConfigField),
        registeredTools: array(toolsValue).map(parseRegisteredTool),
        restartAvailable: record(restartValue).available === true,
        agent: Object.keys(agent).length === 0 ? null : {
            revision: string(agent.revision, "missing"),
            fileExists: agent.file_exists === true,
            source: typeof agent.source === "string" ? agent.source : "not_configured",
            editable: agent.editable === true,
            readOnly: agent.read_only === true,
            pendingRestart: agent.pending_restart === true,
            savedValue: agent.saved_value,
            runningValue: agent.running_value,
        },
    };
}
function parseRegisteredTool(value) {
    const item = record(value);
    return {
        name: string(item.name, "unknown"),
        description: string(item.description, ""),
    };
}
function parseConfigField(value) {
    const item = record(value);
    const valueType = item.value_type === "boolean" || item.value_type === "integer" || item.value_type === "string_list" ? item.value_type : "string";
    const sensitivity = item.sensitivity === "secret" || item.sensitivity === "restricted" ? item.sensitivity : "public";
    const source = typeof item.source === "string" ? item.source : "not_configured";
    return {
        key: string(item.key, "unknown"),
        module: string(item.module, "unknown"),
        valueType,
        source,
        overridden: item.overridden === true,
        editable: item.editable === true,
        configured: item.configured === true,
        valid: item.valid === true,
        revision: nullableString(item.revision),
        sensitivity,
        applyMode: item.apply_mode === "immediate" ? "immediate" : "restart",
        savedValue: item.saved_value,
        effectiveValue: item.effective_value,
        runningValue: item.running_value,
        pendingRestart: item.pending_restart === true,
    };
}
function parseProvider(value) {
    const item = record(value);
    const upstream = record(item.upstream);
    return {
        name: string(item.name, "unknown"),
        model: string(item.model, "unknown"),
        streaming: nullableBoolean(item.streaming),
        configured: item.configured === true,
        upstreamState: string(upstream.state, "unknown"),
        lastCheckedAt: nullableString(upstream.last_checked_at),
        errorSummary: nullableString(upstream.error_summary),
    };
}
function parsePlatform(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知平台"),
        configured: item.configured === true,
        enabled: item.enabled === true,
        state: runtimeState(item.state),
        lastEventAt: nullableString(item.last_event_at),
        lastErrorSummary: nullableString(item.last_error_summary),
        readyAt: nullableString(item.ready_at),
        resumedAt: nullableString(item.resumed_at),
        capabilityScopes: array(item.capability_scopes).map(parseCapabilityScope),
    };
}
function parseCapabilityScope(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知作用域"),
        enabled: item.enabled === true,
        capabilities: parseDirectionalCapabilities(item.capabilities),
    };
}
function parseCapabilities(value) {
    const item = record(value);
    return {
        text: valueState(item.text),
        markdown: valueState(item.markdown),
        image: valueState(item.image),
        file: valueState(item.file),
        mixedMessage: valueState(item.mixed_message),
        streaming: valueState(item.streaming),
    };
}
function parseDirectionalCapabilities(value) {
    const item = record(value);
    return {
        inbound: parseCapabilities(item.inbound),
        outbound: parseCapabilities(item.outbound),
    };
}
function parseStorage(value) {
    const item = record(value);
    return {
        id: string(item.id, "unknown"),
        label: string(item.label, "未知存储"),
        pathSummary: string(item.path_summary, "not_available"),
        state: runtimeState(item.state),
        exists: nullableBoolean(item.exists),
        readable: nullableBoolean(item.readable),
        writable: nullableBoolean(item.writable),
        errorSummary: nullableString(item.error_summary),
        schemaSummary: nullableString(item.schema_summary),
    };
}
function parseConfiguration(value) {
    const item = record(value);
    return {
        listen: string(item.listen, "unknown"),
        corsAllowlistConfigured: item.cors_allowlist_configured === true,
        rssEnabled: item.rss_enabled === true,
        toolCallingEnabled: item.tool_calling_enabled === true,
    };
}
function record(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value)
        ? value
        : {};
}
function array(value) {
    return Array.isArray(value) ? value : [];
}
function string(value, fallback) {
    return typeof value === "string" && value.length > 0 ? value : fallback;
}
function requiredString(value, field) {
    if (typeof value !== "string" || value.length === 0) {
        throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
    }
    return value;
}
function requiredBoolean(value, field) {
    if (typeof value !== "boolean")
        throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
    return value;
}
function requiredFiniteNumber(value, field) {
    const number = finiteNumber(value);
    if (number === null)
        throw new ConsoleApiError(`管理接口返回了无效 ${field}`, "invalid_response");
    return number;
}
function nullableString(value) {
    return typeof value === "string" && value.length > 0 ? value : null;
}
function nullableBoolean(value) {
    return typeof value === "boolean" ? value : null;
}
function finiteNumber(value) {
    return typeof value === "number" && Number.isFinite(value) ? value : null;
}
function runtimeState(value) {
    return value === "online" || value === "offline" || value === "available" || value === "not_available" || value === "not_configured"
        ? value
        : "unknown";
}
function valueState(value) {
    return value === "supported" || value === "disabled" || value === "unsupported" || value === "not_available" || value === "not_configured"
        ? value
        : "unknown";
}
