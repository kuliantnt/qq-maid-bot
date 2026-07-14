use super::*;

pub(crate) fn write_prompt_set(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    for file_name in crate::runtime::prompt::PROMPT_FILES {
        fs::write(dir.join(file_name), format!("{file_name} content")).unwrap();
    }
}

fn mock_revision_input(prompt: &str) -> Option<Value> {
    let (_, json_text) = prompt.split_once("修订输入 JSON：")?;
    serde_json::from_str(json_text.trim()).ok()
}

fn mock_current_todo_draft(prompt: &str) -> Option<Value> {
    mock_revision_input(prompt)?.get("current_draft").cloned()
}

fn mock_revision_user_input(prompt: &str) -> String {
    mock_revision_input(prompt)
        .and_then(|value| {
            value
                .get("user_input")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

pub(crate) fn mock_memory_draft_reply(prompt: &str, operation: Option<&str>) -> String {
    if prompt.contains("invalid-memory-create") {
        return "不是 JSON".to_owned();
    }
    if prompt.contains("null-memory-create") {
        return json!({ "content": null }).to_string();
    }
    if prompt.contains("empty-memory-create") {
        return json!({ "content": "" }).to_string();
    }
    if prompt.contains("fenced-memory-create") {
        return format!(
            "```json\n{}\n```",
            json!({ "content": "技术方案回复时先给结论和风险" })
        );
    }
    if prompt.contains("先给结论和风险") {
        return json!({ "content": "技术方案回复时先给结论和风险" }).to_string();
    }
    if prompt.contains("回复技术方案时，请先给结论") {
        return json!({ "content": "技术方案回复时请先给结论" }).to_string();
    }
    if matches!(operation, Some("create")) {
        return json!({ "content": null }).to_string();
    }
    format!("回复：{prompt}")
}

pub(crate) fn mock_todo_parse_reply(prompt: &str) -> String {
    if prompt.contains("invalid-json") {
        return "不是 JSON".to_owned();
    }
    // 火车行程识别分支：操作为 train_add 时，根据用户原文判断是否为火车行程。
    if prompt.contains("操作：train_add") {
        return mock_train_todo_parse_reply(prompt);
    }
    if prompt.contains("操作：add_revise") || prompt.contains("操作：edit_revise") {
        return mock_todo_revise_reply(prompt);
    }
    if prompt.contains("操作：edit_patch") {
        if prompt.contains("时间需要改成这个月底之前完成") {
            return json!({
                "due_date": "2026-06-30",
                "due_at": null,
                "reminder_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("理解错了，实际上标题还是示例项目审查，内容是之前的标题")
        {
            return json!({
                "title": "示例项目审查",
                "detail": "之前的标题"
            })
            .to_string();
        }
        if prompt.contains("内容改成 示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成")
        {
            return json!({
                "detail": "示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成",
                "due_date": "2026-06-30",
                "due_at": null,
                "reminder_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("先做一份给负责人看看") {
            return json!({
                "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
                "due_date": "2026-06-30",
                "due_at": null,
                "reminder_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("月底前需要和负责人理一下") {
            return json!({
                "title": "示例材料需要重新做",
                "detail": "需要和负责人理一下",
                "due_date": "2026-06-30",
                "due_at": null,
                "reminder_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("示例系统维保 - 2026 做完了") {
            return json!({
                "title": "示例系统维保 - 2026"
            })
            .to_string();
        }
        if prompt.contains("改成明天检查服务") || prompt.contains("明天检查服务") {
            return json!({
                "title": "检查服务"
            })
            .to_string();
        }
        return json!({}).to_string();
    }
    if prompt.contains("无时间") || prompt.contains("买牛奶") {
        return json!({
            "title": "买牛奶",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("三天后检查日志") {
        return json!({
            "title": "检查日志",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("G34 版本 bug 明天修") {
        return json!({
            "title": "G34 版本 bug",
            "detail": "明天修",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("K20-回归问题 今天跟进") {
        return json!({
            "title": "K20-回归问题",
            "detail": "今天跟进",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("train-not-train") {
        return json!({
            "title": "会议室到机房检查",
            "detail": "普通待办，不是火车行程",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("2026年6月15日提交报告") {
        return json!({
            "title": "提交报告",
            "detail": null,
            "due_date": "2026-06-15",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "date"
        })
        .to_string();
    }
    if prompt.contains("月底复盘") {
        return json!({
            "title": "复盘",
            "detail": null,
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("先做一份给负责人看看") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("月底前需要和负责人理一下") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "需要和负责人理一下",
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("示例系统维保 - 2026 做完了") {
        return json!({
            "title": "示例系统维保 - 2026",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("检查服务器") || prompt.contains("检查 server") {
        return json!({
            "title": "检查服务器",
            "detail": "server",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("查交通") || prompt.contains("交通") {
        return json!({
            "title": "查交通",
            "detail": "交通",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("检查数据库") || prompt.contains("数据库") {
        return json!({
            "title": "检查数据库",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("改成明天检查服务") || prompt.contains("明天检查服务") {
        return json!({
            "title": "检查服务",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    json!({
        "title": "待办",
        "detail": null,
        "due_date": null,
        "due_at": null,
        "reminder_at": null,
        "time_precision": "none"
    })
    .to_string()
}

fn mock_todo_revise_reply(prompt: &str) -> String {
    let user_input = mock_revision_user_input(prompt);
    if user_input.contains("标题改成准备材料")
        || user_input.contains("详情补充先发负责人")
        || user_input.contains("时间这个月底前")
    {
        return json!({
            "title": "准备材料",
            "detail": "先发负责人",
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("时间需要改成这个月底之前完成") {
        return json!({
            "title": "示例系统维保 - 2026",
            "detail": null,
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("理解错了，实际上标题还是示例项目审查，内容是之前的标题")
    {
        return json!({
            "title": "示例项目审查",
            "detail": "之前的标题",
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if user_input.contains("内容改成 示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成")
    {
        return json!({
            "title": "示例项目审查",
            "detail": "示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成",
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("先做一份给负责人看看") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
            "due_date": "2026-06-30",
            "due_at": null,
            "reminder_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("改成明天检查服务") || user_input.contains("明天检查服务")
    {
        return json!({
            "title": "检查服务",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "reminder_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    mock_current_todo_draft(prompt)
        .unwrap_or_else(|| {
            json!({
                "title": "待办",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": "none"
            })
        })
        .to_string()
}

/// 火车行程识别 mock：根据用户原文判断是否输出 kind=train。
///
/// 测试用 mock 只覆盖关键字段识别；真实时刻由 MockTrainExecutor 提供。
fn mock_train_todo_parse_reply(prompt: &str) -> String {
    // 从 prompt 中提取用户原文（"用户原文：" 之后的部分）。
    let user_text = prompt
        .split_once("用户原文：")
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    if user_text.contains("train-not-train")
        || user_text.contains("G34 版本 bug 明天修")
        || user_text.contains("K20-回归问题 今天跟进")
    {
        return json!({
            "kind": "todo",
            "title": "普通待办"
        })
        .to_string();
    }
    // 非 JSON 输出（测试 LLM 回空回退普通 Todo）
    if user_text.contains("train-invalid-json") {
        return "不是 JSON".to_owned();
    }
    // 自然语言输入优先：明天坐 G34 从杭州东去北京南
    if user_text.contains("坐 G34") || user_text.contains("坐G34") {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("G34") && user_text.matches("杭州东").count() >= 2 {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "杭州东",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("G34") && user_text.matches("南京南").count() >= 2 {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "南京南",
            "to_station": "南京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 结构化输入：/todo add G34 杭州东 北京南 明天 05车12A 8站台
    if user_text.contains("G34") && user_text.contains("杭州东") && user_text.contains("北京南")
    {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": "05车12A",
            "platform": "8站台",
            "note": null
        })
        .to_string();
    }
    if user_text.contains("1461") && user_text.contains("北京") && user_text.contains("上海") {
        return json!({
            "kind": "train",
            "train_code": "1461",
            "from_station": "北京",
            "to_station": "上海",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 跨日行程：Z281 杭州 西安
    if user_text.contains("Z281") {
        return json!({
            "kind": "train",
            "train_code": "Z281",
            "from_station": "杭州",
            "to_station": "西安",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("K20") {
        return json!({
            "kind": "train",
            "train_code": "K20",
            "from_station": "中途站",
            "to_station": "终到站",
            "travel_date": "2026-06-25",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 缺少日期的火车行程
    if user_text.contains("G99") {
        return json!({
            "kind": "train",
            "train_code": "G99",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": null,
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 站点不匹配的火车行程
    if user_text.contains("G50") {
        return json!({
            "kind": "train",
            "train_code": "G50",
            "from_station": "上海",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 站点顺序错误的火车行程
    if user_text.contains("G88") {
        return json!({
            "kind": "train",
            "train_code": "G88",
            "from_station": "北京南",
            "to_station": "杭州东",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 普通 Todo 输入：回退普通待办 JSON
    json!({
        "title": "买牛奶",
        "detail": null,
        "due_date": null,
        "due_at": null,
        "reminder_at": null,
        "time_precision": "none"
    })
    .to_string()
}
