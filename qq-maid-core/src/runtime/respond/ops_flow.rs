//! `/ops` 确定性命令入口。
//!
//! Respond 层只把服务端权威请求字段投影为 Ops 上下文并渲染即时回执；权限、
//! 参数规则、后台执行和通知入队均由 `tools::ops` 领域负责。

use crate::runtime::tools::ops::{OpsRequestContext, ParsedOpsCommand};

use super::{RespondRequest, RespondResponse, RustRespondService, common::command_response};

impl RustRespondService {
    pub(super) fn handle_ops_command(
        &self,
        command: ParsedOpsCommand,
        req: &RespondRequest,
    ) -> RespondResponse {
        let reply = self.ops_service.accept(
            command,
            OpsRequestContext {
                conversation_kind: req.conversation_kind,
                conversation_id: req.conversation_id.clone(),
                user_id: req.user_id.clone(),
                user_identity_source: req.user_identity_source,
                group_member_role: req.group_member_role.clone(),
                platform: req.platform.clone(),
                account_id: req.account_id.clone(),
                inbound_id: req.message_id.clone(),
            },
        );
        command_response(reply, None, Some("ops"))
    }
}
