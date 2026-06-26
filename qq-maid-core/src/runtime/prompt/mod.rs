//! 系统提示词加载与成员编号映射。
//!
//! 按职责拆分为：
//! - `prompt_files`：固定 prompt 加载；
//! - `member_mapping`：成员编号映射与身份提示。

mod member_mapping;
mod prompt_files;

use std::{fs, path::PathBuf};

use serde_json::Value;

use crate::error::LlmError;

pub use member_mapping::{
    MemberIdMatch, MemberMapping, build_member_identity_context, find_member_id_mentions,
    normalize_member_mapping, unknown_member_id_reply,
};
pub use prompt_files::PROMPT_FILES;

/// 提示词加载配置。
///
/// 包含系统提示词目录和成员编号映射文件。Markdown 知识库由 `runtime::knowledge`
/// 在普通聊天链路动态检索，避免整份资料进入稳定 system prompt 前缀。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// 存放系统提示词文件的目录
    pub prompt_dir: PathBuf,
    /// 默认公开配置是否允许缺失 prompt 时回退到内置通用提示词
    pub use_builtin_prompt_defaults: bool,
    /// 成员编号映射 JSON 文件路径
    pub member_id_mapping_file: PathBuf,
}

impl PromptConfig {
    /// 创建新的提示词配置。
    pub fn new(prompt_dir: impl Into<PathBuf>, member_id_mapping_file: impl Into<PathBuf>) -> Self {
        Self {
            prompt_dir: prompt_dir.into(),
            use_builtin_prompt_defaults: false,
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

    /// 加载固定系统提示词和成员映射。
    ///
    /// 非普通聊天调用方只需要固定 prompt；知识库片段只在普通聊天 flow 中按需附加。
    pub fn load_system_prompts(&self) -> Result<Vec<String>, LlmError> {
        let mut prompts = self.load_static_system_prompts()?;
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
        Ok(member_mapping::build_member_id_mapping_prompt(&mapping))
    }

    /// 在文本中查找所有已知的成员编号提及。
    pub fn find_member_id_mentions(&self, text: &str) -> Result<Vec<MemberIdMatch>, LlmError> {
        let mapping = self.load_member_id_mapping()?;
        Ok(find_member_id_mentions(text, &mapping))
    }

    fn load_static_system_prompts(&self) -> Result<Vec<String>, LlmError> {
        prompt_files::load_static_system_prompts(&self.prompt_dir, self.use_builtin_prompt_defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    fn write_prompt_set(dir: &std::path::Path) {
        fs::create_dir_all(dir).unwrap();
        for file_name in PROMPT_FILES {
            fs::write(dir.join(file_name), format!("{file_name} content")).unwrap();
        }
    }

    #[test]
    fn load_system_prompts_inserts_member_mapping_after_fixed_prompts() {
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

        let member_index = prompts
            .iter()
            .position(|prompt| prompt.contains("成员编号映射来自外部配置文件"))
            .unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len() + 1);
        assert!(member_index >= PROMPT_FILES.len());
    }

    #[test]
    fn load_system_prompts_without_member_mapping_only_returns_fixed_prompts() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);

        let config = PromptConfig::new(&prompt_dir, base.join("member.json"));
        let prompts = config.load_system_prompts().unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
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
}
