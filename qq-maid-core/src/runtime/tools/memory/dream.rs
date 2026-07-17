// Portions of this file are adapted from xai-org/grok-build's xai-grok-memory
// Dream implementation. Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified for qq-maid-bot: SQLite Session/Memory targets, strict JSON output,
// actor-scoped group profiles, opt-out enforcement, and transactional checkpoints.

//! 从既有 Session 历史异步提取有长期价值的 SystemDerived Memory。

use std::{collections::HashMap, time::SystemTime};

use qq_maid_llm::provider::{
    DynLlmProvider,
    types::{ChatMessage, ChatRequest},
};
use serde::Deserialize;
use tracing::{info, warn};

use super::{
    MemoryActor, MemoryOperations, contains_sensitive_text, normalize_explicit_memory_content,
    storage::{
        DreamCandidate, DreamCompletion, DreamContext, DreamFinalizeStats, DreamLimits,
        DreamSession, MemoryCategory, MemoryStore, MemoryTarget,
    },
};

const DREAM_SYSTEM_PROMPT: &str = "\
You extract durable long-term memory from already completed chat sessions.\n\
Return exactly one JSON object matching this schema:\n\
{\"memories\":[{\"content\":\"...\",\"category\":\"note|preference|identity|relation|instruction\",\"attribute_key\":null,\"worth_saving\":true}]}\n\
If there is nothing durable to save, return exactly NO_REPLY.\n\n\
Keep only stable facts that are likely useful in future conversations: enduring preferences, identity, relationships, recurring constraints, or lasting instructions.\n\
Discard greetings, small talk, tool results, assistant claims, temporary status, one-off plans, current progress, next steps, secrets, credentials, sensitive personal data, and facts whose subject is uncertain.\n\
Do not infer missing facts. Do not output real user IDs, group IDs, scopes, visibility, permissions, source fields, or any database identifiers.\n\
Each content must be a standalone concise fact about the single conversation subject represented by the input.\n\
Set worth_saving=false when a candidate is uncertain or not durable.\n\
Use attribute_key only for a stable replaceable attribute such as nickname or timezone; otherwise use null.\n\
Do not include Markdown fences or explanatory text.";

#[derive(Debug, Clone, Copy)]
pub struct MemoryDreamConfig {
    pub enabled: bool,
    pub min_interval_seconds: u64,
    pub min_new_sessions: usize,
    pub max_sessions: usize,
    pub max_input_chars: usize,
    pub max_output_memories: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryDreamContext {
    pub actor: MemoryActor,
    pub target: MemoryTarget,
    pub conversation_scope_key: String,
    pub actor_ref: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MemoryDreamRunStats {
    pub input_sessions: usize,
    pub inserted_count: usize,
    pub duplicate_count: usize,
    pub conflict_count: usize,
    pub no_reply: bool,
    pub filtered_count: usize,
    pub truncated: bool,
}

#[derive(Clone)]
pub(crate) struct MemoryDreamWorker {
    provider: DynLlmProvider,
    store: MemoryStore,
    config: MemoryDreamConfig,
}

#[derive(Debug)]
struct PreparedDreamInput {
    text: String,
    checkpoint_updated_at: String,
    checkpoint_session_id: String,
    checkpoint_message_id: i64,
    input_sessions: usize,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DreamModelOutput {
    memories: Vec<DreamModelCandidate>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DreamModelCandidate {
    content: String,
    category: MemoryCategory,
    #[serde(default)]
    attribute_key: Option<String>,
    worth_saving: bool,
}

impl MemoryDreamWorker {
    pub(crate) fn new(
        provider: DynLlmProvider,
        store: MemoryStore,
        config: MemoryDreamConfig,
    ) -> Self {
        Self {
            provider,
            store,
            config,
        }
    }

    pub(crate) fn schedule(&self, context: MemoryDreamContext) {
        if !self.config.enabled {
            return;
        }
        let worker = self.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run_once(context).await {
                // 不记录模型输出、Session/Memory 正文或任何原始身份字段。
                warn!(error_code = error, "memory Dream batch failed");
            }
        });
    }

    pub(crate) async fn run_once(
        &self,
        context: MemoryDreamContext,
    ) -> Result<Option<MemoryDreamRunStats>, &'static str> {
        if !self.config.enabled {
            return Ok(None);
        }
        MemoryOperations::new(self.store.clone())
            .authorize_dream_target(&context.actor, &context.target)
            .map_err(|_| "dream_target_forbidden")?;
        let storage_context = DreamContext {
            actor_scope_id: context.actor.personal_scope_id.clone(),
            target: context.target,
            conversation_scope_key: context.conversation_scope_key,
            actor_ref: context.actor_ref,
        };
        let now = unix_epoch();
        let Some(claim) = self
            .store
            .claim_dream(
                &storage_context,
                DreamLimits {
                    min_interval_seconds: self.config.min_interval_seconds,
                    min_new_sessions: self.config.min_new_sessions,
                    max_sessions: self.config.max_sessions,
                },
                now,
            )
            .map_err(|_| "dream_claim_failed")?
        else {
            return Ok(None);
        };

        let prepared = match prepare_input(
            &claim.sessions,
            self.config.max_input_chars,
            claim.has_more_sessions,
        ) {
            Some(prepared) => prepared,
            None => {
                let _ = self.store.release_dream(&storage_context, &claim.token);
                return Err("dream_input_empty");
            }
        };

        // 关键边界：数据库租约事务已在 claim_dream 返回前结束；模型调用期间没有事务。
        let request = ChatRequest {
            session_id: "memory-dream".to_owned(),
            model: context.model.clone(),
            messages: vec![
                ChatMessage::system(DREAM_SYSTEM_PROMPT),
                ChatMessage::user(format!(
                    "Completed session input (oldest to newest):\n{}",
                    prepared.text
                )),
            ],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: HashMap::from([
                ("purpose".to_owned(), "memory_dream".to_owned()),
                ("health_observation".to_owned(), "ignore".to_owned()),
            ]),
        };
        let raw_output = match self.provider.chat(request).await {
            Ok(outcome) => outcome.reply,
            Err(_) => {
                let _ = self.store.release_dream(&storage_context, &claim.token);
                return Err("dream_model_failed");
            }
        };

        let no_reply = is_no_reply(&raw_output);
        let parsed = if no_reply {
            Vec::new()
        } else {
            match parse_model_output(&raw_output, self.config.max_output_memories) {
                Ok(candidates) => candidates,
                Err(code) => {
                    let _ = self.store.release_dream(&storage_context, &claim.token);
                    return Err(code);
                }
            }
        };
        let model_candidate_count = parsed.len();
        let candidates = parsed
            .into_iter()
            .filter_map(sanitize_candidate)
            .collect::<Vec<_>>();
        let filtered_count = model_candidate_count.saturating_sub(candidates.len());
        let finalize = self.store.complete_dream(
            &storage_context,
            &claim.token,
            DreamCompletion {
                checkpoint_message_id: prepared.checkpoint_message_id,
                checkpoint_updated_at: &prepared.checkpoint_updated_at,
                checkpoint_session_id: &prepared.checkpoint_session_id,
                candidates: &candidates,
                input_count: prepared.input_sessions,
                truncated: prepared.truncated,
                now_epoch: unix_epoch(),
            },
        );
        let DreamFinalizeStats {
            inserted_count,
            duplicate_count,
            conflict_count,
        } = match finalize {
            Ok(stats) => stats,
            Err(_) => {
                // 事务回滚后检查点未变；只尽力释放仍属于本任务的租约。
                let _ = self.store.release_dream(&storage_context, &claim.token);
                return Err("dream_commit_failed");
            }
        };
        info!(
            input_sessions = prepared.input_sessions,
            inserted_count,
            duplicate_count,
            conflict_count,
            filtered_count,
            no_reply,
            truncated = prepared.truncated,
            "memory Dream batch completed"
        );
        Ok(Some(MemoryDreamRunStats {
            input_sessions: prepared.input_sessions,
            inserted_count,
            duplicate_count,
            conflict_count,
            no_reply,
            filtered_count,
            truncated: prepared.truncated,
        }))
    }
}

fn prepare_input(
    sessions: &[DreamSession],
    max_chars: usize,
    has_more_sessions: bool,
) -> Option<PreparedDreamInput> {
    if sessions.is_empty() || max_chars == 0 {
        return None;
    }
    let mut text = String::new();
    let mut last = None;
    let mut truncated = has_more_sessions;
    for (index, session) in sessions.iter().enumerate() {
        let block = render_session(index + 1, session);
        let separator = usize::from(!text.is_empty());
        let used = text.chars().count();
        let available = max_chars.saturating_sub(used + separator);
        if available == 0 {
            truncated = true;
            break;
        }
        let block_chars = block.chars().count();
        if block_chars > available {
            if last.is_some() {
                truncated = true;
                break;
            }
            text.extend(block.chars().take(available));
            truncated = true;
        } else {
            if separator == 1 {
                text.push('\n');
            }
            text.push_str(&block);
        }
        last = Some((
            session.updated_at.clone(),
            session.session_id.clone(),
            session.checkpoint_message_id,
            index + 1,
        ));
        if block_chars > available {
            break;
        }
    }
    let (checkpoint_updated_at, checkpoint_session_id, checkpoint_message_id, input_sessions) =
        last?;
    Some(PreparedDreamInput {
        text,
        checkpoint_updated_at,
        checkpoint_session_id,
        checkpoint_message_id,
        input_sessions,
        truncated,
    })
}

fn render_session(index: usize, session: &DreamSession) -> String {
    let mut lines = Vec::new();
    if let Some(summary) = session.summary.as_deref().and_then(safe_input_line) {
        lines.push(format!("Summary: {summary}"));
    }
    lines.extend(
        session
            .user_messages
            .iter()
            .filter_map(|message| safe_input_line(message))
            .map(|message| format!("User: {message}")),
    );
    if lines.is_empty() {
        lines.push("(no durable-safe user text after local filtering)".to_owned());
    }
    format!("[Session {index}]\n{}", lines.join("\n"))
}

fn safe_input_line(raw: &str) -> Option<String> {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || contains_dream_sensitive_text(&text) || is_small_talk_noise(&text) {
        return None;
    }
    Some(text)
}

fn is_small_talk_noise(text: &str) -> bool {
    let normalized = text
        .trim_matches(|ch: char| {
            ch.is_whitespace() || ch.is_ascii_punctuation() || "，。！？～~".contains(ch)
        })
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "hi" | "hello"
            | "hey"
            | "你好"
            | "您好"
            | "早"
            | "早上好"
            | "晚上好"
            | "在吗"
            | "谢谢"
            | "感谢"
            | "再见"
            | "拜拜"
            | "哈哈"
            | "哈哈哈"
    )
}

fn parse_model_output(
    raw: &str,
    max_output_memories: usize,
) -> Result<Vec<DreamModelCandidate>, &'static str> {
    let text = strip_single_json_fence(raw.trim());
    let output =
        serde_json::from_str::<DreamModelOutput>(text).map_err(|_| "dream_output_invalid_json")?;
    if output.memories.len() > max_output_memories {
        return Err("dream_output_too_many_memories");
    }
    Ok(output.memories)
}

fn strip_single_json_fence(text: &str) -> &str {
    let Some(body) = text.strip_prefix("```json") else {
        return text;
    };
    body.strip_suffix("```").map(str::trim).unwrap_or(text)
}

fn sanitize_candidate(candidate: DreamModelCandidate) -> Option<DreamCandidate> {
    if !candidate.worth_saving {
        return None;
    }
    let content = normalize_explicit_memory_content(&candidate.content)?;
    if contains_dream_sensitive_text(&content) || contains_scope_or_identity_field(&content) {
        return None;
    }
    let attribute_key = match candidate.attribute_key {
        None => None,
        Some(key) => {
            let key = key.trim().to_ascii_lowercase();
            if key.is_empty()
                || key.len() > 64
                || !key.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
                })
            {
                return None;
            }
            Some(key)
        }
    };
    Some(DreamCandidate {
        content,
        category: candidate.category,
        attribute_key,
    })
}

/// Dream 比显式记忆写入更保守：无法确认用途的联系方式和长数字也不进入模型或 Memory。
fn contains_dream_sensitive_text(text: &str) -> bool {
    if contains_sensitive_text(text) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "手机号",
        "手机号码",
        "电话号码",
        "qq号",
        "微信号",
        "家庭住址",
        "详细地址",
        "真实姓名",
        "邮箱地址",
        "email address",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    let mut consecutive_digits = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            consecutive_digits += 1;
            if consecutive_digits >= 6 {
                return true;
            }
        } else {
            consecutive_digits = 0;
        }
    }
    false
}

fn contains_scope_or_identity_field(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "user_id",
        "userid",
        "group_id",
        "groupid",
        "scope_id",
        "subject_id",
        "openid",
        "unionid",
        "guild_id",
        "channel_id",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_no_reply(raw: &str) -> bool {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        == "noreply"
}

fn unix_epoch() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use crate::{
        error::LlmError,
        runtime::{
            respond::tests::support::MockProvider,
            session::{SessionMeta, SessionStore, SessionTurnActor},
        },
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
    };

    use super::super::{
        MemoryKind, MemoryQuery, MemorySourceType, MemoryStatus, MemoryVisibility,
        SaveMemoryRequest,
    };
    use super::*;

    fn test_config() -> MemoryDreamConfig {
        MemoryDreamConfig {
            enabled: true,
            min_interval_seconds: 0,
            min_new_sessions: 1,
            max_sessions: 20,
            max_input_chars: 32_000,
            max_output_memories: 8,
        }
    }

    fn test_stores() -> (MemoryStore, SessionStore) {
        let database =
            SqliteDatabase::open_temp("memory-dream", APP_MIGRATIONS).expect("open database");
        (
            MemoryStore::new(database.clone()),
            SessionStore::new(database),
        )
    }

    fn private_actor(user: &str) -> MemoryActor {
        MemoryActor {
            user_id: user.to_owned(),
            personal_scope_id: user.to_owned(),
            group_scope_id: None,
            can_manage_group_memory: false,
        }
    }

    fn private_context(user: &str) -> MemoryDreamContext {
        MemoryDreamContext {
            actor: private_actor(user),
            target: MemoryTarget::personal(user),
            conversation_scope_key: format!("private:{user}"),
            actor_ref: None,
            model: Some("mock-dream".to_owned()),
        }
    }

    fn add_private_session(store: &SessionStore, user: &str, text: &str) {
        let _ = add_private_session_with_id(store, user, text);
    }

    fn add_private_session_with_id(store: &SessionStore, user: &str, text: &str) -> String {
        let meta = SessionMeta::new(
            format!("private:{user}"),
            Some(user.to_owned()),
            None,
            None,
            None,
            "test",
        );
        let mut session = store.create(&meta, "", false).expect("create session");
        store
            .append_exchange(&mut session, text, "assistant reply not used by Dream")
            .expect("append exchange");
        session.session_id
    }

    fn group_context(group: &str, user: &str) -> MemoryDreamContext {
        let scope_key = format!("group:{group}");
        let actor_ref = SessionTurnActor::actor_ref_for_user(&scope_key, user).unwrap();
        MemoryDreamContext {
            actor: MemoryActor {
                user_id: user.to_owned(),
                personal_scope_id: user.to_owned(),
                group_scope_id: Some(group.to_owned()),
                can_manage_group_memory: false,
            },
            target: MemoryTarget::group_profile(group, user),
            conversation_scope_key: scope_key,
            actor_ref: Some(actor_ref),
            model: Some("mock-dream".to_owned()),
        }
    }

    fn add_group_session(store: &SessionStore, group: &str, turns: &[(&str, &str)]) {
        let scope_key = format!("group:{group}");
        let meta = SessionMeta::new(
            scope_key.clone(),
            Some("request-user".to_owned()),
            Some(group.to_owned()),
            None,
            None,
            "test",
        );
        let mut session = store.create(&meta, "", false).expect("create session");
        for (user, text) in turns {
            session.set_turn_actor(Some(SessionTurnActor {
                actor_ref: SessionTurnActor::actor_ref_for_user(&scope_key, user),
                display_name: None,
                display_name_source: None,
                group_member_role: None,
                identity_source: None,
            }));
            store
                .append_exchange(&mut session, text, "assistant reply not used by Dream")
                .expect("append group exchange");
        }
    }

    fn active_memories(
        store: &MemoryStore,
        actor: &MemoryActor,
        target: MemoryTarget,
    ) -> Vec<super::super::MemoryRecord> {
        MemoryOperations::new(store.clone())
            .list(actor, MemoryQuery::active(target))
            .expect("list memories")
    }

    fn worker(
        store: &MemoryStore,
        provider: MockProvider,
        config: MemoryDreamConfig,
    ) -> MemoryDreamWorker {
        MemoryDreamWorker::new(Arc::new(provider), store.clone(), config)
    }

    #[test]
    fn no_reply_validation_is_strict_but_format_tolerant() {
        assert!(is_no_reply("NO_REPLY"));
        assert!(is_no_reply(" no-reply \n"));
        assert!(!is_no_reply("NO_REPLY because nothing was found"));
    }

    #[test]
    fn model_output_rejects_permission_fields() {
        let raw = r#"{"memories":[],"scope_id":"forged"}"#;
        assert_eq!(
            parse_model_output(raw, 2).unwrap_err(),
            "dream_output_invalid_json"
        );
    }

    #[tokio::test]
    async fn private_session_dream_writes_system_derived_personal_memory() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我长期偏好简洁的中文回答");
        let provider = MockProvider::with_dream_replies(vec![Ok(
            r#"{"memories":[{"content":"用户长期偏好简洁的中文回答","category":"preference","attribute_key":null,"worth_saving":true}]}"#,
        )]);
        let stats = worker(&store, provider, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(stats.inserted_count, 1);
        let records = active_memories(&store, &private_actor("u1"), MemoryTarget::personal("u1"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source_type, MemorySourceType::SystemDerived);
        assert_eq!(records[0].memory_kind, MemoryKind::Personal);
        assert_eq!(records[0].visibility, MemoryVisibility::Private);
    }

    #[tokio::test]
    async fn group_dream_only_writes_current_members_profile_and_keeps_groups_isolated() {
        let (store, sessions) = test_stores();
        add_group_session(
            &sessions,
            "g1",
            &[("u1", "我一直喜欢红茶"), ("u2", "我一直喜欢咖啡")],
        );
        add_group_session(&sessions, "g2", &[("u1", "我在这个群喜欢绿茶")]);
        let provider = MockProvider::with_dream_replies(vec![
            Ok(
                r#"{"memories":[{"content":"当前成员长期喜欢红茶","category":"preference","attribute_key":"drink","worth_saving":true}]}"#,
            ),
            Ok(
                r#"{"memories":[{"content":"当前成员长期喜欢绿茶","category":"preference","attribute_key":"drink","worth_saving":true}]}"#,
            ),
        ]);
        let observable = provider.clone();
        let worker = worker(&store, provider, test_config());
        let g1 = group_context("g1", "u1");
        let g2 = group_context("g2", "u1");
        worker.run_once(g1.clone()).await.unwrap().unwrap();
        worker.run_once(g2.clone()).await.unwrap().unwrap();

        let g1_records = active_memories(&store, &g1.actor, g1.target.clone());
        let g2_records = active_memories(&store, &g2.actor, g2.target.clone());
        assert_eq!(g1_records[0].content, "当前成员长期喜欢红茶");
        assert_eq!(g2_records[0].content, "当前成员长期喜欢绿茶");
        assert_eq!(g1_records[0].memory_kind, MemoryKind::GroupProfile);
        assert_eq!(g1_records[0].subject_id.as_deref(), Some("u1"));
        assert_eq!(g1_records[0].visibility, MemoryVisibility::GroupMembers);
        let requests = observable.requests();
        let first_input = &requests[0].messages.last().unwrap().content;
        assert!(first_input.contains("我一直喜欢红茶"));
        assert!(!first_input.contains("我一直喜欢咖啡"));
        let u2 = group_context("g1", "u2");
        assert!(active_memories(&store, &u2.actor, u2.target).is_empty());
        assert!(
            MemoryOperations::new(store.clone())
                .authorize_dream_target(&g1.actor, &MemoryTarget::group("g1"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn group_profile_opt_out_skips_model_and_write() {
        let (store, sessions) = test_stores();
        add_group_session(&sessions, "g1", &[("u1", "我长期喜欢红茶")]);
        let context = group_context("g1", "u1");
        MemoryOperations::new(store.clone())
            .set_group_profile_enabled(&context.actor, &context.target, false)
            .unwrap();
        let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
        let observable = provider.clone();

        assert!(
            worker(&store, provider, test_config())
                .run_once(context)
                .await
                .unwrap()
                .is_none()
        );
        assert!(observable.requests().is_empty());
    }

    #[tokio::test]
    async fn duplicate_and_user_confirmed_conflict_do_not_replace_confirmed_memory() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我的长期称呼偏好没有变化");
        let actor = private_actor("u1");
        let target = MemoryTarget::personal("u1");
        MemoryOperations::new(store.clone())
            .save(SaveMemoryRequest {
                actor: actor.clone(),
                target: target.clone(),
                content: "请称呼用户为小一".to_owned(),
                source_text: String::new(),
                category: MemoryCategory::Identity,
                legacy_scope: "private".to_owned(),
                visibility: MemoryVisibility::Private,
                source_type: MemorySourceType::UserConfirmed,
                source_ref: None,
                confirmed_at: None,
                pinned: false,
                attribute_key: Some("nickname".to_owned()),
                relation_subject_id: None,
                relation_object_id: None,
            })
            .unwrap();
        let provider = MockProvider::with_dream_replies(vec![Ok(
            r#"{"memories":[{"content":"请称呼用户为小一","category":"identity","attribute_key":"nickname","worth_saving":true},{"content":"请称呼用户为小二","category":"identity","attribute_key":"nickname","worth_saving":true}]}"#,
        )]);
        let stats = worker(&store, provider, test_config())
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(stats.duplicate_count, 1);
        assert_eq!(stats.conflict_count, 1);
        let records = active_memories(&store, &actor, target);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].content, "请称呼用户为小一");
        assert_eq!(records[0].status, MemoryStatus::Active);
        assert_eq!(records[0].source_type, MemorySourceType::UserConfirmed);
    }

    #[tokio::test]
    async fn invalid_output_and_model_failure_leave_batch_retryable() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
        let invalid = MockProvider::with_dream_replies(vec![Ok("not-json")]);
        assert_eq!(
            worker(&store, invalid, test_config())
                .run_once(private_context("u1"))
                .await
                .unwrap_err(),
            "dream_output_invalid_json"
        );
        let failed = MockProvider::with_dream_replies(vec![Err(LlmError::provider(
            "dream unavailable",
            "test",
        ))]);
        assert_eq!(
            worker(&store, failed, test_config())
                .run_once(private_context("u1"))
                .await
                .unwrap_err(),
            "dream_model_failed"
        );
        let retry = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
        assert!(
            worker(&store, retry, test_config())
                .run_once(private_context("u1"))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn no_reply_and_all_safety_filtered_candidates_advance_checkpoint() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
        let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
        let no_reply_worker = worker(&store, provider, test_config());
        let stats = no_reply_worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();
        assert!(stats.no_reply);
        assert!(
            no_reply_worker
                .run_once(private_context("u1"))
                .await
                .unwrap()
                .is_none()
        );

        add_private_session(&sessions, "u2", "我长期使用某个凭据");
        let filtered = MockProvider::with_dream_replies(vec![Ok(
            r#"{"memories":[{"content":"用户的 access token 是 secret-value","category":"note","attribute_key":null,"worth_saving":true}]}"#,
        )]);
        let filtered_worker = worker(&store, filtered, test_config());
        let stats = filtered_worker
            .run_once(private_context("u2"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stats.filtered_count, 1);
        assert_eq!(stats.inserted_count, 0);
        assert!(
            filtered_worker
                .run_once(private_context("u2"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn database_failure_rolls_back_memory_and_checkpoint() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
        store.abort_memory_insert_for_test().unwrap();
        let output = r#"{"memories":[{"content":"用户长期喜欢结构化回答","category":"preference","attribute_key":null,"worth_saving":true}]}"#;
        let provider = MockProvider::with_dream_replies(vec![Ok(output)]);
        assert_eq!(
            worker(&store, provider, test_config())
                .run_once(private_context("u1"))
                .await
                .unwrap_err(),
            "dream_commit_failed"
        );
        assert!(
            active_memories(&store, &private_actor("u1"), MemoryTarget::personal("u1")).is_empty()
        );
        let storage_context = DreamContext {
            actor_scope_id: "u1".to_owned(),
            target: MemoryTarget::personal("u1"),
            conversation_scope_key: "private:u1".to_owned(),
            actor_ref: None,
        };
        assert!(
            store
                .claim_dream(
                    &storage_context,
                    DreamLimits {
                        min_interval_seconds: 0,
                        min_new_sessions: 1,
                        max_sessions: 20,
                    },
                    unix_epoch(),
                )
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn truncated_input_processes_tail_in_later_batch() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", &"长期偏好甲".repeat(500));
        add_private_session(&sessions, "u1", "长期偏好乙");
        let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
        let observable = provider.clone();
        let mut config = test_config();
        config.max_input_chars = 1000;
        config.min_new_sessions = 2;
        let worker = worker(&store, provider, config);
        let first = worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();
        let second = worker
            .run_once(private_context("u1"))
            .await
            .unwrap()
            .unwrap();

        assert!(first.truncated);
        assert_eq!(first.input_sessions, 1);
        assert_eq!(second.input_sessions, 1);
        assert_eq!(observable.requests().len(), 2);
    }

    #[tokio::test]
    async fn concurrent_dream_claims_execute_model_only_once() {
        let (store, sessions) = test_stores();
        add_private_session(&sessions, "u1", "我长期喜欢结构化回答");
        let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")])
            .with_dream_delay(Duration::from_millis(100));
        let observable = provider.clone();
        let worker = worker(&store, provider, test_config());
        let first = tokio::spawn({
            let worker = worker.clone();
            async move { worker.run_once(private_context("u1")).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = worker.run_once(private_context("u1")).await.unwrap();
        let first = first.await.unwrap().unwrap();

        assert!(first.is_some());
        assert!(second.is_none());
        assert_eq!(observable.requests().len(), 1);
    }

    #[tokio::test]
    async fn title_only_session_update_does_not_reprocess_old_messages() {
        let (store, sessions) = test_stores();
        let session_id = add_private_session_with_id(&sessions, "u1", "我长期喜欢结构化回答");
        let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY")]);
        let worker = worker(&store, provider, test_config());
        assert!(
            worker
                .run_once(private_context("u1"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            sessions
                .update_title_if_current(
                    &session_id,
                    crate::runtime::session::DEFAULT_SESSION_TITLE,
                    "后台标题",
                )
                .unwrap()
        );

        assert!(
            worker
                .run_once(private_context("u1"))
                .await
                .unwrap()
                .is_none()
        );
    }
}
