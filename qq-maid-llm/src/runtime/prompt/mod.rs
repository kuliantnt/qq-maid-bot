//! 系统提示词加载与成员编号映射。
//!
//! 负责加载系统提示词文件（如 `maid_system.md`、`mode_rules.md` 等）、
//! 可选世界观文件，管理成员编号到名称、描述的映射关系，并提供基于正则的成员提及识别。

use std::{fs, path::PathBuf, sync::LazyLock};

use regex::Regex;
use serde_json::Value;

use crate::error::LlmError;

/// 需要从 `PROMPT_DIR` 加载的系统提示词文件列表（按顺序拼接）。
///
/// 世界观不再强绑定到 `innerworld_lore.md`，只通过可选的 `WORLD_FILE` 注入。
pub const PROMPT_FILES: &[&str] = &["maid_system.md", "mode_rules.md", "session_context.md"];

/// 默认系统提示词：公开仓库缺少私有 prompt 时使用，避免本地启动直接失败。
///
/// 这些内容必须保持通用，不包含私人世界观、群聊成员或真实业务材料。
const DEFAULT_PROMPTS: &[(&str, &str)] = &[
    (
        "maid_system.md",
        "你是一个通用 QQ 机器人助手。请用简洁、自然、可靠的中文回答用户问题；不知道或缺少上下文时直接说明，并优先询问必要信息。不要编造个人身份、群聊设定或外部事实。",
    ),
    (
        "mode_rules.md",
        "根据用户请求选择合适的回答方式：普通聊天保持简短；需要整理、方案或步骤时使用清晰结构；涉及现实风险、隐私或账号安全时保持谨慎，不输出敏感信息。",
    ),
    (
        "session_context.md",
        "多轮对话中可以参考已提供的会话上下文和历史消息。短句通常视为对当前话题的补充；用户明确切换主题时再开启新话题。slash 指令由程序处理，不要假装执行未提供的工具。",
    ),
];

/// 匹配用户消息中成员编号自称的正则。
///
/// 匹配模式如 "我是407"、"编号 123 来了" 等。
static MEMBER_MENTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[，,。.!！?？\s：:])(?:我是|这里是|这边是|我这边是|我是编号|编号是)?\s*([1-9]\d{2})(?:\s*(?:来了|在|报到|上线))?(?:$|[，,。.!！?？\s])",
    )
    .unwrap()
});

/// 提示词加载配置。
///
/// 包含系统提示词目录、可选世界观文件和成员编号映射文件的路径。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// 存放系统提示词文件的目录
    pub prompt_dir: PathBuf,
    /// 默认公开配置是否允许缺失 prompt 时回退到内置通用提示词
    pub use_builtin_prompt_defaults: bool,
    /// 可选世界观提示词文件；未配置时不注入世界观
    pub world_file: Option<PathBuf>,
    /// 成员编号映射 JSON 文件路径
    pub member_id_mapping_file: PathBuf,
}

/// 从文本中匹配到的成员编号及对应信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberIdMatch {
    /// 成员编号（三位数字）
    pub member_id: String,
    /// 成员昵称
    pub name: Option<String>,
    /// 成员描述/设定
    pub profile: Option<String>,
}

impl PromptConfig {
    /// 创建新的提示词配置。
    pub fn new(prompt_dir: impl Into<PathBuf>, member_id_mapping_file: impl Into<PathBuf>) -> Self {
        Self {
            prompt_dir: prompt_dir.into(),
            use_builtin_prompt_defaults: false,
            world_file: None,
            member_id_mapping_file: member_id_mapping_file.into(),
        }
    }

    /// 设置是否允许从内置公开默认 prompt 回退。
    ///
    /// 只有应用使用默认 `PROMPT_DIR` 时才应开启；用户显式配置目录后保持严格报错，
    /// 防止路径写错时静默使用通用 prompt 掩盖配置问题。
    pub fn with_builtin_prompt_defaults(mut self, enabled: bool) -> Self {
        self.use_builtin_prompt_defaults = enabled;
        self
    }

    /// 设置可选世界观文件。
    ///
    /// `None` 表示通用助手模式；一旦配置了路径，文件缺失、不可读或为空都属于配置错误。
    pub fn with_world_file(mut self, world_file: Option<PathBuf>) -> Self {
        self.world_file = world_file;
        self
    }

    /// 加载所有系统提示词文件，并拼接成员编号映射提示。
    ///
    /// 默认公开配置允许缺失 `PROMPT_DIR` 文件时回退到内置通用 prompt；
    /// 用户显式配置的 prompt 目录和 `WORLD_FILE` 会严格校验，避免误用错误路径。
    pub fn load_system_prompts(&self) -> Result<Vec<String>, LlmError> {
        let mut prompts = Vec::new();
        for file_name in PROMPT_FILES {
            let path = self.prompt_dir.join(file_name);
            match load_required_prompt_file(&path, "prompt file") {
                Ok(content) => prompts.push(content),
                Err(_) if self.use_builtin_prompt_defaults && !path.exists() => {
                    prompts.push(default_prompt_content(file_name)?.to_owned());
                }
                Err(err) => return Err(err),
            }
        }
        if let Some(world_file) = &self.world_file {
            prompts.push(load_required_prompt_file(world_file, "world file")?);
        }
        if let Some(prompt) = self.build_member_id_mapping_prompt()? {
            prompts.push(prompt);
        }
        Ok(prompts)
    }

    /// 加载成员编号映射文件。
    ///
    /// 如果文件不存在则返回空映射。
    pub fn load_member_id_mapping(&self) -> Result<MemberMapping, LlmError> {
        if !self.member_id_mapping_file.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&self.member_id_mapping_file).map_err(|err| {
            LlmError::config(format!(
                "failed to read member id mapping file {}: {err}",
                self.member_id_mapping_file.display()
            ))
        })?;
        let value = serde_json::from_str::<Value>(&text).map_err(|err| {
            LlmError::config(format!(
                "failed to parse member id mapping file {}: {err}",
                self.member_id_mapping_file.display()
            ))
        })?;
        Ok(normalize_member_mapping(&value))
    }

    /// 构建成员编号映射的提示文本，供系统提示使用。
    pub fn build_member_id_mapping_prompt(&self) -> Result<Option<String>, LlmError> {
        let mapping = self.load_member_id_mapping()?;
        Ok(build_member_id_mapping_prompt(&mapping))
    }

    /// 在文本中查找所有已知的成员编号提及。
    pub fn find_member_id_mentions(&self, text: &str) -> Result<Vec<MemberIdMatch>, LlmError> {
        let mapping = self.load_member_id_mapping()?;
        Ok(find_member_id_mentions(text, &mapping))
    }
}

fn load_required_prompt_file(path: &std::path::Path, label: &str) -> Result<String, LlmError> {
    if !path.exists() {
        return Err(LlmError::config(format!(
            "{label} missing: {}",
            path.display()
        )));
    }
    let content = fs::read_to_string(path).map_err(|err| {
        LlmError::config(format!("failed to read {label} {}: {err}", path.display()))
    })?;
    if content.trim().is_empty() {
        return Err(LlmError::config(format!(
            "{label} is empty: {}",
            path.display()
        )));
    }
    Ok(content.trim().to_owned())
}

fn default_prompt_content(file_name: &str) -> Result<&'static str, LlmError> {
    DEFAULT_PROMPTS
        .iter()
        .find_map(|(name, content)| (*name == file_name).then_some(*content))
        .ok_or_else(|| LlmError::config(format!("missing builtin default prompt for {file_name}")))
}

/// 成员映射类型：(成员编号, 名称, 描述) 的三元组列表。
pub type MemberMapping = Vec<(String, String, String)>;

/// 将 JSON 格式的成员编号映射归一化为标准的三元组列表。
///
/// 支持两种 JSON 格式：
/// - 字符串值：`"407": "名称：描述"`
/// - 对象值：`"407": {"name": "名称", "profile": "描述"}`
pub fn normalize_member_mapping(value: &Value) -> MemberMapping {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut mapping = Vec::new();
    for (member_id, raw) in object {
        if !is_member_id(member_id) {
            continue;
        }
        if let Some(text) = raw.as_str() {
            let (name, profile) = split_member_text(text);
            if !name.is_empty() {
                mapping.push((member_id.clone(), name, profile));
            }
            continue;
        }
        if let Some(item) = raw.as_object() {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            let profile = item
                .get("profile")
                .or_else(|| item.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            if !name.is_empty() {
                mapping.push((member_id.clone(), name, profile));
            }
        }
    }
    mapping.sort_by(|left, right| left.0.cmp(&right.0));
    mapping
}

/// 根据成员映射生成系统提示文本，告知 LLM 成员编号对应的身份信息。
pub fn build_member_id_mapping_prompt(mapping: &MemberMapping) -> Option<String> {
    let rows = mapping
        .iter()
        .filter(|(_, name, _)| !name.trim().is_empty())
        .map(|(member_id, name, profile)| {
            let description = if profile.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", profile.trim())
            };
            format!("- {member_id} = {}{description}", name.trim())
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    Some(format!(
        "成员编号映射来自外部配置文件。当当前用户消息出现成员编号或明确自称时，优先使用这些配置判断当前说话者；不要重新发明编号含义，也不要仅凭上一轮前台默认延续：\n{}",
        rows.join("\n")
    ))
}

/// 从文本中查找所有成员编号提及，并匹配映射中的身份信息。
///
/// 使用正则匹配三位数编号，去重后返回匹配结果。
pub fn find_member_id_mentions(text: &str, mapping: &MemberMapping) -> Vec<MemberIdMatch> {
    let mut seen = Vec::<String>::new();
    let mut matches = Vec::new();
    for capture in MEMBER_MENTION_PATTERN.captures_iter(text.trim()) {
        let Some(member_id) = capture.get(1).map(|item| item.as_str().to_owned()) else {
            continue;
        };
        if seen.contains(&member_id) {
            continue;
        }
        seen.push(member_id.clone());
        let member = mapping.iter().find(|(id, _, _)| id == &member_id);
        matches.push(MemberIdMatch {
            member_id,
            name: member.map(|(_, name, _)| name.clone()),
            profile: member.map(|(_, _, profile)| profile.clone()),
        });
    }
    matches
}

/// 生成未知成员编号的回复，如果存在相似编号则给出提示建议。
pub fn unknown_member_id_reply(member_id: &str, mapping: &MemberMapping) -> String {
    let suggestion = suggest_member_id(member_id, mapping)
        .map(|(id, name)| format!("你是想说 {id} {name}，还是"))
        .unwrap_or_default();
    format!("当前编号映射里没有 {member_id}。是不是写错了？{suggestion}需要补充一个新成员？")
}

/// 根据匹配到的成员编号列表，构建本轮对话的身份上下文提示。
pub fn build_member_identity_context(matches: &[MemberIdMatch]) -> Option<String> {
    let rows = matches
        .iter()
        .filter_map(|item| {
            let name = item.name.as_deref()?;
            let description = item
                .profile
                .as_deref()
                .filter(|profile| !profile.trim().is_empty())
                .map(|profile| format!("：{}", profile.trim()))
                .unwrap_or_default();
            Some(format!(
                "- {} = {}{description}",
                item.member_id,
                name.trim()
            ))
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    Some(format!(
        "本轮用户消息命中了已知成员编号。判断当前说话者时，请优先按以下身份理解；如命中多个编号，可以理解为多人同时前台，不要重新发明编号含义：\n{}",
        rows.join("\n")
    ))
}

/// 根据后缀（末两位）匹配相似成员编号，用于纠错提示。
fn suggest_member_id(member_id: &str, mapping: &MemberMapping) -> Option<(String, String)> {
    let suffix = member_id.get(member_id.len().saturating_sub(2)..)?;
    mapping
        .iter()
        .find(|(candidate_id, _, _)| candidate_id != member_id && candidate_id.ends_with(suffix))
        .map(|(id, name, _)| (id.clone(), name.clone()))
}

/// 以中文冒号分割成员信息文本，返回 (名称, 描述)。
fn split_member_text(text: &str) -> (String, String) {
    let mut parts = text.splitn(2, '：');
    let name = parts.next().unwrap_or("").trim().to_owned();
    let profile = parts.next().unwrap_or("").trim().to_owned();
    (name, profile)
}

/// 判断字符串是否为合法成员编号（三位数字，首位非零）。
fn is_member_id(value: &str) -> bool {
    value.len() == 3
        && value
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '1'..='9'))
        && value.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn write_prompt_set(dir: &std::path::Path) {
        fs::create_dir_all(dir).unwrap();
        for file_name in PROMPT_FILES {
            fs::write(dir.join(file_name), format!("{file_name} content")).unwrap();
        }
    }

    #[test]
    fn prompt_files_load_successfully() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let mapping_file = base.join("member.json");
        fs::write(
            &mapping_file,
            r#"{"407":{"name":"测试成员","profile":"示例成员设定"}}"#,
        )
        .unwrap();

        let config = PromptConfig::new(&prompt_dir, &mapping_file);
        let prompts = config.load_system_prompts().unwrap();

        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("maid_system.md"))
        );
        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("407 = 测试成员"))
        );
    }

    #[test]
    fn default_prompt_dir_missing_files_uses_builtin_prompts() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        let config =
            PromptConfig::new(&base, base.join("member.json")).with_builtin_prompt_defaults(true);

        let prompts = config.load_system_prompts().unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("通用 QQ 机器人助手"))
        );
        let joined = prompts.join("\n");
        assert!(!joined.contains("innerworld_lore"));
        assert!(!joined.contains("小女仆"));
        assert!(!joined.contains("真实成员"));
    }

    #[test]
    fn explicit_prompt_dir_missing_file_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        let config = PromptConfig::new(&base, base.join("member.json"));

        let err = config.load_system_prompts().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("prompt file missing"));
    }

    #[test]
    fn explicit_prompt_dir_empty_file_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        write_prompt_set(&base);
        fs::write(base.join("mode_rules.md"), "  \n").unwrap();
        let config = PromptConfig::new(&base, base.join("member.json"));

        let err = config.load_system_prompts().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("prompt file is empty"));
    }

    #[test]
    fn absolute_prompt_dir_loads_external_files() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("private-prompts");
        write_prompt_set(&prompt_dir);
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"));

        let prompts = config.load_system_prompts().unwrap();

        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("mode_rules.md content"))
        );
    }

    #[test]
    fn relative_prompt_dir_loads_files_outside_repo() {
        let base = std::env::temp_dir().join(format!("qq-maid-private-{}", Uuid::new_v4()));
        let prompt_dir = base.join("config").join("prompts");
        write_prompt_set(&prompt_dir);
        let relative_prompt_dir = relative_path_from_current_dir(&prompt_dir);
        let relative_mapping_file = relative_path_from_current_dir(&base.join("member.json"));
        let config = PromptConfig::new(relative_prompt_dir, relative_mapping_file);

        let prompts = config.load_system_prompts().unwrap();

        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("session_context.md content"))
        );
    }

    #[test]
    fn world_file_absent_keeps_generic_prompt_set() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"));

        let prompts = config.load_system_prompts().unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
    }

    #[test]
    fn world_file_loads_as_independent_prompt() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let world_file = base.join("world.md");
        fs::write(&world_file, "外部世界观内容").unwrap();
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"))
            .with_world_file(Some(world_file));

        let prompts = config.load_system_prompts().unwrap();

        assert!(prompts.iter().any(|prompt| prompt == "外部世界观内容"));
    }

    #[test]
    fn world_file_missing_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"))
            .with_world_file(Some(base.join("missing-world.md")));

        let err = config.load_system_prompts().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("world file missing"));
    }

    #[test]
    fn world_file_empty_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let world_file = base.join("world.md");
        fs::write(&world_file, "\n").unwrap();
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"))
            .with_world_file(Some(world_file));

        let err = config.load_system_prompts().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("world file is empty"));
    }

    #[test]
    fn world_file_unreadable_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let world_file = base.join("world-dir");
        fs::create_dir_all(&world_file).unwrap();
        let config = PromptConfig::new(&prompt_dir, base.join("member.json"))
            .with_world_file(Some(world_file));

        let err = config.load_system_prompts().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("failed to read world file"));
    }

    #[test]
    fn missing_member_mapping_returns_empty_mapping() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let config = PromptConfig::new(base.join("prompts"), base.join("missing-member.json"));

        let mapping = config.load_member_id_mapping().unwrap();

        assert!(mapping.is_empty());
    }

    #[test]
    fn invalid_member_mapping_json_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        let mapping_file = base.join("member.json");
        fs::write(&mapping_file, "{invalid json").unwrap();
        let config = PromptConfig::new(base.join("prompts"), mapping_file);

        let err = config.load_member_id_mapping().unwrap_err();

        assert_eq!(err.code, "config");
        assert!(
            err.message
                .contains("failed to parse member id mapping file")
        );
    }

    #[test]
    fn member_mentions_use_external_mapping() {
        let mapping = normalize_member_mapping(&serde_json::json!({
            "407": {"name": "测试成员", "profile": "示例成员设定"},
            "507": {"name": "另一个", "profile": ""}
        }));

        let matches = find_member_id_mentions("我是407", &mapping);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name.as_deref(), Some("测试成员"));
        assert!(unknown_member_id_reply("507", &mapping).contains("507"));
    }

    fn relative_path_from_current_dir(path: &std::path::Path) -> PathBuf {
        let current_dir = std::env::current_dir().unwrap();
        let base_components = current_dir.components().collect::<Vec<_>>();
        let path_components = path.components().collect::<Vec<_>>();
        let common_len = base_components
            .iter()
            .zip(path_components.iter())
            .take_while(|(left, right)| left == right)
            .count();

        let mut relative = PathBuf::new();
        for _ in common_len..base_components.len() {
            relative.push("..");
        }
        for component in &path_components[common_len..] {
            relative.push(component.as_os_str());
        }
        relative
    }
}
