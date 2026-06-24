use serde::Deserialize;
use serde_json::Value;

use crate::error::LlmError;
use crate::runtime::weather::types::{
    AirQualitySummary, WeatherAlert, WeatherLifeIndex, WeatherSupplement,
};

use super::{
    QWEATHER_EMPTY_CODE, QWEATHER_QAQI_CODE, QWEATHER_SUCCESS_CODE,
    util::{non_empty_string, qweather_code_error, value_to_display_string},
};

/// 和风天气 v1 API 通用元数据。
#[derive(Debug, Deserialize)]
pub(super) struct QWeatherV1Metadata {
    /// true 表示请求成功但无数据。
    #[serde(rename = "zeroResult")]
    zero_result: Option<bool>,
}

/// 和风天气实时预警 API 响应。
#[derive(Debug, Deserialize)]
pub(super) struct QWeatherAlertResponse {
    metadata: Option<QWeatherV1Metadata>,
    #[serde(default)]
    alerts: Vec<QWeatherAlert>,
}

/// 和风天气实时预警数据。
#[derive(Debug, Deserialize)]
struct QWeatherAlert {
    #[serde(rename = "senderName")]
    sender_name: Option<String>,
    #[serde(rename = "issuedTime")]
    issued_time: Option<String>,
    #[serde(rename = "messageType")]
    message_type: Option<QWeatherAlertMessageType>,
    #[serde(rename = "eventType")]
    event_type: Option<QWeatherAlertEventType>,
    severity: Option<String>,
    color: Option<QWeatherAlertColor>,
    #[serde(rename = "expireTime")]
    expire_time: Option<String>,
    headline: Option<String>,
    description: Option<String>,
}

/// 和风天气预警消息性质。
#[derive(Debug, Deserialize)]
struct QWeatherAlertMessageType {
    code: Option<String>,
}

/// 和风天气预警事件类型。
#[derive(Debug, Deserialize)]
struct QWeatherAlertEventType {
    name: Option<String>,
}

/// 和风天气预警颜色字段。
#[derive(Debug, Deserialize)]
struct QWeatherAlertColor {
    code: Option<String>,
}

impl QWeatherAlert {
    /// 官方字段明确表示取消时过滤；不根据时间自行推断是否仍然生效。
    fn is_cancelled(&self) -> bool {
        self.message_type
            .as_ref()
            .and_then(|message_type| message_type.code.as_deref())
            .map(|code| code.trim().eq_ignore_ascii_case("cancel"))
            .unwrap_or(false)
    }

    /// 转换为公开预警摘要。标题缺失时用事件名或描述兜底，仍避免展示空预警。
    fn into_weather_alert(self) -> Option<WeatherAlert> {
        let event_name = self
            .event_type
            .and_then(|event_type| non_empty_string(event_type.name));
        let description = non_empty_string(self.description);
        let headline = non_empty_string(self.headline)
            .or_else(|| event_name.clone())
            .or_else(|| description.clone())?;
        Some(WeatherAlert {
            headline,
            event_name,
            severity: non_empty_string(self.severity),
            color_code: self.color.and_then(|color| non_empty_string(color.code)),
            sender_name: non_empty_string(self.sender_name),
            issued_time: non_empty_string(self.issued_time),
            expire_time: non_empty_string(self.expire_time),
            description,
        })
    }
}

/// 和风天气实时空气质量 API 响应。
#[derive(Debug, Deserialize)]
pub(super) struct QWeatherAirQualityResponse {
    metadata: Option<QWeatherV1Metadata>,
    #[serde(default)]
    indexes: Vec<QWeatherAirQualityIndex>,
}

/// 和风天气空气质量指数。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct QWeatherAirQualityIndex {
    code: Option<String>,
    name: Option<String>,
    aqi: Option<Value>,
    #[serde(rename = "aqiDisplay")]
    aqi_display: Option<String>,
    level: Option<String>,
    category: Option<String>,
    #[serde(rename = "primaryPollutant")]
    primary_pollutant: Option<QWeatherAirQualityPollutant>,
}

/// 和风天气首要污染物摘要。
#[derive(Debug, Clone, Deserialize)]
struct QWeatherAirQualityPollutant {
    code: Option<String>,
    name: Option<String>,
}

impl QWeatherAirQualityIndex {
    /// 转换为公开空气质量摘要；没有任何可展示 AQI 值时视为无数据。
    fn into_air_quality_summary(self) -> Option<AirQualitySummary> {
        let aqi_display = non_empty_string(self.aqi_display)
            .or_else(|| self.aqi.as_ref().and_then(value_to_display_string))?;
        let primary_pollutant = self.primary_pollutant.and_then(|pollutant| {
            non_empty_string(pollutant.name).or_else(|| non_empty_string(pollutant.code))
        });
        Some(AirQualitySummary {
            code: non_empty_string(self.code),
            name: non_empty_string(self.name),
            aqi_display,
            level: non_empty_string(self.level),
            category: non_empty_string(self.category),
            primary_pollutant,
        })
    }
}

/// 和风天气生活指数 API 响应。
#[derive(Debug, Deserialize)]
pub(super) struct QWeatherIndicesResponse {
    code: String,
    #[serde(default)]
    daily: Vec<QWeatherIndexDaily>,
}

/// 和风天气单条生活指数预报。
#[derive(Debug, Deserialize)]
struct QWeatherIndexDaily {
    date: Option<String>,
    #[serde(rename = "type")]
    type_id: Option<String>,
    name: Option<String>,
    level: Option<String>,
    category: Option<String>,
    text: Option<String>,
}

impl QWeatherIndexDaily {
    /// 转换为公开生活指数摘要。核心标识字段缺失时跳过该条，保留其他可用记录。
    fn into_weather_life_index(self) -> Option<WeatherLifeIndex> {
        Some(WeatherLifeIndex {
            date: non_empty_string(self.date)?,
            type_id: non_empty_string(self.type_id)?,
            name: non_empty_string(self.name)?,
            level: non_empty_string(self.level),
            category: non_empty_string(self.category),
            text: non_empty_string(self.text),
        })
    }
}

/// 将预警响应转换为附加摘要状态。
pub(super) fn weather_alert_supplement(
    body: QWeatherAlertResponse,
) -> WeatherSupplement<Vec<WeatherAlert>> {
    let zero_result = body.metadata.and_then(|metadata| metadata.zero_result);
    let alerts = body
        .alerts
        .into_iter()
        .filter(|alert| !alert.is_cancelled())
        .filter_map(QWeatherAlert::into_weather_alert)
        .collect::<Vec<_>>();

    if zero_result == Some(true) || alerts.is_empty() {
        return WeatherSupplement::empty(zero_result);
    }
    WeatherSupplement::available(alerts)
}

/// 将空气质量响应转换为附加摘要状态。
pub(super) fn air_quality_supplement(
    body: QWeatherAirQualityResponse,
) -> WeatherSupplement<AirQualitySummary> {
    let zero_result = body.metadata.and_then(|metadata| metadata.zero_result);
    if zero_result == Some(true) {
        return WeatherSupplement::empty(zero_result);
    }
    let Some(index) = select_air_quality_index(body.indexes) else {
        return WeatherSupplement::empty(zero_result);
    };
    let Some(summary) = index.into_air_quality_summary() else {
        return WeatherSupplement::empty(zero_result);
    };

    WeatherSupplement::available(summary)
}

/// 将生活指数响应转换为附加摘要状态。
pub(super) fn life_indices_supplement(
    body: QWeatherIndicesResponse,
) -> Result<WeatherSupplement<Vec<WeatherLifeIndex>>, LlmError> {
    if body.code == QWEATHER_EMPTY_CODE {
        return Ok(WeatherSupplement::empty(None));
    }
    if body.code != QWEATHER_SUCCESS_CODE {
        return Err(qweather_code_error(
            "QWeather weather indices 3d",
            &body.code,
        ));
    }

    let indices = body
        .daily
        .into_iter()
        .filter_map(QWeatherIndexDaily::into_weather_life_index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return Ok(WeatherSupplement::empty(None));
    }
    Ok(WeatherSupplement::available(indices))
}

/// 从空气质量索引列表中选择最适合展示的一项。
///
/// 和风 v1 会返回当地 AQI 与 QAQI；当地标准不是固定代码，
/// 因此只把官方通用 `qaqi` 作为回退标记，其余可用指数视为当地标准。
fn select_air_quality_index(
    indexes: Vec<QWeatherAirQualityIndex>,
) -> Option<QWeatherAirQualityIndex> {
    let indexes = indexes
        .into_iter()
        .filter(air_quality_index_has_value)
        .collect::<Vec<_>>();
    indexes
        .iter()
        .find(|index| !is_qaqi_index(index))
        .cloned()
        .or_else(|| indexes.iter().find(|index| is_qaqi_index(index)).cloned())
        .or_else(|| indexes.into_iter().next())
}

/// 判断空气质量索引是否有可展示 AQI 值。
fn air_quality_index_has_value(index: &QWeatherAirQualityIndex) -> bool {
    index
        .aqi_display
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || index
            .aqi
            .as_ref()
            .and_then(value_to_display_string)
            .is_some()
}

/// 判断空气质量索引是否为和风通用 QAQI。
fn is_qaqi_index(index: &QWeatherAirQualityIndex) -> bool {
    index
        .code
        .as_deref()
        .is_some_and(|code| code.trim().eq_ignore_ascii_case(QWEATHER_QAQI_CODE))
}

#[cfg(test)]
mod tests {
    use super::{
        QWEATHER_EMPTY_CODE, QWeatherAirQualityResponse, QWeatherAlertResponse,
        QWeatherIndicesResponse, air_quality_supplement, life_indices_supplement,
        weather_alert_supplement,
    };
    use crate::runtime::weather::types::WeatherSupplementStatus;

    #[test]
    fn weather_alert_supplement_parses_alerts_and_zero_result() {
        let body: QWeatherAlertResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": false },
            "alerts": [
                {
                    "senderName": "杭州市气象台",
                    "issuedTime": "2026-06-12T18:00+08:00",
                    "messageType": { "code": "alert" },
                    "eventType": { "name": "大风" },
                    "severity": "minor",
                    "color": { "code": "blue" },
                    "expireTime": "2026-06-13T18:00+08:00",
                    "headline": "大风蓝色预警",
                    "description": "预计未来24小时阵风较大。"
                },
                {
                    "messageType": { "code": "cancel" },
                    "eventType": { "name": "雷电" },
                    "headline": "取消的预警不展示"
                }
            ]
        }))
        .unwrap();

        let supplement = weather_alert_supplement(body);

        assert_eq!(supplement.status, WeatherSupplementStatus::Available);
        let alerts = supplement.data.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].headline, "大风蓝色预警");
        assert_eq!(alerts[0].color_code.as_deref(), Some("blue"));

        let empty_body: QWeatherAlertResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": true },
            "alerts": []
        }))
        .unwrap();
        let empty = weather_alert_supplement(empty_body);

        assert_eq!(empty.status, WeatherSupplementStatus::Empty);
        assert_eq!(empty.zero_result, Some(true));
    }

    #[test]
    fn air_quality_supplement_prefers_local_then_qaqi_then_first_available() {
        let local_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": false },
            "indexes": [
                { "code": "qaqi", "name": "QAQI", "aqi": 0.8, "aqiDisplay": "0.8", "category": "Excellent" },
                { "code": "cn-mee", "name": "AQI（CN）", "aqi": 42, "aqiDisplay": "42", "category": "优",
                  "primaryPollutant": { "code": "pm2p5", "name": "PM2.5" } }
            ]
        }))
        .unwrap();

        let local = air_quality_supplement(local_body).data.unwrap();
        assert_eq!(local.code.as_deref(), Some("cn-mee"));
        assert_eq!(local.aqi_display, "42");
        assert_eq!(local.primary_pollutant.as_deref(), Some("PM2.5"));

        let qaqi_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "indexes": [
                { "code": "qaqi", "name": "QAQI", "aqi": 0.9, "category": "Excellent" }
            ]
        }))
        .unwrap();
        let qaqi = air_quality_supplement(qaqi_body).data.unwrap();
        assert_eq!(qaqi.code.as_deref(), Some("qaqi"));
        assert_eq!(qaqi.aqi_display, "0.9");

        let fallback_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "indexes": [
                { "name": "Unknown AQI", "aqiDisplay": "11" }
            ]
        }))
        .unwrap();
        let fallback = air_quality_supplement(fallback_body).data.unwrap();
        assert_eq!(fallback.name.as_deref(), Some("Unknown AQI"));
        assert_eq!(fallback.aqi_display, "11");
    }

    #[test]
    fn life_indices_supplement_parses_empty_and_error_codes() {
        let body: QWeatherIndicesResponse = serde_json::from_value(serde_json::json!({
            "code": "200",
            "daily": [
                { "date": "2026-06-12", "type": "1", "name": "运动指数", "level": "2", "category": "较适宜", "text": "适合适量运动。" },
                { "date": "2026-06-12", "type": "3", "name": "穿衣指数", "level": "6", "category": "热", "text": "建议短袖。" },
                { "date": "", "type": "5", "name": "紫外线指数", "category": "强" }
            ]
        }))
        .unwrap();

        let supplement = life_indices_supplement(body).unwrap();
        assert_eq!(supplement.status, WeatherSupplementStatus::Available);
        let indices = supplement.data.unwrap();
        assert_eq!(indices.len(), 2);
        assert_eq!(indices[0].name, "运动指数");

        let empty_body = QWeatherIndicesResponse {
            code: QWEATHER_EMPTY_CODE.to_owned(),
            daily: Vec::new(),
        };
        assert_eq!(
            life_indices_supplement(empty_body).unwrap().status,
            WeatherSupplementStatus::Empty
        );

        let err = life_indices_supplement(QWeatherIndicesResponse {
            code: "401".to_owned(),
            daily: Vec::new(),
        })
        .unwrap_err();
        assert_eq!(err.code, "http_error");
    }
}
