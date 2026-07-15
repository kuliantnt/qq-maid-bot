//! PreparedAction 按业务域分发；具体 payload 与执行仍留在对应 tools domain。

use crate::{
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord},
        tools::{memory::MEMORY_PENDING_DOMAIN, todo::TODO_PENDING_DOMAIN},
    },
};

use super::{RespondRequest, RespondResponse, RustRespondService};

impl RustRespondService {
    pub(crate) async fn handle_pending_operation(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(domain) = session
            .pending_operation
            .as_ref()
            .map(|pending| pending.domain().to_owned())
        else {
            return Ok(None);
        };
        match domain.as_str() {
            TODO_PENDING_DOMAIN => {
                self.handle_pending_todo_lifecycle(req, user_text, meta, session)
                    .await
            }
            MEMORY_PENDING_DOMAIN => {
                self.handle_pending_memory_lifecycle(req, user_text, meta, session)
                    .await
            }
            _ => Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "这条待确认操作版本无效，已清理，请重新发起。",
                "pending_domain_invalid",
            )?)),
        }
    }
}
