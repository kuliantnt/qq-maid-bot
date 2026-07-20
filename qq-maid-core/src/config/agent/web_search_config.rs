use std::collections::HashMap;

use qq_maid_llm::web_search::{
    DEFAULT_MAX_RESULTS, MAX_RESULTS_LIMIT, WebSearchBackend, WebSearchConfig, WebSearchDepth,
    WebSearchTimeRange, WebSearchTopic,
};
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

use super::SearchRouteFile;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct ToolsConfigFile {
    #[serde(default)]
    pub(in crate::config) web_search: WebSearchConfigFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct WebSearchConfigFile {
    #[serde(default = "default_web_search_backend")]
    pub(in crate::config) backend: String,
    #[serde(default = "default_web_search_max_results")]
    pub(in crate::config) max_results: u8,
    #[serde(default = "default_web_search_depth")]
    pub(in crate::config) search_depth: String,
    #[serde(default = "default_web_search_topic")]
    pub(in crate::config) topic: String,
    #[serde(default)]
    pub(in crate::config) time_range: Option<String>,
    /// OpenAI/Gemini 等模型原生搜索 route 统一收敛在联网搜索配置下。
    #[serde(default)]
    pub(in crate::config) routes: HashMap<String, SearchRouteFile>,
    #[serde(default = "default_web_search_connect_timeout_seconds")]
    pub(in crate::config) connect_timeout_seconds: u64,
    #[serde(default = "default_web_search_first_response_timeout_seconds")]
    pub(in crate::config) first_response_timeout_seconds: u64,
    #[serde(default = "default_web_search_total_timeout_seconds")]
    pub(in crate::config) total_timeout_seconds: u64,
}

impl Default for WebSearchConfigFile {
    fn default() -> Self {
        Self {
            backend: default_web_search_backend(),
            max_results: default_web_search_max_results(),
            search_depth: default_web_search_depth(),
            topic: default_web_search_topic(),
            time_range: None,
            routes: HashMap::new(),
            connect_timeout_seconds: default_web_search_connect_timeout_seconds(),
            first_response_timeout_seconds: default_web_search_first_response_timeout_seconds(),
            total_timeout_seconds: default_web_search_total_timeout_seconds(),
        }
    }
}

pub(super) fn web_search_from_file(
    file: &WebSearchConfigFile,
) -> Result<WebSearchConfig, LlmError> {
    if !(1..=MAX_RESULTS_LIMIT).contains(&file.max_results) {
        return Err(LlmError::config(format!(
            "tools.web_search.max_results must be between 1 and {MAX_RESULTS_LIMIT}"
        )));
    }
    if file.connect_timeout_seconds == 0
        || file.first_response_timeout_seconds == 0
        || file.total_timeout_seconds == 0
    {
        return Err(LlmError::config(
            "tools.web_search timeout values must be greater than zero",
        ));
    }
    if file.connect_timeout_seconds > file.first_response_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.connect_timeout_seconds must not exceed first_response_timeout_seconds",
        ));
    }
    if file.first_response_timeout_seconds > file.total_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.first_response_timeout_seconds must not exceed total_timeout_seconds",
        ));
    }
    let time_range = file
        .time_range
        .as_deref()
        .map(|value| WebSearchTimeRange::parse_config(value, "tools.web_search.time_range"))
        .transpose()?;
    Ok(WebSearchConfig {
        default_backend: WebSearchBackend::parse_config(&file.backend, "tools.web_search.backend")?,
        // 场景 route 会在请求级覆盖；AppConfig::llm_config 再换成私聊默认 route。
        default_model: "gpt-search".to_owned(),
        max_results: file.max_results,
        search_depth: WebSearchDepth::parse_config(
            &file.search_depth,
            "tools.web_search.search_depth",
        )?,
        topic: WebSearchTopic::parse_config(&file.topic, "tools.web_search.topic")?,
        time_range,
        connect_timeout_seconds: file.connect_timeout_seconds,
        first_response_timeout_seconds: file.first_response_timeout_seconds,
        total_timeout_seconds: file.total_timeout_seconds,
    })
}

fn default_web_search_backend() -> String {
    "provider_native".to_owned()
}

const fn default_web_search_max_results() -> u8 {
    DEFAULT_MAX_RESULTS
}

fn default_web_search_depth() -> String {
    "basic".to_owned()
}

fn default_web_search_topic() -> String {
    "general".to_owned()
}

const fn default_web_search_connect_timeout_seconds() -> u64 {
    10
}

const fn default_web_search_first_response_timeout_seconds() -> u64 {
    30
}

const fn default_web_search_total_timeout_seconds() -> u64 {
    60
}
