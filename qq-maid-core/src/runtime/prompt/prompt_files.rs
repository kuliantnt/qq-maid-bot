//! 固定系统提示词文件的加载。
//!
//! 该模块负责加载三个必需的系统提示词文件（`maid_system.md`、`mode_rules.md`、
//!  `session_context.md`）。
//!
//! 加载逻辑的核心约束：
//! - 默认 `PROMPT_DIR` 缺少文件时，回退到内置通用 prompt（避免本地启动直接失败）；
//! - 用户显式配置 `PROMPT_DIR` 后严格校验：文件缺失、不可读或为空都属于配置错误；
//! - 本地知识资料由 `runtime::knowledge` 动态检索，不在这里整份注入。
//!
//! 加载后的 prompt 列表按声明顺序拼接，由上层 `PromptConfig` 决定最终组合。

use std::{fs, path::Path};

use crate::error::LlmError;

/// 需要从 `PROMPT_DIR` 加载的系统提示词文件列表（按顺序拼接）。
pub const PROMPT_FILES: &[&str] = &["maid_system.md", "mode_rules.md", "session_context.md"];

/// 默认系统提示词：公开仓库缺少私有 prompt 时使用，避免本地启动直接失败。
///
/// 这些内容必须保持通用，不包含私人设定、群聊成员或真实业务材料。
const DEFAULT_PROMPTS: &[(&str, &str)] = &[
    (
        "maid_system.md",
        "你是一个小女仆机器人助手。请用简洁、自然、可靠的中文回答用户问题；不知道或缺少上下文时直接说明，并优先询问必要信息。不要编造个人身份、群聊设定或外部事实。\n\n工具使用原则：\n1. 需要读取或变更外部状态（待办、记忆、知识库、文件、实时信息等）时，必须先依赖对应工具、程序查询结果或已注入资料，取得结果后再回复；没有成功结果时，不得凭会话印象回答或声称已经执行。\n2. 知识库问题优先依据已注入资料回答；资料不足时说明不足，不要尝试调用未提供的知识库工具。\n3. 禁止假执行：没有收到工具或程序成功结果时，不得使用 \"已添加\" \"已记录\" \"已删除\" \"已更新\" \"已完成\" 等已经执行的表述；只能说明需要工具确认，或工具没有成功返回所以不能确认已经完成。\n4. 回复前先判断：这是否只是闲聊、解释或创作；是否需要读取外部状态；是否需要写入、修改或删除外部状态。后两类必须先依赖工具、程序查询结果或已注入资料再回复。\n\n工具的具体能力、参数和适用范围由系统提供的工具描述决定，不要在 prompt 中复述。",
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

/// 加载固定 prompt 文件。
///
/// 返回的 prompt 列表顺序始终为三个固定文件。
/// 此函数不涉及知识检索上下文，由上层 flow 负责组合。
pub(super) fn load_static_system_prompts(
    prompt_dir: &Path,
    use_builtin_prompt_defaults: bool,
) -> Result<Vec<String>, LlmError> {
    let mut prompts = Vec::new();
    for file_name in PROMPT_FILES {
        let path = prompt_dir.join(file_name);
        match load_required_text_file(&path, "prompt file") {
            Ok(content) => prompts.push(content),
            Err(_) if use_builtin_prompt_defaults && !path.exists() => {
                prompts.push(default_prompt_content(file_name)?.to_owned());
            }
            Err(err) => return Err(err),
        }
    }
    Ok(prompts)
}

/// 加载必需的文本文件（固定 prompt）。
///
/// 三层校验，按顺序止于第一个不满足的条件：
/// 1. 路径不存在 → `config` error
/// 2. 读取失败（如无权限、路径是目录） → `config` error
/// 3. 内容为空（trim 后为空串） → `config` error
///
/// 通过校验后返回 trim 后的完整内容。
pub(super) fn load_required_text_file(path: &Path, label: &str) -> Result<String, LlmError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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

        let prompts = load_static_system_prompts(&prompt_dir, false).unwrap();

        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("maid_system.md"))
        );
    }

    #[test]
    fn default_prompt_dir_missing_files_uses_builtin_prompts() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();

        let prompts = load_static_system_prompts(&base, true).unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("小女仆机器人助手"))
        );
        let joined = prompts.join("\n");
        assert!(!joined.contains("私人设定"));
        assert!(!joined.contains("真实成员"));
        assert!(joined.contains("工具、程序查询结果或已注入资料"));
        assert!(joined.contains("不要尝试调用未提供的知识库工具"));
    }

    #[test]
    fn explicit_prompt_dir_missing_file_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();

        let err = load_static_system_prompts(&base, false).unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("prompt file missing"));
    }

    #[test]
    fn explicit_prompt_dir_empty_file_returns_clear_error() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        write_prompt_set(&base);
        fs::write(base.join("mode_rules.md"), "  \n").unwrap();

        let err = load_static_system_prompts(&base, false).unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("prompt file is empty"));
    }

    #[test]
    fn absolute_prompt_dir_loads_external_files() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("private-prompts");
        write_prompt_set(&prompt_dir);

        let prompts = load_static_system_prompts(&prompt_dir, false).unwrap();

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

        let prompts = load_static_system_prompts(&relative_prompt_dir, false).unwrap();

        assert!(
            prompts
                .iter()
                .any(|prompt| prompt.contains("session_context.md content"))
        );
    }

    #[test]
    fn fixed_prompt_set_has_no_extra_world_prompt() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);

        let prompts = load_static_system_prompts(&prompt_dir, false).unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
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
