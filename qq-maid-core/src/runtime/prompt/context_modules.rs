use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use crate::error::LlmError;

use super::prompt_files::load_required_text_file;

const SUPPORTED_CONTEXT_MODULES_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
struct RawContextModulesFile {
    version: u32,
    limits: RawContextModuleLimits,
    #[serde(default)]
    modules: Vec<RawContextModule>,
}

#[derive(Debug, Deserialize)]
struct RawContextModuleLimits {
    max_dynamic_modules: usize,
    max_total_chars: usize,
}

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

#[derive(Debug)]
struct ContextModule {
    id: String,
    file: PathBuf,
    always: bool,
    keywords: Vec<String>,
    priority: i32,
    declaration_order: usize,
}

#[derive(Debug)]
struct LoadedModule {
    id: String,
    content: String,
    char_count: usize,
}

pub(super) fn load_context_module_prompts(
    index_file: &Path,
    user_text: &str,
) -> Result<Vec<String>, LlmError> {
    let config = ContextModulesFile::load(index_file)?;
    config.select_prompts(user_text)
}

impl ContextModulesFile {
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
    fn matches(&self, normalized_user_text: &str) -> bool {
        self.keywords
            .iter()
            .any(|keyword| normalized_user_text.contains(&keyword.to_lowercase()))
    }
}

fn load_module_content(module: &ContextModule) -> Result<LoadedModule, LlmError> {
    let label = format!("context module file for module {}", module.id);
    let content = load_required_text_file(&module.file, &label)?;
    Ok(LoadedModule {
        id: module.id.clone(),
        char_count: content.chars().count(),
        content,
    })
}

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
        return Ok(raw_path.to_path_buf());
    }
    let normalized = normalize_relative_path(raw_path).ok_or_else(|| {
        LlmError::config(format!(
            "context module path escapes index directory for module {module_id} in {}: {raw_file}",
            source_file.display()
        ))
    })?;
    Ok(base_dir.join(normalized))
}

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
}
