//! 随版本审核发布的可信连接模板；模型目录无权覆盖这里的安全元数据。

use serde::Serialize;

use crate::config::agent::AgentProviderKind;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: AgentProviderKind,
    pub base_url: &'static str,
    pub auth_header: &'static str,
    pub auth_scheme: &'static str,
}

pub fn provider_presets() -> Vec<ProviderPreset> {
    use AgentProviderKind::{OpenAiCompatible as Chat, OpenAiResponses as Responses};
    // 地址复用现有公开配置和已支持的 Adapter，不引入未实现协议的品牌模板。
    [
        (
            "opencode_zen",
            "OpenCode Zen Responses",
            Responses,
            "https://opencode.ai/zen/v1",
        ),
        (
            "opencode_zen_chat",
            "OpenCode Zen Chat",
            Chat,
            "https://opencode.ai/zen/v1",
        ),
        (
            "opencode_go",
            "OpenCode Go",
            Chat,
            "https://opencode.ai/zen/go/v1",
        ),
        (
            "openai_custom",
            "OpenAI Responses",
            Responses,
            "https://api.openai.com/v1",
        ),
        (
            "deepseek_custom",
            "DeepSeek",
            Chat,
            "https://api.deepseek.com",
        ),
        (
            "bigmodel_custom",
            "智谱 BigModel",
            Chat,
            "https://open.bigmodel.cn/api/paas/v4",
        ),
        (
            "gemini_custom",
            "Gemini Chat",
            Chat,
            "https://generativelanguage.googleapis.com/v1beta/openai",
        ),
    ]
    .into_iter()
    .map(|(id, name, kind, base_url)| ProviderPreset {
        id,
        name,
        kind,
        base_url,
        auth_header: "Authorization",
        auth_scheme: "Bearer",
    })
    .collect()
}
