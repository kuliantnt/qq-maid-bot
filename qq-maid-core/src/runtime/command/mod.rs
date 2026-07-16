/// 解析后的 slash 命令。
///
/// 将用户以 `/` 开头的消息拆分为动作、参数和原始命令文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// 标准化后的动作名称（如 "new"、"weather"）
    pub action: String,
    /// 命令的参数部分（动作之后的文本）
    pub argument: String,
    /// 用户输入的原始命令（不含 `/`，全小写）
    pub raw_command: String,
}

/// 判断文本是否为需要由 Core 继续解析的斜杠命令候选。
///
/// 这里只识别前缀，不判断命令是否注册；未知群命令会在确定性分派末尾静默收口。
pub fn is_slash_command_candidate(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with('/') || text.starts_with('／')
}

/// 解析 slash 命令文本。
///
/// 如果输入不以 `/` 开头则返回 `None`，否则提取动作和参数，
/// 并将动作通过 [`normalize_command`] 标准化为标准动作名称。
pub fn parse_slash_command(text: &str) -> Option<ParsedCommand> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }
    let command_text = text.trim_start_matches('/').trim();
    if command_text.is_empty() {
        return None;
    }
    let mut parts = command_text.splitn(2, char::is_whitespace);
    let raw_command = parts.next()?.trim().to_ascii_lowercase();
    let argument = parts.next().unwrap_or("").trim().to_owned();
    let action = normalize_command(&raw_command)?;
    Some(ParsedCommand {
        action,
        argument,
        raw_command,
    })
}

/// 将用户输入的动作名称归一化为内部标准名称。
///
/// 支持中文别名（如 "新建" -> "new"，"记忆" -> "memory"），
/// 未知动作返回 `None`。
fn normalize_command(command: &str) -> Option<String> {
    let action = match command {
        "new" | "新建" => "new",
        "rename" | "重命名" | "改名" => "rename",
        "resume" | "恢复" | "继续" => "resume",
        "list" | "列表" => "list",
        "clear" | "清空" | "清除" => "clear",
        "state" | "状态" => "state",
        "compact" | "压缩" | "整理" => "compact",
        "help" | "帮助" => "help",
        "memory" | "记忆" | "记" | "zy" => "memory",
        "todo" | "待办" | "代办" | "任务" => "todo",
        "rss" | "订阅" => "rss",
        "查" | "查询" | "search" => "web_search",
        "train" | "火车" => "train",
        "weather" | "天气" => "weather",
        "rader" | "radar" | "雷达" => "radar",
        "set" | "设置" => "set",
        "unset" | "取消设置" | "清除设置" => "unset",
        _ => return None,
    };
    Some(action.to_owned())
}
