//! Agent 场景策略配置。
//!
//! 配置文件只保存非敏感、可版本管理的运行策略；Provider key、base URL 等仍由环境变量提供。

use std::{collections::HashMap, env, fs, path::Path};

use serde::Deserialize;

use crate::{
    error::LlmError,
    provider::types::{ModelProvider, ModelRoute, ReasoningEffort},
};

pub const DEFAULT_AGENT_CONFIG_PATH: &str = "config/agent.toml";
pub const AGENT_CONFIG_FILE_ENV: &str = "AGENT_CONFIG_FILE";

const DEFAULT_PRIVATE_PROFILE: &str = "balanced";
const DEFAULT_GROUP_PROFILE: &str = "fast";
const DEFAULT_PRIVATE_ENABLED_TOOLS: &[&str] = &[
    "get_weather",
    "get_train_schedule",
    "get_rss_recent_items",
    "list_todos",
    "get_todo",
    "create_todo",
    "complete_todos",
    "edit_todo",
    "cancel_todo",
    "restore_todos",
    "delete_todos",
    "merge_todos",
];
const DEFAULT_GROUP_ENABLED_TOOLS: &[&str] =
    &["get_weather", "get_train_schedule", "get_rss_recent_items"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatScene {
    Private,
    Group,
}

impl ChatScene {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LegacyAgentConfig {
    pub main_model: String,
    pub max_output_tokens: u64,
    pub openai_search_model: String,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub tool_calling_max_rounds: u64,
    pub group_llm_model: Option<String>,
    pub private_llm_model: Option<String>,
    pub group_openai_search_model: Option<String>,
    pub private_openai_search_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeConfig {
    source: AgentConfigSource,
    providers: HashMap<String, AgentProviderConfig>,
    routes: HashMap<String, ModelRoute>,
    search_routes: HashMap<String, String>,
    profiles: HashMap<String, AgentProfile>,
    scenes: AgentScenes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConfigSource {
    BuiltInLegacy,
    File(String),
}

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub main_route: String,
    pub aux_route: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tool_rounds: usize,
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AgentScenePolicy {
    pub enabled: bool,
    pub profile: String,
    pub main_route: Option<String>,
    pub search_route: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tool_rounds: Option<usize>,
    pub max_output_tokens: Option<u64>,
    pub tool_calling_enabled: Option<bool>,
    pub enabled_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct AgentScenes {
    pub private: AgentScenePolicy,
    pub group: AgentScenePolicy,
}

#[derive(Debug, Clone)]
pub struct AgentProviderConfig {
    pub id: ModelProvider,
    pub kind: AgentProviderKind,
    pub base_url: String,
    pub api_key_env: String,
    pub auth_header: String,
    pub auth_scheme: Option<String>,
    pub request_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProviderKind {
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

#[derive(Debug, Clone)]
pub struct ResolvedAgentPolicy {
    pub scene: ChatScene,
    pub enabled: bool,
    pub profile: String,
    pub main_model: String,
    pub main_route: ModelRoute,
    pub aux_model: Option<String>,
    pub search_model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tool_rounds: usize,
    pub max_output_tokens: Option<u64>,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub enabled_tools: Vec<String>,
    pub source: AgentConfigSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfigFile {
    version: u32,
    #[serde(default)]
    providers: HashMap<String, ProviderFile>,
    #[serde(default)]
    model_routes: HashMap<String, RouteFile>,
    #[serde(default)]
    search_routes: HashMap<String, SearchRouteFile>,
    #[serde(default)]
    profiles: HashMap<String, ProfileFile>,
    scenes: ScenesFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteFile {
    candidates: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFile {
    kind: AgentProviderKind,
    base_url: String,
    api_key_env: String,
    #[serde(default = "default_auth_header")]
    auth_header: String,
    #[serde(default = "default_auth_scheme")]
    auth_scheme: Option<String>,
    #[serde(default)]
    request_timeout_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRouteFile {
    model: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    main_route: String,
    #[serde(default)]
    aux_route: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    max_tool_rounds: usize,
    #[serde(default)]
    max_output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenesFile {
    private: SceneFile,
    group: SceneFile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneFile {
    #[serde(default = "default_true")]
    enabled: bool,
    profile: String,
    #[serde(default)]
    main_route: Option<String>,
    #[serde(default)]
    search_route: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    max_tool_rounds: Option<usize>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    tool_calling_enabled: Option<bool>,
    #[serde(default)]
    enabled_tools: Option<Vec<String>>,
}

impl AgentRuntimeConfig {
    pub fn load(legacy: LegacyAgentConfig) -> Result<Self, LlmError> {
        let override_path = env::var(AGENT_CONFIG_FILE_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let path = override_path
            .clone()
            .unwrap_or_else(|| DEFAULT_AGENT_CONFIG_PATH.to_owned());
        if Path::new(&path).exists() {
            let text = fs::read_to_string(&path).map_err(|err| {
                LlmError::config(format!(
                    "failed to read {AGENT_CONFIG_FILE_ENV} `{path}`: {err}"
                ))
            })?;
            Self::from_toml(&text, AgentConfigSource::File(path), legacy)
        } else if override_path.is_some() {
            Err(LlmError::config(format!(
                "{AGENT_CONFIG_FILE_ENV} points to missing file `{path}`"
            )))
        } else {
            Ok(Self::from_legacy(legacy)?)
        }
    }

    pub fn from_legacy(legacy: LegacyAgentConfig) -> Result<Self, LlmError> {
        let mut routes = HashMap::new();
        routes.insert(
            "main".to_owned(),
            ModelRoute::parse_config(&legacy.main_model, "LLM_MODEL")?,
        );
        routes.insert(
            "private_main".to_owned(),
            ModelRoute::parse_config(
                legacy
                    .private_llm_model
                    .as_deref()
                    .unwrap_or(&legacy.main_model),
                "PRIVATE_LLM_MODEL",
            )?,
        );
        routes.insert(
            "group_main".to_owned(),
            ModelRoute::parse_config(
                legacy
                    .group_llm_model
                    .as_deref()
                    .unwrap_or(&legacy.main_model),
                "GROUP_LLM_MODEL",
            )?,
        );
        routes.insert(
            "aux".to_owned(),
            ModelRoute::parse_config(&legacy.main_model, "LLM_MODEL")?,
        );

        let mut search_routes = HashMap::new();
        search_routes.insert("search".to_owned(), legacy.openai_search_model.clone());
        search_routes.insert(
            "private_search".to_owned(),
            legacy
                .private_openai_search_model
                .unwrap_or_else(|| legacy.openai_search_model.clone()),
        );
        search_routes.insert(
            "group_search".to_owned(),
            legacy
                .group_openai_search_model
                .unwrap_or_else(|| legacy.openai_search_model.clone()),
        );

        let max_tool_rounds = legacy.tool_calling_max_rounds as usize;
        let profiles = HashMap::from([
            (
                "fast".to_owned(),
                AgentProfile {
                    main_route: "group_main".to_owned(),
                    aux_route: Some("aux".to_owned()),
                    reasoning_effort: Some(ReasoningEffort::Low),
                    max_tool_rounds: max_tool_rounds.clamp(1, 2),
                    max_output_tokens: Some(legacy.max_output_tokens),
                },
            ),
            (
                "balanced".to_owned(),
                AgentProfile {
                    main_route: "private_main".to_owned(),
                    aux_route: Some("aux".to_owned()),
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    max_tool_rounds,
                    max_output_tokens: Some(legacy.max_output_tokens),
                },
            ),
            (
                "deep".to_owned(),
                AgentProfile {
                    main_route: "private_main".to_owned(),
                    aux_route: Some("aux".to_owned()),
                    reasoning_effort: Some(ReasoningEffort::High),
                    max_tool_rounds: max_tool_rounds.max(8),
                    max_output_tokens: Some(legacy.max_output_tokens),
                },
            ),
        ]);
        let scenes = AgentScenes {
            private: AgentScenePolicy {
                enabled: true,
                profile: DEFAULT_PRIVATE_PROFILE.to_owned(),
                main_route: Some("private_main".to_owned()),
                search_route: Some("private_search".to_owned()),
                reasoning_effort: None,
                max_tool_rounds: None,
                max_output_tokens: None,
                tool_calling_enabled: Some(legacy.tool_calling_enabled),
                enabled_tools: None,
            },
            group: AgentScenePolicy {
                enabled: true,
                profile: DEFAULT_GROUP_PROFILE.to_owned(),
                main_route: Some("group_main".to_owned()),
                search_route: Some("group_search".to_owned()),
                reasoning_effort: None,
                max_tool_rounds: None,
                max_output_tokens: None,
                tool_calling_enabled: Some(legacy.group_tool_calling_enabled),
                enabled_tools: None,
            },
        };
        Ok(Self {
            source: AgentConfigSource::BuiltInLegacy,
            providers: HashMap::new(),
            routes,
            search_routes,
            profiles,
            scenes,
        })
    }

    fn from_toml(
        text: &str,
        source: AgentConfigSource,
        legacy: LegacyAgentConfig,
    ) -> Result<Self, LlmError> {
        let file: AgentConfigFile = toml::from_str(text)
            .map_err(|err| LlmError::config(format!("failed to parse agent config: {err}")))?;
        if file.version != 1 {
            return Err(LlmError::config(format!(
                "unsupported agent config version {}; supported: 1",
                file.version
            )));
        }

        let mut config = Self::from_legacy(legacy)?;
        config.source = source;
        for (name, provider) in file.providers {
            let provider = provider_from_file(&name, provider)?;
            config.providers.insert(name, provider);
        }
        for (name, route) in file.model_routes {
            if route.candidates.is_empty() {
                return Err(LlmError::config(format!(
                    "agent model route `{name}` must contain at least one candidate"
                )));
            }
            let joined = route.candidates.join(",");
            config
                .routes
                .insert(name.clone(), ModelRoute::parse_config(&joined, &name)?);
        }
        for (name, route) in file.search_routes {
            let model = super::openai_model_name(&route.model, &format!("search_routes.{name}"))?;
            config.search_routes.insert(name, model);
        }
        for (name, profile) in file.profiles {
            validate_positive("max_tool_rounds", profile.max_tool_rounds)?;
            if let Some(tokens) = profile.max_output_tokens {
                validate_positive("max_output_tokens", tokens as usize)?;
            }
            config.profiles.insert(
                name,
                AgentProfile {
                    main_route: profile.main_route,
                    aux_route: profile.aux_route,
                    reasoning_effort: profile.reasoning_effort,
                    max_tool_rounds: profile.max_tool_rounds,
                    max_output_tokens: profile.max_output_tokens,
                },
            );
        }
        let legacy_private_tool_calling =
            config.scenes.private.tool_calling_enabled.unwrap_or(true);
        let legacy_group_tool_calling = config.scenes.group.tool_calling_enabled.unwrap_or(false);
        config.scenes = AgentScenes {
            private: scene_from_file(file.scenes.private, legacy_private_tool_calling),
            group: scene_from_file(file.scenes.group, legacy_group_tool_calling),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn resolve(&self, scene: ChatScene) -> Result<ResolvedAgentPolicy, LlmError> {
        let scene_policy = match scene {
            ChatScene::Private => &self.scenes.private,
            ChatScene::Group => &self.scenes.group,
        };
        let profile = self.profiles.get(&scene_policy.profile).ok_or_else(|| {
            LlmError::config(format!(
                "agent scene `{}` references unknown profile `{}`",
                scene.as_str(),
                scene_policy.profile
            ))
        })?;
        let main_route_name = scene_policy
            .main_route
            .as_deref()
            .unwrap_or(profile.main_route.as_str());
        let main_route = self.routes.get(main_route_name).ok_or_else(|| {
            LlmError::config(format!(
                "agent profile `{}` references unknown route `{main_route_name}`",
                scene_policy.profile
            ))
        })?;
        let aux_model = match profile.aux_route.as_deref() {
            Some(name) => Some(
                self.routes
                    .get(name)
                    .ok_or_else(|| {
                        LlmError::config(format!(
                            "agent profile `{}` references unknown aux route `{name}`",
                            scene_policy.profile
                        ))
                    })?
                    .display(),
            ),
            None => None,
        };
        let search_route_name = scene_policy.search_route.as_deref().unwrap_or("search");
        let search_model = self
            .search_routes
            .get(search_route_name)
            .cloned()
            .ok_or_else(|| {
                LlmError::config(format!(
                    "agent scene `{}` references unknown search route `{search_route_name}`",
                    scene.as_str()
                ))
            })?;
        let max_tool_rounds = scene_policy
            .max_tool_rounds
            .unwrap_or(profile.max_tool_rounds)
            .max(1);
        let enabled_tools = scene_policy
            .enabled_tools
            .clone()
            .unwrap_or_else(|| default_enabled_tools(scene));
        Ok(ResolvedAgentPolicy {
            scene,
            enabled: scene_policy.enabled,
            profile: scene_policy.profile.clone(),
            main_model: main_route.display(),
            main_route: main_route.clone(),
            aux_model,
            search_model,
            reasoning_effort: scene_policy.reasoning_effort.or(profile.reasoning_effort),
            max_tool_rounds,
            max_output_tokens: scene_policy.max_output_tokens.or(profile.max_output_tokens),
            tool_calling_enabled: scene_policy.tool_calling_enabled.unwrap_or(true),
            group_tool_calling_enabled: matches!(scene, ChatScene::Group)
                && scene_policy.tool_calling_enabled.unwrap_or(false),
            enabled_tools,
            source: self.source.clone(),
        })
    }

    pub fn diagnostic_summary(&self) -> Result<serde_json::Value, LlmError> {
        let private = self.resolve(ChatScene::Private)?;
        let group = self.resolve(ChatScene::Group)?;
        Ok(serde_json::json!({
            "source": self.source_label(),
            "private": private.diagnostic_summary(),
            "group": group.diagnostic_summary(),
        }))
    }

    pub fn source_label(&self) -> String {
        match &self.source {
            AgentConfigSource::BuiltInLegacy => "built_in_legacy".to_owned(),
            AgentConfigSource::File(path) => path.clone(),
        }
    }

    pub fn configured_model_routes(&self) -> Vec<(String, ModelRoute)> {
        self.routes
            .iter()
            .map(|(name, route)| (format!("agent.model_routes.{name}"), route.clone()))
            .collect()
    }

    pub fn provider_configs(&self) -> Vec<AgentProviderConfig> {
        self.providers.values().cloned().collect()
    }

    fn validate(&self) -> Result<(), LlmError> {
        if self.routes.is_empty() {
            return Err(LlmError::config(
                "agent config must define at least one model route",
            ));
        }
        if self.profiles.is_empty() {
            return Err(LlmError::config(
                "agent config must define at least one profile",
            ));
        }
        validate_scene_enabled_tools("private", &self.scenes.private.enabled_tools)?;
        validate_scene_enabled_tools("group", &self.scenes.group.enabled_tools)?;
        self.resolve(ChatScene::Private)?;
        self.resolve(ChatScene::Group)?;
        Ok(())
    }
}

#[cfg(test)]
impl AgentRuntimeConfig {
    pub(crate) fn with_group_enabled_tools_for_test(mut self, tools: &[&str]) -> Self {
        self.scenes.group.enabled_tools =
            Some(tools.iter().map(|tool| (*tool).to_owned()).collect());
        self.scenes.group.tool_calling_enabled = Some(true);
        self
    }
}

impl ResolvedAgentPolicy {
    pub fn diagnostic_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "scene": self.scene.as_str(),
            "enabled": self.enabled,
            "profile": self.profile,
            "main_route": self.main_model,
            "aux_route": self.aux_model,
            "search_model": self.search_model,
            "reasoning_effort": self.reasoning_effort.map(ReasoningEffort::as_str),
            "max_tool_rounds": self.max_tool_rounds,
            "max_output_tokens": self.max_output_tokens,
            "tool_calling_enabled": self.tool_calling_enabled,
            "group_tool_calling_enabled": self.group_tool_calling_enabled,
            "enabled_tools": &self.enabled_tools,
            "source": match &self.source {
                AgentConfigSource::BuiltInLegacy => "built_in_legacy",
                AgentConfigSource::File(path) => path.as_str(),
            },
        })
    }
}

fn scene_from_file(scene: SceneFile, default_tool_calling_enabled: bool) -> AgentScenePolicy {
    AgentScenePolicy {
        enabled: scene.enabled,
        profile: scene.profile,
        main_route: scene.main_route,
        search_route: scene.search_route,
        reasoning_effort: scene.reasoning_effort,
        max_tool_rounds: scene.max_tool_rounds,
        max_output_tokens: scene.max_output_tokens,
        tool_calling_enabled: scene
            .tool_calling_enabled
            .or(Some(default_tool_calling_enabled)),
        enabled_tools: scene.enabled_tools.map(normalize_enabled_tools),
    }
}

fn default_enabled_tools(scene: ChatScene) -> Vec<String> {
    match scene {
        ChatScene::Private => DEFAULT_PRIVATE_ENABLED_TOOLS,
        ChatScene::Group => DEFAULT_GROUP_ENABLED_TOOLS,
    }
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

fn normalize_enabled_tools(values: Vec<String>) -> Vec<String> {
    let mut tools = Vec::new();
    for value in values {
        let tool = value.trim();
        if !tool.is_empty() && !tools.iter().any(|existing| existing == tool) {
            tools.push(tool.to_owned());
        }
    }
    tools
}

fn validate_scene_enabled_tools(scene: &str, tools: &Option<Vec<String>>) -> Result<(), LlmError> {
    let Some(tools) = tools else {
        return Ok(());
    };
    for tool in tools {
        if !DEFAULT_PRIVATE_ENABLED_TOOLS.contains(&tool.as_str()) {
            return Err(LlmError::config(format!(
                "agent scene `{scene}` enabled_tools contains unknown tool `{tool}`"
            )));
        }
    }
    Ok(())
}

fn validate_positive(name: &str, value: usize) -> Result<(), LlmError> {
    if value == 0 {
        return Err(LlmError::config(format!("{name} must be positive")));
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

fn default_auth_header() -> String {
    "Authorization".to_owned()
}

fn default_auth_scheme() -> Option<String> {
    Some("Bearer".to_owned())
}

fn provider_from_file(name: &str, provider: ProviderFile) -> Result<AgentProviderConfig, LlmError> {
    let id = ModelProvider::parse_prefix(name)
        .map_err(|err| LlmError::config(format!("invalid providers.{name}: {}", err.message)))?;
    if !matches!(id, ModelProvider::Custom(_)) {
        return Err(LlmError::config(format!(
            "providers.{name} cannot override built-in provider `{}`",
            id.as_str()
        )));
    }
    let base_url = provider.base_url.trim();
    if base_url.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.base_url must not be empty"
        )));
    }
    let api_key_env = provider.api_key_env.trim();
    if api_key_env.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.api_key_env must not be empty"
        )));
    }
    let auth_header = provider.auth_header.trim();
    if auth_header.is_empty() {
        return Err(LlmError::config(format!(
            "providers.{name}.auth_header must not be empty"
        )));
    }
    if let Some(seconds) = provider.request_timeout_seconds {
        validate_positive("request_timeout_seconds", seconds as usize)?;
    }
    Ok(AgentProviderConfig {
        id,
        kind: provider.kind,
        base_url: base_url.to_owned(),
        api_key_env: api_key_env.to_owned(),
        auth_header: auth_header.to_owned(),
        auth_scheme: provider
            .auth_scheme
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        request_timeout_seconds: provider.request_timeout_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> LegacyAgentConfig {
        LegacyAgentConfig {
            main_model: "openai:gpt-main".to_owned(),
            max_output_tokens: 1200,
            openai_search_model: "gpt-search".to_owned(),
            tool_calling_enabled: true,
            group_tool_calling_enabled: false,
            tool_calling_max_rounds: 5,
            group_llm_model: Some("openai:gpt-fast".to_owned()),
            private_llm_model: None,
            group_openai_search_model: Some("gpt-search-fast".to_owned()),
            private_openai_search_model: None,
        }
    }

    #[test]
    fn legacy_config_resolves_private_and_group_profiles() {
        let config = AgentRuntimeConfig::from_legacy(legacy()).unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();

        assert_eq!(private.profile, "balanced");
        assert_eq!(private.main_model, "openai:gpt-main");
        assert_eq!(private.search_model, "gpt-search");
        assert_eq!(private.max_tool_rounds, 5);
        assert!(private.tool_calling_enabled);

        assert_eq!(group.profile, "fast");
        assert_eq!(group.main_model, "openai:gpt-fast");
        assert_eq!(group.search_model, "gpt-search-fast");
        assert_eq!(
            group.enabled_tools,
            vec!["get_weather", "get_train_schedule", "get_rss_recent_items"]
        );
        assert!(!group.enabled_tools.iter().any(|name| name == "list_todos"));
        assert!(!group.group_tool_calling_enabled);
    }

    #[test]
    fn toml_config_overrides_routes_profiles_and_scenes() {
        let text = r#"
version = 1

[model_routes.main]
candidates = ["openai:gpt-main", "deepseek:deepseek-chat"]

[model_routes.fast]
candidates = ["openai:gpt-fast"]

[model_routes.aux]
candidates = ["openai:gpt-aux"]

[search_routes.search]
model = "gpt-search"

[profiles.fast]
main_route = "fast"
aux_route = "aux"
reasoning_effort = "low"
max_tool_rounds = 2
max_output_tokens = 800

[profiles.balanced]
main_route = "main"
aux_route = "aux"
reasoning_effort = "medium"
max_tool_rounds = 6
max_output_tokens = 1600

[profiles.deep]
main_route = "main"
aux_route = "aux"
reasoning_effort = "high"
max_tool_rounds = 8
max_output_tokens = 3200

[scenes.private]
enabled = true
profile = "deep"
tool_calling_enabled = true

[scenes.group]
enabled = true
profile = "fast"
tool_calling_enabled = false
enabled_tools = ["get_weather", "list_todos", "get_weather"]
"#;

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert_eq!(private.profile, "deep");
        assert_eq!(private.main_model, "openai:gpt-main,deepseek:deepseek-chat");
        assert_eq!(private.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(private.max_tool_rounds, 8);
        assert_eq!(private.max_output_tokens, Some(3200));
        assert_eq!(group.profile, "fast");
        assert_eq!(group.main_model, "openai:gpt-fast");
        assert!(!group.group_tool_calling_enabled);
        assert_eq!(group.enabled_tools, vec!["get_weather", "list_todos"]);
    }

    #[test]
    fn toml_config_accepts_openai_compatible_mimo_provider() {
        let text = r#"
version = 1

[providers.mimo]
kind = "openai_compatible"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"
auth_header = "Authorization"
auth_scheme = "Bearer"
request_timeout_seconds = 45

[model_routes.private_main]
candidates = ["mimo:mimo-v2.5-pro", "deepseek:deepseek-chat"]

[profiles.fast]
main_route = "private_main"
max_tool_rounds = 2

[profiles.balanced]
main_route = "private_main"
max_tool_rounds = 5

[profiles.deep]
main_route = "private_main"
max_tool_rounds = 8

[scenes.private]
enabled = true
profile = "balanced"

[scenes.group]
enabled = true
profile = "fast"
"#;

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap();

        let providers = config.provider_configs();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, ModelProvider::Custom("mimo".to_owned()));
        assert_eq!(providers[0].api_key_env, "MIMO_API_KEY");
        assert_eq!(providers[0].request_timeout_seconds, Some(45));
        let private = config.resolve(ChatScene::Private).unwrap();
        assert_eq!(
            private.main_model,
            "mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
        );
    }

    #[test]
    fn toml_config_rejects_removed_provider_retry_fields() {
        let text = r#"
version = 1

[providers.mimo]
kind = "openai_compatible"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"
max_retries = 3

[profiles.fast]
main_route = "group_main"
max_tool_rounds = 2

[profiles.balanced]
main_route = "private_main"
max_tool_rounds = 5

[profiles.deep]
main_route = "private_main"
max_tool_rounds = 8

[scenes.private]
enabled = true
profile = "balanced"

[scenes.group]
enabled = true
profile = "fast"
"#;

        let err = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap_err();

        assert_eq!(err.stage, "config");
        assert!(err.message.contains("unknown field `max_retries`"));
    }

    #[test]
    fn default_agent_toml_inherits_env_model_routes() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let mut legacy = legacy();
        legacy.main_model = "deepseek:deepseek-chat".to_owned();
        legacy.private_llm_model = Some("deepseek:deepseek-chat".to_owned());
        legacy.group_llm_model = Some("bigmodel:glm-5.2".to_owned());
        legacy.private_openai_search_model = Some("gpt-private-search".to_owned());
        legacy.group_openai_search_model = Some("gpt-group-search".to_owned());

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert_eq!(private.main_model, "deepseek:deepseek-chat");
        assert_eq!(private.aux_model.as_deref(), Some("deepseek:deepseek-chat"));
        assert_eq!(private.search_model, "gpt-private-search");
        assert_eq!(group.main_model, "bigmodel:glm-5.2");
        assert_eq!(group.aux_model.as_deref(), Some("deepseek:deepseek-chat"));
        assert_eq!(group.search_model, "gpt-group-search");
        assert_eq!(
            config.source,
            AgentConfigSource::File("config/agent.toml".to_owned())
        );
    }

    #[test]
    fn default_agent_toml_does_not_declare_model_routes() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let active_config = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!active_config.contains("[model_routes."));
        assert!(!active_config.contains("candidates ="));
        assert!(!active_config.contains("openai:"));
        assert!(!active_config.contains("deepseek:"));
        assert!(!active_config.contains("[search_routes."));
        assert!(!active_config.contains("bigmodel:"));
        assert!(!active_config.contains("glm-"));
    }

    #[test]
    fn default_agent_toml_preserves_private_and_group_scene_routes() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let mut legacy = legacy();
        legacy.main_model = "openai:gpt-shared".to_owned();
        legacy.private_llm_model = Some("deepseek:deepseek-chat".to_owned());
        legacy.group_llm_model = Some("bigmodel:glm-5.2".to_owned());

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert_eq!(private.profile, "balanced");
        assert_eq!(private.main_model, "deepseek:deepseek-chat");
        assert_eq!(private.reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(private.max_tool_rounds, 5);
        assert!(private.tool_calling_enabled);

        assert_eq!(group.profile, "fast");
        assert_eq!(group.main_model, "bigmodel:glm-5.2");
        assert_eq!(group.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(group.max_tool_rounds, 2);
        assert_eq!(
            group.enabled_tools,
            vec!["get_weather", "get_train_schedule", "get_rss_recent_items"]
        );
        assert!(!group.group_tool_calling_enabled);
    }

    #[test]
    fn default_agent_toml_keeps_deepseek_only_routes_provider_neutral() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let mut legacy = legacy();
        legacy.main_model = "deepseek:deepseek-chat".to_owned();
        legacy.private_llm_model = None;
        legacy.group_llm_model = None;

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let route_displays = config
            .configured_model_routes()
            .into_iter()
            .map(|(name, route)| (name, route.display()))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            route_displays.get("agent.model_routes.private_main"),
            Some(&"deepseek:deepseek-chat".to_owned())
        );
        assert_eq!(
            route_displays.get("agent.model_routes.group_main"),
            Some(&"deepseek:deepseek-chat".to_owned())
        );
        assert_eq!(
            route_displays.get("agent.model_routes.aux"),
            Some(&"deepseek:deepseek-chat".to_owned())
        );
    }

    #[test]
    fn default_agent_toml_keeps_bigmodel_only_routes_provider_neutral() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let mut legacy = legacy();
        legacy.main_model = "bigmodel:glm-5.2".to_owned();
        legacy.private_llm_model = None;
        legacy.group_llm_model = None;

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert_eq!(private.main_model, "bigmodel:glm-5.2");
        assert_eq!(group.main_model, "bigmodel:glm-5.2");
        assert_eq!(private.aux_model.as_deref(), Some("bigmodel:glm-5.2"));
        assert_eq!(group.aux_model.as_deref(), Some("bigmodel:glm-5.2"));
    }

    #[test]
    fn default_agent_toml_lets_private_and_group_env_override_llm_model() {
        let text = include_str!("../../../runtime/config/agent.toml");
        let mut legacy = legacy();
        legacy.main_model = "openai:gpt-shared".to_owned();
        legacy.private_llm_model = Some("deepseek:deepseek-chat".to_owned());
        legacy.group_llm_model = Some("bigmodel:glm-5.2".to_owned());

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert_eq!(private.main_model, "deepseek:deepseek-chat");
        assert_eq!(group.main_model, "bigmodel:glm-5.2");
        assert_eq!(private.aux_model.as_deref(), Some("openai:gpt-shared"));
        assert_eq!(group.aux_model.as_deref(), Some("openai:gpt-shared"));
    }

    #[test]
    fn toml_config_without_tool_calling_flag_inherits_legacy_switches() {
        let text = r#"
version = 1

[scenes.private]
profile = "balanced"

[scenes.group]
profile = "fast"
"#;
        let mut legacy = legacy();
        legacy.tool_calling_enabled = false;
        legacy.group_tool_calling_enabled = true;

        let config = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy,
        )
        .unwrap();

        let private = config.resolve(ChatScene::Private).unwrap();
        let group = config.resolve(ChatScene::Group).unwrap();
        assert!(!private.tool_calling_enabled);
        assert!(!private.group_tool_calling_enabled);
        assert!(group.tool_calling_enabled);
        assert!(group.group_tool_calling_enabled);
    }

    #[test]
    fn toml_config_rejects_unknown_profile() {
        let text = r#"
version = 1

[scenes.private]
profile = "missing"

[scenes.group]
profile = "fast"
"#;

        let err = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn toml_config_rejects_unknown_search_route() {
        let text = r#"
version = 1

[scenes.private]
profile = "balanced"
search_route = "missing"

[scenes.group]
profile = "fast"
"#;

        let err = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("unknown search route"));
    }

    #[test]
    fn toml_config_rejects_unknown_enabled_tool() {
        let text = r#"
version = 1

[scenes.private]
profile = "balanced"

[scenes.group]
profile = "fast"
enabled_tools = ["get_weather", "run_shell"]
"#;

        let err = AgentRuntimeConfig::from_toml(
            text,
            AgentConfigSource::File("config/agent.toml".to_owned()),
            legacy(),
        )
        .unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("unknown tool `run_shell`"));
    }
}
