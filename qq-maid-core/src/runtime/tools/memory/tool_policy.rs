//! Memory Tool 的请求级暴露策略。

use super::{SAVE_MEMORY_TOOL_NAME, route::has_explicit_memory_write_intent};

pub(crate) fn enabled_tool_names_for_request<'a>(
    enabled_tools: Vec<&'a str>,
    user_text: &str,
) -> Vec<&'a str> {
    let save_allowed = has_explicit_memory_write_intent(user_text);
    enabled_tools
        .into_iter()
        .filter(|name| *name != SAVE_MEMORY_TOOL_NAME || save_allowed)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn save_memory_is_only_exposed_for_explicit_write_intent() {
        let tools = vec!["get_weather", super::SAVE_MEMORY_TOOL_NAME];
        assert_eq!(
            super::enabled_tool_names_for_request(tools.clone(), "我最近在学 Rust"),
            ["get_weather"]
        );
        assert_eq!(
            super::enabled_tool_names_for_request(tools, "记住我喜欢简短回复"),
            ["get_weather", super::SAVE_MEMORY_TOOL_NAME]
        );
    }
}
