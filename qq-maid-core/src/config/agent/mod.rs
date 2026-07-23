//! Agent 场景策略配置。
//!
//! 配置文件只保存非敏感、可版本管理的运行策略；Provider key、base URL 等仍由环境变量提供。

use std::{collections::HashMap, env, fs, fs::OpenOptions, io::Write, path::Path};

use serde::{Deserialize, Serialize};

use qq_maid_llm::{
    provider::types::{ModelProvider, ModelRoute, ReasoningEffort},
    web_search::{WebSearchBackend, WebSearchConfig, WebSearchTimeRange},
};

use crate::error::LlmError;

mod web_search_config;

pub(in crate::config) use web_search_config::ToolsConfigFile;
use web_search_config::web_search_from_file;

pub const DEFAULT_AGENT_CONFIG_PATH: &str = "config/agent.toml";
pub const AGENT_CONFIG_FILE_ENV: &str = "AGENT_CONFIG_FILE";
const DEFAULT_AGENT_CONFIG: &str = include_str!("../../../../runtime/config/agent.example.toml");

/// 空配置目录首次启动时安装公开的默认 Agent 策略。
///
/// 显式 `AGENT_CONFIG_FILE` 永远不自动创建，避免把拼错的高级部署路径静默纠正；仅默认
/// 路径缺失时使用 `create_new`，不会覆盖人工文件或并发创建结果。
pub fn ensure_default_agent_config(environment: &HashMap<String, String>) -> Result<(), LlmError> {
    ensure_default_agent_config_at(environment, Path::new(DEFAULT_AGENT_CONFIG_PATH))
}

fn ensure_default_agent_config_at(
    environment: &HashMap<String, String>,
    path: &Path,
) -> Result<(), LlmError> {
    if environment
        .get(AGENT_CONFIG_FILE_ENV)
        .is_some_and(|value| !value.trim().is_empty())
        || path.exists()
    {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        LlmError::config(format!(
            "failed to create default agent config directory: {error}"
        ))
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        LlmError::config(format!(
            "failed to inspect default agent config directory: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LlmError::config(
            "default agent config parent must be a directory and not a symbolic link",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => file
            .write_all(DEFAULT_AGENT_CONFIG.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                LlmError::config(format!("failed to persist default agent config: {error}"))
            }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(LlmError::config(format!(
            "failed to create default agent config: {error}"
        ))),
    }
}

const ALL_ENABLED_TOOL_NAMES: &[&str] = &[
    "image_generation",
    "get_weather",
    "get_train_schedule",
    "get_rss_recent_items",
    "manage_rss_subscriptions",
    "web_search",
    "knowledge_search",
    "list_todos",
    "get_todo",
    "create_todo",
    "complete_todos",
    "edit_todo",
    "manage_recurring_reminder",
    "restore_todos",
    "delete_todos",
    "merge_todos",
    "save_memory",
];

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
pub struct AgentRuntimeConfig {
    source: AgentConfigSource,
    /// 保留进程启动时实际读取的文档，用于区分 saved 与 running。
    document: Option<AgentConfigDocument>,
    providers: HashMap<String, AgentProviderConfig>,
    routes: HashMap<String, ModelRoute>,
    web_search_routes: HashMap<String, String>,
    profiles: HashMap<String, AgentProfile>,
    scenes: AgentScenes,
    knowledge_mode: KnowledgeRetrievalMode,
    knowledge_embedding: KnowledgeEmbeddingConfig,
    web_search: WebSearchConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentConfigSource {
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
    pub tool_calling_enabled: bool,
    pub enabled_tools: Vec<String>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
    pub search_backend: WebSearchBackend,
    pub search_model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tool_rounds: usize,
    pub max_output_tokens: Option<u64>,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub enabled_tools: Vec<String>,
    pub knowledge_mode: KnowledgeRetrievalMode,
    pub source: AgentConfigSource,
}

/// 知识检索只保留主路径和紧急回退路径，不维护长期 hybrid 分支。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRetrievalMode {
    #[default]
    Preflight,
    Tool,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEmbeddingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_knowledge_embedding_cache_dir")]
    pub cache_dir: String,
}

impl Default for KnowledgeEmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_dir: default_knowledge_embedding_cache_dir(),
        }
    }
}

fn default_knowledge_embedding_cache_dir() -> String {
    "cache/knowledge-embedding".to_owned()
}

impl KnowledgeRetrievalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Tool => "tool",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct AgentConfigDocument {
    version: u32,
    #[serde(default)]
    pub(in crate::config) knowledge: KnowledgeConfigFile,
    #[serde(default)]
    pub(in crate::config) tools: ToolsConfigFile,
    #[serde(default)]
    providers: HashMap<String, ProviderFile>,
    #[serde(default)]
    pub(in crate::config) model_routes: HashMap<String, RouteFile>,
    #[serde(default)]
    pub(in crate::config) profiles: HashMap<String, AgentProfileConfig>,
    pub(in crate::config) scenes: ScenesFile,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct KnowledgeConfigFile {
    #[serde(default)]
    pub(in crate::config) mode: KnowledgeRetrievalMode,
    #[serde(default)]
    pub(in crate::config) embedding: KnowledgeEmbeddingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct RouteFile {
    pub(in crate::config) candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct SearchRouteFile {
    pub(in crate::config) model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileConfig {
    pub main_route: String,
    #[serde(default)]
    pub aux_route: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 新增 profile 字段时保持旧配置可启动；5 轮与 balanced 默认档位一致。
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: usize,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct ScenesFile {
    pub(in crate::config) private: AgentSceneConfig,
    pub(in crate::config) group: AgentSceneConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSceneConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub profile: String,
    #[serde(default)]
    pub main_route: Option<String>,
    #[serde(default)]
    pub search_route: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub max_tool_rounds: Option<usize>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Tool Calling 默认关闭，避免旧配置升级后意外开放写入类工具。
    #[serde(default)]
    pub tool_calling_enabled: bool,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
}

impl AgentRuntimeConfig {
    pub fn load() -> Result<Self, LlmError> {
        let environment = env::vars().collect::<HashMap<_, _>>();
        Self::load_from_environment(&environment)
    }

    pub fn load_from_environment(environment: &HashMap<String, String>) -> Result<Self, LlmError> {
        let override_path = environment
            .get(AGENT_CONFIG_FILE_ENV)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let path = override_path
            .clone()
            .unwrap_or_else(|| DEFAULT_AGENT_CONFIG_PATH.to_owned());
        #[cfg(test)]
        let path = if override_path.is_none() && !Path::new(&path).exists() {
            format!(
                "{}/../runtime/config/agent.example.toml",
                env!("CARGO_MANIFEST_DIR")
            )
        } else {
            path
        };
        if !Path::new(&path).exists() {
            return Err(LlmError::config(format!(
                "{AGENT_CONFIG_FILE_ENV} points to missing file `{path}`"
            )));
        }
        let text = fs::read_to_string(&path).map_err(|err| {
            LlmError::config(format!(
                "failed to read {AGENT_CONFIG_FILE_ENV} `{path}`: {err}"
            ))
        })?;
        Self::from_toml(&text, AgentConfigSource::File(path))
    }

    /// 为只读 `config check` 校验 Agent 策略，不触发首次启动的文件安装。
    ///
    /// 未显式覆盖路径且默认活动文件尚不存在时，校验与当前二进制同版的内嵌模板；其他
    /// 情况仍交给正式加载器，以保证已有活动文件和显式路径继续采用严格文件语义。
    pub fn validate_for_read_only_check(
        environment: &HashMap<String, String>,
    ) -> Result<(), LlmError> {
        let has_explicit_path = environment
            .get(AGENT_CONFIG_FILE_ENV)
            .is_some_and(|value| !value.trim().is_empty());
        let default_exists = Path::new(DEFAULT_AGENT_CONFIG_PATH)
            .try_exists()
            .unwrap_or(true);
        if !has_explicit_path && !default_exists {
            return Self::from_toml(
                DEFAULT_AGENT_CONFIG,
                AgentConfigSource::File(DEFAULT_AGENT_CONFIG_PATH.to_owned()),
            )
            .map(drop);
        }
        Self::load_from_environment(environment).map(drop)
    }

    pub(in crate::config) fn from_toml(
        text: &str,
        source: AgentConfigSource,
    ) -> Result<Self, LlmError> {
        let file: AgentConfigDocument = toml::from_str(text)
            .map_err(|err| LlmError::config(format!("failed to parse agent config: {err}")))?;
        Self::from_document(file, source)
    }

    pub(in crate::config) fn from_document(
        file: AgentConfigDocument,
        source: AgentConfigSource,
    ) -> Result<Self, LlmError> {
        if file.version != 1 {
            return Err(LlmError::config(format!(
                "unsupported agent config version {}; supported: 1",
                file.version
            )));
        }

        let document = file.clone();
        let web_search = web_search_from_file(&file.tools.web_search)?;
        let mut providers = HashMap::new();
        for (name, provider) in file.providers {
            let provider = provider_from_file(&name, provider)?;
            providers.insert(name, provider);
        }
        let mut routes = HashMap::new();
        for (name, route) in file.model_routes {
            if route.candidates.is_empty() {
                return Err(LlmError::config(format!(
                    "agent model route `{name}` must contain at least one candidate"
                )));
            }
            let joined = route.candidates.join(",");
            routes.insert(name.clone(), ModelRoute::parse_config(&joined, &name)?);
        }
        let mut web_search_routes = HashMap::new();
        for (name, route) in file.tools.web_search.routes {
            let model =
                super::openai_model_name(&route.model, &format!("tools.web_search.routes.{name}"))?;
            web_search_routes.insert(name, model);
        }
        let mut profiles = HashMap::new();
        for (name, profile) in file.profiles {
            validate_positive("max_tool_rounds", profile.max_tool_rounds)?;
            if let Some(tokens) = profile.max_output_tokens {
                validate_positive("max_output_tokens", tokens as usize)?;
            }
            profiles.insert(
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
        let config = Self {
            source,
            document: Some(document),
            providers,
            routes,
            web_search_routes,
            profiles,
            knowledge_mode: file.knowledge.mode,
            knowledge_embedding: file.knowledge.embedding,
            web_search,
            scenes: AgentScenes {
                private: scene_from_file(file.scenes.private),
                group: scene_from_file(file.scenes.group),
            },
        };
        config.validate()?;
        Ok(config)
    }

    pub(in crate::config) fn managed_path(&self) -> std::path::PathBuf {
        match &self.source {
            AgentConfigSource::File(path) => path.into(),
        }
    }

    pub(in crate::config) fn document(&self) -> Option<&AgentConfigDocument> {
        self.document.as_ref()
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
        let search_model = match self.web_search.default_backend {
            WebSearchBackend::ProviderNative => self
                .web_search_routes
                .get(search_route_name)
                .cloned()
                .ok_or_else(|| {
                    LlmError::config(format!(
                        "agent scene `{}` references unknown search route `{search_route_name}`",
                        scene.as_str()
                    ))
                })?,
            // Tavily 和 disabled 不调用模型原生搜索，允许配置中完全移除旧 search route。
            WebSearchBackend::Tavily | WebSearchBackend::Disabled => self
                .web_search_routes
                .get(search_route_name)
                .cloned()
                .unwrap_or_default(),
        };
        let max_tool_rounds = scene_policy
            .max_tool_rounds
            .unwrap_or(profile.max_tool_rounds)
            .max(1);
        let enabled_tools = scene_policy.enabled_tools.clone();
        Ok(ResolvedAgentPolicy {
            scene,
            enabled: scene_policy.enabled,
            profile: scene_policy.profile.clone(),
            main_model: main_route.display(),
            main_route: main_route.clone(),
            aux_model,
            search_backend: self.web_search.default_backend,
            search_model,
            reasoning_effort: scene_policy.reasoning_effort.or(profile.reasoning_effort),
            max_tool_rounds,
            max_output_tokens: scene_policy.max_output_tokens.or(profile.max_output_tokens),
            tool_calling_enabled: scene_policy.tool_calling_enabled,
            group_tool_calling_enabled: matches!(scene, ChatScene::Group)
                && scene_policy.tool_calling_enabled,
            enabled_tools,
            knowledge_mode: self.knowledge_mode,
            source: self.source.clone(),
        })
    }

    pub fn diagnostic_summary(&self) -> Result<serde_json::Value, LlmError> {
        let private = self.resolve(ChatScene::Private)?;
        let group = self.resolve(ChatScene::Group)?;
        Ok(serde_json::json!({
            "source": self.source_label(),
            "knowledge": {
                "mode": self.knowledge_mode.as_str(),
                "embedding": {
                    "enabled": self.knowledge_embedding.enabled,
                    "cache_dir": &self.knowledge_embedding.cache_dir,
                },
            },
            "tools": {
                "web_search": {
                    "backend": self.web_search.default_backend.as_str(),
                    "max_results": self.web_search.max_results,
                    "search_depth": self.web_search.search_depth.as_str(),
                    "topic": self.web_search.topic.as_str(),
                    "time_range": self.web_search.time_range.map(WebSearchTimeRange::as_str),
                    "connect_timeout_seconds": self.web_search.connect_timeout_seconds,
                    "first_response_timeout_seconds": self.web_search.first_response_timeout_seconds,
                    "total_timeout_seconds": self.web_search.total_timeout_seconds,
                },
            },
            "private": private.diagnostic_summary(),
            "group": group.diagnostic_summary(),
        }))
    }

    pub fn source_label(&self) -> String {
        match &self.source {
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

    pub fn knowledge_embedding(&self) -> &KnowledgeEmbeddingConfig {
        &self.knowledge_embedding
    }

    pub fn web_search(&self) -> &WebSearchConfig {
        &self.web_search
    }

    fn validate(&self) -> Result<(), LlmError> {
        if self.knowledge_embedding.cache_dir.trim().is_empty() {
            return Err(LlmError::config(
                "agent knowledge embedding cache_dir must not be empty",
            ));
        }
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
        for (name, profile) in &self.profiles {
            if !self.routes.contains_key(&profile.main_route) {
                return Err(LlmError::config(format!(
                    "agent profile `{name}` references unknown route `{}`",
                    profile.main_route
                )));
            }
            if let Some(aux_route) = profile.aux_route.as_deref()
                && !self.routes.contains_key(aux_route)
            {
                return Err(LlmError::config(format!(
                    "agent profile `{name}` references unknown aux route `{aux_route}`"
                )));
            }
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
    pub(crate) fn for_test(
        main_model: &str,
        search_model: &str,
        private_tool_calling: bool,
        group_tool_calling: bool,
        max_tool_rounds: usize,
    ) -> Self {
        let mut routes = HashMap::new();
        routes.insert(
            "main".to_owned(),
            ModelRoute::parse_config(main_model, "test.model_routes.main").unwrap(),
        );
        let profiles = HashMap::from([
            (
                "balanced".to_owned(),
                AgentProfile {
                    main_route: "main".to_owned(),
                    aux_route: None,
                    reasoning_effort: Some(ReasoningEffort::Medium),
                    max_tool_rounds,
                    max_output_tokens: Some(1200),
                },
            ),
            (
                "fast".to_owned(),
                AgentProfile {
                    main_route: "main".to_owned(),
                    aux_route: None,
                    reasoning_effort: Some(ReasoningEffort::Low),
                    max_tool_rounds,
                    max_output_tokens: Some(1200),
                },
            ),
        ]);
        let private_tools = ALL_ENABLED_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let group_tools = [
            "get_weather",
            "get_train_schedule",
            "get_rss_recent_items",
            "manage_rss_subscriptions",
            "web_search",
            "knowledge_search",
            "save_memory",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        Self {
            source: AgentConfigSource::File("test-agent.toml".to_owned()),
            document: None,
            providers: HashMap::new(),
            routes,
            web_search_routes: HashMap::from([("search".to_owned(), search_model.to_owned())]),
            profiles,
            knowledge_mode: KnowledgeRetrievalMode::Preflight,
            knowledge_embedding: KnowledgeEmbeddingConfig::default(),
            web_search: WebSearchConfig::default(),
            scenes: AgentScenes {
                private: AgentScenePolicy {
                    enabled: true,
                    profile: "balanced".to_owned(),
                    main_route: None,
                    search_route: Some("search".to_owned()),
                    reasoning_effort: None,
                    max_tool_rounds: None,
                    max_output_tokens: None,
                    tool_calling_enabled: private_tool_calling,
                    enabled_tools: private_tools,
                },
                group: AgentScenePolicy {
                    enabled: true,
                    profile: "fast".to_owned(),
                    main_route: None,
                    search_route: Some("search".to_owned()),
                    reasoning_effort: None,
                    max_tool_rounds: None,
                    max_output_tokens: None,
                    tool_calling_enabled: group_tool_calling,
                    enabled_tools: group_tools,
                },
            },
        }
    }

    pub(crate) fn with_group_enabled_tools_for_test(mut self, tools: &[&str]) -> Self {
        self.scenes.group.enabled_tools = tools.iter().map(|tool| (*tool).to_owned()).collect();
        self.scenes.group.tool_calling_enabled = true;
        self
    }

    pub(crate) fn with_knowledge_mode_for_test(mut self, mode: KnowledgeRetrievalMode) -> Self {
        self.knowledge_mode = mode;
        self
    }

    pub(crate) fn with_scene_models_for_test(
        mut self,
        private_main: &str,
        private_aux: Option<&str>,
        group_main: &str,
        group_aux: Option<&str>,
    ) -> Self {
        self.routes.insert(
            "test_private_main".to_owned(),
            ModelRoute::parse_config(private_main, "test_private_main").unwrap(),
        );
        self.routes.insert(
            "test_group_main".to_owned(),
            ModelRoute::parse_config(group_main, "test_group_main").unwrap(),
        );
        self.scenes.private.main_route = Some("test_private_main".to_owned());
        self.scenes.group.main_route = Some("test_group_main".to_owned());

        let private_profile = self.scenes.private.profile.clone();
        let private_aux_route = private_aux.map(|model| {
            self.routes.insert(
                "test_private_aux".to_owned(),
                ModelRoute::parse_config(model, "test_private_aux").unwrap(),
            );
            "test_private_aux".to_owned()
        });
        self.profiles.get_mut(&private_profile).unwrap().aux_route = private_aux_route;

        let group_profile = self.scenes.group.profile.clone();
        let group_aux_route = group_aux.map(|model| {
            self.routes.insert(
                "test_group_aux".to_owned(),
                ModelRoute::parse_config(model, "test_group_aux").unwrap(),
            );
            "test_group_aux".to_owned()
        });
        self.profiles.get_mut(&group_profile).unwrap().aux_route = group_aux_route;
        self
    }
}

impl ResolvedAgentPolicy {
    /// 解析内部辅助任务模型，同时保留 `aux_model=None` 表示未显式配置 aux_route 的诊断语义。
    pub fn resolve_auxiliary_model(&self, explicit_override: Option<&str>) -> Option<String> {
        explicit_override
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .or_else(|| {
                self.aux_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
            })
            .or_else(|| {
                let model = self.main_model.trim();
                (!model.is_empty()).then_some(model)
            })
            .map(str::to_owned)
    }

    pub fn diagnostic_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "scene": self.scene.as_str(),
            "enabled": self.enabled,
            "profile": self.profile,
            "main_route": self.main_model,
            "aux_route": self.aux_model,
            "search_model": self.search_model,
            "search_backend": self.search_backend.as_str(),
            "reasoning_effort": self.reasoning_effort.map(ReasoningEffort::as_str),
            "max_tool_rounds": self.max_tool_rounds,
            "max_output_tokens": self.max_output_tokens,
            "tool_calling_enabled": self.tool_calling_enabled,
            "group_tool_calling_enabled": self.group_tool_calling_enabled,
            "enabled_tools": &self.enabled_tools,
            "knowledge_mode": self.knowledge_mode.as_str(),
            "source": match &self.source {
                AgentConfigSource::File(path) => path.as_str(),
            },
        })
    }
}

fn scene_from_file(scene: AgentSceneConfig) -> AgentScenePolicy {
    AgentScenePolicy {
        enabled: scene.enabled,
        profile: scene.profile,
        main_route: scene.main_route,
        search_route: scene.search_route,
        reasoning_effort: scene.reasoning_effort,
        max_tool_rounds: scene.max_tool_rounds,
        max_output_tokens: scene.max_output_tokens,
        tool_calling_enabled: scene.tool_calling_enabled,
        enabled_tools: normalize_enabled_tools(scene.enabled_tools),
    }
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

fn validate_scene_enabled_tools(scene: &str, tools: &[String]) -> Result<(), LlmError> {
    for tool in tools {
        if !ALL_ENABLED_TOOL_NAMES.contains(&tool.as_str()) {
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

fn default_max_tool_rounds() -> usize {
    5
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
mod tests;
