use serde::Deserialize;

use crate::error::LlmError;

use super::super::types::WeatherLocation;
use super::util::parse_f64_field;

/// 地名的行政区划偏好映射，用于消除同名地点歧义。
///
/// 格式：(用户输入的短名称, 期望的完整行政区划名)
const WEATHER_PLACE_PREFERENCES: &[(&str, &str)] = &[
    ("西湖", "杭州市西湖区"),
    ("西湖区", "杭州市西湖区"),
    ("萧山", "杭州市萧山区"),
    ("萧山区", "杭州市萧山区"),
    ("江北", "重庆市江北区"),
    ("江北区", "重庆市江北区"),
];

/// 城市查询覆盖映射，用于某些地名直接查询不到时改用更准确的查询词。
const WEATHER_LOOKUP_OVERRIDES: &[(&str, &str)] = &[("江北", "重庆江北"), ("江北区", "重庆江北")];

/// 和风天气地理编码 API 响应。
#[derive(Debug, Deserialize)]
pub(super) struct QWeatherGeoResponse {
    /// 状态码
    pub(super) code: String,
    /// 地理位置列表
    #[serde(default)]
    pub(super) location: Vec<QWeatherGeoLocation>,
}

/// 和风天气地理编码结果。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct QWeatherGeoLocation {
    /// 地点名称
    pub(super) name: String,
    /// 地点 ID（用于后续天气查询）
    pub(super) id: String,
    /// 纬度（字符串）
    lat: String,
    /// 经度（字符串）
    lon: String,
    /// 省级行政区
    pub(super) adm1: String,
    /// 地级行政区
    pub(super) adm2: String,
    /// 国家
    country: String,
    /// 时区
    tz: String,
    /// 区域排名（数字越小优先级越高）
    rank: String,
}

impl QWeatherGeoLocation {
    /// 转换为公开的 WeatherLocation 类型。
    pub(super) fn to_weather_location(&self) -> Result<WeatherLocation, LlmError> {
        Ok(WeatherLocation {
            id: Some(self.id.clone()),
            name: self.name.clone(),
            country: Some(self.country.clone()).filter(|value| !value.trim().is_empty()),
            admin1: Some(self.adm1.clone()).filter(|value| !value.trim().is_empty()),
            admin2: Some(self.adm2.clone()).filter(|value| !value.trim().is_empty()),
            timezone: Some(self.tz.clone()).filter(|value| !value.trim().is_empty()),
            latitude: parse_f64_field(&self.lat, "QWeather location lat")?,
            longitude: parse_f64_field(&self.lon, "QWeather location lon")?,
        })
    }

    /// 获取排名数值，用于地点优先级比较。
    fn rank_value(&self) -> i64 {
        self.rank.trim().parse::<i64>().unwrap_or(0)
    }

    /// 生成该地点的所有匹配键，用于与用户输入进行模糊匹配。
    fn match_keys(&self) -> Vec<String> {
        let adm2_city = append_city_suffix(&self.adm2);
        vec![
            self.name.clone(),
            format!("{}{}", self.adm2, self.name),
            format!("{}{}", adm2_city, self.name),
            format!("{}{}{}", self.adm1, self.adm2, self.name),
            format!("{}{}{}", self.adm1, adm2_city, self.name),
        ]
    }
}

/// 从多个候选地点中选择最匹配的一个。
///
/// 选择策略：
/// 1. 只考虑中国境内的地点
/// 2. 优先使用 `WEATHER_PLACE_PREFERENCES` 配置的偏好地点
/// 3. 其次尝试完全匹配名称
/// 4. 最后按和风天气排名选择（数字越小越优）
/// 5. 如果多个地点排名相同，则视为歧义返回错误
pub(super) fn select_location(
    original_city: &str,
    candidates: Vec<QWeatherGeoLocation>,
) -> Result<QWeatherGeoLocation, LlmError> {
    let candidates = candidates
        .into_iter()
        .filter(|location| is_china_country(&location.country))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return Err(LlmError::new(
            "not_found",
            format!("QWeather GeoAPI found no China match for `{original_city}`"),
            "weather",
        ));
    }
    if candidates.len() == 1 {
        return Ok(candidates.into_iter().next().unwrap());
    }

    if let Some(preferred) = preferred_location(original_city, &candidates) {
        return Ok(preferred);
    }

    if let Some(exact) = exact_location(original_city, &candidates) {
        return Ok(exact);
    }

    let mut ranked = candidates;
    ranked.sort_by_key(QWeatherGeoLocation::rank_value);
    let top_rank = ranked[0].rank_value();
    let second_rank = ranked.get(1).map(QWeatherGeoLocation::rank_value);
    if second_rank.is_none_or(|rank| top_rank < rank) {
        return Ok(ranked.remove(0));
    }

    Err(LlmError::new(
        "not_found",
        format!("QWeather GeoAPI found multiple ambiguous matches for `{original_city}`"),
        "weather",
    ))
}

/// 获取用于 API 查询的城市名称，应用覆盖映射。
pub(super) fn lookup_city_query(city: &str) -> String {
    let key = normalize_place_key(city);
    WEATHER_LOOKUP_OVERRIDES
        .iter()
        .find_map(|(alias, query)| (normalize_place_key(alias) == key).then_some(*query))
        .unwrap_or(city)
        .to_owned()
}

/// 在候选中寻找与原始城市名完全匹配的地点（唯一时返回）。
fn exact_location(
    original_city: &str,
    candidates: &[QWeatherGeoLocation],
) -> Option<QWeatherGeoLocation> {
    let key = normalize_place_key(original_city);
    let exact = candidates
        .iter()
        .filter(|location| {
            location
                .match_keys()
                .iter()
                .any(|candidate| place_keys_match(candidate, &key))
        })
        .collect::<Vec<_>>();
    (exact.len() == 1).then(|| exact[0]).cloned()
}

/// 根据 `WEATHER_PLACE_PREFERENCES` 配置查找首选地点。
fn preferred_location(
    original_city: &str,
    candidates: &[QWeatherGeoLocation],
) -> Option<QWeatherGeoLocation> {
    let key = normalize_place_key(original_city);
    let target = WEATHER_PLACE_PREFERENCES
        .iter()
        .find_map(|(alias, preferred)| (normalize_place_key(alias) == key).then_some(*preferred))?;
    let target = normalize_admin_place_key(target);

    candidates
        .iter()
        .find(|location| {
            location
                .match_keys()
                .iter()
                .any(|candidate| normalize_admin_place_key(candidate) == target)
        })
        .cloned()
}

/// 判断国家名称是否指向中国。
fn is_china_country(country: &str) -> bool {
    matches!(
        normalize_place_key(country).as_str(),
        "中国" | "中华人民共和国" | "china" | "cn" | "prc"
    )
}

/// 为地名追加"市"后缀（如果尚未包含合适的行政区划后缀）。
fn append_city_suffix(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.ends_with('市')
        || trimmed.ends_with("自治州")
        || trimmed.ends_with('盟')
        || trimmed.ends_with("地区")
    {
        return trimmed.to_owned();
    }
    format!("{trimmed}市")
}

/// 标准化地名：去除首尾空格、转为小写、移除空格和逗号。
fn normalize_place_key(input: &str) -> String {
    input
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '\u{3000}', ',', '，'], "")
}

/// 标准化行政区划键：在 `normalize_place_key` 基础上移除"省"、"市"等后缀。
fn normalize_admin_place_key(input: &str) -> String {
    normalize_place_key(input)
        .replace("自治州", "")
        .replace("地区", "")
        .replace(['省', '市', '区', '县', '乡', '镇', '盟'], "")
}

/// 判断候选地点键是否与标准化查询字符串匹配。
fn place_keys_match(candidate: &str, normalized_query: &str) -> bool {
    normalize_place_key(candidate) == normalized_query
        || normalize_admin_place_key(candidate) == normalize_admin_place_key(normalized_query)
}

#[cfg(test)]
mod tests {
    use super::{QWeatherGeoLocation, lookup_city_query, select_location};

    fn location(name: &str, adm1: &str, adm2: &str, rank: &str) -> QWeatherGeoLocation {
        QWeatherGeoLocation {
            name: name.to_owned(),
            id: format!("{adm2}-{name}"),
            lat: "30.25".to_owned(),
            lon: "120.16".to_owned(),
            adm1: adm1.to_owned(),
            adm2: adm2.to_owned(),
            country: "中国".to_owned(),
            tz: "Asia/Shanghai".to_owned(),
            rank: rank.to_owned(),
        }
    }

    fn west_lake_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("西湖区", "重庆市", "重庆", "20"),
            location("西湖区", "浙江省", "杭州", "10"),
        ]
    }

    fn xiaoshan_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("萧山区", "浙江省", "杭州", "10"),
            location("萧山区", "其它省", "其它", "20"),
        ]
    }

    fn jiangbei_chongqing_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("江北区", "重庆市", "重庆", "20"),
            location("江北区", "浙江省", "宁波", "10"),
        ]
    }

    fn hangzhou_exact_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("杭州", "浙江省", "杭州", "11"),
            location("萧山", "浙江省", "杭州", "23"),
            location("桐庐", "浙江省", "杭州", "33"),
        ]
    }

    fn jiangbei_rank_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("重庆", "重庆市", "重庆", "11"),
            location("永川", "重庆市", "重庆", "23"),
            location("北碚", "重庆市", "重庆", "35"),
        ]
    }

    fn west_lake_no_district_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("西湖乡", "台湾省", "苗栗县", "77"),
            location("西湖", "浙江省", "杭州", "35"),
            location("西湖", "江西省", "南昌", "35"),
        ]
    }

    fn no_china_candidate() -> Vec<QWeatherGeoLocation> {
        vec![QWeatherGeoLocation {
            country: "美国".to_owned(),
            ..location("西湖区", "浙江省", "杭州", "10")
        }]
    }

    fn ambiguous_same_rank_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("重名地", "甲省", "甲市", "10"),
            location("重名地", "乙省", "乙市", "10"),
        ]
    }

    /// 合并 8 个 select_location 成功路径测试为表驱动测试。
    /// 每个 case 名称对应原独立测试函数，便于失败定位。
    #[test]
    fn select_location_prefers_best_match() {
        struct Case {
            /// 原测试函数名，失败时用于定位
            name: &'static str,
            query: &'static str,
            candidates: Vec<QWeatherGeoLocation>,
            expected_adm1: &'static str,
            expected_adm2: &'static str,
            expected_name: &'static str,
        }

        let cases = [
            Case {
                name: "select_location_prefers_hangzhou_west_lake_for_short_name",
                query: "西湖",
                candidates: west_lake_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖区",
            },
            Case {
                name: "select_location_prefers_hangzhou_west_lake_for_district_name",
                query: "西湖区",
                candidates: west_lake_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖区",
            },
            Case {
                name: "select_location_prefers_hangzhou_xiaoshan_for_short_name",
                query: "萧山",
                candidates: xiaoshan_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "萧山区",
            },
            Case {
                name: "select_location_prefers_hangzhou_xiaoshan_for_district_name",
                query: "萧山区",
                candidates: xiaoshan_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "萧山区",
            },
            Case {
                name: "select_location_prefers_chongqing_jiangbei",
                query: "江北",
                candidates: jiangbei_chongqing_candidates(),
                expected_adm1: "重庆市",
                expected_adm2: "重庆",
                expected_name: "江北区",
            },
            Case {
                name: "select_location_prefers_exact_city_name_before_rank",
                query: "杭州",
                candidates: hangzhou_exact_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "杭州",
            },
            Case {
                name: "select_location_treats_lower_qweather_rank_as_better",
                query: "江北",
                candidates: jiangbei_rank_candidates(),
                expected_adm1: "重庆市",
                expected_adm2: "重庆",
                expected_name: "重庆",
            },
            Case {
                name: "preference_matches_qweather_locations_without_district_suffix",
                query: "西湖区",
                candidates: west_lake_no_district_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖",
            },
        ];

        for case in &cases {
            let selected = select_location(case.query, case.candidates.clone())
                .unwrap_or_else(|e| panic!("case '{}' failed: unwrap error {:?}", case.name, e));
            assert_eq!(
                selected.adm1, case.expected_adm1,
                "case '{}' failed: adm1 mismatch",
                case.name
            );
            assert_eq!(
                selected.adm2, case.expected_adm2,
                "case '{}' failed: adm2 mismatch",
                case.name
            );
            assert_eq!(
                selected.name, case.expected_name,
                "case '{}' failed: name mismatch",
                case.name
            );
        }
    }

    /// 合并 2 个 select_location not_found 错误路径测试。
    #[test]
    fn select_location_returns_not_found() {
        struct Case {
            name: &'static str,
            query: &'static str,
            candidates: Vec<QWeatherGeoLocation>,
        }

        let cases = [
            Case {
                name: "select_location_returns_not_found_for_no_china_match",
                query: "西湖",
                candidates: no_china_candidate(),
            },
            Case {
                name: "select_location_returns_not_found_for_ambiguous_same_rank",
                query: "重名地",
                candidates: ambiguous_same_rank_candidates(),
            },
        ];

        for case in &cases {
            let err = select_location(case.query, case.candidates.clone()).expect_err(&format!(
                "case '{}' failed: expected Err, got Ok",
                case.name
            ));
            assert_eq!(
                err.code, "not_found",
                "case '{}' failed: expected code 'not_found', got '{}'",
                case.name, err.code
            );
        }
    }

    #[test]
    fn lookup_city_query_uses_chongqing_for_short_jiangbei() {
        assert_eq!(lookup_city_query("江北"), "重庆江北");
        assert_eq!(lookup_city_query("江北区"), "重庆江北");
        assert_eq!(lookup_city_query("宁波江北"), "宁波江北");
    }
}
