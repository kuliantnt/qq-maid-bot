//! 暗骰的私有结果只写 Outbox，不进入群响应、会话历史或模型。
use super::{RollCommand, dice::csprng_roller, parse_roll_command_with_default_die_sides};
use crate::{
    runtime::{
        push::{PushTarget, PushTargetType},
        session::now_iso_cn,
    },
    storage::notification::{NotificationOutboxStore, NotificationUpsert},
};
use qq_maid_common::identity_context::ConversationKind;

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

pub(crate) fn execute_extended_command(
    command: &ExtendedRollCommand,
    sides: u8,
    name: Option<&str>,
    kind: ConversationKind,
    target: Option<&PushTarget>,
    store: &NotificationOutboxStore,
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
    let mut roller = csprng_roller();
    let mut lines = vec![format!(
        "暗骰{}{}",
        name.map(|n| format!(" · {n}")).unwrap_or_default(),
        reason.map(|r| format!(" · {r}")).unwrap_or_default()
    )];
    for _ in 0..repetitions {
        let Ok(result) = expression.roll(&mut roller) else {
            return "暗骰计算失败，未发送结果。".to_owned();
        };
        lines.push(format!(
            "{}：{} = {}",
            result.expression,
            result.calculation(),
            result.total
        ));
    }
    let result = lines.join("\n");
    if private {
        return result;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let request = NotificationUpsert {
        source_type: "hidden_roll".to_owned(),
        source_id: id.clone(),
        dedupe_key: format!("hidden_roll:{id}"),
        target: target.expect("已校验群内私发目标").clone(),
        channel: "push".to_owned(),
        kind: "hidden_roll".to_owned(),
        payload: serde_json::json!({"text":result}),
        scheduled_at: now_iso_cn(),
        max_attempts: 3,
        reactivate_cancelled: false,
    };
    match store.upsert(request) {
        Ok(_) => "暗骰结果已进入私发队列，请留意机器人私聊；尚未确认送达。".to_owned(),
        Err(_) => "暗骰结果入队失败，未发送结果，请稍后重试。".to_owned(),
    }
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
