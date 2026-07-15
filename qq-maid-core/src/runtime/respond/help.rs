//! `/help` 分层帮助文案。
//!
//! 模块命令只在这里维护一份：完整帮助复用精简命令列表，模块帮助再补充行为说明，
//! 避免 `/help all` 与 `/help <模块>` 随功能演进后互相矛盾。

use super::{command_render::CommandRender, common::CommandBody};
use qq_maid_common::markdown_strip::strip_markdown_for_chat;

struct HelpModule {
    key: &'static str,
    aliases: &'static [&'static str],
    title: &'static str,
    summary: &'static str,
    commands: &'static [&'static str],
    notes: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HelpContext {
    pub is_group: bool,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
}

impl HelpContext {
    fn todo_writes_available(self) -> bool {
        self.tool_calling_enabled && (!self.is_group || self.group_tool_calling_enabled)
    }
}

const HELP_MODULES: &[HelpModule] = &[
    HelpModule {
        key: "chat",
        aliases: &["对话", "聊天"],
        title: "💬 对话",
        summary: "直接发送消息即可聊天；普通聊天不会自动写入长期记忆。",
        commands: &["- 直接发送消息：进入当前会话的聊天流程"],
        notes: &["- 需要保存长期信息时，可直接说“记住……”或使用 `/memory 内容`。"],
    },
    HelpModule {
        key: "todo",
        aliases: &["待办", "任务"],
        title: "✅ 待办",
        summary: "管理当前用户和当前聊天范围内的待办。中文别名：`/待办`、`/任务`。",
        commands: &[
            "- `/todo`：查看未完成待办",
            "- `/todo all`：查看全部待办",
            "- `/todo search 关键词`：搜索未完成待办",
            "- `/todo done`、`/todo undo`：查看已完成待办",
            "- 写操作请直接用自然语言，例如：`帮我新增待办：明天检查日志`、`完成第一条待办`、`删除已完成待办`。",
        ],
        notes: &[
            "- Todo 写操作已统一走模型 Tool 调用；slash 入口只保留查询能力，避免同一操作存在两套执行链路。",
            "- 新增、修改、完成和恢复会直接执行；取消或删除待办/提醒会先进入删除确认。",
            "- 引用新增、修改、完成或提醒消息后，可以继续说“删了这个”“恢复这个”“跳过这次”“以后别提醒了”。",
            "- 重复提醒里，“今天已完成”会完成当前周期并安排下次；“跳过这次”只跳过当前周期；“以后别提醒了/关掉重复提醒”会关闭后续重复提醒；“取消这个提醒/待办”按删除确认处理。",
            "- 当前不保存独立暂停状态；需要暂停到某时，请直接说清楚新的提醒时间。",
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
            "- `/rss recent [数量]`：查看最近抓取的条目，默认 5 条，最多 20 条",
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
        key: "radar",
        aliases: &["rader", "雷达"],
        title: "🛰️ 雷达",
        summary: "查看 Codex Radar 和 Claude Code Radar 的公开摘要卡片。",
        commands: &[
            "- `/rader`、`/radar`、`/雷达`：查看两个雷达摘要",
            "- `/rader codex`：只看 Codex Radar",
            "- `/rader claude`：只看 Claude Code Radar",
            "- `/rader issue codex`、`/rader issue claude`：查看反馈入口",
        ],
        notes: &[
            "- 雷达命令只读取公开数据源，不进入普通聊天 Tool Loop。",
            "- 外部数据失败或字段缺失时，会显示明确降级提示。",
        ],
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
        summary: "明确的新增记忆请求校验通过后直接写入；破坏性管理操作仍需当前操作者确认。中文别名：`/记忆`、`/记`。",
        commands: &[
            "- `/memory 内容`：直接保存个人记忆",
            "- `/memory personal 内容`：保存个人记忆",
            "- `/memory profile 内容`：保存你在当前群的个人画像",
            "- `/memory group 内容`：保存当前群公共记忆（仅群主或管理员）",
            "- `/memory` 或 `/memory list`：查看当前范围列表；可追加关键词搜索",
            "- `/memory show 序号`、`/memory edit 序号 新内容`、`/memory delete 序号`：管理最近列表",
            "- `/memory clear`：清空当前范围，执行前仍需确认",
            "- `/memory profile stop|enable`：停止或重新授权当前群画像写入",
        ],
        notes: &[
            "- 普通聊天不会自动写长期记忆；新增直接写入，修改、删除、清空和画像授权变更继续按现有确认流程处理。",
            "- 群聊建议显式指定 `profile` 或 `group`；范围不明确时会先询问，不会直接写入。",
        ],
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
        key: "settings",
        aliases: &["设置", "偏好"],
        title: "⚙️ 设置",
        summary: "管理当前会话里的个人偏好；展示名只用于显示，不代表现实身份认证。",
        commands: &[
            "- `/set 昵称 脸脸`：设置当前会话里的展示名（别名 `nickname` / `display_name`）",
            "- `/set 昵称`：查看当前展示名",
            "- `/unset 昵称`：清除展示名",
        ],
        notes: &[
            "- 展示名按稳定身份绑定在当前私聊或群聊，群 A 设置不会污染群 B。",
            "- 展示名只用于显示和帮助机器人理解上下文，不影响权限判断，也不等于现实身份认证。",
            "- 昵称长度限 1~32 个字符，不能包含换行；缺少稳定身份时无法设置。",
        ],
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
pub(super) fn format_help_reply(argument: &str, context: HelpContext) -> CommandBody {
    let module = argument.trim().to_ascii_lowercase();
    if module.is_empty() {
        return format_help_home(context);
    }
    if matches!(module.as_str(), "all" | "全部") {
        return format_all_help(context);
    }
    if let Some(help) = HELP_MODULES
        .iter()
        .find(|help| help.key == module || help.aliases.contains(&module.as_str()))
    {
        return format_module_help(help, context);
    }
    format_unknown_help(&module)
}

fn format_help_home(context: HelpContext) -> CommandBody {
    let mut render = CommandRender::new();
    render.title("女仆长助手");
    render.blank();
    render.paragraph("可以陪你聊天，也可以管理待办、订阅 RSS / Atom、查询天气和整理会话。");
    render.blank();
    render.subtitle("常用功能卡片");
    render.bullet("💬 对话：直接发送消息");
    // 常用功能里的命令示例需要同时给出纯文本和 Markdown 两通道：
    // 纯文本侧不能带反引号，否则 QQ 纯文本渲染会把反引号内容吞掉；
    // Markdown 侧保留行内代码反引号，便于支持 Markdown 的客户端高亮命令。
    if context.todo_writes_available() {
        render.push_pair(
            "· ✅ 待办：/todo".to_owned(),
            "- ✅ 待办：`/todo`".to_owned(),
        );
    } else {
        render.push_pair(
            "· ✅ 待办：/todo（写操作请私聊）".to_owned(),
            "- ✅ 待办：`/todo`（写操作请私聊）".to_owned(),
        );
    }
    render.push_pair(
        "· 📰 RSS / Atom：/rss".to_owned(),
        "- 📰 RSS / Atom：`/rss`".to_owned(),
    );
    render.push_pair(
        "· 🌤 天气：/天气 杭州".to_owned(),
        "- 🌤 天气：`/天气 杭州`".to_owned(),
    );
    render.push_pair(
        "· 🛰️ 雷达：/rader".to_owned(),
        "- 🛰️ 雷达：`/rader`".to_owned(),
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
        "· ⚙️ 设置：/set 昵称 脸脸".to_owned(),
        "- ⚙️ 设置：`/set 昵称 脸脸`".to_owned(),
    );
    render.push_pair(
        "· 🩺 状态：私聊发送 /ping".to_owned(),
        "- 🩺 状态：私聊发送 `/ping`".to_owned(),
    );
    render.blank();
    render.subtitle("帮助入口卡片");
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
        "· 更多模块：translation、memory、session、settings、status".to_owned(),
        "- 更多模块：`translation`、`memory`、`session`、`settings`、`status`".to_owned(),
    );
    if context.is_group && !context.group_tool_calling_enabled {
        render.blank();
        render.subtitle("群聊说明");
        render.paragraph("群聊默认不执行待办写入、天气等工具调用，避免长时间占用群聊回复队列；待办查询仍可使用 /todo。");
        render.paragraph("需要写待办时请私聊发送自然语言指令；如需试用群聊工具，可由运维开启 TOOL_CALLING_GROUP_ENABLED。");
    } else if !context.tool_calling_enabled {
        render.blank();
        render.subtitle("工具说明");
        render.paragraph("当前未启用工具调用，待办写操作暂不可用；待办查询仍可使用 /todo。");
    }
    render.build()
}

fn format_all_help(context: HelpContext) -> CommandBody {
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
        rows.extend(module_commands(help, context));
    }
    rows.push(String::new());
    rows.push("输入 `/help <模块>` 查看对应功能卡片、行为说明和示例。".to_owned());
    let markdown = rows.join("\n");
    CommandBody::dual(strip_markdown_for_chat(&markdown), markdown)
}

fn format_module_help(help: &HelpModule, context: HelpContext) -> CommandBody {
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
    rows.extend(module_commands(help, context));
    let notes = module_notes(help, context);
    if !notes.is_empty() {
        rows.push(String::new());
        rows.push("## 说明".to_owned());
        rows.extend(notes);
    }
    let markdown = rows.join("\n");
    CommandBody::dual(strip_markdown_for_chat(&markdown), markdown)
}

fn module_commands(help: &HelpModule, context: HelpContext) -> Vec<String> {
    if help.key != "todo" || context.todo_writes_available() {
        return help
            .commands
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
    }
    let write_notice = if context.is_group && !context.group_tool_calling_enabled {
        "- 写操作：群聊默认关闭工具调用，请在私聊中用自然语言发起；群聊中仍可用 `/todo` 查询。"
    } else {
        "- 写操作：当前未启用工具调用，暂不可用；仍可使用 `/todo` 查询。"
    };
    help.commands
        .iter()
        .map(|line| {
            if line.contains("写操作请直接用自然语言") {
                write_notice.to_owned()
            } else {
                (*line).to_owned()
            }
        })
        .collect()
}

fn module_notes(help: &HelpModule, context: HelpContext) -> Vec<String> {
    let mut notes = help
        .notes
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    if help.key != "todo" {
        return notes;
    }
    if context.is_group && !context.group_tool_calling_enabled {
        notes.push(
            "- 群聊工具调用默认关闭，避免 Todo 写入、天气等工具操作阻塞群聊回复；如需开放，可配置 `TOOL_CALLING_GROUP_ENABLED=true`。"
                .to_owned(),
        );
    } else if !context.tool_calling_enabled {
        notes.push("- 当前 `TOOL_CALLING_ENABLED=false`，Todo 写操作不会执行。".to_owned());
    }
    notes
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
