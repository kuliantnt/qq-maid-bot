//! 系统提示词加载。
//!
//! 按职责拆分为：
//! - `prompt_files`：固定 prompt 加载。

mod prompt_files;

use std::path::PathBuf;

use crate::error::LlmError;

pub use prompt_files::PROMPT_FILES;

/// 提示词加载配置。
///
/// Markdown 知识库由 `runtime::tools::knowledge` 受控检索，
/// 避免整份资料进入稳定 system prompt 前缀。
#[derive(Debug, Clone)]
pub struct PromptConfig {
    /// 存放系统提示词文件的目录
    pub prompt_dir: PathBuf,
    /// 默认公开配置是否允许缺失 prompt 时回退到内置通用提示词
    pub use_builtin_prompt_defaults: bool,
}

impl PromptConfig {
    /// 创建新的提示词配置。
    pub fn new(prompt_dir: impl Into<PathBuf>) -> Self {
        Self {
            prompt_dir: prompt_dir.into(),
            use_builtin_prompt_defaults: false,
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

    /// 加载固定系统提示词。
    ///
    /// 非普通聊天调用方只需要固定 prompt；知识库片段只在普通聊天 flow 中按需附加。
    pub fn load_system_prompts(&self) -> Result<Vec<String>, LlmError> {
        self.load_static_system_prompts()
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
    fn load_system_prompts_returns_fixed_prompts() {
        let base = std::env::temp_dir().join(format!("qq-maid-prompts-{}", Uuid::new_v4()));
        let prompt_dir = base.join("prompts");
        write_prompt_set(&prompt_dir);

        let config = PromptConfig::new(&prompt_dir);
        let prompts = config.load_system_prompts().unwrap();

        assert_eq!(prompts.len(), PROMPT_FILES.len());
    }
}
