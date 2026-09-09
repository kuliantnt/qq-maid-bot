//! 暗骰的私有结果只写 Outbox，不进入群响应、会话历史或模型。
use std::fmt::Write as _;

use super::{RollCommand, dice::csprng_roller, parse_roll_command_with_default_die_sides};
use crate::{
    runtime::{
        push::{PushTarget, PushTargetType},
        session::now_iso_cn,
    },
    storage::notification::{
        NotificationInsertOutcome, NotificationOutboxStore, NotificationUpsert,
    },
};
use qq_maid_common::identity_context::ConversationKind;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub(crate) struct ExtendedRollCommand {
    argument: String,
    delegate: bool,
}

pub(crate) fn parse_extended_command(text: &str) -> Option<ExtendedRollCommand> {
    let body = text.trim().strip_prefix('/')?;
    let lower = body.to_ascii_lowercase();
    for alias in ["rxh", "rhx", "rhd", "rdh", "rh", "rx"] {
        let Some(suffix) = lower.strip_prefix(alias) else {
            continue;
        };
        if !suffix.is_empty()
            && !suffix.starts_with(char::is_whitespace)
            && !super::looks_like_compact_roll_suffix(suffix)
            && !super::is_cjk_reason_start(suffix)
        {
            continue;
        }
        let argument = body[alias.len()..].trim();
        return Some(ExtendedRollCommand {
            argument: if matches!(alias, "rhd" | "rdh")
                && !suffix.is_empty()
                && !suffix.starts_with(char::is_whitespace)
            {
                super::compact_rd_expression(argument)
            } else {
                argument.to_owned()
            },
            delegate: matches!(alias, "rx" | "rxh" | "rhx"),
        });
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extended_command(
    command: &ExtendedRollCommand,
    sides: u8,
    name: Option<&str>,
    kind: ConversationKind,
    target: Option<&PushTarget>,
    store: &NotificationOutboxStore,
    inbound_id: Option<&str>,
    platform: &str,
    account_id: Option<&str>,
    conversation_id: Option<&str>,
) -> String {
    let mut roller = csprng_roller();
    execute_extended_command_with_roller(
        command,
        sides,
        name,
        kind,
        target,
        store,
        inbound_id,
        platform,
        account_id,
        conversation_id,
        &mut roller,
    )
}

/// 可注入 Roller 的暗骰执行入口；测试用固定 Roller 验证幂等与投递链路。
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extended_command_with_roller<R: super::dice::Roller>(
    command: &ExtendedRollCommand,
    sides: u8,
    name: Option<&str>,
    kind: ConversationKind,
    target: Option<&PushTarget>,
    store: &NotificationOutboxStore,
    inbound_id: Option<&str>,
    platform: &str,
    account_id: Option<&str>,
    conversation_id: Option<&str>,
    roller: &mut R,
) -> String {
    if command.delegate {
        return "代骰入口已识别；人物卡与被代骰者上下文尚未接入，暂不执行。无状态投掷请用 /r，暗骰请用 /rh。".to_owned();
    }
    let private = matches!(
        kind,
        ConversationKind::Private | ConversationKind::ServiceAccount
    );
    let target = target.filter(|t| {
        t.target_type == PushTargetType::Private
            && !t.target_id.trim().is_empty()
            && t.account_id.as_ref().is_some_and(|a| !a.trim().is_empty())
    });
    if !private && target.is_none() {
        return "当前平台未提供可信私发目标，无法执行群内暗骰。请私聊使用 /rh；本次未投骰。"
            .to_owned();
    }
    // 群聊暗骰属于“随机结果 + 外部通知副作用”的高副作用入口。首次执行者通过
    // insert-if-absent 在同一事务内领取稳定幂等键、完成 RNG 并写入最终 payload；
    // 重复或并发重放只会命中已有任务，不会重新投骰或覆盖第一次的结果。
    let dedupe_key = if private {
        None
    } else {
        let Some(inbound_id) = inbound_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return "当前消息缺少可信消息 ID，无法避免暗骰重复执行；本次未投骰。".to_owned();
        };
        Some(hidden_roll_dedupe_key(
            platform,
            account_id,
            conversation_id,
            inbound_id,
        ))
    };
    let parsed =
        parse_roll_command_with_default_die_sides(&format!("/r {}", command.argument), sides);
    let (expression, repetitions, reason) = match parsed {
        Some(RollCommand::Default) => (super::dice::DiceExpression::default_d20(), 1, None),
        Some(RollCommand::DiceExpression { expression }) => (expression, 1, None),
        Some(RollCommand::DiceBatch {
            expression,
            repetitions,
            reason,
        }) => (expression, repetitions, reason),
        _ => return "暗骰参数无效；只支持本地骰式及原因，不执行模型判定。".to_owned(),
    };
    if private {
        return match render_hidden_roll_result(
            &expression,
            repetitions,
            name,
            reason.as_deref(),
            roller,
        ) {
            Ok(result) => result,
            Err(()) => "暗骰计算失败，未发送结果。".to_owned(),
        };
    }
    let dedupe_key = dedupe_key.expect("非私聊暗骰必须具有可信幂等键");
    let placeholder_text = "暗骰结果生成中";
    let request = NotificationUpsert {
        source_type: "hidden_roll".to_owned(),
        source_id: dedupe_key.clone(),
        dedupe_key: dedupe_key.clone(),
        target: target.expect("已校验群内私发目标").clone(),
        channel: "push".to_owned(),
        kind: "hidden_roll".to_owned(),
        // 占位 payload 只在未提交事务内短暂存在；Worker 只能看到事务提交后的最终结果。
        payload: serde_json::json!({
            "message_type": "text",
            "text": placeholder_text,
            "fallback_text": placeholder_text,
        }),
        scheduled_at: now_iso_cn(),
        max_attempts: 3,
        reactivate_cancelled: false,
    };
    match store.insert_if_absent_with(request, || {
        let result =
            render_hidden_roll_result(&expression, repetitions, name, reason.as_deref(), roller)
                .map_err(|()| "暗骰计算失败".to_owned())?;
        let fallback_text = result.clone();
        // payload 必须满足 Notification Worker 的校验：message_type 与 text 均非空。
        // OneBot 文本私发在仓库当前约定中使用 "text"。
        Ok(serde_json::json!({
            "message_type": "text",
            "text": result,
            "fallback_text": fallback_text,
        }))
    }) {
        Ok(NotificationInsertOutcome::Inserted) => {
            "暗骰结果已进入私发队列，请留意机器人私聊；尚未确认送达。".to_owned()
        }
        Ok(NotificationInsertOutcome::AlreadyExists) => {
            "该暗骰结果已进入私发队列，请留意机器人私聊；不会重复投骰。".to_owned()
        }
        Err(_) => "暗骰结果入队失败，未发送结果，请稍后重试。".to_owned(),
    }
}

fn render_hidden_roll_result<R: super::dice::Roller>(
    expression: &super::dice::DiceExpression,
    repetitions: u8,
    name: Option<&str>,
    reason: Option<&str>,
    roller: &mut R,
) -> Result<String, ()> {
    let mut lines = vec![format!(
        "暗骰{}{}",
        name.map(|n| format!(" · {n}")).unwrap_or_default(),
        reason.map(|r| format!(" · {r}")).unwrap_or_default()
    )];
    for _ in 0..repetitions {
        let Ok(result) = expression.roll(roller) else {
            return Err(());
        };
        lines.push(format!(
            "{}：{} = {}",
            result.expression,
            result.calculation(),
            result.total
        ));
    }
    Ok(lines.join("\n"))
}

/// 由平台 / 账号 / 会话 / 可信 message_id 构造暗骰专用稳定幂等键。
/// 不同 conversation 或账号即使 message_id 碰撞也会被 scope 隔离，
/// 同一可信入站消息重放则始终命中同一个 dedupe_key。
fn hidden_roll_dedupe_key(
    platform: &str,
    account_id: Option<&str>,
    conversation_id: Option<&str>,
    inbound_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(platform.trim().as_bytes());
    hasher.update([0]);
    hasher.update(
        account_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .as_bytes(),
    );
    hasher.update([0]);
    hasher.update(
        conversation_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .as_bytes(),
    );
    hasher.update([0]);
    hasher.update(inbound_id.trim().as_bytes());
    let mut output = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(output, "{byte:02x}");
    }
    format!("hidden_roll:{output}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_aliases_preserve_spaced_and_compact_dice_semantics() {
        for (input, expected) in [
            ("/rh d20", "d20"),
            ("/rhd d20", "d20"),
            ("/rdh20", "d20"),
            ("/rhd优势+2", "d20优势+2"),
            ("/rh2#d6", "2#d6"),
            ("/rh", ""),
        ] {
            assert_eq!(parse_extended_command(input).unwrap().argument, expected);
        }
        assert!(parse_extended_command("/rhistory").is_none());
    }
}
