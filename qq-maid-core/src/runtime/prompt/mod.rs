//! 系统提示词加载与成员编号映射。
//!
//! 按职责拆分为：
//! - `prompt_files`：固定 prompt 和可选世界观加载；
//! - `member_mapping`：成员编号映射与身份提示；
//! - `context_modules`：普通聊天链路专用的可配置上下文模块。

mod context_modules;
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
/// 包含系统提示词目录、可选世界观文件、成员编号映射文件和普通聊天专用的上下文模块索引。
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
    /// 普通聊天上下文模块索引；未配置时完全关闭该能力
    pub context_modules_file: Option<PathBuf>,
}

impl PromptConfig {
    /// 创建新的提示词配置。
    pub fn new(prompt_dir: impl Into<PathBuf>, member_id_mapping_file: impl Into<PathBuf>) -> Self {
        Self {
            prompt_dir: prompt_dir.into(),
            use_builtin_prompt_defaults: false,
            world_file: None,
            member_id_mapping_file: member_id_mapping_file.into(),
            context_modules_file: None,
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

    /// 设置普通聊天链路可选上下文模块索引。
    ///
    /// 该能力只影响普通聊天 system prompt 组装，不应扩散到 todo、memory 或 compact 流程。
    pub fn with_context_modules_file(mut self, context_modules_file: Option<PathBuf>) -> Self {
        self.context_modules_file = context_modules_file;
        self
    }

    /// 加载不带上下文模块的系统提示词。
    ///
    /// 该方法保留给非普通聊天调用方，行为与旧版本保持一致：
    /// 只拼接固定 prompt、可选世界观和成员编号映射。
    pub fn load_system_prompts(&self) -> Result<Vec<String>, LlmError> {
        let mut prompts = self.load_static_system_prompts()?;
        if let Some(prompt) = self.build_member_id_mapping_prompt()? {
            prompts.push(prompt);
        }
        Ok(prompts)
    }

    /// 加载普通聊天专用 system prompts。
    ///
    /// 只有这里会按当前轮 `user_text` 额外选择上下文模块，保证 todo、memory、compact 等流程不受影响。
    pub fn load_chat_system_prompts(&self, user_text: &str) -> Result<Vec<String>, LlmError> {
        let mut prompts = self.load_static_system_prompts()?;
        prompts.extend(self.load_context_module_prompts(user_text)?);
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
        prompt_files::load_static_system_prompts(
            &self.prompt_dir,
            self.use_builtin_prompt_defaults,
            self.world_file.as_deref(),
        )
    }

    fn load_context_module_prompts(&self, user_text: &str) -> Result<Vec<String>, LlmError> {
        let Some(index_file) = &self.context_modules_file else {
            tracing::debug!("context modules disabled for chat prompt selection");
            return Ok(Vec::new());
        };
        context_modules::load_context_module_prompts(index_file, user_text)
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
    fn load_chat_system_prompts_inserts_context_modules_before_member_mapping() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let mapping_file = base.join("member.json");
        fs::write(
            &mapping_file,
            r#"{"407":{"name":"测试成员","profile":"示例成员设定"}}"#,
        )
        .unwrap();
        let world_file = base.join("world.md");
        fs::write(&world_file, "外部世界观内容").unwrap();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("always.md"), "常驻模块").unwrap();
        fs::write(context_dir.join("deploy.md"), "部署模块").unwrap();
        let context_modules_file = base.join("context_modules.toml");
        fs::write(
            &context_modules_file,
            r#"
version = 1

[limits]
max_dynamic_modules = 2
max_total_chars = 64

[[modules]]
id = "always"
file = "context/always.md"
always = true

[[modules]]
id = "deploy"
file = "context/deploy.md"
keywords = ["部署"]
priority = 90
"#,
        )
        .unwrap();

        let config = PromptConfig::new(&prompt_dir, &mapping_file)
            .with_world_file(Some(world_file))
            .with_context_modules_file(Some(context_modules_file));
        let prompts = config.load_chat_system_prompts("请看部署步骤").unwrap();

        let member_index = prompts
            .iter()
            .position(|prompt| prompt.contains("成员编号映射来自外部配置文件"))
            .unwrap();
        let world_index = prompts
            .iter()
            .position(|prompt| prompt == "外部世界观内容")
            .unwrap();
        let always_index = prompts
            .iter()
            .position(|prompt| prompt == "常驻模块")
            .unwrap();
        let deploy_index = prompts
            .iter()
            .position(|prompt| prompt == "部署模块")
            .unwrap();

        assert!(world_index < always_index);
        assert!(always_index < deploy_index);
        assert!(deploy_index < member_index);
    }

    #[test]
    fn load_system_prompts_keeps_previous_behavior_even_when_context_modules_are_configured() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("deploy.md"), "部署模块").unwrap();
        let context_modules_file = base.join("context_modules.toml");
        fs::write(
            &context_modules_file,
            r#"
version = 1

[limits]
max_dynamic_modules = 1
max_total_chars = 64

[[modules]]
id = "deploy"
file = "context/deploy.md"
keywords = ["部署"]
priority = 90
"#,
        )
        .unwrap();

        let config = PromptConfig::new(&prompt_dir, base.join("member.json"))
            .with_context_modules_file(Some(context_modules_file));
        let prompts = config.load_system_prompts().unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
        assert!(!prompts.iter().any(|prompt| prompt == "部署模块"));
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
