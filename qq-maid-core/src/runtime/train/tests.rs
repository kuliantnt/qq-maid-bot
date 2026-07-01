use super::*;

fn sample_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "G34".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "杭州东".to_owned(),
        end_station: "北京南".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "杭州东".to_owned(),
                arrive_time: None,
                departure_time: Some("07:05".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "G34".to_owned(),
            },
            TrainStop {
                station_no: 2,
                station_name: "南京南".to_owned(),
                arrive_time: Some("09:20".to_owned()),
                departure_time: Some("09:22".to_owned()),
                stopover_minutes: Some(2),
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "G34".to_owned(),
            },
            TrainStop {
                station_no: 3,
                station_name: "北京南".to_owned(),
                arrive_time: Some("11:40".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "G34".to_owned(),
            },
        ],
        full_train_code: None,
        corporation: None,
        train_style: None,
        dept_train: None,
    }
}

fn cross_day_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "Z281".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "杭州".to_owned(),
        end_station: "西安".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "杭州".to_owned(),
                arrive_time: None,
                departure_time: Some("23:40".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "Z281".to_owned(),
            },
            TrainStop {
                station_no: 5,
                station_name: "西安".to_owned(),
                arrive_time: Some("08:15".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 1,
                day_difference_reliable: true,
                station_train_code: "Z281".to_owned(),
            },
        ],
        full_train_code: None,
        corporation: None,
        train_style: None,
        dept_train: None,
    }
}

#[test]
fn normalize_station_name_strips_trailing_suffix() {
    assert_eq!(normalize_station_name("杭州东站"), "杭州东");
    assert_eq!(normalize_station_name("杭州东"), "杭州东");
    assert_eq!(normalize_station_name(" 杭州 "), "杭州");
}

#[test]
fn find_stop_matches_with_or_without_suffix() {
    let schedule = sample_schedule();
    assert!(find_stop_by_name(&schedule, "杭州东站").is_some());
    assert!(find_stop_by_name(&schedule, "杭州东").is_some());
    assert!(find_stop_by_name(&schedule, "上海").is_none());
}

#[test]
fn validate_trip_returns_times_for_same_day() {
    let schedule = sample_schedule();
    let trip = validate_train_trip(&schedule, "杭州东", "北京南").unwrap();
    assert_eq!(
        trip.departure_at,
        NaiveDate::from_ymd_opt(2026, 6, 24)
            .unwrap()
            .and_hms_opt(7, 5, 0)
            .unwrap()
    );
    assert_eq!(
        trip.arrive_at,
        NaiveDate::from_ymd_opt(2026, 6, 24)
            .unwrap()
            .and_hms_opt(11, 40, 0)
            .unwrap()
    );
}

#[test]
fn validate_trip_returns_times_for_midway_same_day_boarding() {
    let schedule = sample_schedule();
    let trip = validate_train_trip(&schedule, "南京南", "北京南").unwrap();
    assert_eq!(
        trip.departure_at,
        NaiveDate::from_ymd_opt(2026, 6, 24)
            .unwrap()
            .and_hms_opt(9, 22, 0)
            .unwrap()
    );
    assert_eq!(
        trip.arrive_at,
        NaiveDate::from_ymd_opt(2026, 6, 24)
            .unwrap()
            .and_hms_opt(11, 40, 0)
            .unwrap()
    );
}

#[test]
fn validate_trip_handles_cross_day_arrival() {
    let schedule = cross_day_schedule();
    let trip = validate_train_trip(&schedule, "杭州", "西安").unwrap();
    assert_eq!(
        trip.departure_at,
        NaiveDate::from_ymd_opt(2026, 6, 24)
            .unwrap()
            .and_hms_opt(23, 40, 0)
            .unwrap()
    );
    assert_eq!(
        trip.arrive_at,
        NaiveDate::from_ymd_opt(2026, 6, 25)
            .unwrap()
            .and_hms_opt(8, 15, 0)
            .unwrap()
    );
}

#[test]
fn validate_trip_rejects_missing_from_station() {
    let schedule = sample_schedule();
    assert_eq!(
        validate_train_trip(&schedule, "上海", "北京南").unwrap_err(),
        TrainTripError::FromStationNotFound {
            station: "上海".to_owned()
        }
    );
}

#[test]
fn validate_trip_rejects_missing_to_station() {
    let schedule = sample_schedule();
    assert_eq!(
        validate_train_trip(&schedule, "杭州东", "上海").unwrap_err(),
        TrainTripError::ToStationNotFound {
            station: "上海".to_owned()
        }
    );
}

#[test]
fn validate_trip_rejects_reversed_order() {
    let schedule = sample_schedule();
    assert_eq!(
        validate_train_trip(&schedule, "北京南", "杭州东").unwrap_err(),
        TrainTripError::StationOrderReversed {
            from_station: "北京南".to_owned(),
            to_station: "杭州东".to_owned()
        }
    );
}

#[test]
fn validate_trip_rejects_same_station() {
    let schedule = sample_schedule();
    assert_eq!(
        validate_train_trip(&schedule, "南京南", "南京南").unwrap_err(),
        TrainTripError::SameStation {
            station: "南京南".to_owned()
        }
    );
}

#[test]
fn parse_train_time_accepts_hhmm_and_hhmmss() {
    assert_eq!(
        parse_train_time("07:05").unwrap(),
        NaiveTime::from_hms_opt(7, 5, 0).unwrap()
    );
    assert_eq!(
        parse_train_time("07:05:30").unwrap(),
        NaiveTime::from_hms_opt(7, 5, 30).unwrap()
    );
}

#[test]
fn parse_train_time_rejects_empty_placeholder_and_invalid_values() {
    assert!(parse_train_time("").is_none());
    assert!(parse_train_time("--").is_none());
    assert!(parse_train_time("--:--").is_none());
    assert!(parse_train_time("25:00").is_none());
    assert!(parse_train_time("07:65").is_none());
    assert!(parse_train_time("7:05").is_none());
}

#[test]
fn validate_trip_rejects_invalid_time_without_midnight_fallback() {
    let mut schedule = sample_schedule();
    schedule.stops[0].departure_time = Some("25:99".to_owned());
    assert_eq!(
        validate_train_trip(&schedule, "杭州东", "北京南").unwrap_err(),
        TrainTripError::InvalidTime {
            station: "杭州东".to_owned(),
            field: "startTime",
            value: "25:99".to_owned()
        }
    );
}

#[test]
fn validate_trip_rejects_unreliable_day_difference_without_same_day_fallback() {
    let mut schedule = cross_day_schedule();
    schedule.stops[1].day_difference = 0;
    schedule.stops[1].day_difference_reliable = false;
    assert_eq!(
        validate_train_trip(&schedule, "杭州", "西安").unwrap_err(),
        TrainTripError::InvalidDayDifference {
            station: "西安".to_owned(),
            day_difference: "缺失或非法".to_owned()
        }
    );
}

#[test]
fn validate_trip_rejects_arrival_before_departure() {
    let mut schedule = sample_schedule();
    schedule.stops[0].departure_time = Some("12:00".to_owned());
    schedule.stops[2].arrive_time = Some("11:00".to_owned());
    assert!(matches!(
        validate_train_trip(&schedule, "杭州东", "北京南").unwrap_err(),
        TrainTripError::ArrivalBeforeDeparture { .. }
    ));
}

#[test]
fn train_api_response_falls_back_for_missing_station_fields() {
    let schedule = TrainApiResponse {
        status: true,
        error_msg: String::new(),
        data: Some(TrainApiData {
            train_detail: Some(TrainApiDetail {
                train_code: Some("1461".to_owned()),
                stop_time: vec![
                    TrainApiStop {
                        station_no: None,
                        station_name: Some("北京".to_owned()),
                        arrive_time: Some("----".to_owned()),
                        start_time: Some("16:00".to_owned()),
                        stopover_time: None,
                        day_difference: None,
                        station_train_code: None,
                        jiaolu_corporation_code: None,
                        jiaolu_train_style: None,
                        jiaolu_dept_train: None,
                    },
                    TrainApiStop {
                        station_no: Some(String::new()),
                        station_name: Some("上海".to_owned()),
                        arrive_time: Some("08:10".to_owned()),
                        start_time: Some("----".to_owned()),
                        stopover_time: Some("5".to_owned()),
                        day_difference: Some(String::new()),
                        station_train_code: Some("1461".to_owned()),
                        jiaolu_corporation_code: None,
                        jiaolu_train_style: None,
                        jiaolu_dept_train: None,
                    },
                ],
                station_train_code_all: None,
            }),
        }),
    }
    .into_schedule(TrainScheduleRequest {
        train_code: "1461".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
    })
    .unwrap();

    assert_eq!(schedule.stops[0].station_no, 1);
    assert_eq!(schedule.stops[0].day_difference, 0);
    assert!(!schedule.stops[0].day_difference_reliable);
    assert_eq!(schedule.stops[1].station_no, 2);
    assert_eq!(schedule.stops[1].day_difference, 0);
    assert!(!schedule.stops[1].day_difference_reliable);
}

#[test]
fn train_api_response_falls_back_for_invalid_station_fields() {
    let schedule = TrainApiResponse {
        status: true,
        error_msg: String::new(),
        data: Some(TrainApiData {
            train_detail: Some(TrainApiDetail {
                train_code: Some("1461".to_owned()),
                stop_time: vec![TrainApiStop {
                    station_no: Some("A01".to_owned()),
                    station_name: Some("蚌埠".to_owned()),
                    arrive_time: Some("00:47".to_owned()),
                    start_time: Some("00:51".to_owned()),
                    stopover_time: Some("4".to_owned()),
                    day_difference: Some("oops".to_owned()),
                    station_train_code: Some("1461".to_owned()),
                    jiaolu_corporation_code: None,
                    jiaolu_train_style: None,
                    jiaolu_dept_train: None,
                }],
                station_train_code_all: None,
            }),
        }),
    }
    .into_schedule(TrainScheduleRequest {
        train_code: "1461".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
    })
    .unwrap();

    assert_eq!(schedule.stops[0].station_no, 1);
    assert_eq!(schedule.stops[0].day_difference, 0);
    assert!(!schedule.stops[0].day_difference_reliable);
}

#[test]
fn train_api_response_accepts_numeric_station_fields() {
    let response = serde_json::from_value::<TrainApiResponse>(serde_json::json!({
        "status": true,
        "errorMsg": "",
        "data": {
            "trainDetail": {
                "trainCode": "1461",
                "stopTime": [
                    {
                        "stationNo": 16,
                        "stationName": "蚌埠",
                        "arriveTime": "00:47",
                        "startTime": "00:51",
                        "stopover_time": "4",
                        "dayDifference": 1,
                        "stationTrainCode": "1461"
                    }
                ]
            }
        }
    }))
    .unwrap();

    let schedule = response
        .into_schedule(TrainScheduleRequest {
            train_code: "1461".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        })
        .unwrap();
    assert_eq!(schedule.stops[0].station_no, 16);
    assert_eq!(schedule.stops[0].day_difference, 1);
    assert!(schedule.stops[0].day_difference_reliable);
}

#[test]
fn train_api_response_parses_optional_train_detail_fields() {
    // 12306 可选字段：`stationTrainCodeAll` 位于 `trainDetail` 顶层；
    // `jiaolu_corporation_code`、`jiaolu_train_style`、`jiaolu_dept_train`
    // 位于每个 `stopTime` 站点内，同一趟车各站值一致，取首站即可。
    // 存在且非空时应解析到 TrainSchedule 对应字段。
    let response = serde_json::from_value::<TrainApiResponse>(serde_json::json!({
        "status": true,
        "errorMsg": "",
        "data": {
            "trainDetail": {
                "trainCode": "D3233",
                "stationTrainCodeAll": "D3233/D3234",
                "stopTime": [
                    {
                        "stationNo": 1,
                        "stationName": "杭州东",
                        "arriveTime": "----",
                        "startTime": "14:32",
                        "stopover_time": "0",
                        "dayDifference": 0,
                        "stationTrainCode": "D3233",
                        "jiaolu_corporation_code": "南昌客运段",
                        "jiaolu_train_style": "CRH2A",
                        "jiaolu_dept_train": "南昌车辆段"
                    }
                ]
            }
        }
    }))
    .unwrap();

    let schedule = response
        .into_schedule(TrainScheduleRequest {
            train_code: "D3233".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        })
        .unwrap();
    assert_eq!(schedule.full_train_code.as_deref(), Some("D3233/D3234"));
    assert_eq!(schedule.corporation.as_deref(), Some("南昌客运段"));
    assert_eq!(schedule.train_style.as_deref(), Some("CRH2A"));
    assert_eq!(schedule.dept_train.as_deref(), Some("南昌车辆段"));
}

#[test]
fn train_api_response_omits_missing_optional_train_detail_fields() {
    // 12306 未返回可选字段时，TrainSchedule 对应字段应为 None，
    // 不推测、不补造。
    let response = serde_json::from_value::<TrainApiResponse>(serde_json::json!({
        "status": true,
        "errorMsg": "",
        "data": {
            "trainDetail": {
                "trainCode": "G1",
                "stopTime": [
                    {
                        "stationNo": 1,
                        "stationName": "北京南",
                        "arriveTime": "----",
                        "startTime": "06:30",
                        "stopover_time": "0",
                        "dayDifference": 0,
                        "stationTrainCode": "G1"
                    }
                ]
            }
        }
    }))
    .unwrap();

    let schedule = response
        .into_schedule(TrainScheduleRequest {
            train_code: "G1".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        })
        .unwrap();
    assert!(schedule.full_train_code.is_none());
    assert!(schedule.corporation.is_none());
    assert!(schedule.train_style.is_none());
    assert!(schedule.dept_train.is_none());
}

#[test]
fn train_api_response_treats_empty_optional_fields_as_none() {
    // 12306 返回了字段但值为空串时，应归一化为 None。
    let response = serde_json::from_value::<TrainApiResponse>(serde_json::json!({
        "status": true,
        "errorMsg": "",
        "data": {
            "trainDetail": {
                "trainCode": "G1",
                "stationTrainCodeAll": "   ",
                "stopTime": [
                    {
                        "stationNo": 1,
                        "stationName": "北京南",
                        "arriveTime": "----",
                        "startTime": "06:30",
                        "stopover_time": "0",
                        "dayDifference": 0,
                        "stationTrainCode": "G1",
                        "jiaolu_corporation_code": "",
                        "jiaolu_train_style": "",
                        "jiaolu_dept_train": ""
                    }
                ]
            }
        }
    }))
    .unwrap();

    let schedule = response
        .into_schedule(TrainScheduleRequest {
            train_code: "G1".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        })
        .unwrap();
    assert!(schedule.full_train_code.is_none());
    assert!(schedule.corporation.is_none());
    assert!(schedule.train_style.is_none());
    assert!(schedule.dept_train.is_none());
}

#[test]
fn train_api_response_parses_real_g2_optional_fields() {
    // 用 12306 真实返回的 G2 数据验证字段位置解析正确：
    // - `stationTrainCodeAll` 在 trainDetail 顶层；
    // - `jiaolu_corporation_code`、`jiaolu_train_style`、`jiaolu_dept_train`
    //   在 stopTime 站点内，取首站值。
    let response = serde_json::from_value::<TrainApiResponse>(serde_json::json!({
        "status": true,
        "errorMsg": "",
        "data": {
            "trainDetail": {
                "trainCode": "G2",
                "stationTrainCodeAll": "G2",
                "stopTime": [
                    {
                        "stationNo": "01",
                        "stationName": "上海虹桥",
                        "arriveTime": "0643",
                        "startTime": "0643",
                        "stopover_time": "0",
                        "dayDifference": "0",
                        "stationTrainCode": "G2",
                        "jiaolu_corporation_code": "天津客运段",
                        "jiaolu_train_style": "CR400BF-Z",
                        "jiaolu_dept_train": "北京动车段"
                    }
                ]
            }
        }
    }))
    .unwrap();

    let schedule = response
        .into_schedule(TrainScheduleRequest {
            train_code: "G2".to_owned(),
            travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        })
        .unwrap();
    assert_eq!(schedule.train_code, "G2");
    assert_eq!(schedule.full_train_code.as_deref(), Some("G2"));
    assert_eq!(schedule.corporation.as_deref(), Some("天津客运段"));
    assert_eq!(schedule.train_style.as_deref(), Some("CR400BF-Z"));
    assert_eq!(schedule.dept_train.as_deref(), Some("北京动车段"));
    // 首站到发时间应被规范化为 HH:MM。
    assert_eq!(schedule.stops[0].departure_time.as_deref(), Some("06:43"));
}
