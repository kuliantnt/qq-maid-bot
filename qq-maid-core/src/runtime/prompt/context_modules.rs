//! 普通聊天链路专用的可配置上下文模块。
//!
//! ## 设计意图
//!
//! 不同于固定 prompt（`maid_system.md`、`mode_rules.md` 等），上下文模块是**按需注入**
//! 的可选 system prompt 片段。模块内容放在 `context/*.md` 私有文件中，通过一个 TOML
//! 索引文件（`CONTEXT_MODULES_FILE`）声明哪些模块在什么条件下启用。
//!
//! ## 约束（非普通聊天链路不使用）
//!
//! 上下文模块**只在** `PromptConfig::load_chat_system_prompts` 中生效。todo、
//! memory、compact、session 管理等流程走 `load_system_prompts`，不会调用本模块。
//! 这样确保这些固定流程的 system prompt 结构稳定可控。
//!
//! ## 选择算法
//!
//! 模块分两类：
//! - **常驻（always=true）**：无条件注入，按声明顺序排列。
//! - **动态（always=false）**：按 keywords 和当前 user_text 做大小写无关匹配。
//!
//! 动态模块的选择流程：
//! 1. 匹配所有 keywords 命中的动态模块；
//! 2. 按 priority 降序 → id 字母序排序；
//! 3. 截断到 `max_dynamic_modules` 限制（按优先级从低到高丢弃）；
//! 4. 累加字符数，超出 `max_total_chars` 时按动态模块优先级从低到高丢弃
//!    （常驻模块不参与预算裁剪，常驻超出是硬错误）；
//! 5. 最终顺序：常驻 → 动态（高优到低优）。
//!
//! ## 安全约束
//!
//! - 模块文件路径只允许相对索引文件所在目录声明，不允许 `../` 逃逸；
//! - 绝对路径通过 `is_absolute()` 拒绝；
//! - 模块文件为空是硬错误；
//! - 索引文件缺失/不可解析/版本不支持是硬错误。

use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use crate::error::LlmError;

use super::prompt_files::load_required_text_file;

/// 当前支持的上下文模块索引文件版本号。
///
/// 版本号用于防止未来索引格式变更后静默错误加载旧格式配置。
const SUPPORTED_CONTEXT_MODULES_VERSION: u32 = 1;

/// TOML 反序列化的原始索引文件结构。
///
/// 字段名和 TOML key 完全对齐；`modules` 未声明时等价于空列表。
#[derive(Debug, Deserialize)]
struct RawContextModulesFile {
    version: u32,
    limits: RawContextModuleLimits,
    #[serde(default)]
    modules: Vec<RawContextModule>,
}

/// 模块预算限制。
///
/// 两个限制独立生效：
/// - `max_dynamic_modules`：同一请求最多注入几个动态模块；
/// - `max_total_chars`：常驻 + 最终选定动态模块的总字符数上限。
#[derive(Debug, Deserialize)]
struct RawContextModuleLimits {
    max_dynamic_modules: usize,
    max_total_chars: usize,
}

/// TOML 反序列化的原始模块声明。
///
/// `keywords`、`always`、`priority` 缺失时使用默认值（空向量、false、0）。
#[derive(Debug, Deserialize)]
struct RawContextModule {
    id: String,
    file: String,
    #[serde(default)]
    always: bool,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    priority: i32,
}

/// 校验并解析后的索引文件（比 `Raw*` 多了路径解析和去重校验）。
#[derive(Debug)]
struct ContextModulesFile {
    source_file: PathBuf,
    limits: ContextModuleLimits,
    modules: Vec<ContextModule>,
}

#[derive(Debug)]
struct ContextModuleLimits {
    max_dynamic_modules: usize,
    max_total_chars: usize,
}

/// 解析后的单个模块定义。
///
/// `declaration_order` 只在常驻模块排序时使用（保持 TOML 中的声明顺序），
/// 与 `priority` 无关。
#[derive(Debug)]
struct ContextModule {
    id: String,
    file: PathBuf,
    always: bool,
    keywords: Vec<String>,
    priority: i32,
    declaration_order: usize,
}

/// 加载并校验后的模块内容及其元信息。
///
/// `char_count` 用于预算累计，`content` 是 trim 后的完整 prompt 文本。
#[derive(Debug)]
struct LoadedModule {
    id: String,
    content: String,
    char_count: usize,
}

/// 入口：从索引文件读取上下文模块，按 user_text 选择命中的模块 prompt。
///
/// 返回的 prompt 列表顺序由 `select_prompts` 保证：常驻 → 动态（高优到低优）。
/// 索引文件不存在/不可解析/版本不匹配/常驻超预算均作为硬错误返回。
pub(super) fn load_context_module_prompts(
    index_file: &Path,
    user_text: &str,
) -> Result<Vec<String>, LlmError> {
    let config = ContextModulesFile::load(index_file)?;
    config.select_prompts(user_text)
}

impl ContextModulesFile {
    /// 从 TOML 索引文件加载并校验。
    ///
    /// 失败场景：
    /// - 文件不存在、不可读、TOML 解析失败；
    /// - 版本不匹配、预算限制为零、模块 id 为空/重复；
    /// - 模块文件路径逃逸、绝对路径或 keywords 只包含空字符串。
    fn load(path: &Path) -> Result<Self, LlmError> {
        if !path.exists() {
            return Err(LlmError::config(format!(
                "context modules index missing: {}",
                path.display()
            )));
        }
        let text = fs::read_to_string(path).map_err(|err| {
            LlmError::config(format!(
                "failed to read context modules index {}: {err}",
                path.display()
            ))
        })?;
        let raw = toml::from_str::<RawContextModulesFile>(&text).map_err(|err| {
            LlmError::config(format!(
                "failed to parse context modules index {}: {err}",
                path.display()
            ))
        })?;
        Self::from_raw(path, raw)
    }

    /// 从原始 TOML 解析结果构建校验后的 `ContextModulesFile`。
    ///
    /// 校验内容：
    /// - 版本号匹配；
    /// - `max_dynamic_modules` 和 `max_total_chars` 必须为正；
    /// - 所有模块 id 非空且不重复；
    /// - 模块文件路径不允许逃逸索引目录（防止 `../` 路径穿越）；
    /// - keywords 中去掉纯空字符串后不能为空。
    fn from_raw(path: &Path, raw: RawContextModulesFile) -> Result<Self, LlmError> {
        if raw.version != SUPPORTED_CONTEXT_MODULES_VERSION {
            return Err(LlmError::config(format!(
                "unsupported context modules version {} in {}",
                raw.version,
                path.display()
            )));
        }
        if raw.limits.max_dynamic_modules == 0 {
            return Err(LlmError::config(format!(
                "context modules max_dynamic_modules must be positive in {}",
                path.display()
            )));
        }
        if raw.limits.max_total_chars == 0 {
            return Err(LlmError::config(format!(
                "context modules max_total_chars must be positive in {}",
                path.display()
            )));
        }

        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut seen_ids = HashSet::new();
        let mut modules = Vec::new();
        for (index, raw_module) in raw.modules.into_iter().enumerate() {
            let id = raw_module.id.trim().to_owned();
            if id.is_empty() {
                return Err(LlmError::config(format!(
                    "context module id is empty in {}",
                    path.display()
                )));
            }
            if !seen_ids.insert(id.clone()) {
                return Err(LlmError::config(format!(
                    "duplicate context module id {id} in {}",
                    path.display()
                )));
            }
            let file = resolve_module_file_path(base_dir, path, &id, raw_module.file.trim())?;
            let keywords = normalize_keywords(path, &id, raw_module.keywords)?;
            modules.push(ContextModule {
                id,
                file,
                always: raw_module.always,
                keywords,
                priority: raw_module.priority,
                declaration_order: index,
            });
        }

        Ok(Self {
            source_file: path.to_path_buf(),
            limits: ContextModuleLimits {
                max_dynamic_modules: raw.limits.max_dynamic_modules,
                max_total_chars: raw.limits.max_total_chars,
            },
            modules,
        })
    }

    /// 核心选择算法：根据 user_text 选出应注入的常驻模块和动态模块。
    ///
    /// 详细流程见文件级注释；这里补充三个关键决策点：
    ///
    /// 1. **常驻模块先加载并校验预算**：常驻模块超出 `max_total_chars` 是硬错误，
    ///    因为这意味着无论如何都会超预算，应让运维提前调整。
    ///
    /// 2. **动态模块按优先级从低到高裁剪**：`loaded_dynamic` 已按 priority 降序排列，
    ///    预算不足时从末尾（最低优先级）pop，保护高优模块。
    ///
    /// 3. **diagnostic log 记录完整选择过程**：包括选中的模块 id、因数量限制跳过的、
    ///    因预算裁剪的，方便运维调参。
    fn select_prompts(&self, user_text: &str) -> Result<Vec<String>, LlmError> {
        let mut always_modules = self
            .modules
            .iter()
            .filter(|module| module.always)
            .collect::<Vec<_>>();
        always_modules.sort_by_key(|module| module.declaration_order);

        let mut loaded_always = always_modules
            .into_iter()
            .map(load_module_content)
            .collect::<Result<Vec<_>, _>>()?;
        let always_total_chars = loaded_always
            .iter()
            .map(|module| module.char_count)
            .sum::<usize>();
        if always_total_chars > self.limits.max_total_chars {
            return Err(LlmError::config(format!(
                "always context modules exceed max_total_chars in {}: {} > {}",
                self.source_file.display(),
                always_total_chars,
                self.limits.max_total_chars
            )));
        }

        let normalized_user_text = user_text.to_lowercase();
        let mut matched_dynamic = self
            .modules
            .iter()
            .filter(|module| !module.always && module.matches(&normalized_user_text))
            .collect::<Vec<_>>();
        matched_dynamic.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });

        let matched_dynamic_count = matched_dynamic.len();
        let skipped_by_limit_ids = matched_dynamic
            .iter()
            .skip(self.limits.max_dynamic_modules)
            .map(|module| module.id.clone())
            .collect::<Vec<_>>();
        matched_dynamic.truncate(self.limits.max_dynamic_modules);

        let mut loaded_dynamic = matched_dynamic
            .into_iter()
            .map(load_module_content)
            .collect::<Result<Vec<_>, _>>()?;
        let mut skipped_by_budget_ids = Vec::new();
        let mut total_chars = always_total_chars
            + loaded_dynamic
                .iter()
                .map(|module| module.char_count)
                .sum::<usize>();
        while total_chars > self.limits.max_total_chars {
            let Some(removed) = loaded_dynamic.pop() else {
                break;
            };
            total_chars -= removed.char_count;
            skipped_by_budget_ids.push(removed.id);
        }

        let selected_ids = loaded_always
            .iter()
            .map(|module| module.id.clone())
            .chain(loaded_dynamic.iter().map(|module| module.id.clone()))
            .collect::<Vec<_>>();
        tracing::debug!(
            source = %self.source_file.display(),
            context_modules_enabled = true,
            always_module_count = loaded_always.len(),
            matched_dynamic_count,
            selected_module_ids = ?selected_ids,
            skipped_by_limit_ids = ?skipped_by_limit_ids,
            skipped_by_budget_ids = ?skipped_by_budget_ids,
            total_chars,
            "context modules evaluated"
        );

        let mut prompts = loaded_always
            .drain(..)
            .map(|module| module.content)
            .collect::<Vec<_>>();
        prompts.extend(loaded_dynamic.into_iter().map(|module| module.content));
        Ok(prompts)
    }
}

impl ContextModule {
    /// 判断当前用户消息是否命中该模块的关键词。
    ///
    /// 匹配规则：大小写不敏感的 `contains` 匹配，任一 keyword 命中即视为匹配。
    fn matches(&self, normalized_user_text: &str) -> bool {
        self.keywords
            .iter()
            .any(|keyword| normalized_user_text.contains(&keyword.to_lowercase()))
    }
}

/// 加载单个模块文件并计算字符数。
///
/// 复用 `prompt_files::load_required_text_file` 的三层校验
///（路径存在 → 可读 → 非空），确保模块文件质量。
fn load_module_content(module: &ContextModule) -> Result<LoadedModule, LlmError> {
    let label = format!("context module file for module {}", module.id);
    let content = load_required_text_file(&module.file, &label)?;
    Ok(LoadedModule {
        id: module.id.clone(),
        char_count: content.chars().count(),
        content,
    })
}

/// 归一化关键词列表：去掉纯空字符串，小写去重，保留原始书写供日志展示。
///
/// 如果去掉空白后 keywords 为空且原始列表非空（全是空字符串），返回硬错误。
/// 如果原始列表就是空的（`always=true` 的模块可以不填 keywords），则静默跳过。
fn normalize_keywords(
    source_file: &Path,
    module_id: &str,
    raw_keywords: Vec<String>,
) -> Result<Vec<String>, LlmError> {
    let had_raw_keywords = !raw_keywords.is_empty();
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for keyword in raw_keywords {
        let trimmed = keyword.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_lowercase();
        if seen.insert(key) {
            normalized.push(trimmed.to_owned());
        }
    }
    if !normalized.is_empty() || !had_raw_keywords {
        return Ok(normalized);
    }
    Err(LlmError::config(format!(
        "context module keywords are empty for module {module_id} in {}",
        source_file.display()
    )))
}

/// 解析模块文件路径，阻止路径穿越。
///
/// 规则：
/// - 索引文件所在目录作为 `base_dir`；
/// - 相对路径通过 `normalize_relative_path` 规范化，`../` 出界直接拒绝；
/// - 绝对路径拒绝（防止引用索引目录外的任意文件）。
fn resolve_module_file_path(
    base_dir: &Path,
    source_file: &Path,
    module_id: &str,
    raw_file: &str,
) -> Result<PathBuf, LlmError> {
    if raw_file.is_empty() {
        return Err(LlmError::config(format!(
            "context module file is empty for module {module_id} in {}",
            source_file.display()
        )));
    }
    let raw_path = Path::new(raw_file);
    if raw_path.is_absolute() {
        return Err(LlmError::config(format!(
            "context module path must be relative to index directory for module {module_id} in {}: {raw_file}",
            source_file.display()
        )));
    }
    let normalized = normalize_relative_path(raw_path).ok_or_else(|| {
        LlmError::config(format!(
            "context module path escapes index directory for module {module_id} in {}: {raw_file}",
            source_file.display()
        ))
    })?;
    Ok(base_dir.join(normalized))
}

/// 规范化相对路径，拒绝路径穿越。
///
/// - `..` 只在目录栈非空时允许，栈空时返回 `None`（代表逃逸失败）；
/// - `RootDir` 和 `Prefix` 直接拒绝（不应出现在相对路径声明中）；
/// - `.` 直接跳过。
fn normalize_relative_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn base_dir() -> PathBuf {
        std::env::temp_dir().join(format!("qq-maid-context-modules-{}", Uuid::new_v4()))
    }

    fn write_index(base: &Path, body: &str) -> PathBuf {
        let path = base.join("context_modules.toml");
        fs::create_dir_all(base).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn always_and_dynamic_modules_load_in_expected_order() {
        let base = base_dir();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("always.md"), "常驻模块").unwrap();
        fs::write(context_dir.join("deploy.md"), "部署模块").unwrap();
        let index = write_index(
            &base,
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
        );

        let prompts = load_context_module_prompts(&index, "请看部署步骤").unwrap();

        assert_eq!(prompts, vec!["常驻模块".to_owned(), "部署模块".to_owned()]);
    }

    #[test]
    fn dynamic_modules_sort_by_priority_then_id_and_limit() {
        let base = base_dir();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("a.md"), "A 模块").unwrap();
        fs::write(context_dir.join("b.md"), "B 模块").unwrap();
        fs::write(context_dir.join("c.md"), "C 模块").unwrap();
        let index = write_index(
            &base,
            r#"
version = 1

[limits]
max_dynamic_modules = 2
max_total_chars = 64

[[modules]]
id = "b"
file = "context/b.md"
keywords = ["deploy"]
priority = 90

[[modules]]
id = "a"
file = "context/a.md"
keywords = ["DEPLOY"]
priority = 90

[[modules]]
id = "c"
file = "context/c.md"
keywords = ["deploy"]
priority = 80
"#,
        );

        let prompts = load_context_module_prompts(&index, "please DEPLOY this").unwrap();

        assert_eq!(prompts, vec!["A 模块".to_owned(), "B 模块".to_owned()]);
    }

    #[test]
    fn dynamic_modules_drop_from_low_priority_end_when_budget_exceeded() {
        let base = base_dir();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("high.md"), "高优模块").unwrap();
        fs::write(context_dir.join("mid.md"), "中优模块").unwrap();
        fs::write(context_dir.join("low.md"), "低优模块").unwrap();
        let index = write_index(
            &base,
            r#"
version = 1

[limits]
max_dynamic_modules = 3
max_total_chars = 9

[[modules]]
id = "high"
file = "context/high.md"
keywords = ["部署"]
priority = 100

[[modules]]
id = "mid"
file = "context/mid.md"
keywords = ["部署"]
priority = 80

[[modules]]
id = "low"
file = "context/low.md"
keywords = ["部署"]
priority = 60
"#,
        );

        let prompts = load_context_module_prompts(&index, "部署一下").unwrap();

        assert_eq!(prompts, vec!["高优模块".to_owned(), "中优模块".to_owned()]);
    }

    #[test]
    fn always_modules_exceeding_budget_return_clear_error() {
        let base = base_dir();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(
            context_dir.join("always.md"),
            "这里是一段明显超预算的常驻模块",
        )
        .unwrap();
        let index = write_index(
            &base,
            r#"
version = 1

[limits]
max_dynamic_modules = 1
max_total_chars = 4

[[modules]]
id = "always"
file = "context/always.md"
always = true
"#,
        );

        let err = load_context_module_prompts(&index, "随便聊聊").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(
            err.message
                .contains("always context modules exceed max_total_chars")
        );
    }

    #[test]
    fn empty_module_file_returns_clear_error() {
        let base = base_dir();
        let context_dir = base.join("context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(context_dir.join("deploy.md"), "\n").unwrap();
        let index = write_index(
            &base,
            r#"
version = 1

[limits]
max_dynamic_modules = 1
max_total_chars = 64

[[modules]]
id = "deploy"
file = "context/deploy.md"
keywords = ["部署"]
"#,
        );

        let err = load_context_module_prompts(&index, "部署一下").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(
            err.message
                .contains("context module file for module deploy is empty")
        );
    }

    #[test]
    fn path_escape_returns_clear_error() {
        let base = base_dir();
        let index = write_index(
            &base,
            r#"
version = 1

[limits]
max_dynamic_modules = 1
max_total_chars = 64

[[modules]]
id = "deploy"
file = "../deploy.md"
keywords = ["部署"]
"#,
        );

        let err = load_context_module_prompts(&index, "部署一下").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(
            err.message
                .contains("context module path escapes index directory")
        );
    }

    #[test]
    fn absolute_path_returns_clear_error() {
        let base = base_dir();
        let absolute_file = base.join("context").join("deploy.md");
        let index = write_index(
            &base,
            &format!(
                r#"
version = 1

[limits]
max_dynamic_modules = 1
max_total_chars = 64

[[modules]]
id = "deploy"
file = "{}"
keywords = ["部署"]
"#,
                absolute_file.display()
            ),
        );

        let err = load_context_module_prompts(&index, "部署一下").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(
            err.message
                .contains("context module path must be relative to index directory")
        );
    }
}
