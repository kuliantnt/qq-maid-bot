//! `/help` 分层帮助文案。
//!
//! 模块命令只在这里维护一份：完整帮助复用精简命令列表，模块帮助再补充行为说明，
//! 避免 `/help all` 与 `/help <模块>` 随功能演进后互相矛盾。

use super::{
    command_render::CommandRender, common::CommandBody, markdown_strip::strip_markdown_for_chat,
};

struct HelpModule {
    key: &'static str,
    aliases: &'static [&'static str],
    title: &'static str,
    summary: &'static str,
    commands: &'static [&'static str],
    notes: &'static [&'static str],
}

const HELP_MODULES: &[HelpModule] = &[
    HelpModule {
        key: "chat",
        aliases: &["对话", "聊天"],
        title: "💬 对话",
        summary: "直接发送消息即可聊天；普通聊天不会自动写入长期记忆。",
        commands: &["- 直接发送消息：进入当前会话的聊天流程"],
        notes: &["- 需要保存长期信息时，请明确使用 `/memory 内容`。"],
    },
    HelpModule {
        key: "todo",
        aliases: &["待办", "任务"],
        title: "✅ 待办",
        summary: "管理当前用户和当前私聊或群聊范围内的待办。中文别名：`/待办`、`/任务`。",
        commands: &[
            "- `/todo`：查看未完成待办",
            "- `/todo all`：查看全部待办",
            "- `/todo search 关键词`：搜索未完成待办",
            "- `/todo done`、`/todo undo`：查看已完成待办",
            "- 写操作请直接用自然语言，例如：`帮我新增待办：明天检查日志`、`完成第一条待办`、`删除已完成待办`。",
        ],
        notes: &[
            "- Todo 写操作已统一走模型 Tool 调用；slash 入口只保留查询能力，避免同一操作存在两套执行链路。",
            "- 新增、修改、完成和恢复会直接执行；取消未完成待办和永久删除已完成/已取消待办会先进入待确认状态。",
            "- 火车行程不再由 `/todo add` 自动校验；需要查时刻请使用 `/火车 车次 [日期]`。",
        ],
    },
    HelpModule {
        key: "rss",
        aliases: &["订阅"],
        title: "📰 RSS / Atom",
        summary: "订阅源会绑定到添加时的当前私聊或群聊。中文别名：`/订阅`。",
        commands: &[
            "- `/rss`：查看当前聊天目标的订阅",
            "- `/rss add RSS地址 [名称]`：添加订阅，可选自定义名称",
            "- `/rss delete 编号或订阅ID`：删除订阅",
            "- `/rss test RSS地址`：测试抓取和解析，不创建订阅",
        ],
        notes: &[
            "- 示例：`/rss add https://example.com/feed.xml 示例订阅`",
            "- 同时支持 RSS 和 Atom。首次添加会把已有条目标记为已读，不推送历史文章。",
            "- 之后按系统配置周期检查；新文章或实际状态更新会自动推送，同一版本不会重复推送。",
            "- 外语标题和摘要会尽力翻译为简体中文；翻译失败时回退到原文。",
            "- 常见错误包括地址缺失或无效、抓取失败、内容无法解析，以及删除目标不存在。",
        ],
    },
    HelpModule {
        key: "weather",
        aliases: &["天气"],
        title: "🌤 天气",
        summary: "查询城市当前天气和预报。",
        commands: &[
            "- `/天气 杭州`、`/天气杭州`：查询指定城市",
            "- `/杭州天气`、`/weather 杭州`：等价写法",
        ],
        notes: &["- 城市为空、无法识别或天气服务未配置时，会返回明确提示。"],
    },
    HelpModule {
        key: "search",
        aliases: &["查询", "搜索", "火车"],
        title: "🔎 联网查询",
        summary: "提供联网搜索和列车时刻查询两类查询命令。",
        commands: &[
            "- `/查 问题`：联网查询；别名 `/查询`、`/search`",
            "- `/火车 G1 [日期]`：查询指定车次的经停时刻；日期默认今天",
        ],
        notes: &[
            "- 中文联网查询支持紧凑写法，例如 `/查今天的 Rust 新闻`。",
            "- `/火车` 当前只查询时刻表，支持 今天、明天、后天、YYYY-MM-DD、YYYY年M月D日 或 M月D日。",
        ],
    },
    HelpModule {
        key: "translation",
        aliases: &["translate", "翻译"],
        title: "🌐 翻译",
        summary: "翻译文本，不读取普通聊天历史。",
        commands: &[
            "- `/翻译 文本`：默认翻译为简体中文",
            "- `/翻译日语 文本`、`/翻译成英语 文本`：指定目标语言",
        ],
        notes: &["- 翻译依赖已配置的模型服务；未配置或超时时会返回明确提示。"],
    },
    HelpModule {
        key: "memory",
        aliases: &["记忆"],
        title: "🧠 长期记忆",
        summary: "长期记忆只由明确指令创建，并在用户确认后写入。中文别名：`/记忆`、`/记`。",
        commands: &[
            "- `/memory`、`/memory list [关键词]`：查看或搜索记忆",
            "- `/memory 内容`：创建待确认记忆草稿",
            "- `/memory show 序号`：查看记忆详情",
            "- `/memory edit 序号 新内容`：修改记忆",
            "- `/memory delete 序号`：删除记忆",
        ],
        notes: &["- 普通聊天不会自动写长期记忆；管理操作优先使用最近列表中的序号。"],
    },
    HelpModule {
        key: "session",
        aliases: &["会话"],
        title: "🗂 会话",
        summary: "管理当前聊天范围内的对话上下文。",
        commands: &[
            "- `/new [标题]`：开启新会话",
            "- `/rename [标题]`：重命名；无标题时尝试自动生成",
            "- `/resume [编号]`：查看或恢复历史会话；中文别名 `/恢复`",
            "- `/clear`：清空当前上下文",
            "- `/state`：查看当前会话状态",
            "- `/compact`：压缩当前长对话",
            "- `/list`：已弃用的会话列表兼容别名，推荐 `/resume`",
        ],
        notes: &["- 会话按当前私聊或群聊范围隔离；清空上下文不会删除旧会话档案。"],
    },
    HelpModule {
        key: "status",
        aliases: &["状态", "诊断"],
        title: "🩺 状态与诊断",
        summary: "查看会话状态或机器人运行状态。",
        commands: &[
            "- `/state`：查看当前会话状态",
            "- 私聊 `/ping`：查看运行状态摘要",
            "- 私聊 `/ping check`：主动验证一次 LLM 上游调用",
            "- 私聊 `/ping all`：查看完整诊断信息",
        ],
        notes: &["- `/ping` 仅支持 QQ 私聊，群聊中不提供该诊断入口。"],
    },
];

/// 按参数生成帮助首页、完整帮助、模块帮助或未知模块提示。
pub(super) fn format_help_reply(argument: &str) -> CommandBody {
    let module = argument.trim().to_ascii_lowercase();
    if module.is_empty() {
        return format_help_home();
    }
    if matches!(module.as_str(), "all" | "全部") {
        return format_all_help();
    }
    if let Some(help) = HELP_MODULES
        .iter()
        .find(|help| help.key == module || help.aliases.contains(&module.as_str()))
    {
        return format_module_help(help);
    }
    format_unknown_help(&module)
}

fn format_help_home() -> CommandBody {
    let mut render = CommandRender::new();
    render.title("女仆长助手");
    render.blank();
    render.paragraph("可以陪你聊天，也可以管理待办、订阅 RSS / Atom、查询天气和整理会话。");
    render.blank();
    render.subtitle("常用功能");
    render.bullet("💬 对话：直接发送消息");
    // 常用功能里的命令示例需要同时给出纯文本和 Markdown 两通道：
    // 纯文本侧不能带反引号，否则 QQ 纯文本渲染会把反引号内容吞掉；
    // Markdown 侧保留行内代码反引号，便于支持 Markdown 的客户端高亮命令。
    render.push_pair(
        "· ✅ 待办：/todo".to_owned(),
        "- ✅ 待办：`/todo`".to_owned(),
    );
    render.push_pair(
        "· 📰 RSS / Atom：/rss".to_owned(),
        "- 📰 RSS / Atom：`/rss`".to_owned(),
    );
    render.push_pair(
        "· 🌤 天气：/天气 杭州".to_owned(),
        "- 🌤 天气：`/天气 杭州`".to_owned(),
    );
    render.push_pair(
        "· 🔎 查询：/查 问题、/火车 G1".to_owned(),
        "- 🔎 查询：`/查 问题`、`/火车 G1`".to_owned(),
    );
    render.push_pair(
        "· 🧠 记忆：/memory".to_owned(),
        "- 🧠 记忆：`/memory`".to_owned(),
    );
    render.push_pair(
        "· 🗂 会话：/state".to_owned(),
        "- 🗂 会话：`/state`".to_owned(),
    );
    render.push_pair(
        "· 🩺 状态：私聊发送 /ping".to_owned(),
        "- 🩺 状态：私聊发送 `/ping`".to_owned(),
    );
    render.blank();
    render.subtitle("查看详细帮助");
    render.push_pair(
        "· /help all：查看全部公开命令".to_owned(),
        "- `/help all`：查看全部公开命令".to_owned(),
    );
    render.push_pair(
        "· /help <模块>：查看模块用法".to_owned(),
        "- `/help <模块>`：查看模块用法".to_owned(),
    );
    render.push_pair(
        "· 常用模块：chat、todo、rss、weather、search".to_owned(),
        "- 常用模块：`chat`、`todo`、`rss`、`weather`、`search`".to_owned(),
    );
    render.push_pair(
        "· 更多模块：translation、memory、session、status".to_owned(),
        "- 更多模块：`translation`、`memory`、`session`、`status`".to_owned(),
    );
    render.build()
}

fn format_all_help() -> CommandBody {
    let mut rows = vec![
        "# 全部帮助".to_owned(),
        String::new(),
        "## ℹ️ 帮助".to_owned(),
        "- `/help`、`/帮助`：查看功能总览".to_owned(),
        "- `/help all`：查看本页".to_owned(),
        "- `/help <模块>`：查看模块说明".to_owned(),
    ];
    for help in HELP_MODULES {
        rows.push(String::new());
        rows.push(format!("## {}", help.title));
        rows.extend(help.commands.iter().map(|line| (*line).to_owned()));
    }
    rows.push(String::new());
    rows.push("输入 `/help <模块>` 查看行为说明和示例。".to_owned());
    let markdown = rows.join("\n");
    CommandBody::dual(strip_markdown_for_chat(&markdown), markdown)
}

fn format_module_help(help: &HelpModule) -> CommandBody {
    // 英文标题与“帮助”之间保留空格，中文标题则直接连接，兼顾 Markdown 和纯文本回退可读性。
    let separator = if help
        .title
        .chars()
        .last()
        .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        " "
    } else {
        ""
    };
    let mut rows = vec![
        format!("# {}{separator}帮助", help.title),
        String::new(),
        help.summary.to_owned(),
    ];
    rows.push(String::new());
    rows.push("## 命令".to_owned());
    rows.extend(help.commands.iter().map(|line| (*line).to_owned()));
    if !help.notes.is_empty() {
        rows.push(String::new());
        rows.push("## 说明".to_owned());
        rows.extend(help.notes.iter().map(|line| (*line).to_owned()));
    }
    let markdown = rows.join("\n");
    CommandBody::dual(strip_markdown_for_chat(&markdown), markdown)
}

fn format_unknown_help(module: &str) -> CommandBody {
    // 用户参数会回显到 Markdown；压缩空白并移除反引号，避免破坏行内代码结构。
    let mut display = module
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "'")
        .chars()
        .take(40)
        .collect::<String>();
    if display.is_empty() {
        display = "（空）".to_owned();
    }
    let modules = HELP_MODULES
        .iter()
        .map(|help| format!("`{}`", help.key))
        .collect::<Vec<_>>()
        .join("、");
    let markdown = format!(
        "未找到帮助模块：`{display}`\n\n可用模块：{modules}\n\n输入 `/help` 查看功能总览。"
    );
    CommandBody::dual(strip_markdown_for_chat(&markdown), markdown)
}
