use crate::runtime::tools::roll::dice::{
    DiceExpression, DiceExpressionParse, DiceRollArgumentParse, parse_expression,
    parse_roll_argument_with_default_die_sides,
};

pub(super) const HELP: &str = "先攻：/ri 12 张三, +2 李四, =d10+3 王五, d20+4 赵六；/ri 优势 张三, 劣势-1 李四；裸骰式省略名称时使用 /nn 或平台展示名；/init [list|end|clr]；/init set 单位 骰式；/init del 单位1 单位2。名称不含空格或逗号；同名覆盖，重启清空。";

#[derive(Clone, Debug)]
pub(crate) enum InitiativeCommand {
    Record(String),
    Set(String),
    Delete(Vec<String>),
    List,
    End,
    Clear,
    Help,
    Invalid,
}

pub(crate) fn parse_command(text: &str) -> Option<InitiativeCommand> {
    let body = text.trim().strip_prefix('/')?;
    let (name, args) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    let args = args.trim();
    Some(match name.to_ascii_lowercase().as_str() {
        "ri" if args == "help" => InitiativeCommand::Help,
        "ri" => InitiativeCommand::Record(args.to_owned()),
        // SealDice 的紧凑清空写法复用同一个领域操作，仍拒绝多余参数。
        "initclr" if args.is_empty() => InitiativeCommand::Clear,
        "initclr" => InitiativeCommand::Invalid,
        "init" => {
            let (action, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
            let rest = rest.trim();
            match action.to_ascii_lowercase().as_str() {
                "" | "list" if rest.is_empty() => InitiativeCommand::List,
                "end" | "ed" if rest.is_empty() => InitiativeCommand::End,
                "clr" | "clear" if rest.is_empty() => InitiativeCommand::Clear,
                "help" if rest.is_empty() => InitiativeCommand::Help,
                "set" => InitiativeCommand::Set(rest.to_owned()),
                "del" | "rm" if !rest.is_empty() => {
                    InitiativeCommand::Delete(rest.split_whitespace().map(str::to_owned).collect())
                }
                _ => InitiativeCommand::Invalid,
            }
        }
        _ => return None,
    })
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '，'))
}

pub(super) fn parse_entries(
    input: &str,
    default_name: Option<&str>,
    set: bool,
) -> Result<Vec<(String, DiceExpression)>, &'static str> {
    if input.chars().count() > 2000 || input.chars().any(char::is_control) {
        return Err("先攻输入过长或包含控制字符。");
    }
    let parts: Vec<_> = if set {
        vec![input]
    } else {
        input.split([',', '，']).collect()
    };
    if parts.len() > 20 {
        return Err("一次最多录入 20 个单位。");
    }
    let mut entries = Vec::new();
    for part in parts {
        let part = part.trim();
        let (name, expression) = if set {
            let (name, expr) = part.split_once(char::is_whitespace).ok_or(HELP)?;
            let DiceExpressionParse::Parsed(expression) = parse_expression(expr.trim()) else {
                return Err("先攻骰式无效；不支持人物属性或多轮骰点。");
            };
            (name.to_owned(), expression)
        } else {
            parse_record(part, default_name)?
        };
        if !valid_name(&name) {
            return Err("请指定 1–64 字的单位名，不含空格、逗号或控制字符；也可先 /set 昵称。");
        }
        entries.push((name, expression));
    }
    if entries.iter().map(|(_, e)| e.total_dice()).sum::<u32>() > 200 {
        return Err("一次先攻命令最多投掷 200 颗骰子。");
    }
    Ok(entries)
}

fn parse_record(
    part: &str,
    default_name: Option<&str>,
) -> Result<(String, DiceExpression), &'static str> {
    if let Some(expr) = part.strip_prefix('=') {
        return parse_record_expression(expr, default_name)?.ok_or("先攻骰式无效。");
    }
    if part.starts_with(['+', '-']) || part.starts_with("优势") || part.starts_with("劣势") {
        return parse_record_expression(&format!("d20{part}"), default_name)?
            .ok_or("先攻骰式无效。");
    }
    if part.starts_with(|c: char| c.is_ascii_digit()) {
        // 固定数值只消费数字；复杂骰式必须显式使用 =，避免静默误算。
        let end = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        if !part[end..].is_empty() && !part[end..].starts_with(char::is_whitespace) {
            return Err(HELP);
        }
        let DiceExpressionParse::Parsed(expr) = parse_expression(&part[..end]) else {
            return Err(HELP);
        };
        let name = part[end..].trim();
        return Ok((
            if name.is_empty() {
                default_name.unwrap_or("")
            } else {
                name
            }
            .to_owned(),
            expr,
        ));
    }

    // 裸骰式复用 Roll domain 的完整表达式/“骰式 + 名称”前缀解析；只有确认
    // 不是骰式时才把整个 token 当单位名，避免把 d20+4 误建成角色名。
    if let Some(entry) = parse_record_expression(part, default_name)? {
        return Ok(entry);
    }
    Ok((
        if part.is_empty() {
            default_name.unwrap_or("")
        } else {
            part
        }
        .to_owned(),
        DiceExpression::default_d20(),
    ))
}

fn parse_record_expression(
    text: &str,
    default_name: Option<&str>,
) -> Result<Option<(String, DiceExpression)>, &'static str> {
    match parse_roll_argument_with_default_die_sides(text, 20) {
        DiceRollArgumentParse::Parsed { spec, reason } => {
            if spec.repetitions != 1 {
                return Err("先攻不支持多轮骰点，请用逗号分隔多个单位。");
            }
            Ok(Some((
                reason.or(default_name).unwrap_or("").to_owned(),
                spec.expression,
            )))
        }
        DiceRollArgumentParse::Invalid(_) => Err("先攻骰式无效。"),
        DiceRollArgumentParse::NotDiceExpression => Ok(None),
    }
}
