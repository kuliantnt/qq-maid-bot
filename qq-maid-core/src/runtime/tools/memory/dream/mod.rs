// Portions of this file are adapted from xai-org/grok-build's xai-grok-memory
// Dream implementation. Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified for qq-maid-bot: SQLite Session/Memory targets, strict JSON output,
// actor-scoped group profiles, opt-out enforcement, and transactional checkpoints.

//! 从既有 Session 历史异步提取有长期价值的 SystemDerived Memory。

use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

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
        DreamMessage, MemoryCategory, MemoryStore, MemoryTarget,
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
    input_messages: usize,
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
            &claim.messages,
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
                    "Session user-message input (oldest to newest):\n{}",
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
                input_count: prepared.input_messages,
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
            input_messages = prepared.input_messages,
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
    messages: &[DreamMessage],
    max_chars: usize,
    has_more_sessions: bool,
) -> Option<PreparedDreamInput> {
    if messages.is_empty() || max_chars == 0 {
        return None;
    }
    let mut text = String::new();
    let mut last = None;
    let mut truncated = has_more_sessions;
    let mut input_sessions = HashSet::new();
    let mut input_messages = 0;
    for message in messages {
        let first_for_session = input_sessions.insert(message.session_id.as_str());
        let block = render_message(input_sessions.len(), message, first_for_session);
        let separator = usize::from(!text.is_empty());
        let used = text.chars().count();
        let block_chars = block.chars().count();
        let exceeds_limit = used.saturating_add(separator).saturating_add(block_chars) > max_chars;
        if exceeds_limit && last.is_some() {
            if first_for_session {
                input_sessions.remove(message.session_id.as_str());
            }
            truncated = true;
            break;
        }
        if separator == 1 {
            text.push('\n');
        }
        // 字符上限是批次软上限：若首条消息单独超限，必须完整交给模型，避免检查点
        // 推进到整条消息时丢失尾部；该批不再追加后续消息，也不引入字符偏移检查点。
        text.push_str(&block);
        last = Some((
            message.updated_at.clone(),
            message.session_id.clone(),
            message.message_id,
        ));
        input_messages += 1;
        if exceeds_limit {
            break;
        }
    }
    let (checkpoint_updated_at, checkpoint_session_id, checkpoint_message_id) = last?;
    Some(PreparedDreamInput {
        text,
        checkpoint_updated_at,
        checkpoint_session_id,
        checkpoint_message_id,
        input_sessions: input_sessions.len(),
        input_messages,
        truncated: truncated || checkpoint_message_id != messages.last()?.message_id,
    })
}

fn render_message(index: usize, message: &DreamMessage, include_header: bool) -> String {
    let mut lines = Vec::new();
    if include_header {
        lines.push(format!("[Session {index}]"));
    }
    lines.push(render_user_line(&message.content));
    lines.join("\n")
}

fn render_user_line(content: &str) -> String {
    safe_input_line(content)
        .map(|content| format!("User: {content}"))
        .unwrap_or_else(|| "User: (content omitted by local safety filter)".to_owned())
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
mod tests;
