import type { ConfigFieldSnapshot } from "../../types.js";

/** 配置字段中文标签（沿用旧版 FIELD_LABELS 的完整映射）。 */
export const FIELD_LABELS: Record<string, string> = {
  "provider.openai.enabled": "启用 OpenAI",
  "provider.deepseek.enabled": "启用 DeepSeek",
  "provider.bigmodel.enabled": "启用 BigModel",
  "provider.gemini.enabled": "启用 Gemini",
  "command.prefix": "聊天命令前缀",
  "delivery.tts.provider": "语音 Provider",
  "delivery.tts.qwen_api_key": "千问 TTS API Key",
  "delivery.tts.qwen_base_url": "千问 TTS Base URL",
  "delivery.tts.qwen_model": "千问 TTS 模型",
  "delivery.tts.qwen_voice": "千问 TTS 音色",
  "delivery.tts.request_timeout_seconds": "请求超时（秒）",
  "delivery.tts.max_text_chars": "最大朗读字符数",
  "provider.openai.base_url": "OpenAI Base URL",
  "provider.openai.api_mode": "OpenAI API 模式",
  "provider.openai.api_key": "OpenAI API Key",
  "provider.deepseek.base_url": "DeepSeek Base URL",
  "provider.deepseek.api_key": "DeepSeek API Key",
  "provider.bigmodel.base_url": "BigModel Base URL",
  "provider.bigmodel.api_key": "BigModel API Key",
  "provider.gemini.base_url": "Gemini Base URL",
  "provider.gemini.api_key": "Gemini API Key",
  "provider.mimo.api_key": "MiMo API Key",
  "provider.opencode.api_key": "OpenCode API Key",
  "tools.web_search.tavily.api_key": "Tavily API Key",
  "weather.qweather.api_key": "和风天气 API Key",
  "weather.qweather.api_host": "QWeather API Host",
  "weather.qweather.geo_host": "QWeather Geo Host",
  "platform.qq_official.enabled": "QQ 官方入口",
  "platform.qq_official.app_id": "QQ AppID",
  "platform.qq_official.app_secret": "QQ AppSecret",
  "platform.onebot11.enabled": "OneBot 11 入口",
  "platform.onebot11.bind_host": "OneBot 绑定地址",
  "platform.onebot11.bind_port": "OneBot 绑定端口",
  "platform.onebot11.websocket_path": "OneBot WebSocket 路径",
  "platform.onebot11.access_token": "OneBot Access Token",
  "platform.wechat_service.enabled": "微信服务号入口",
  "platform.wechat_service.token": "微信 Token",
  "platform.wechat_service.app_id": "微信 AppID",
  "platform.wechat_service.app_secret": "微信 AppSecret",
  "platform.wechat_service.encryption_mode": "微信消息加密模式",
  "platform.wechat_service.encoding_aes_key": "微信 EncodingAESKey",
  "platform.wechat_service.bind_host": "微信回调监听地址",
  "platform.wechat_service.bind_port": "微信回调监听端口",
  "platform.wechat_service.callback_path": "微信回调路径",
  "features.rss.enabled": "RSS",
  "features.rss.translation_enabled": "RSS 翻译",
  "features.memory.consolidation_enabled": "Memory 整理",
  "features.memory.dream_enabled": "Session Dream",
  "features.todo.daily_reminder_enabled": "Todo 每日提醒",
  "features.todo.daily_reminder_time": "Todo 提醒时间",
  "console.enabled": "Web 控制台",
  "console.allowed_origins": "Web 控制台允许来源",
  "console.trusted_proxy_ips": "Web 控制台可信代理 IP",
  "console.secure_cookies": "Web 控制台安全 Cookie",
  "bootstrap.listen_host": "Core 监听地址",
  "bootstrap.listen_port": "Core 监听端口",
  "bootstrap.database_file": "数据库文件",
  "bootstrap.database_pool_size": "数据库连接池大小",
  "bootstrap.runtime_config_file": "运行配置文件",
  "bootstrap.master_key_file": "主密钥文件",
  "bootstrap.agent_config_file": "Agent 配置文件",
  "bootstrap.ops_config_file": "运维配置文件",
};

export type ConfigurationBusinessGroup =
  | "models-providers"
  | "model-routing"
  | "online-tools"
  | "memory-knowledge"
  | "replies-voice"
  | "platforms"
  | "tasks-notifications"
  | "system-security"
  | "advanced";

export const BUSINESS_GROUPS: ReadonlyArray<{ id: ConfigurationBusinessGroup; label: string; description: string }> = [
  { id: "models-providers", label: "模型与供应商", description: "配置内置模型服务、自定义 Provider 及对应凭据。" },
  { id: "model-routing", label: "模型路由", description: "为私聊、群聊、辅助任务和搜索场景设置候选模型链。" },
  { id: "online-tools", label: "联网与工具", description: "管理联网搜索、天气服务、Tool Calling 与场景白名单。" },
  { id: "memory-knowledge", label: "记忆与知识库", description: "管理记忆整理、Session Dream 和本地知识检索策略。" },
  { id: "replies-voice", label: "回复与语音", description: "设置聊天命令入口和最终回复的语音能力。" },
  { id: "platforms", label: "平台接入", description: "配置 QQ 官方、OneBot 11 与微信服务号入口。" },
  { id: "tasks-notifications", label: "待办与通知", description: "管理 Todo 每日提醒、RSS 订阅和翻译开关。" },
  { id: "system-security", label: "系统与安全", description: "查看控制台、安全边界、只读启动参数和界面偏好。" },
  { id: "advanced", label: "高级兼容配置", description: "保留当前版本尚未归类的受管字段，不会在其他字段保存时删除。" },
];

export const FIELD_SECTIONS: ReadonlyArray<{
  business: ConfigurationBusinessGroup;
  label: string;
  prefix: string;
  description?: string;
}> = [
  { business: "replies-voice", label: "命令交互", prefix: "command." },
  {
    business: "replies-voice",
    label: "语音回复",
    prefix: "delivery.tts.",
    description: "修改后需要重启。Web 控制台只配置全局 TTS 能力；Provider 关闭或千问 API Key 未配置时，/语音 开启会被拒绝。",
  },
  { business: "models-providers", label: "内置模型服务", prefix: "provider." },
  { business: "platforms", label: "QQ 官方入口", prefix: "platform.qq_official." },
  { business: "platforms", label: "OneBot 11 入口", prefix: "platform.onebot11." },
  { business: "platforms", label: "微信服务号入口", prefix: "platform.wechat_service." },
  { business: "tasks-notifications", label: "RSS 订阅", prefix: "features.rss." },
  { business: "memory-knowledge", label: "记忆策略", prefix: "features.memory." },
  { business: "tasks-notifications", label: "Todo 提醒", prefix: "features.todo." },
  { business: "online-tools", label: "联网搜索凭据", prefix: "tools.web_search." },
  { business: "online-tools", label: "天气服务", prefix: "weather." },
  { business: "system-security", label: "Web 控制台", prefix: "console." },
  { business: "system-security", label: "基础运行（只读）", prefix: "bootstrap." },
];

export function configFieldLabel(key: string): string {
  return FIELD_LABELS[key] ?? key;
}

/** 配置键只在一个业务域出现；未知或后续新增字段进入高级兼容区，不会被静默遗漏。 */
export function businessGroupOf(key: string): ConfigurationBusinessGroup {
  return FIELD_SECTIONS.find((section) => key.startsWith(section.prefix))?.business ?? "advanced";
}

/** 同一业务域内的字段按 FIELD_SECTIONS 分节。 */
export function groupFieldsBySection(fields: readonly ConfigFieldSnapshot[]): Array<{
  label: string;
  description: string | undefined;
  fields: ConfigFieldSnapshot[];
}> {
  const sections = new Map<string, { label: string; description: string | undefined; fields: ConfigFieldSnapshot[] }>();
  for (const field of fields) {
    const section = FIELD_SECTIONS.find((candidate) => field.key.startsWith(candidate.prefix));
    const label = section?.label ?? "未归类字段";
    const entry = sections.get(label) ?? { label, description: section?.description, fields: [] as ConfigFieldSnapshot[] };
    entry.fields.push(field);
    sections.set(label, entry);
  }
  return [...sections.values()];
}
