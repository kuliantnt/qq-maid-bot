//! `/语音` 确定性命令编排。
//!
//! 这里仅把已解析命令交给 voice 领域门面并渲染结果；权限、配置预检和持久化规则
//! 全部保留在 `runtime/tools/voice/`。

use crate::{error::LlmError, runtime::tools::voice::VoiceCommand};

use super::{RespondRequest, RespondResponse, RustRespondService, common::command_response};

impl RustRespondService {
    pub(super) fn handle_voice_command(
        &self,
        command: VoiceCommand,
        request: &RespondRequest,
    ) -> Result<RespondResponse, LlmError> {
        let result = self
            .voice_service
            .execute(command, request)
            .map_err(|error| {
                LlmError::new(
                    error.code(),
                    "语音偏好读写失败，请稍后再试",
                    "voice_preference",
                )
            })?;
        Ok(command_response(result.text, None, Some("voice")))
    }
}
