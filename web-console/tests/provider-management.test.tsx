import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Provider as JotaiProvider } from "jotai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ConsoleApiError,
  discoverConnectionModels,
  fetchModelMetadata,
  testProviderConnection,
  updateAgentConfiguration,
  updateConnectionCredential,
  updateModelOverride,
} from "../src/api.js";
import { ModelManagerPanel } from "../src/features/configuration/model-manager-panel.js";
import { ProviderConnections } from "../src/features/configuration/provider-connections.js";
import type { ConfigFieldSnapshot, ConfigurationSnapshot } from "../src/types.js";

vi.mock("../src/api.js", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/api.js")>();
  return {
    ...actual,
    testProviderConnection: vi.fn(),
    updateAgentConfiguration: vi.fn(),
    updateConnectionCredential: vi.fn(),
    discoverConnectionModels: vi.fn(),
    fetchModelMetadata: vi.fn(),
    updateModelOverride: vi.fn(),
  };
});

const mockedTestConnection = vi.mocked(testProviderConnection);
const mockedUpdateAgent = vi.mocked(updateAgentConfiguration);
const mockedUpdateCredential = vi.mocked(updateConnectionCredential);
const mockedDiscover = vi.mocked(discoverConnectionModels);
const mockedFetchMetadata = vi.mocked(fetchModelMetadata);
const mockedUpdateOverride = vi.mocked(updateModelOverride);

const legacyProvider = {
  display_name: null,
  enabled: true,
  kind: "openai_responses",
  base_url: "https://legacy.example/v1",
  api_key_env: "TEST_PROXY_KEY",
  auth_header: "Authorization",
  auth_scheme: "Bearer",
};

function publicField(overrides: Partial<ConfigFieldSnapshot>): ConfigFieldSnapshot {
  return {
    key: "provider.openai.base_url",
    module: "provider",
    valueType: "string",
    source: "environment",
    overridden: false,
    editable: true,
    configured: true,
    valid: true,
    revision: null,
    sensitivity: "public",
    applyMode: "restart",
    savedValue: "https://api.openai.example/v1",
    effectiveValue: "https://api.openai.example/v1",
    runningValue: "https://api.openai.example/v1",
    pendingRestart: false,
    ...overrides,
  };
}

type CredentialFixture = NonNullable<ConfigurationSnapshot["providers"]>["credentials"];

function snapshotFixture(options: {
  credentials?: CredentialFixture;
  modelRoutes?: Record<string, unknown>;
}): ConfigurationSnapshot {
  const agentValue = {
    providers: { MyProxy: legacyProvider },
    model_routes: options.modelRoutes ?? { 主链: { candidates: ["openai:gpt-test"] } },
  };
  return {
    revision: "runtime-r",
    fileExists: true,
    providers: {
      adapters: ["openai_compatible", "openai_responses"],
      presets: [
        { id: "trusted_template", name: "新增服务端模板", kind: "openai_compatible", base_url: "https://preset.example/v1", auth_header: "Authorization", auth_scheme: "Bearer" },
      ],
      credentials: options.credentials ?? {
        MyProxy: { configured: true, editable: false, revision: "missing", pending_restart: false },
      },
    },
    fields: [
      publicField({ key: "provider.openai.base_url" }),
      publicField({ key: "provider.openai.enabled", valueType: "boolean", savedValue: true, effectiveValue: true }),
    ],
    registeredTools: [],
    restartAvailable: false,
    agent: {
      revision: "agent-a",
      fileExists: true,
      source: "agent_toml",
      editable: true,
      readOnly: false,
      pendingRestart: false,
      savedValue: agentValue,
      runningValue: { providers: { MyProxy: legacyProvider } },
    },
  };
}

function renderConnections(snapshot: ConfigurationSnapshot) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  // 预置一条 configuration 缓存，用于断言 Credential 明文不会进入客户端持久状态。
  queryClient.setQueryData(["configuration"], { seeded: true });
  const onResult = vi.fn();
  const onDraftChange = vi.fn();
  const onSecretDraftChange = vi.fn();
  const tree = (snapshot: ConfigurationSnapshot) => (
    <JotaiProvider>
      <QueryClientProvider client={queryClient}>
        <ProviderConnections
          snapshot={snapshot}
          fields={snapshot.fields}
          publicDraft={{}}
          onDraftChange={onDraftChange}
          onResult={onResult}
          secretFields={snapshot.fields.filter((field) => field.sensitivity === "secret")}
          secretDraft={{}}
          onSecretDraftChange={onSecretDraftChange}
          onRequestSecretClear={() => undefined}
        />
      </QueryClientProvider>
    </JotaiProvider>
  );
  const rendered = render(tree(snapshot));
  const rerender = (next: ConfigurationSnapshot) => rendered.rerender(tree(next));
  const providerList = () => screen.getByRole("complementary", { name: "服务商列表" });
  const selectProvider = async (id: string) => {
    await userEvent.click(within(providerList()).getByRole("button", { name: new RegExp(id) }));
  };
  const customCard = () => screen.getByRole("region", { name: "供应商详情 MyProxy" });
  return { queryClient, onResult, providerList, selectProvider, customCard, rerender, onDraftChange, onSecretDraftChange };
}

function persistenceDump(queryClient: QueryClient): string {
  // 查询缓存与 localStorage 全量 dump：任何 Credential 明文落盘都会在这里出现。
  const caches = queryClient.getQueryCache().getAll().map((query) => JSON.stringify(query.state.data));
  return `${caches.join("\n")}\n${JSON.stringify({ ...localStorage })}`;
}

beforeEach(() => {
  localStorage.clear();
  mockedUpdateAgent.mockResolvedValue({ revision: "runtime-r" } as never);
  mockedUpdateCredential.mockResolvedValue({ revision: "runtime-r" } as never);
  // 模型管理面板内联在详情栏：默认给一条空 metadata，避免无关用例渲染查询错误告警。
  mockedFetchMetadata.mockResolvedValue({ revision: "models-r0", provider: "", models: [], overrides: [], catalog_source: {} });
  mockedTestConnection.mockResolvedValue({
    network: "success",
    authentication: "success",
    adapter: "success",
    model_call: "success",
    elapsed_ms: 123,
    http_status: 200,
    category: "ok",
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

describe("ProviderConnections 自定义连接", () => {
  it("历史 MyProxy 卡片按原始 ID 展示与保存，ID 不可修改", async () => {
    const view = renderConnections(snapshotFixture({}));
    // 主从布局：默认选中第一个服务商（内置 OpenAI），需在左侧列表选择 MyProxy。
    await view.selectProvider("MyProxy");
    const card = view.customCard();
    const identity = within(card).getByLabelText("Connection ID");
    expect(identity).toHaveValue("MyProxy");
    expect(identity).toBeDisabled();
    expect(within(card).getByText("Credential：已配置")).toBeInTheDocument();
    expect(within(card).getByText(/Credential 引用：TEST_PROXY_KEY（不可修改）/)).toBeInTheDocument();

    await userEvent.click(within(card).getByRole("button", { name: "保存连接" }));
    await waitFor(() => expect(mockedUpdateAgent).toHaveBeenCalledTimes(1));
    expect(mockedUpdateAgent).toHaveBeenCalledWith("agent-a", [
      {
        action: "set_provider",
        id: "MyProxy",
        provider: {
          display_name: "MyProxy",
          enabled: true,
          kind: "openai_responses",
          base_url: "https://legacy.example/v1",
          chat_fallback: false,
          auth_header: "Authorization",
          auth_scheme: "Bearer",
          request_timeout_seconds: null,
        },
      },
    ]);
  });

  it("新建连接拒绝大小写别名冲突，成功时提交 canonical set_provider", async () => {
    const view = renderConnections(snapshotFixture({}));
    await userEvent.click(screen.getByRole("button", { name: "+ 新建供应商" }));
    const dialog = screen.getByRole("dialog");

    // 冲突：与已有 MyProxy 大小写不敏感重复，本地直接拒绝，不发请求。
    await userEvent.type(within(dialog).getByLabelText("Connection ID"), "MyProxy");
    await userEvent.type(within(dialog).getByLabelText("Base URL"), "https://new.example/v1");
    await userEvent.click(within(dialog).getByRole("button", { name: "创建连接" }));
    expect(await screen.findByText("该 Connection ID 已存在（大小写不敏感），请使用新的 ID")).toBeInTheDocument();
    expect(mockedUpdateAgent).not.toHaveBeenCalled();

    // 修正为新 ID 后按 canonical 小写提交，预设字段沿用服务端模板。
    const identity = within(dialog).getByLabelText("Connection ID");
    await userEvent.clear(identity);
    await userEvent.type(identity, "new_router");
    await userEvent.selectOptions(within(dialog).getByLabelText("供应商预设"), "trusted_template");
    await userEvent.click(within(dialog).getByRole("button", { name: "创建连接" }));
    await waitFor(() => expect(mockedUpdateAgent).toHaveBeenCalledTimes(1));
    const change = mockedUpdateAgent.mock.calls[0]?.[1]?.[0] as Record<string, unknown>;
    expect(change.action).toBe("set_provider");
    expect(change.id).toBe("new_router");
    expect(change.provider).toMatchObject({ kind: "openai_compatible", base_url: "https://preset.example/v1" });
    expect("api_key_env" in (change.provider as Record<string, unknown>)).toBe(false);
  });

  it("Credential 支持替换与显式确认清除，明文不进入客户端持久状态", async () => {
    const snapshot = snapshotFixture({
      credentials: { MyProxy: { configured: true, editable: true, revision: "secret-a", pending_restart: true } },
    });
    const view = renderConnections(snapshot);
    await view.selectProvider("MyProxy");
    const card = view.customCard();
    const keyInput = within(card).getByLabelText("API 密钥（Credential）");
    expect(keyInput).toHaveAttribute("type", "password");
    expect(keyInput).toHaveValue("");

    const secret = "test-only-sensitive-value";
    await userEvent.type(keyInput, secret);
    await userEvent.click(within(card).getByRole("button", { name: "保存 API Key" }));
    await waitFor(() => expect(mockedUpdateCredential).toHaveBeenCalledWith("MyProxy", "agent-a", "secret-a", secret));
    // 保存成功后立即清空输入；明文不出现在查询缓存、localStorage 或页面。
    await waitFor(() => expect(within(card).getByLabelText("API 密钥（Credential）")).toHaveValue(""));
    expect(persistenceDump(view.queryClient)).not.toContain(secret);
    expect(screen.queryByText(secret)).not.toBeInTheDocument();

    await userEvent.click(within(card).getByRole("button", { name: "清除 API Key" }));
    expect(await screen.findByText(/确认清除/)).toBeInTheDocument();
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "清除" }));
    await waitFor(() => expect(mockedUpdateCredential).toHaveBeenLastCalledWith("MyProxy", "agent-a", "secret-a", null));
    expect(persistenceDump(view.queryClient)).not.toContain(secret);
  });

  it("连接测试调用真实诊断接口并展示分类结果", async () => {
    const view = renderConnections(snapshotFixture({}));
    await view.selectProvider("MyProxy");
    const card = view.customCard();
    await userEvent.type(within(card).getByLabelText("测试模型 ID（将产生一次最小真实调用）"), "gpt-test");
    mockedTestConnection.mockResolvedValueOnce({
      network: "success",
      authentication: "failed",
      adapter: "success",
      model_call: "not_tested",
      elapsed_ms: 88,
      http_status: 401,
      category: "auth_rejected",
    });
    await userEvent.click(within(card).getByRole("button", { name: "测试已保存连接" }));
    await waitFor(() => expect(mockedTestConnection).toHaveBeenCalledWith("MyProxy", "agent-a", "gpt-test"));
    // 模型调用未测试必须与网络/认证成功区分展示，不把诊断成功等同于真实调用成功。
    const status = await screen.findByText(/网络：成功 · 认证：失败 · 协议：成功 · 模型调用：未测试 · 88 ms · HTTP 401 · auth_rejected/);
    expect(status).toHaveAttribute("role", "status");
  });

  it.each(["credential", "connection"])("自定义连接 %s revision 改变后清除旧错误诊断", async (change) => {
    const snapshot = snapshotFixture({ credentials: { MyProxy: { configured: true, editable: true, revision: "key-a", pending_restart: false } } });
    const view = renderConnections(snapshot);
    mockedFetchMetadata.mockResolvedValue({ revision: "models-r1", provider: "myproxy", models: [{ id: "known" }], overrides: [], catalog_source: {} });
    await view.selectProvider("MyProxy");
    await screen.findByText("模型 ID：known");
    mockedTestConnection.mockRejectedValue(new Error("旧凭据认证失败"));
    await userEvent.click(screen.getByRole("button", { name: "测试已保存连接" }));
    await screen.findByText("旧凭据认证失败");
    await userEvent.click(screen.getByRole("button", { name: "检查" }));
    await screen.findByText(/✗ 旧凭据认证失败/);
    const next = structuredClone(snapshot);
    if (change === "credential") next.providers!.credentials.MyProxy!.revision = "key-b";
    else next.agent!.revision = "agent-b";
    view.rerender(next);
    expect(screen.getByText("最近测试：尚未测试")).toBeInTheDocument();
    expect(screen.queryByText(/旧凭据认证失败/)).not.toBeInTheDocument();
  });

  it("删除供应商需确认；服务端拒绝（仍被路线引用）时原样展示错误，不伪造成功", async () => {
    const view = renderConnections(snapshotFixture({}));
    await view.selectProvider("MyProxy");
    const card = view.customCard();
    mockedUpdateAgent.mockRejectedValueOnce(
      new ConsoleApiError("模型路线 主链 仍引用 Connection MyProxy，请先调整路线", "config_invalid", 400),
    );
    await userEvent.click(within(card).getByRole("button", { name: "删除供应商" }));
    expect(await screen.findByText(/仍被路线引用时会拒绝/)).toBeInTheDocument();
    await userEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "删除" }));
    await waitFor(() => expect(mockedUpdateAgent).toHaveBeenCalledWith("agent-a", [{ action: "remove_provider", id: "MyProxy" }]));
    expect(await screen.findByRole("alert")).toHaveTextContent(/仍引用 Connection MyProxy/);
    expect(view.onResult).toHaveBeenCalledWith({ error: true, text: expect.stringContaining("仍引用 Connection MyProxy") });
  });

  it("revision 冲突时不覆盖服务器新版本，保留本地输入并提示比较", async () => {
    const view = renderConnections(snapshotFixture({}));
    await view.selectProvider("MyProxy");
    const card = view.customCard();
    mockedUpdateAgent.mockRejectedValueOnce(new ConsoleApiError("配置已被其他操作修改", "config_conflict", 409));
    await userEvent.click(within(card).getByRole("button", { name: "保存连接" }));
    await waitFor(() => expect(mockedUpdateAgent).toHaveBeenCalled());
    expect(await screen.findByRole("alert")).toHaveTextContent("未覆盖服务器版本");
    expect(screen.getByRole("alert")).toHaveTextContent("请刷新后比较");
  });
});

describe("ProviderConnections 内置连接", () => {
  function withSecret() {
    const snapshot = snapshotFixture({});
    snapshot.fields.push(publicField({ key: "provider.openai.api_key", sensitivity: "secret", revision: "key-a", savedValue: null, effectiveValue: null, runningValue: null }));
    return snapshot;
  }

  it("密钥只有 password 入口，只写 secretDraft", async () => {
    const view = renderConnections(withSecret());
    const pane = screen.getByRole("region", { name: "供应商详情 openai" });
    const inputs = within(pane).getAllByLabelText(/API 密钥|API Key/i);
    expect(inputs).toHaveLength(1);
    expect(inputs[0]).toHaveAttribute("type", "password");
    expect(pane.querySelectorAll('input[type="password"]')).toHaveLength(1);
    expect(within(pane).queryByText("更多设置")).not.toBeInTheDocument();
    await userEvent.type(inputs[0]!, "x");
    expect(view.onSecretDraftChange).toHaveBeenLastCalledWith("provider.openai.api_key", { value: "x", clear: false });
    expect(view.onDraftChange).not.toHaveBeenCalled();
  });

  it.each(["replace", "clear", "connection"])("%s 后连接与逐模型诊断失效，编辑草稿保留", async (change) => {
    mockedFetchMetadata.mockResolvedValue({ revision: "models-r1", provider: "openai", models: [{ id: "known" }], overrides: [], catalog_source: {} });
    const snapshot = withSecret();
    const view = renderConnections(snapshot);
    await screen.findByText("模型 ID：known");
    await userEvent.click(screen.getByRole("button", { name: "测试已保存连接" }));
    await screen.findByText(/模型调用：成功/);
    await userEvent.click(screen.getByRole("button", { name: "检查" }));
    await screen.findByText(/✓ 可用/);
    await userEvent.click(screen.getByRole("button", { name: "编辑" }));
    await userEvent.type(screen.getByLabelText("显示名称（留空继承）"), "保留草稿");
    const next = structuredClone(snapshot);
    if (change === "connection") next.revision = "runtime-new";
    else {
      const field = next.fields.find((field) => field.sensitivity === "secret")!;
      field.revision = "key-new";
      field.configured = change !== "clear";
    }
    view.rerender(next);
    expect(screen.getByText("最近测试：尚未测试")).toBeInTheDocument();
    expect(screen.queryByText(/✓ 可用/)).not.toBeInTheDocument();
    expect(screen.getByLabelText("显示名称（留空继承）")).toHaveValue("保留草稿");
  });

  it("旧配置诊断迟到时不能恢复成功状态", async () => {
    mockedFetchMetadata.mockResolvedValue({ revision: "models-r1", provider: "openai", models: [{ id: "known" }], overrides: [], catalog_source: {} });
    let finish!: (value: Record<string, unknown>) => void;
    const pending = new Promise<Record<string, unknown>>((resolve) => { finish = resolve; });
    mockedTestConnection.mockReturnValue(pending);
    const snapshot = withSecret();
    const view = renderConnections(snapshot);
    await screen.findByText("模型 ID：known");
    await userEvent.click(screen.getByRole("button", { name: "测试已保存连接" }));
    await userEvent.click(screen.getByRole("button", { name: "检查" }));
    view.rerender({ ...snapshot, revision: "runtime-new" });
    await act(async () => { finish({ model_call: "success", elapsed_ms: 1 }); await pending; });
    expect(screen.getByText("最近测试：尚未测试")).toBeInTheDocument();
    expect(screen.queryByText(/✓ 可用|模型调用：成功/)).not.toBeInTheDocument();
  });

  it("内置卡片承载 provider.* 公开字段并内联模型管理面板", () => {
    renderConnections(snapshotFixture({}));
    // 主从布局默认选中第一个服务商（内置 OpenAI）。
    const pane = screen.getByRole("region", { name: "供应商详情 openai" });
    expect(within(pane).getByLabelText("OpenAI Base URL")).toHaveValue("https://api.openai.example/v1");
    expect(within(pane).getByRole("region", { name: "模型管理 openai" })).toBeInTheDocument();
  });
});

describe("模型管理面板", () => {
  const metadata = {
    revision: "models-r1",
    provider: "myproxy",
    models: [
      { id: "known", display_name: "本地显示名称", enabled: false, status: "beta", provenance: { display_name: "local_override" } },
    ],
    overrides: [{ provider: "myproxy", id: "known", enabled: false }],
    catalog_source: {},
  };

  async function openManager() {
    const view = renderConnections(snapshotFixture({}));
    mockedFetchMetadata.mockResolvedValue(metadata);
    await view.selectProvider("MyProxy");
    return view;
  }

  function renderDiscoveryManager() {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
    const initial = { connection: "openai", discoveryRevision: "connection-a", credentialRevision: "credential-a" };
    const tree = (scope: typeof initial) => (
      <QueryClientProvider client={client}>
        <ModelManagerPanel snapshot={snapshotFixture({})} enabled {...scope} />
      </QueryClientProvider>
    );
    const view = render(tree(initial));
    return { rerender: (change: keyof typeof initial) => view.rerender(tree({ ...initial, [change]: "new-value" })) };
  }

  it.each(["connection", "discoveryRevision", "credentialRevision"] as const)("%s 改变清除发现列表及成功文案，保留编辑草稿", async (change) => {
    mockedDiscover.mockResolvedValueOnce({ state: "success", models: [{ id: "old-model" }] });
    const view = renderDiscoveryManager();
    await userEvent.click(screen.getByRole("button", { name: "同步模型" }));
    await screen.findByText("模型 ID：old-model");
    expect(screen.getByRole("status")).toHaveTextContent("获取成功");
    await userEvent.click(screen.getByRole("button", { name: "编辑" }));
    await userEvent.type(screen.getByLabelText("显示名称（留空继承）"), "尚未保存的草稿");

    view.rerender(change);

    expect(screen.queryByText("模型 ID：old-model")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "加入 Route" })).not.toBeInTheDocument();
    expect(screen.getByRole("status")).not.toHaveTextContent("获取成功");
    expect(screen.getByLabelText("模型 ID")).toHaveValue("old-model");
    expect(screen.getByLabelText("显示名称（留空继承）")).toHaveValue("尚未保存的草稿");
  });

  describe.each(["connection", "discoveryRevision", "credentialRevision"] as const)("%s 改变后的迟到发现请求", (change) => {
    it.each(["success", "error"])("旧请求 %s 不覆盖当前版本结果及文案", async (outcome) => {
      let resolve!: (value: Record<string, unknown>) => void;
      let reject!: (reason: Error) => void;
      const pending = new Promise<Record<string, unknown>>((done, fail) => { resolve = done; reject = fail; });
      mockedDiscover.mockReturnValueOnce(pending);
      const view = renderDiscoveryManager();
      await userEvent.click(screen.getByRole("button", { name: "同步模型" }));
      expect(mockedDiscover).toHaveBeenLastCalledWith("openai", "connection-a");
      view.rerender(change);
      // 旧请求仍在等待时，新版本可以独立同步。
      mockedDiscover.mockResolvedValueOnce({ state: "success", models: [{ id: "new-model" }] });
      await userEvent.click(screen.getByRole("button", { name: "同步模型" }));
      await screen.findByText("模型 ID：new-model");
      await act(async () => {
        if (outcome === "success") resolve({ state: "success", models: [{ id: "old-model" }] });
        else reject(new Error("旧版本获取失败"));
        await pending.catch(() => undefined);
      });
      expect(screen.queryByText("模型 ID：old-model")).not.toBeInTheDocument();
      expect(screen.getByText("模型 ID：new-model")).toBeInTheDocument();
      expect(screen.getByRole("status")).toHaveTextContent("获取成功");
      expect(screen.getByRole("status")).not.toHaveTextContent("旧版本获取失败");
    });
  });

  it("发现结果与本地 metadata 合并展示，discovery 状态互不混淆", async () => {
    await openManager();
    const knownRow = () => screen.getByText("模型 ID：known").closest("article")!;
    expect(await screen.findByText("模型 ID：known")).toBeInTheDocument();
    // 徽标化展示：来源（本地覆盖）、本地禁用状态与目录 status 互不混淆。
    expect(within(knownRow()).getByText("本地覆盖")).toBeInTheDocument();
    expect(within(knownRow()).getByText("已禁用")).toBeInTheDocument();
    expect(within(knownRow()).getByText("beta")).toBeInTheDocument();

    mockedDiscover.mockResolvedValueOnce({ state: "success", models: [{ id: "private/unknown" }], category: "ok", http_status: 200, elapsed_ms: 10 });
    await userEvent.click(screen.getByRole("button", { name: "同步模型" }));
    expect(mockedDiscover).toHaveBeenCalledWith("MyProxy", "agent-a");
    expect(await screen.findByText("模型 ID：private/unknown")).toBeInTheDocument();
    // 控制行与 footer 状态都会展示获取结果文案
    const labels = await screen.findAllByText(/获取成功：1 个模型/);
    expect(labels.length).toBeGreaterThan(0);
  });

  it("启用/禁用模型调用独立 CAS 的 model-override 接口", async () => {
    await openManager();
    await screen.findByText("模型 ID：known");
    mockedUpdateOverride.mockResolvedValueOnce({ revision: "runtime-r" } as never);
    await userEvent.click(screen.getByRole("switch", { name: "启用模型 known" }));
    await waitFor(() => expect(mockedUpdateOverride).toHaveBeenCalledWith("MyProxy", "models-r1", {
      provider: "myproxy",
      id: "known",
      enabled: true,
    }));
  });

  it("模型加入既有 Route 时构造 provider:model 候选并提交 set_model_route", async () => {
    await openManager();
    mockedDiscover.mockResolvedValueOnce({ state: "success", models: [{ id: "private/unknown" }], category: "ok", http_status: 200, elapsed_ms: 10 });
    await userEvent.click(screen.getByRole("button", { name: "同步模型" }));
    const row = await screen.findByText("模型 ID：private/unknown").then((element) => element.closest("article")!);
    await userEvent.selectOptions(within(row).getByLabelText("为 private/unknown 选择 Route"), "主链");
    mockedUpdateAgent.mockResolvedValueOnce({ revision: "runtime-r" } as never);
    await userEvent.click(within(row).getByRole("button", { name: "加入 Route" }));
    await waitFor(() => expect(mockedUpdateAgent).toHaveBeenCalledWith("agent-a", [
      { action: "set_model_route", name: "主链", candidates: ["openai:gpt-test", "myproxy:private/unknown"] },
    ]));
    expect(await screen.findByText(/已加入 Route 主链；重启后生效/)).toBeInTheDocument();
  });

  it.each([
    new ConsoleApiError("model conflict", "config_conflict", 409),
    new ConsoleApiError("服务端保存失败", "internal_error", 500),
    new Error("网络连接中断"),
  ])("编辑保存失败保留输入：%s", async (failure) => {
    await openManager();
    await screen.findByText("模型 ID：known");
    await userEvent.click(screen.getByRole("button", { name: "编辑" }));
    await userEvent.type(screen.getByLabelText("显示名称（留空继承）"), "尚未保存的名称");
    mockedUpdateOverride.mockRejectedValueOnce(failure);
    await userEvent.click(screen.getByRole("button", { name: "保存本地模型" }));
    const message = failure instanceof ConsoleApiError && failure.status === 409 ? /本地输入已保留/ : failure.message;
    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(screen.getByLabelText("显示名称（留空继承）")).toHaveValue("尚未保存的名称");
    expect(screen.queryByText("本地模型已保存；重启后生效。")).not.toBeInTheDocument();
    expect(mockedUpdateOverride).toHaveBeenCalledWith("MyProxy", "models-r1", expect.objectContaining({ display_name: "尚未保存的名称" }));
  });

  it("编辑保存等待真实成功后才关闭，行内启停不关闭编辑器", async () => {
    await openManager();
    await screen.findByText("模型 ID：known");
    await userEvent.click(screen.getByRole("button", { name: "编辑" }));
    await userEvent.type(screen.getByLabelText("显示名称（留空继承）"), "编辑名称");
    mockedUpdateOverride.mockResolvedValueOnce({ revision: "models-r2" } as never);
    await userEvent.click(screen.getByRole("switch", { name: "启用模型 known" }));
    await screen.findByText("本地模型已保存；重启后生效。");
    expect(screen.getByLabelText("显示名称（留空继承）")).toHaveValue("编辑名称");
    let finish!: () => void;
    mockedUpdateOverride.mockImplementationOnce(() => new Promise((resolve) => { finish = () => resolve({ revision: "models-r3" } as never); }));
    await userEvent.click(screen.getByRole("button", { name: "保存本地模型" }));
    expect(screen.getByLabelText("显示名称（留空继承）")).toHaveValue("编辑名称");
    await act(async () => { finish(); });
    await waitFor(() => expect(screen.queryByLabelText("显示名称（留空继承）")).not.toBeInTheDocument());
  });

  it("模型保存 revision 冲突时提示刷新且不覆盖服务器版本", async () => {
    await openManager();
    await screen.findByText("模型 ID：known");
    mockedUpdateOverride.mockRejectedValueOnce(new ConsoleApiError("model conflict", "config_conflict", 409));
    await userEvent.click(screen.getByRole("switch", { name: "启用模型 known" }));
    expect(await screen.findByText(/未覆盖服务器版本/)).toBeInTheDocument();
  });
});
