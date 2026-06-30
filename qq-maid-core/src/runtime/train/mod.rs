//! 列车时刻查询运行时模块。
//!
//! 该模块对 12306 数据源做最小封装：
//! - 对外暴露稳定的查询请求、结果和执行器 trait；
//! - 将 HTTP 请求、JSON 解析和错误分类收敛在这里；
//! - 上层 `/火车` flow 只依赖 trait，不直接感知 12306 接口细节。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{config::AppConfig, error::LlmError};

/// 12306 列车时刻接口地址。
const TRAIN_QUERY_URL: &str =
    "https://mobile.12306.cn/wxxcx/wechat/main/travelServiceQrcodeTrainInfo";

/// 列车时刻查询请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainScheduleRequest {
    /// 车次，例如 `G1`、`D1234` 或 `1461`。
    pub train_code: String,
    /// 查询日期，按中国标准时间解释。
    pub travel_date: NaiveDate,
}

/// 单个经停站信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainStop {
    /// 站序。
    pub station_no: u32,
    /// 车站名。
    pub station_name: String,
    /// 到达时间，格式固定为 `HH:MM`；始发站通常为空。
    pub arrive_time: Option<String>,
    /// 出发时间，格式固定为 `HH:MM`；终到站通常为空。
    pub departure_time: Option<String>,
    /// 停留分钟数。
    pub stopover_minutes: Option<u32>,
    /// 相对发车日的跨日偏移。
    pub day_difference: i32,
    /// `dayDifference` 是否由 12306 原值可靠解析得到。
    ///
    /// `/火车` 查询回复仍可沿用 `day_difference=0` 的宽松回退展示时刻表，
    /// 但火车 Todo 校验层必须借助该标记拒绝“不可信但被兜底成 0”的情况，
    /// 避免把跨日行程误写成当天提醒。
    pub day_difference_reliable: bool,
    /// 该站对应的站内车次显示值；部分跨线车会与主车次不同。
    pub station_train_code: String,
}

/// 完整列车时刻表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainSchedule {
    /// 规范化后的车次。
    pub train_code: String,
    /// 查询日期。
    pub travel_date: NaiveDate,
    /// 始发站。
    pub start_station: String,
    /// 终到站。
    pub end_station: String,
    /// 全部经停站。
    pub stops: Vec<TrainStop>,
    /// 完整车次，来自 12306 `stationTrainCodeAll`；字段缺失或为空时为 `None`。
    /// 跨线车可能会返回形如 `D3233/D3234` 的完整车次，与主车次不同。
    pub full_train_code: Option<String>,
    /// 担当客运段，来自 12306 `jiaolu_corporation_code`；字段缺失或为空时为 `None`。
    pub corporation: Option<String>,
    /// 车型信息，来自 12306 `jiaolu_train_style`；字段缺失或为空时为 `None`。
    pub train_style: Option<String>,
    /// 配属，来自 12306 `jiaolu_dept_train`；字段缺失或为空时为 `None`。
    pub dept_train: Option<String>,
}

/// 列车查询执行器 trait。
#[async_trait]
pub trait TrainExecutor: Send + Sync {
    /// 查询指定日期的列车时刻表。
    async fn query_train_schedule(
        &self,
        req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError>;

    /// 返回执行器名称，供 diagnostics 使用。
    fn provider_name(&self) -> &'static str;
}

/// 动态派发的列车查询执行器。
pub type DynTrainExecutor = Arc<dyn TrainExecutor>;

/// 根据配置构建默认 12306 查询执行器。
pub fn build_train_executor(config: &AppConfig) -> Result<DynTrainExecutor, LlmError> {
    Ok(Arc::new(Train12306Executor::new(config)?))
}

/// 12306 列车时刻执行器。
pub struct Train12306Executor {
    client: reqwest::Client,
}

impl Train12306Executor {
    /// 构造执行器，沿用全局请求超时配置，避免单命令长期阻塞。
    pub fn new(config: &AppConfig) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build 12306 train HTTP client: {err}"))
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl TrainExecutor for Train12306Executor {
    async fn query_train_schedule(
        &self,
        req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        let start_day = req.travel_date.format("%Y%m%d").to_string();
        let response = self
            .client
            .post(TRAIN_QUERY_URL)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(format!("trainCode={}&startDay={start_day}", req.train_code))
            .send()
            .await
            .map_err(map_train_request_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(train_status_error(status));
        }

        let payload: TrainApiResponse = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid 12306 train JSON: {err}"), "train_json")
        })?;
        payload.into_schedule(req)
    }

    fn provider_name(&self) -> &'static str {
        "12306"
    }
}

#[derive(Debug, Deserialize)]
struct TrainApiResponse {
    #[serde(default)]
    status: bool,
    #[serde(rename = "errorMsg", default)]
    error_msg: String,
    #[serde(default)]
    data: Option<TrainApiData>,
}

#[derive(Debug, Deserialize)]
struct TrainApiData {
    #[serde(rename = "trainDetail", default)]
    train_detail: Option<TrainApiDetail>,
}

#[derive(Debug, Deserialize)]
struct TrainApiDetail {
    #[serde(rename = "trainCode", default)]
    train_code: Option<String>,
    #[serde(rename = "stopTime", default)]
    stop_time: Vec<TrainApiStop>,
    /// 完整车次（跨线车可能形如 `D3233/D3234`），12306 部分车次可能不返回。
    /// 该字段位于 `trainDetail` 顶层，对整趟车唯一。
    #[serde(rename = "stationTrainCodeAll", default)]
    station_train_code_all: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrainApiStop {
    #[serde(
        rename = "stationNo",
        default,
        deserialize_with = "deserialize_optional_stringish"
    )]
    station_no: Option<String>,
    #[serde(rename = "stationName", default)]
    station_name: Option<String>,
    #[serde(rename = "arriveTime", default)]
    arrive_time: Option<String>,
    #[serde(rename = "startTime", default)]
    start_time: Option<String>,
    #[serde(rename = "stopover_time", default)]
    stopover_time: Option<String>,
    #[serde(
        rename = "dayDifference",
        default,
        deserialize_with = "deserialize_optional_stringish"
    )]
    day_difference: Option<String>,
    #[serde(rename = "stationTrainCode", default)]
    station_train_code: Option<String>,
    /// 担当客运段，12306 部分车次可能不返回。
    /// 该字段位于每个 `stopTime` 站点内，同一趟车各站值一致，取首站即可。
    #[serde(rename = "jiaolu_corporation_code", default)]
    jiaolu_corporation_code: Option<String>,
    /// 车型信息，12306 部分车次可能不返回。
    /// 该字段位于每个 `stopTime` 站点内，同一趟车各站值一致，取首站即可。
    #[serde(rename = "jiaolu_train_style", default)]
    jiaolu_train_style: Option<String>,
    /// 配属，12306 部分车次可能不返回。
    /// 该字段位于每个 `stopTime` 站点内，同一趟车各站值一致，取首站即可。
    #[serde(rename = "jiaolu_dept_train", default)]
    jiaolu_dept_train: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringishValue {
    String(String),
    Signed(i64),
    Unsigned(u64),
}

fn deserialize_optional_stringish<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<StringishValue>::deserialize(deserializer)?;
    Ok(value.map(|value| match value {
        StringishValue::String(text) => text,
        StringishValue::Signed(number) => number.to_string(),
        StringishValue::Unsigned(number) => number.to_string(),
    }))
}

impl TrainApiResponse {
    fn into_schedule(self, req: TrainScheduleRequest) -> Result<TrainSchedule, LlmError> {
        if !self.status {
            let message = if self.error_msg.trim().is_empty() {
                "12306 train service returned unsuccessful status".to_owned()
            } else {
                format!("12306 train service error: {}", self.error_msg.trim())
            };
            return Err(LlmError::provider(message, "train_status"));
        }

        let Some(detail) = self.data.and_then(|data| data.train_detail) else {
            return Err(no_schedule_error());
        };
        if detail.stop_time.is_empty() {
            return Err(no_schedule_error());
        }

        let mut stops = Vec::with_capacity(detail.stop_time.len());
        // `jiaolu_corporation_code`、`jiaolu_train_style`、`jiaolu_dept_train` 位于
        // 每个 `stopTime` 站点内，同一趟车各站值一致，取首站即可。
        let mut first_corporation: Option<String> = None;
        let mut first_train_style: Option<String> = None;
        let mut first_dept_train: Option<String> = None;
        for (index, stop) in detail.stop_time.into_iter().enumerate() {
            if index == 0 {
                first_corporation = trim_optional_field(stop.jiaolu_corporation_code.clone());
                first_train_style = trim_optional_field(stop.jiaolu_train_style.clone());
                first_dept_train = trim_optional_field(stop.jiaolu_dept_train.clone());
            }
            let (day_difference, day_difference_reliable) =
                parse_day_difference_field(stop.day_difference.as_deref());
            stops.push(TrainStop {
                // `stationNo` 和 `dayDifference` 在 12306 返回里偶发缺失或格式漂移，
                // `/火车` 旧能力应尽量保留时刻表，而不是让整趟车直接硬失败。
                // 更严格的出发/到达时间约束继续留给下游火车 Todo 校验层处理。
                station_no: parse_station_no_field(stop.station_no.as_deref(), index),
                station_name: required_train_field(stop.station_name, "stationName")?,
                arrive_time: normalize_train_time(stop.arrive_time.as_deref()),
                departure_time: normalize_train_time(stop.start_time.as_deref()),
                stopover_minutes: parse_u32_field(stop.stopover_time.as_deref()),
                day_difference,
                day_difference_reliable,
                station_train_code: stop
                    .station_train_code
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| req.train_code.clone()),
            });
        }

        let start_station = stops
            .first()
            .map(|stop| stop.station_name.clone())
            .ok_or_else(no_schedule_error)?;
        let end_station = stops
            .last()
            .map(|stop| stop.station_name.clone())
            .ok_or_else(no_schedule_error)?;
        let train_code = detail
            .train_code
            .and_then(|value| {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_ascii_uppercase())
            })
            .unwrap_or_else(|| req.train_code.clone());

        Ok(TrainSchedule {
            train_code,
            travel_date: req.travel_date,
            start_station,
            end_station,
            stops,
            // 以下 4 个字段来自 12306 可选属性，缺失或为空时统一存为 `None`，
            // 渲染层据此省略对应行，不推测、不补造。
            // `stationTrainCodeAll` 位于 `trainDetail` 顶层；
            // 其余 3 个 `jiaolu_*` 字段位于 `stopTime` 站点内，取首站值。
            full_train_code: trim_optional_field(detail.station_train_code_all),
            corporation: first_corporation,
            train_style: first_train_style,
            dept_train: first_dept_train,
        })
    }
}

fn required_train_field(value: Option<String>, field_name: &str) -> Result<String, LlmError> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            LlmError::provider(
                format!("12306 train response missing required field: {field_name}"),
                "train_json",
            )
        })
}

/// 将 12306 可选字符串字段归一化为 `Option<String>`：去首尾空白，空串视为 `None`。
///
/// 用于 `stationTrainCodeAll`、`jiaolu_corporation_code` 等可选字段，
/// 缺失或为空时返回 `None`，渲染层据此省略对应行。
fn trim_optional_field(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn parse_station_no_field(value: Option<&str>, index: usize) -> u32 {
    parse_u32_field(value).unwrap_or_else(|| (index + 1) as u32)
}

fn parse_day_difference_field(value: Option<&str>) -> (i32, bool) {
    match parse_i32_field(value) {
        Some(value) => (value, true),
        None => (0, false),
    }
}

fn normalize_train_time(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value == "----" || value == "--:--" {
        return None;
    }
    if value.len() == 4 && value.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(format!("{}:{}", &value[..2], &value[2..]));
    }
    Some(value.to_owned())
}

fn parse_u32_field(value: Option<&str>) -> Option<u32> {
    let value = value?.trim();
    (!value.is_empty()).then_some(())?;
    value.parse::<u32>().ok()
}

fn parse_i32_field(value: Option<&str>) -> Option<i32> {
    let value = value?.trim();
    (!value.is_empty()).then_some(())?;
    value.parse::<i32>().ok()
}

fn map_train_request_error(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        return LlmError::timeout("train");
    }
    LlmError::http(format!("12306 train request failed: {err}"))
}

fn train_status_error(status: StatusCode) -> LlmError {
    let message = if status == StatusCode::NOT_FOUND {
        "12306 train service returned 404".to_owned()
    } else {
        format!("12306 train service returned HTTP {status}")
    };
    LlmError::http(message)
}

fn no_schedule_error() -> LlmError {
    LlmError::new(
        "no_schedule",
        "no train schedule found for the requested date",
        "train",
    )
}

/// 站名归一化：去除首尾空白并去掉末尾的“站”字，用于把“杭州东站”与“杭州东”视作同一站。
///
/// 注意：只做最小归一化，不会把“杭州”替换成“杭州东”，避免静默改变用户语义。
pub fn normalize_station_name(name: &str) -> String {
    let trimmed = name.trim();
    let without_suffix = trimmed.strip_suffix('站').unwrap_or(trimmed);
    without_suffix.trim().to_owned()
}

/// 在时刻表中按站名查找经停站。
///
/// 匹配规则：用户输入与经停站名都经过 [`normalize_station_name`] 归一化后做精确比较。
/// 找不到时返回 `None`，由调用方决定如何提示用户。
pub fn find_stop_by_name<'a>(schedule: &'a TrainSchedule, station: &str) -> Option<&'a TrainStop> {
    let target = normalize_station_name(station);
    schedule
        .stops
        .iter()
        .find(|stop| normalize_station_name(&stop.station_name) == target)
}

/// 火车行程草稿，承载 LLM 解析结果和 12306 校验后的时间。
///
/// `departure_at` / `arrive_at` 在校验成功后填充，用于 Todo 提醒；
/// 未校验时（例如 LLM 解析阶段）可以为 `None`。
///
/// 该结构保留在 `runtime::train`，供后续 Train Tool 或其它业务复用；
/// `/todo add` 已不再内置火车行程解析与 12306 校验。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrainTodoDraft {
    /// 车次，例如 `G34`。
    pub train_code: String,
    /// 出发站名（用户输入或 LLM 归一化后的值）。
    pub from_station: String,
    /// 到达站名。
    pub to_station: String,
    /// 乘车日期（中国标准时间）。
    pub travel_date: NaiveDate,
    /// 座位号，可选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// 站台，可选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 备注，可选。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 校验后的出发时间（含跨日），确认写入时用于 Todo `due_at`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_at: Option<String>,
    /// 校验后的到达时间（含跨日）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive_at: Option<String>,
}

impl TrainTodoDraft {
    /// 根据校验结果填充出发/到达时间，返回新的草稿。
    pub fn with_validation(mut self, validation: &TrainTripValidation) -> Self {
        self.departure_at = Some(
            validation
                .departure_at
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        );
        self.arrive_at = Some(validation.arrive_at.format("%Y-%m-%d %H:%M:%S").to_string());
        self
    }

    /// 是否已经完成时刻校验。
    pub fn is_validated(&self) -> bool {
        self.departure_at.is_some() && self.arrive_at.is_some()
    }
}

/// 火车行程校验结果，承载已确认的出发/到达站和对应时间。
///
/// 时间字段已经结合 `day_difference` 计算成完整的 `NaiveDateTime`（中国标准时间），
/// 上层可直接用于 Todo 提醒，不需要再次处理跨日。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainTripValidation {
    /// 出发站经停信息（来自 12306 返回）。
    pub from_stop: TrainStop,
    /// 到达站经停信息。
    pub to_stop: TrainStop,
    /// 出发站的发车时间（含跨日）。
    pub departure_at: NaiveDateTime,
    /// 到达站的到达时间（含跨日）。
    pub arrive_at: NaiveDateTime,
}

/// 火车行程校验失败原因。
///
/// 区分“站点不存在”、“站点顺序错误”等情况，便于上层给出针对性提示，
/// 而不是统一报“车次不存在”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrainTripError {
    /// 出发站不在经停站列表中。
    FromStationNotFound { station: String },
    /// 到达站不在经停站列表中。
    ToStationNotFound { station: String },
    /// 出发站位于到达站之后，方向错误。
    StationOrderReversed {
        from_station: String,
        to_station: String,
    },
    /// 出发站和到达站解析到同一个经停站。
    SameStation { station: String },
    /// 出发站缺少发车时间（理论上始发站也会有发车时间，缺失说明数据异常）。
    MissingDepartureTime { station: String },
    /// 到达站缺少到达时间。
    MissingArriveTime { station: String },
    /// 12306 返回了无法解析的时间字段。
    InvalidTime {
        station: String,
        field: &'static str,
        value: String,
    },
    /// 12306 返回了异常的跨日字段。
    InvalidDayDifference {
        station: String,
        day_difference: String,
    },
    /// 计算后的到达时间早于出发时间，说明候选日期或接口数据不可信。
    ArrivalBeforeDeparture {
        departure_at: String,
        arrive_at: String,
    },
}

/// 校验火车行程并计算出发/到达时间。
///
/// 步骤：
/// 1. 在经停站中找到出发站和到达站；
/// 2. 校验出发站位于到达站之前；
/// 3. 读取出发站 `startTime` 和到达站 `arriveTime`；
/// 4. 结合各自 `day_difference` 计算完整 `NaiveDateTime`。
///
/// 跨日计算以 `schedule.travel_date` 为列车始发日基准，`day_difference`
/// 表示相对该始发日的偏移天数。上层如果拿到的是“用户上车日期”，需要先
/// 选择能让出发站实际日期对齐的候选始发日，不能直接把两者混用。
pub fn validate_train_trip(
    schedule: &TrainSchedule,
    from_station: &str,
    to_station: &str,
) -> Result<TrainTripValidation, TrainTripError> {
    let from_stop = find_stop_by_name(schedule, from_station).ok_or_else(|| {
        TrainTripError::FromStationNotFound {
            station: from_station.to_owned(),
        }
    })?;
    let to_stop = find_stop_by_name(schedule, to_station).ok_or_else(|| {
        TrainTripError::ToStationNotFound {
            station: to_station.to_owned(),
        }
    })?;
    if from_stop.station_no == to_stop.station_no {
        return Err(TrainTripError::SameStation {
            station: from_stop.station_name.clone(),
        });
    }
    if from_stop.station_no > to_stop.station_no {
        return Err(TrainTripError::StationOrderReversed {
            from_station: from_station.to_owned(),
            to_station: to_station.to_owned(),
        });
    }
    let departure_time = from_stop.departure_time.as_deref().ok_or_else(|| {
        TrainTripError::MissingDepartureTime {
            station: from_stop.station_name.clone(),
        }
    })?;
    let arrive_time =
        to_stop
            .arrive_time
            .as_deref()
            .ok_or_else(|| TrainTripError::MissingArriveTime {
                station: to_stop.station_name.clone(),
            })?;
    // Provider 层会把缺失/非法 `dayDifference` 兜底成 0 以保住 `/火车` 查询展示，
    // 但火车 Todo 需要写入绝对提醒时间，不能把这种不可信数据当作“当天”继续计算。
    let from_day_difference = ensure_reliable_day_difference(from_stop)?;
    let to_day_difference = ensure_reliable_day_difference(to_stop)?;
    let departure_at = compose_train_datetime(
        schedule.travel_date,
        departure_time,
        from_day_difference,
        &from_stop.station_name,
        "startTime",
    )?;
    let arrive_at = compose_train_datetime(
        schedule.travel_date,
        arrive_time,
        to_day_difference,
        &to_stop.station_name,
        "arriveTime",
    )?;
    if arrive_at < departure_at {
        return Err(TrainTripError::ArrivalBeforeDeparture {
            departure_at: departure_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            arrive_at: arrive_at.format("%Y-%m-%d %H:%M:%S").to_string(),
        });
    }
    Ok(TrainTripValidation {
        from_stop: from_stop.clone(),
        to_stop: to_stop.clone(),
        departure_at,
        arrive_at,
    })
}

fn ensure_reliable_day_difference(stop: &TrainStop) -> Result<i32, TrainTripError> {
    if stop.day_difference_reliable {
        return Ok(stop.day_difference);
    }
    Err(TrainTripError::InvalidDayDifference {
        station: stop.station_name.clone(),
        day_difference: "缺失或非法".to_owned(),
    })
}

/// 把 `HH:MM` / `HH:MM:SS` 时间和 `day_difference` 组合成完整的 `NaiveDateTime`。
///
/// 时间字段和跨日字段都来自 12306。任何异常都必须显式报错，不能兜底为
/// `00:00` 或当天日期，否则会写入错误提醒。
fn compose_train_datetime(
    base_date: NaiveDate,
    value: &str,
    day_difference: i32,
    station: &str,
    field: &'static str,
) -> Result<NaiveDateTime, TrainTripError> {
    if day_difference < 0 {
        return Err(TrainTripError::InvalidDayDifference {
            station: station.to_owned(),
            day_difference: day_difference.to_string(),
        });
    }
    let time = parse_train_time(value).ok_or_else(|| TrainTripError::InvalidTime {
        station: station.to_owned(),
        field,
        value: value.to_owned(),
    })?;
    let date = base_date + chrono::Duration::days(day_difference as i64);
    Ok(NaiveDateTime::new(date, time))
}

/// 严格解析 12306 时间字段。
///
/// 当前接口常见格式为 `HH:MM`；部分接口或未来变化可能返回 `HH:MM:SS`，
/// 这里明确支持秒字段，但不做截断、不使用默认值。
fn parse_train_time(value: &str) -> Option<NaiveTime> {
    let value = value.trim();
    if value.is_empty() || matches!(value, "--" | "----" | "--:--") {
        return None;
    }
    let parts = value.split(':').collect::<Vec<_>>();
    let [hour, minute] = parts.as_slice() else {
        let [hour, minute, second] = parts.as_slice() else {
            return None;
        };
        let hour = parse_fixed_width_time_part(hour)?;
        let minute = parse_fixed_width_time_part(minute)?;
        let second = parse_fixed_width_time_part(second)?;
        return NaiveTime::from_hms_opt(hour, minute, second);
    };
    let hour = parse_fixed_width_time_part(hour)?;
    let minute = parse_fixed_width_time_part(minute)?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn parse_fixed_width_time_part(value: &str) -> Option<u32> {
    (value.len() == 2 && value.chars().all(|ch| ch.is_ascii_digit()))
        .then(|| value.parse::<u32>().ok())?
}

#[cfg(test)]
mod tests;
