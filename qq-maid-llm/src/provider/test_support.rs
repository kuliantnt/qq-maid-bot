//! provider 单测共享辅助。
//!
//! 这里只放多个 provider 测试都会复用的轻量 stub，避免把同一组测试工具在
//! DeepSeek / BigModel / OpenAI 之间各复制一份。

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    error::LlmError,
    tool::{Tool, ToolContext, ToolMetadata, ToolOutput},
};

pub(crate) struct WeatherToolStub {
    weather: &'static str,
}

impl WeatherToolStub {
    pub(crate) fn new(weather: &'static str) -> Self {
        Self { weather }
    }
}

#[async_trait]
impl Tool for WeatherToolStub {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "get_weather".to_owned(),
            description: "get weather".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        Ok(ToolOutput::json(json!({
            "ok": true,
            "city": arguments["city"],
            "weather": self.weather
        })))
    }
}

pub(crate) fn test_tool_context() -> ToolContext {
    ToolContext {
        task_id: "task-1".to_owned(),
        user_id: Some("u1".to_owned()),
        scope_id: "private:u1".to_owned(),
        tool_call_id: None,
    }
}
