use chrono::NaiveDate;

use crate::runtime::{
    respond::{
        common::truncate_chars,
        weather_flow::{
            alert_icon, append_alert_lines, format_alert_detail, format_alert_title,
            format_forecast_day_label, format_weather_reply, parse_weather_command,
            weather_code_label, weather_reference_date,
        },
    },
    tools::weather::{
        AirQualitySummary, CurrentWeather, DailyWeather, WeatherAlert, WeatherLifeIndex,
        WeatherLocation, WeatherOutcome, WeatherSupplement,
    },
};

/// 这些单测覆盖天气回复的内部格式约束，统一放在 `respond/tests` 下管理，
/// 避免 `weather_flow.rs` 同时承载实现和大块测试代码。
#[test]
fn parse_weather_command_accepts_variants() {
    /// 合并多个解析变体为表驱动测试，便于在拆分后继续按 case 名定位失败。
    struct ExpectedCommand {
        action: &'static str,
        argument: &'static str,
        raw_command: &'static str,
    }

    struct Case {
        name: &'static str,
        input: &'static str,
        expected: Option<ExpectedCommand>,
    }

    let cases = [
        Case {
            name: "parse_weather_command_accepts_attached_city",
            input: "/天气杭州",
            expected: Some(ExpectedCommand {
                action: "weather",
                argument: "杭州",
                raw_command: "天气",
            }),
        },
        Case {
            name: "parse_weather_command_accepts_spaced_city",
            input: "/天气 杭州",
            expected: Some(ExpectedCommand {
                action: "weather",
                argument: "杭州",
                raw_command: "天气",
            }),
        },
        Case {
            name: "parse_weather_command_accepts_city_weather_suffix",
            input: "/杭州天气",
            expected: Some(ExpectedCommand {
                action: "weather",
                argument: "杭州",
                raw_command: "天气",
            }),
        },
        Case {
            name: "parse_weather_command_accepts_english_alias",
            input: "/weather Hangzhou",
            expected: Some(ExpectedCommand {
                action: "weather",
                argument: "Hangzhou",
                raw_command: "weather",
            }),
        },
        Case {
            name: "parse_weather_command_ignores_plain_city_weather_suffix",
            input: "杭州天气",
            expected: None,
        },
        Case {
            name: "parse_weather_command_keeps_empty_city_for_usage_reply",
            input: "/天气",
            expected: Some(ExpectedCommand {
                action: "weather",
                argument: "",
                raw_command: "天气",
            }),
        },
    ];

    for case in &cases {
        let result = parse_weather_command(case.input);
        match &case.expected {
            None => assert!(
                result.is_none(),
                "case '{}' failed: expected None, got {:?}",
                case.name,
                result
            ),
            Some(expected) => {
                let command = result.unwrap_or_else(|| {
                    panic!("case '{}' failed: expected Some, got None", case.name)
                });
                assert_eq!(
                    command.action, expected.action,
                    "case '{}' failed: action mismatch",
                    case.name
                );
                assert_eq!(
                    command.argument, expected.argument,
                    "case '{}' failed: argument mismatch",
                    case.name
                );
                assert_eq!(
                    command.raw_command, expected.raw_command,
                    "case '{}' failed: raw_command mismatch",
                    case.name
                );
            }
        }
    }
}

#[test]
fn format_weather_reply_uses_compact_markdown_sections() {
    let reply = format_weather_reply(&WeatherOutcome {
        location: WeatherLocation {
            id: Some("101210101".to_owned()),
            name: "杭州".to_owned(),
            country: Some("中国".to_owned()),
            admin1: Some("浙江".to_owned()),
            admin2: Some("杭州".to_owned()),
            timezone: Some("Asia/Shanghai".to_owned()),
            latitude: 30.29,
            longitude: 120.16,
        },
        current: CurrentWeather {
            time: "2026-06-12T20:15".to_owned(),
            temperature_c: 27.7,
            apparent_temperature_c: Some(28.5),
            weather_code: 104,
            humidity_percent: Some(86),
            precipitation_mm: Some(1.2),
            pressure_hpa: Some(1006),
            wind_direction: Some("东北风".to_owned()),
            wind_scale: Some("3".to_owned()),
            wind_speed_kmh: Some(6.7),
        },
        daily: vec![
            daily("2026-06-12", 104),
            daily("2026-06-13", 306),
            daily("2026-06-14", 305),
        ],
        provider: "mock-weather".to_owned(),
        elapsed_ms: 7,
        forecast_days: 3,
        alerts: WeatherSupplement::available(vec![
            WeatherAlert {
                headline: "杭州市气象台发布大风蓝色预警".to_owned(),
                event_name: Some("大风".to_owned()),
                severity: Some("minor".to_owned()),
                color_code: Some("blue".to_owned()),
                sender_name: Some("杭州市气象台".to_owned()),
                issued_time: Some("2026-06-12T18:00+08:00".to_owned()),
                expire_time: Some("2026-06-13T18:00+08:00".to_owned()),
                description: Some("预计未来24小时阵风较大，请注意户外高空物品安全。".to_owned()),
            },
            WeatherAlert {
                headline: "杭州市气象台发布雷电黄色预警".to_owned(),
                event_name: Some("雷电".to_owned()),
                severity: Some("moderate".to_owned()),
                color_code: Some("yellow".to_owned()),
                sender_name: Some("杭州市气象台".to_owned()),
                issued_time: Some("2026-06-12T19:00+08:00".to_owned()),
                expire_time: Some("2026-06-13T06:00+08:00".to_owned()),
                description: Some("局地可能出现雷电活动，短时风雨较明显。".to_owned()),
            },
            WeatherAlert {
                headline: "第三条预警不应展示".to_owned(),
                event_name: Some("测试".to_owned()),
                severity: None,
                color_code: None,
                sender_name: None,
                issued_time: None,
                expire_time: None,
                description: None,
            },
        ]),
        air_quality: WeatherSupplement::available(AirQualitySummary {
            code: Some("cn-mee".to_owned()),
            name: Some("AQI（CN）".to_owned()),
            aqi_display: "42".to_owned(),
            level: Some("1".to_owned()),
            category: Some("优".to_owned()),
            primary_pollutant: Some("PM2.5".to_owned()),
        }),
        life_indices: WeatherSupplement::available(vec![
            WeatherLifeIndex {
                date: "2026-06-12".to_owned(),
                type_id: "1".to_owned(),
                name: "运动指数".to_owned(),
                level: Some("2".to_owned()),
                category: Some("较适宜".to_owned()),
                text: Some("适合进行适量户外活动。".to_owned()),
            },
            WeatherLifeIndex {
                date: "2026-06-12".to_owned(),
                type_id: "3".to_owned(),
                name: "穿衣指数".to_owned(),
                level: Some("6".to_owned()),
                category: Some("热".to_owned()),
                text: Some("建议短袖。".to_owned()),
            },
            WeatherLifeIndex {
                date: "2026-06-13".to_owned(),
                type_id: "1".to_owned(),
                name: "运动指数".to_owned(),
                level: Some("3".to_owned()),
                category: Some("较不宜".to_owned()),
                text: Some("次日不在摘要中展示。".to_owned()),
            },
        ]),
    });

    assert!(reply.starts_with("# 🌦 杭州天气"));
    assert!(reply.contains("**浙江，中国**"));
    assert!(reply.contains("**当前 20:15｜阴｜27.7°C**"));
    assert!(reply.contains("体感 28.5°C · 湿度 86% · 东北风 3级"));
    assert!(reply.contains("空气质量：**AQI 42（优）** · 首要污染物 PM2.5"));
    assert!(reply.contains("## ⚠️ 预警"));
    assert!(reply.contains("- 🔵 **大风蓝色预警**"));
    assert!(reply.contains("- 🟡 **雷电黄色预警**"));
    assert!(reply.contains("今日 18:00—明日 18:00"));
    assert!(reply.contains("## 📅 未来 3 天"));
    assert!(reply.contains("- **今天 周五**：小雨转阴，21～32.5°C，东风 1-3级"));
    assert!(reply.contains("- **明天 周六**：小雨转阴，21～32.5°C，东风 1-3级"));
    assert!(reply.contains("- **后天 周日**：小雨转阴，21～32.5°C，东风 1-3级"));
    assert!(reply.contains("## 🧭 生活指数"));
    assert!(reply.contains("生活指数"));
    assert!(reply.contains("运动：较适宜｜穿衣：热"));
    assert!(reply.contains("> 数据来源：和风天气"));
    assert!(!reply.contains("气压"));
    assert!(!reply.contains("第三条预警不应展示"));
    assert!(!reply.contains("次日不在摘要中展示"));
    assert!(!reply.contains("\n| "));
    assert!(!reply.contains("\n|-"));
}

#[test]
fn weather_code_label_maps_mixed_rain_snow_range() {
    // 和风天气 404/405/406 同属雨夹雪类，范围模式不能遗漏任一代码。
    for code in [404, 405, 406] {
        assert_eq!(weather_code_label(code), "雨夹雪", "{code}");
    }
}

#[test]
fn append_alert_lines_reports_empty_alerts() {
    let mut lines = Vec::new();

    append_alert_lines(
        &mut lines,
        &WeatherSupplement::<Vec<WeatherAlert>>::empty(Some(true)),
        Some(NaiveDate::from_ymd_opt(2026, 6, 12).unwrap()),
    );

    assert!(lines.is_empty());
}

#[test]
fn format_weather_reply_omits_empty_alert_section_and_missing_fields() {
    let reply = format_weather_reply(&WeatherOutcome {
        location: WeatherLocation {
            id: Some("101210101".to_owned()),
            name: "杭州".to_owned(),
            country: Some("中国".to_owned()),
            admin1: Some("浙江".to_owned()),
            admin2: Some("杭州".to_owned()),
            timezone: Some("Asia/Shanghai".to_owned()),
            latitude: 30.29,
            longitude: 120.16,
        },
        current: CurrentWeather {
            time: "2026-06-12T20:15".to_owned(),
            temperature_c: 27.0,
            apparent_temperature_c: None,
            weather_code: 104,
            humidity_percent: None,
            precipitation_mm: None,
            pressure_hpa: None,
            wind_direction: None,
            wind_scale: None,
            wind_speed_kmh: None,
        },
        daily: vec![daily("2026-06-12", 104), daily("2026-06-13", 306)],
        provider: "mock-weather".to_owned(),
        elapsed_ms: 7,
        forecast_days: 3,
        alerts: WeatherSupplement::empty(Some(true)),
        air_quality: WeatherSupplement::empty(Some(true)),
        life_indices: WeatherSupplement::available(vec![
            WeatherLifeIndex {
                date: "2026-06-12".to_owned(),
                type_id: "1".to_owned(),
                name: "运动指数".to_owned(),
                level: None,
                category: Some("较适宜".to_owned()),
                text: None,
            },
            WeatherLifeIndex {
                date: "2026-06-12".to_owned(),
                type_id: "3".to_owned(),
                name: "穿衣指数".to_owned(),
                level: None,
                category: None,
                text: None,
            },
        ]),
    });

    assert!(reply.contains("**当前 20:15｜阴｜27°C**"));
    assert!(!reply.contains("## ⚠️ 预警"));
    assert!(!reply.contains("空气质量："));
    assert!(reply.contains("## 📅 未来 2 天"));
    assert!(reply.contains("- **今天 周五**"));
    assert!(reply.contains("- **明天 周六**"));
    assert!(!reply.contains("后天"));
    assert!(reply.contains("运动：较适宜"));
    assert!(!reply.contains("穿衣："));
    assert!(!reply.contains("None"));
    assert!(!reply.contains("null"));
    assert!(!reply.contains("··"));
    assert!(!reply.contains("｜｜"));
}

#[test]
fn format_alert_summary_truncates_long_chinese_body_and_keeps_unknown_color_icon() {
    let alert = WeatherAlert {
        headline: "北京市气象台发布台风预警".to_owned(),
        event_name: Some("台风".to_owned()),
        severity: None,
        color_code: Some("purple".to_owned()),
        sender_name: Some("北京市气象台".to_owned()),
        issued_time: Some("2026-06-12T18:00+08:00".to_owned()),
        expire_time: Some("2026-06-13T06:00+08:00".to_owned()),
        description: Some("北京市气象台发布台风预警：预计今天夜间到明天上午，朝阳区、通州区和顺义区将出现强风和明显降雨，请及时加固临时搭建物，远离广告牌、树木和临时围挡，并注意低洼路段积水风险，同时防范临时工棚、简易板房和高空悬挂物受损，山区道路注意短时积水、树枝坠落和能见度下降风险。".to_owned()),
    };

    let detail =
        format_alert_detail(&alert, Some(NaiveDate::from_ymd_opt(2026, 6, 12).unwrap())).unwrap();

    assert_eq!(alert_icon(alert.color_code.as_deref()), "⚠️");
    assert_eq!(format_alert_title(&alert), "台风预警");
    assert!(detail.contains("今日 18:00—明日 06:00"));
    assert!(detail.contains("预计今天夜间到明天上午"));
    assert!(detail.ends_with('…'));
    assert!(!detail.contains("北京市气象台发布台风预警："));
}

#[test]
fn truncate_chars_preserves_utf8_for_weather_text() {
    assert_eq!(truncate_chars("中文天气预警说明", 6), "中文天气预…");
}

#[test]
fn forecast_day_label_uses_local_timezone_instead_of_array_index() {
    let reference = weather_reference_date(&WeatherOutcome {
        location: WeatherLocation {
            id: None,
            name: "测试".to_owned(),
            country: None,
            admin1: None,
            admin2: None,
            timezone: Some("Asia/Shanghai".to_owned()),
            latitude: 0.0,
            longitude: 0.0,
        },
        current: CurrentWeather {
            time: "2026-06-12T23:30:00+00:00".to_owned(),
            temperature_c: 30.0,
            apparent_temperature_c: None,
            weather_code: 100,
            humidity_percent: None,
            precipitation_mm: None,
            pressure_hpa: None,
            wind_direction: None,
            wind_scale: None,
            wind_speed_kmh: None,
        },
        daily: vec![
            daily("2026-06-13", 100),
            daily("2026-06-14", 100),
            daily("2026-06-15", 100),
        ],
        provider: "mock".to_owned(),
        elapsed_ms: 1,
        forecast_days: 3,
        alerts: WeatherSupplement::default(),
        air_quality: WeatherSupplement::default(),
        life_indices: WeatherSupplement::default(),
    });

    assert_eq!(
        format_forecast_day_label("2026-06-13", reference),
        "今天 周六"
    );
    assert_eq!(
        format_forecast_day_label("2026-06-14", reference),
        "明天 周日"
    );
    assert_eq!(
        format_forecast_day_label("2026-06-15", reference),
        "后天 周一"
    );
}

fn daily(date: &str, weather_code: u16) -> DailyWeather {
    DailyWeather {
        date: date.to_owned(),
        weather_code,
        weather_day: Some("小雨".to_owned()),
        weather_night: Some("阴".to_owned()),
        temperature_max_c: 32.5,
        temperature_min_c: 21.0,
        precipitation_probability_max: Some(69),
        precipitation_mm: Some(2.4),
        humidity_percent: Some(91),
        wind_direction_day: Some("东风".to_owned()),
        wind_scale_day: Some("1-3".to_owned()),
    }
}
