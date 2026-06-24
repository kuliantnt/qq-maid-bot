//! 火车行程 Todo 子流程。
//!
//! 该模块负责把 `/todo add` 中明显像火车行程的输入解析成火车行程草稿，
//! 调用 `TrainExecutor` 查询真实时刻表并校验出发/到达站，最终生成待确认草稿。
//!
//! 约束：
//! - LLM 只负责理解输入（车次、站点、日期、座位、站台），不生成时刻；
//! - 时刻、站序和跨日必须以 12306 返回结果为准；
//! - 校验失败或接口异常时不创建 Todo；
//! - 普通 Todo 输入不会被这里处理，调用方先用本地规则做保守预判。
//!
//! 草稿确认、取消和保存复用普通 `TodoAdd` pending 状态机。本模块只负责解析、
//! 查询、校验，以及把校验后的行程转换成标准 `TodoItemDraft`。

use chrono::{Duration, NaiveDate};
use serde_json::Value;

use crate::{
    error::LlmError,
    runtime::{
        respond::common::extract_json_object,
        session::now_iso_cn,
        todo::{TodoItemDraft, TodoOwner},
        train::{
            TrainScheduleRequest, TrainTodoDraft, TrainTripError, TrainTripValidation,
            validate_train_trip,
        },
    },
};

use crate::runtime::respond::{
    RespondPurpose, RespondRequest, RustRespondService,
    common::{CommandBody, empty_respond_request},
    llm_service::{ChatService, LlmChatService},
    todo_flow::format::format_todo_add_confirm,
};

use std::{collections::HashMap, sync::OnceLock};

/// 火车 Todo 将用户乘车日期映射到 12306 `startDay` 时，最多回看几天。
///
/// 12306 `dayDifference` 是相对列车始发日的偏移，而用户输入的是实际上车日期。
/// 对中途站次日上车的场景，需要查询前几天的始发日候选并校验出发站日期是否对齐。
const TRAIN_TODO_START_DAY_BACKTRACK_DAYS: i64 = 4;

/// LLM 解析火车行程输入的原始结构化结果。
///
/// 字段命名与任务书保持一致，便于 prompt 直接产出；
/// 缺字段或非法字段由 [`parse_train_todo_json`] 统一校验。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrainTodoParse {
    pub kind: Option<String>,
    pub train_code: Option<String>,
    pub from_station: Option<String>,
    pub to_station: Option<String>,
    pub travel_date: Option<String>,
    pub seat: Option<String>,
    pub platform: Option<String>,
    pub note: Option<String>,
}

/// 从 LLM 返回的 JSON 对象解析火车行程输入。
///
/// 返回 `Ok(Some(parse))` 表示识别为火车行程；
/// 返回 `Ok(None)` 表示不是火车行程（`kind` 非 `train`），调用方应回退到普通 Todo；
/// 返回 `Err(message)` 表示识别为火车行程但字段不合法，需要提示用户补充。
pub fn parse_train_todo_json(value: &serde_json::Value) -> Result<Option<TrainTodoParse>, String> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let kind = object
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase());
    if !matches!(kind.as_deref(), Some("train")) {
        return Ok(None);
    }
    let parse = TrainTodoParse {
        kind,
        train_code: json_string_field(object, "train_code"),
        from_station: json_string_field(object, "from_station"),
        to_station: json_string_field(object, "to_station"),
        travel_date: json_string_field(object, "travel_date"),
        seat: json_string_field(object, "seat"),
        platform: json_string_field(object, "platform"),
        note: json_string_field(object, "note"),
    };
    // 基本字段缺失时直接报错，不回退普通 Todo，避免“明天坐 G34 去北京”被误识别成普通待办。
    if parse
        .train_code
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Err("请补充车次，例如：/todo add G34 杭州东 北京南 明天".to_owned());
    }
    if parse
        .from_station
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Err("请补充出发站，例如：/todo add G34 杭州东 北京南 明天".to_owned());
    }
    if parse
        .to_station
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Err("请补充到达站，例如：/todo add G34 杭州东 北京南 明天".to_owned());
    }
    if parse
        .travel_date
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Err("请补充乘车日期，例如：/todo add G34 杭州东 北京南 明天".to_owned());
    }
    Ok(Some(parse))
}

/// 把 LLM 解析结果和 12306 校验结果合并成可写入的草稿。
///
/// 调用方需先通过 [`validate_train_trip`] 拿到 `validation`。
pub fn build_train_todo_draft(
    parse: &TrainTodoParse,
    travel_date: NaiveDate,
    validation: &TrainTripValidation,
) -> TrainTodoDraft {
    TrainTodoDraft {
        train_code: parse
            .train_code
            .clone()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        from_station: parse
            .from_station
            .clone()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        to_station: parse
            .to_station
            .clone()
            .unwrap_or_default()
            .trim()
            .to_owned(),
        travel_date,
        seat: parse
            .seat
            .as_deref()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        platform: parse
            .platform
            .as_deref()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        note: parse
            .note
            .as_deref()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        departure_at: Some(
            validation
                .departure_at
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ),
        arrive_at: Some(validation.arrive_at.format("%Y-%m-%d %H:%M:%S").to_string()),
    }
}

/// 把 12306 校验错误转成用户可见提示。
pub fn format_train_trip_error(train_code: &str, err: &TrainTripError) -> String {
    match err {
        TrainTripError::FromStationNotFound { station } => format!(
            "查询到 {train_code} 的时刻表，但未找到“{station}”站。\n请使用具体车站名称，例如“杭州东”。"
        ),
        TrainTripError::ToStationNotFound { station } => format!(
            "查询到 {train_code} 的时刻表，但未找到“{station}”站。\n请使用具体车站名称，例如“北京南”。"
        ),
        TrainTripError::StationOrderReversed {
            from_station,
            to_station,
        } => format!(
            "{train_code} 的运行方向中，{to_station} 位于 {from_station} 之前，请检查出发站和到达站。"
        ),
        TrainTripError::SameStation { .. } => {
            format!("{train_code} 出发站和到达站不能相同，本次没有创建 Todo。")
        }
        TrainTripError::MissingDepartureTime { station } => {
            format!("{train_code} 的 {station} 站缺少发车时间，本次没有创建 Todo。")
        }
        TrainTripError::MissingArriveTime { station } => {
            format!("{train_code} 的 {station} 站缺少到达时间，本次没有创建 Todo。")
        }
        TrainTripError::InvalidTime {
            station,
            field,
            value,
        } => {
            format!(
                "{train_code} 的 {station} 站 {field} 时间字段异常：{value}，本次没有创建 Todo。"
            )
        }
        TrainTripError::InvalidDayDifference {
            station,
            day_difference,
        } => {
            format!(
                "{train_code} 的 {station} 站跨日字段异常：{day_difference}，本次没有创建 Todo。"
            )
        }
        TrainTripError::ArrivalBeforeDeparture { .. } => {
            format!("{train_code} 的到达时间早于出发时间，本次没有创建 Todo。")
        }
    }
}

/// 把 12306 接口异常转成用户可见提示。
pub fn format_train_query_error(err: &LlmError) -> String {
    match err.code.as_str() {
        "no_schedule" => "该日期未查询到开行信息，本次没有创建 Todo。".to_owned(),
        "timeout" => "铁路时刻服务暂时不可用，本次没有创建 Todo，请稍后重试。".to_owned(),
        _ if err.stage == "train_json" => {
            "铁路时刻服务返回的数据结构异常，本次没有创建 Todo。".to_owned()
        }
        _ => "铁路时刻服务暂时不可用，本次没有创建 Todo，请稍后重试。".to_owned(),
    }
}

/// 校验 LLM 解析出的日期字符串是否为合法 `YYYY-MM-DD`。
pub fn parse_travel_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| "乘车日期需要是 YYYY-MM-DD 格式，例如 2026-06-24。".to_owned())
}

/// 用 12306 时刻表校验火车行程，返回完整草稿。
///
/// 该函数合并了日期解析、时刻查询和站序校验三步，供 todo_flow 直接调用。
pub async fn validate_train_todo(
    executor: &crate::runtime::train::DynTrainExecutor,
    parse: &TrainTodoParse,
) -> Result<TrainTodoDraft, TrainTodoValidationError> {
    let travel_date = parse_travel_date(parse.travel_date.as_deref().unwrap_or(""))
        .map_err(TrainTodoValidationError::BadDate)?;
    let train_code = parse
        .train_code
        .clone()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    let from_station = parse.from_station.as_deref().unwrap_or_default();
    let to_station = parse.to_station.as_deref().unwrap_or_default();
    let mut saw_schedule = false;
    let mut last_no_schedule = None;

    for back_days in 0..=TRAIN_TODO_START_DAY_BACKTRACK_DAYS {
        let candidate_start_day = travel_date - Duration::days(back_days);
        let schedule = match executor
            .query_train_schedule(TrainScheduleRequest {
                train_code: train_code.clone(),
                travel_date: candidate_start_day,
            })
            .await
        {
            Ok(schedule) => schedule,
            Err(err) if err.code == "no_schedule" => {
                last_no_schedule = Some(err);
                continue;
            }
            Err(err) => return Err(TrainTodoValidationError::Query(err)),
        };
        saw_schedule = true;
        let validation =
            validate_train_trip(&schedule, from_station, to_station).map_err(|err| {
                TrainTodoValidationError::Trip {
                    train_code: schedule.train_code.clone(),
                    err,
                }
            })?;
        if validation.departure_at.date() == travel_date {
            return Ok(build_train_todo_draft(parse, travel_date, &validation));
        }
    }

    if saw_schedule {
        return Err(TrainTodoValidationError::UnreliableBoardingDate {
            train_code,
            travel_date,
        });
    }
    Err(TrainTodoValidationError::Query(
        last_no_schedule.unwrap_or_else(|| {
            LlmError::new(
                "no_schedule",
                "no train schedule found for candidate start days",
                "train",
            )
        }),
    ))
}

/// 火车行程校验失败原因。
#[derive(Debug, Clone)]
pub enum TrainTodoValidationError {
    /// 日期格式不合法。
    BadDate(String),
    /// 12306 接口调用失败。
    Query(LlmError),
    /// 时刻校验失败（站点不存在或顺序错误）。
    Trip {
        train_code: String,
        err: TrainTripError,
    },
    /// 候选 startDay 都无法与用户乘车日期对齐。
    UnreliableBoardingDate {
        train_code: String,
        travel_date: NaiveDate,
    },
}

impl TrainTodoValidationError {
    /// 转成用户可见提示。
    pub fn to_user_message(&self) -> String {
        match self {
            Self::BadDate(msg) => msg.clone(),
            Self::Query(err) => format_train_query_error(err),
            Self::Trip { train_code, err } => format_train_trip_error(train_code, err),
            Self::UnreliableBoardingDate {
                train_code,
                travel_date,
            } => format!(
                "无法可靠计算 {train_code} 在 {travel_date} 的出发时间，本次没有创建 Todo。请先用 /火车 {train_code} {travel_date} 核对时刻。"
            ),
        }
    }
}

fn json_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)?
        .as_str()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// 火车行程 add 分支的处理结果。
///
/// `Handled` 表示已识别为火车行程并生成了回复（可能是待确认草稿，也可能是错误提示）；
/// `NotTrain` 表示 LLM 输出不是 `kind=train`，调用方应回退到普通 Todo 流程。
///
/// `Handled` 变体携带的 `PendingTrainAdd` 包含完整草稿，体积较大；
/// 该枚举仅在 `/todo add` 分支内单次使用并立即消费，不需要紧凑布局，因此允许大变体。
#[allow(clippy::large_enum_variant)]
pub enum TrainTodoAddOutcome {
    /// 已识别为火车行程，返回的 CommandBody 已包含待确认草稿或错误提示。
    /// 若 `pending` 为 `Some`，调用方应将其写入 `session.pending_operation`。
    Handled {
        reply: CommandBody,
        pending: Option<PendingTrainAdd>,
    },
    /// 不是火车行程，调用方应走普通 Todo 解析。
    NotTrain,
}

/// 待写入 session 的火车行程 pending 操作。
pub struct PendingTrainAdd {
    pub owner_key: String,
    pub draft: TodoItemDraft,
    pub created_at: String,
}

/// 本地保守预判 `/todo add` 是否明显是火车行程。
///
/// 该函数只做零成本关键词/车次判断，避免普通 Todo 额外调用 `train_add` LLM。
/// 规则故意偏保守：
/// - 规范车次需要再搭配“路线 / 座位站台 / 结构化站名对”等真实行程信号；
/// - 单独出现日期、编号或宽泛中文 token 不足以触发火车分支；
/// - 没有车次时，仍允许“高铁 + 路线 + 日期”这类强铁路语义输入进入识别。
pub(super) fn looks_like_train_todo_add(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let has_train_code = contains_train_code(text);
    let has_rail_keyword = [
        "火车", "列车", "车次", "高铁", "动车", "乘车", "候车", "检票", "站台",
    ]
    .iter()
    .any(|word| text.contains(word));
    let has_route_hint = ["从", "到", "去", "→", "->", "至"]
        .iter()
        .any(|word| text.contains(word));
    let has_date_hint = contains_train_date_hint(text);
    let has_seat_or_platform_hint = ["座", "车厢", "站台", "检票口"]
        .iter()
        .any(|word| text.contains(word));
    let has_station_pair_hint = has_structured_station_pair_after_train_code(text);
    let has_trip_hint = has_rail_keyword || has_route_hint || has_seat_or_platform_hint;

    (has_train_code && (has_trip_hint || has_station_pair_hint))
        || (has_rail_keyword && has_route_hint && has_date_hint)
}

fn contains_train_code(text: &str) -> bool {
    contains_prefixed_train_code(text)
        || text
            .split_whitespace()
            .any(is_numeric_train_code_token)
}

fn contains_prefixed_train_code(text: &str) -> bool {
    static PREFIXED_TRAIN_CODE_RE: OnceLock<regex::Regex> = OnceLock::new();
    PREFIXED_TRAIN_CODE_RE
        .get_or_init(|| {
            regex::Regex::new(r"(?i)(^|[^A-Za-z0-9])([A-Z]{1,2}\d{1,5}次?)([^A-Za-z0-9]|$)")
                .expect("valid train code regex")
        })
        .is_match(text)
}

fn contains_train_date_hint(text: &str) -> bool {
    static TRAIN_WEEKDAY_RE: OnceLock<regex::Regex> = OnceLock::new();
    static TRAIN_ABSOLUTE_DATE_RE: OnceLock<regex::Regex> = OnceLock::new();

    ["今天", "明天", "后天", "大后天"]
        .iter()
        .any(|word| text.contains(word))
        || TRAIN_WEEKDAY_RE
            .get_or_init(|| {
                regex::Regex::new(
                    r"(?u)(^|[^\p{L}\p{N}])(周[一二三四五六日天]|星期[一二三四五六日天])([^\p{L}\p{N}]|$)",
                )
                .expect("valid train weekday regex")
            })
            .is_match(text)
        || TRAIN_ABSOLUTE_DATE_RE
            .get_or_init(|| {
                regex::Regex::new(
                    r"(\d{4}-\d{1,2}-\d{1,2}|\d{4}年\d{1,2}月\d{1,2}[日号]?|\d{1,2}月\d{1,2}[日号]?)",
                )
                .expect("valid train absolute date regex")
            })
            .is_match(text)
}

fn is_numeric_train_code_token(token: &str) -> bool {
    let token = trim_train_token(token);
    let token = token.strip_suffix('次').unwrap_or(token);
    !token.is_empty()
        && (1..=5).contains(&token.chars().count())
        && token.chars().all(|ch| ch.is_ascii_digit())
}

fn has_structured_station_pair_after_train_code(text: &str) -> bool {
    let tokens = text
        .split_whitespace()
        .map(trim_train_token)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let Some(code_index) = tokens
        .iter()
        .position(|token| is_standalone_train_code_token(token))
    else {
        return false;
    };
    tokens
        .iter()
        .skip(code_index + 1)
        .filter(|token| is_plausible_station_token(token))
        .take(2)
        .count()
        >= 2
}

fn is_standalone_train_code_token(token: &str) -> bool {
    let token = trim_train_token(token);
    if token.is_empty() {
        return false;
    }
    let token = token.strip_suffix('次').unwrap_or(token);
    let Some((prefix, digits)) = split_train_code_prefix_and_digits(token) else {
        return is_numeric_train_code_token(token);
    };
    !prefix.is_empty()
        && prefix.chars().all(|ch| ch.is_ascii_alphabetic())
        && prefix.chars().count() <= 2
        && (1..=5).contains(&digits.chars().count())
        && digits.chars().all(|ch| ch.is_ascii_digit())
}

fn split_train_code_prefix_and_digits(token: &str) -> Option<(&str, &str)> {
    let prefix_len = token
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_digit())
        .map(|(index, _)| index)?;
    let (prefix, digits) = token.split_at(prefix_len);
    (!prefix.is_empty() && !digits.is_empty()).then_some((prefix, digits))
}

fn is_plausible_station_token(token: &str) -> bool {
    let token = trim_train_token(token);
    if token.is_empty()
        || is_train_date_like_token(token)
        || contains_obvious_task_hint(token)
        || is_standalone_train_code_token(token)
    {
        return false;
    }
    let normalized = token.strip_suffix('站').unwrap_or(token).trim();
    let len = normalized.chars().count();
    (2..=6).contains(&len) && normalized.chars().all(is_cjk_unified_ideograph)
}

fn is_train_date_like_token(token: &str) -> bool {
    let token = trim_train_token(token);
    matches!(token, "今天" | "明天" | "后天" | "大后天")
        || (token.starts_with('周') && token.chars().count() <= 2)
        || (token.starts_with("星期") && token.chars().count() <= 3)
        || ((token.contains('年') || token.contains('月') || token.contains('号'))
            && token.chars().any(|ch| ch.is_ascii_digit()))
        || ((token.contains('-') || token.contains('/'))
            && token.chars().any(|ch| ch.is_ascii_digit()))
}

fn contains_obvious_task_hint(token: &str) -> bool {
    let lowered = token.to_ascii_lowercase();
    [
        "bug", "issue", "review", "版本", "回归", "跟进", "修复", "发布", "风险", "排期", "进度",
        "需求", "工单", "问题", "代码", "文档", "日志", "会议",
    ]
    .iter()
    .any(|hint| lowered.contains(hint))
}

fn trim_train_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(
            ch,
            ',' | '，'
                | '。'
                | '；'
                | ';'
                | ':'
                | '：'
                | '、'
                | '('
                | ')'
                | '（'
                | '）'
                | '['
                | ']'
                | '【'
                | '】'
                | '<'
                | '>'
                | '《'
                | '》'
                | '"'
                | '\''
        )
    })
}

fn is_cjk_unified_ideograph(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

impl RustRespondService {
    /// 尝试把 `/todo add` 输入识别为火车行程。
    ///
    /// 流程：
    /// 1. 调用 LLM（复用 todo_model）解析输入，prompt 指示其判断是否为火车行程；
    /// 2. 若 LLM 输出 `kind=train`，调用 12306 校验车次、站点和时间；
    /// 3. 校验成功则生成 `TrainTodoDraft` 并返回待确认草稿；
    /// 4. 校验失败或字段缺失则返回错误提示，不创建 Todo；
    /// 5. 若 LLM 输出不是 `kind=train`，返回 `NotTrain`，由调用方回退普通 Todo。
    ///
    /// 这里只调用一次 LLM，避免普通 Todo 场景多一次请求。
    pub(super) async fn try_handle_train_todo_add(
        &self,
        user_text: &str,
        owner: &TodoOwner,
    ) -> Result<TrainTodoAddOutcome, LlmError> {
        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .respond(RespondRequest {
                model: self.todo_model.clone(),
                purpose: RespondPurpose::TodoParse,
                user_text: user_text.to_owned(),
                session: Value::Null,
                metadata: HashMap::from([
                    ("purpose".to_owned(), "todo_parse".to_owned()),
                    ("todo_operation".to_owned(), "train_add".to_owned()),
                ]),
                ..empty_respond_request()
            })
            .await?;

        let Some(json_value) = extract_json_object(&output.reply) else {
            return Ok(TrainTodoAddOutcome::Handled {
                reply: CommandBody::plain(
                    "火车行程解析失败：模型没有返回合法 JSON，本次没有创建 Todo。",
                ),
                pending: None,
            });
        };

        let parse = match parse_train_todo_json(&json_value) {
            Ok(Some(parse)) => parse,
            Ok(None) => return Ok(TrainTodoAddOutcome::NotTrain),
            Err(message) => {
                return Ok(TrainTodoAddOutcome::Handled {
                    reply: CommandBody::plain(message),
                    pending: None,
                });
            }
        };

        // 识别为火车行程，调用 12306 校验。
        match validate_train_todo(&self.train_executor, &parse).await {
            Ok(draft) => {
                let item_draft = train_todo_to_item_draft(&draft, user_text);
                let reply = format_todo_add_confirm(&item_draft);
                Ok(TrainTodoAddOutcome::Handled {
                    reply,
                    pending: Some(PendingTrainAdd {
                        owner_key: owner.key.clone(),
                        draft: item_draft,
                        created_at: now_iso_cn(),
                    }),
                })
            }
            Err(err) => Ok(TrainTodoAddOutcome::Handled {
                reply: CommandBody::plain(err.to_user_message()),
                pending: None,
            }),
        }
    }
}

/// 根据草稿生成 Todo 标题。
pub fn train_todo_title(draft: &TrainTodoDraft) -> String {
    format!(
        "🚄 {} {} → {}",
        draft.train_code, draft.from_station, draft.to_station
    )
}

/// 根据草稿生成 Todo 详情（纯文本，含乘车日期、出发/到达、座位、站台）。
pub fn train_todo_detail(draft: &TrainTodoDraft) -> String {
    let mut rows = vec![format!("乘车日期：{}", draft.travel_date)];
    if let Some(departure) = draft.departure_at.as_deref() {
        rows.push(format!("出发：{}", format_train_clock(departure)));
    }
    if let Some(arrive) = draft.arrive_at.as_deref() {
        rows.push(format!("到达：{}", format_train_clock(arrive)));
    }
    if let Some(seat) = draft.seat.as_deref() {
        rows.push(format!("座位：{}", seat));
    }
    if let Some(platform) = draft.platform.as_deref() {
        rows.push(format!("站台：{}", platform));
    }
    if let Some(note) = draft.note.as_deref() {
        rows.push(format!("备注：{}", note));
    }
    rows.join("\n")
}

/// 把 `YYYY-MM-DD HH:MM:SS` 格式化为 `YYYY-MM-DD HH:MM` 供展示。
fn format_train_clock(value: &str) -> String {
    // 保留日期和时分，去掉秒，便于用户阅读。
    if value.len() >= 16 {
        value[..16].to_owned()
    } else {
        value.to_owned()
    }
}

/// 把校验后的草稿转成 `TodoItemDraft`，复用现有 Todo 存储。
///
/// 时间使用出发站实际发车时间，精度为 `DateTime`，提醒逻辑沿用现有 Todo。
pub fn train_todo_to_item_draft(
    draft: &TrainTodoDraft,
    raw_text: &str,
) -> crate::runtime::todo::TodoItemDraft {
    use crate::runtime::todo::{TodoItemDraft, TodoTimePrecision};
    TodoItemDraft {
        title: train_todo_title(draft),
        detail: Some(train_todo_detail(draft)),
        raw_text: Some(raw_text.to_owned()),
        due_date: Some(draft.travel_date.format("%Y-%m-%d").to_string()),
        due_at: draft.departure_at.clone(),
        time_precision: if draft.departure_at.is_some() {
            TodoTimePrecision::DateTime
        } else {
            TodoTimePrecision::Date
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_train_todo_json_recognizes_train_kind() {
        let value = json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": "05车12A",
            "platform": "8站台",
            "note": null
        });
        let parse = parse_train_todo_json(&value).unwrap().unwrap();
        assert_eq!(parse.train_code.as_deref(), Some("G34"));
        assert_eq!(parse.from_station.as_deref(), Some("杭州东"));
        assert_eq!(parse.seat.as_deref(), Some("05车12A"));
        assert_eq!(parse.platform.as_deref(), Some("8站台"));
    }

    #[test]
    fn parse_train_todo_json_returns_none_for_non_train() {
        let value = json!({"title": "买牛奶"});
        assert!(parse_train_todo_json(&value).unwrap().is_none());
    }

    #[test]
    fn parse_train_todo_json_errors_on_missing_fields() {
        let value = json!({"kind": "train", "train_code": "G34"});
        let err = parse_train_todo_json(&value).unwrap_err();
        assert!(err.contains("出发站"));
    }

    #[test]
    fn build_train_todo_draft_fills_validated_times() {
        let parse = TrainTodoParse {
            kind: Some("train".to_owned()),
            train_code: Some("G34".to_owned()),
            from_station: Some("杭州东".to_owned()),
            to_station: Some("北京南".to_owned()),
            travel_date: Some("2026-06-24".to_owned()),
            seat: Some("05车12A".to_owned()),
            platform: Some("8站台".to_owned()),
            note: None,
        };
        let validation = TrainTripValidation {
            from_stop: crate::runtime::train::TrainStop {
                station_no: 1,
                station_name: "杭州东".to_owned(),
                arrive_time: None,
                departure_time: Some("07:05".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            to_stop: crate::runtime::train::TrainStop {
                station_no: 3,
                station_name: "北京南".to_owned(),
                arrive_time: Some("11:40".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            departure_at: NaiveDate::from_ymd_opt(2026, 6, 24)
                .unwrap()
                .and_hms_opt(7, 5, 0)
                .unwrap(),
            arrive_at: NaiveDate::from_ymd_opt(2026, 6, 24)
                .unwrap()
                .and_hms_opt(11, 40, 0)
                .unwrap(),
        };
        let draft = build_train_todo_draft(
            &parse,
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
            &validation,
        );
        assert_eq!(draft.departure_at.as_deref(), Some("2026-06-24 07:05:00"));
        assert_eq!(draft.arrive_at.as_deref(), Some("2026-06-24 11:40:00"));
        assert_eq!(draft.seat.as_deref(), Some("05车12A"));
    }

    #[test]
    fn build_train_todo_draft_uses_validated_travel_date_only() {
        let parse = TrainTodoParse {
            kind: Some("train".to_owned()),
            train_code: Some("G34".to_owned()),
            from_station: Some("杭州东".to_owned()),
            to_station: Some("北京南".to_owned()),
            travel_date: Some("not-a-date".to_owned()),
            seat: None,
            platform: None,
            note: None,
        };
        let validation = TrainTripValidation {
            from_stop: crate::runtime::train::TrainStop {
                station_no: 99,
                station_name: "杭州东".to_owned(),
                arrive_time: None,
                departure_time: Some("07:05".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            to_stop: crate::runtime::train::TrainStop {
                station_no: 100,
                station_name: "北京南".to_owned(),
                arrive_time: Some("11:40".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            departure_at: NaiveDate::from_ymd_opt(2026, 6, 24)
                .unwrap()
                .and_hms_opt(7, 5, 0)
                .unwrap(),
            arrive_at: NaiveDate::from_ymd_opt(2026, 6, 24)
                .unwrap()
                .and_hms_opt(11, 40, 0)
                .unwrap(),
        };
        let draft = build_train_todo_draft(
            &parse,
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
            &validation,
        );
        assert_eq!(
            draft.travel_date,
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap()
        );
    }

    #[test]
    fn looks_like_train_todo_add_is_conservative() {
        assert!(!looks_like_train_todo_add("买牛奶"));
        assert!(!looks_like_train_todo_add("G34"));
        assert!(!looks_like_train_todo_add("修复 AG34 指标"));
        assert!(!looks_like_train_todo_add("G34 版本 bug"));
        assert!(!looks_like_train_todo_add("G34 版本 bug 明天修"));
        assert!(!looks_like_train_todo_add("K20-回归问题 今天跟进"));
        assert!(!looks_like_train_todo_add("高铁从周口东去北京西"));
        assert!(looks_like_train_todo_add("G99 杭州东 北京南"));
        assert!(looks_like_train_todo_add("1461 北京 上海 明天"));
        assert!(looks_like_train_todo_add("G34 杭州东 北京南 明天"));
        assert!(looks_like_train_todo_add("明天坐 G34 从杭州东去北京南"));
        assert!(looks_like_train_todo_add("明天坐高铁从杭州东去北京南"));
    }

    #[test]
    fn train_todo_detail_omits_empty_fields() {
        let draft = TrainTodoDraft {
            train_code: "G34".to_owned(),
            from_station: "杭州东".to_owned(),
            to_station: "北京南".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
            seat: None,
            platform: None,
            note: None,
            departure_at: Some("2026-06-24 07:05:00".to_owned()),
            arrive_at: Some("2026-06-24 11:40:00".to_owned()),
        };
        let detail = train_todo_detail(&draft);
        assert!(detail.contains("乘车日期：2026-06-24"));
        assert!(detail.contains("出发：2026-06-24 07:05"));
        assert!(detail.contains("到达：2026-06-24 11:40"));
        assert!(!detail.contains("座位"));
        assert!(!detail.contains("站台"));
    }
}
