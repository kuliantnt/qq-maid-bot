use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use regex::Regex;
use serde::Deserialize;

use crate::error::LlmError;

pub const OPS_CONFIG_FILE_ENV: &str = "OPS_CONFIG_FILE";
pub const DEFAULT_OPS_CONFIG_PATH: &str = "config/ops.toml";

const MAX_COMMANDS: usize = 64;
const MAX_ARGS: usize = 16;
const MAX_ARG_BYTES: usize = 1024;
const MAX_TIMEOUT_SECONDS: u64 = 3600;
const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const MAX_CODEX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_CODEX_CONCURRENT_TASKS: usize = 8;
const RESERVED_COMMAND_NAMES: &[&str] = &["codex", "list", "cancel", "stop", "kill", "close"];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsConfig {
    pub enabled: bool,
    pub private: OpsPrivateConfig,
    pub group: OpsGroupConfig,
    pub codex: OpsCodexConfig,
    pub commands: BTreeMap<String, OpsCommandConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsPrivateConfig {
    pub enabled: bool,
    pub allowed_user_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsGroupConfig {
    pub enabled: bool,
    pub allowed_group_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsCodexConfig {
    pub enabled: bool,
    pub program: String,
    pub working_directory: String,
    pub timeout_seconds: u64,
    pub max_prompt_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub profile: String,
    pub sandbox: String,
    pub cancellable: bool,
    pub max_concurrent_tasks: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpsCommandConfig {
    pub program: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_stdout_bytes")]
    pub max_stdout_bytes: usize,
    #[serde(default = "default_stderr_bytes")]
    pub max_stderr_bytes: usize,
    #[serde(default)]
    pub min_args: usize,
    #[serde(default)]
    pub max_args: usize,
    #[serde(default)]
    pub args: BTreeMap<usize, OpsArgRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsArgRule {
    pub allowed_values: Vec<String>,
    pub pattern: Option<String>,
}

impl OpsConfig {
    pub fn load() -> Result<Self, LlmError> {
        let override_path = env::var(OPS_CONFIG_FILE_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let path = override_path
            .clone()
            .unwrap_or_else(|| DEFAULT_OPS_CONFIG_PATH.to_owned());
        if !Path::new(&path).is_file() {
            if override_path.is_some() {
                return Err(LlmError::config(format!(
                    "{OPS_CONFIG_FILE_ENV} points to missing file `{path}`"
                )));
            }
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).map_err(|err| {
            LlmError::config(format!(
                "failed to read {OPS_CONFIG_FILE_ENV} `{path}`: {err}"
            ))
        })?;
        Self::from_toml(&text).map_err(|err| {
            LlmError::config(format!("invalid {OPS_CONFIG_FILE_ENV} `{path}`: {err}"))
        })
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|err| err.to_string())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        validate_id_list("private.allowed_user_ids", &self.private.allowed_user_ids)?;
        validate_id_list("group.allowed_group_ids", &self.group.allowed_group_ids)?;
        if self.commands.len() > MAX_COMMANDS {
            return Err(format!(
                "commands must contain at most {MAX_COMMANDS} entries"
            ));
        }
        self.codex.validate()?;
        for (name, command) in &self.commands {
            if !valid_command_name(name) {
                return Err(format!("invalid ops command name `{name}`"));
            }
            if RESERVED_COMMAND_NAMES.contains(&name.as_str()) {
                return Err(format!(
                    "commands.{name} uses a reserved built-in ops command name"
                ));
            }
            command.validate(name)?;
        }
        Ok(())
    }
}

impl OpsCodexConfig {
    fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        validate_absolute_file("codex.program", &self.program)?;
        validate_absolute_directory("codex.working_directory", &self.working_directory)?;
        if self.timeout_seconds == 0 || self.timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(format!(
                "codex.timeout_seconds must be between 1 and {MAX_TIMEOUT_SECONDS}"
            ));
        }
        if self.max_prompt_bytes == 0 || self.max_prompt_bytes > MAX_CODEX_PROMPT_BYTES {
            return Err(format!(
                "codex.max_prompt_bytes must be between 1 and {MAX_CODEX_PROMPT_BYTES}"
            ));
        }
        validate_capture_limit("codex.max_stdout_bytes", self.max_stdout_bytes)?;
        validate_capture_limit("codex.max_stderr_bytes", self.max_stderr_bytes)?;
        if self.profile.is_empty()
            || self.profile.len() > 128
            || !self
                .profile
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err("codex.profile contains unsupported characters".to_owned());
        }
        if !matches!(self.sandbox.as_str(), "read-only" | "workspace-write") {
            return Err(
                "codex.sandbox must be `read-only` or `workspace-write`; dangerous mode is forbidden"
                    .to_owned(),
            );
        }
        self.canonical_program_directory()?;
        if self.max_concurrent_tasks == 0 || self.max_concurrent_tasks > MAX_CODEX_CONCURRENT_TASKS
        {
            return Err(format!(
                "codex.max_concurrent_tasks must be between 1 and {MAX_CODEX_CONCURRENT_TASKS}"
            ));
        }
        Ok(())
    }

    /// PATH 只前置已解析符号链接和 `..` 的程序父目录，避免把工作区可写目录交给
    /// `/usr/bin/env node` 之类的解释器搜索。
    pub(super) fn canonical_program_directory(&self) -> Result<PathBuf, String> {
        let program_parent = Path::new(&self.program)
            .parent()
            .ok_or_else(|| "codex.program has no parent directory".to_owned())?;
        let program_directory = fs::canonicalize(program_parent).map_err(|_| {
            "codex.program parent directory could not be safely canonicalized".to_owned()
        })?;
        if self.sandbox == "workspace-write" {
            let working_directory = fs::canonicalize(&self.working_directory).map_err(|_| {
                "codex.working_directory could not be safely canonicalized".to_owned()
            })?;
            if program_directory.starts_with(&working_directory) {
                return Err(
                    "codex.program parent directory must be outside codex.working_directory when sandbox is `workspace-write`"
                        .to_owned(),
                );
            }
        }
        Ok(program_directory)
    }
}

impl OpsCommandConfig {
    fn validate(&self, name: &str) -> Result<(), String> {
        let program = self.program.trim();
        if program.is_empty() || program.contains('\0') {
            return Err(format!("commands.{name}.program is invalid"));
        }
        if !Path::new(program).is_absolute() {
            return Err(format!("commands.{name}.program must be an absolute path"));
        }
        if self.timeout_seconds == 0 || self.timeout_seconds > MAX_TIMEOUT_SECONDS {
            return Err(format!(
                "commands.{name}.timeout_seconds must be between 1 and {MAX_TIMEOUT_SECONDS}"
            ));
        }
        for (field, value) in [
            ("max_stdout_bytes", self.max_stdout_bytes),
            ("max_stderr_bytes", self.max_stderr_bytes),
        ] {
            validate_capture_limit(&format!("commands.{name}.{field}"), value)?;
        }
        if self.min_args > self.max_args || self.max_args > MAX_ARGS {
            return Err(format!(
                "commands.{name} requires 0 <= min_args <= max_args <= {MAX_ARGS}"
            ));
        }
        for (index, rule) in &self.args {
            if *index >= self.max_args {
                return Err(format!(
                    "commands.{name}.args.{index} is outside max_args {}",
                    self.max_args
                ));
            }
            rule.validate(name, *index)?;
        }
        Ok(())
    }

    pub(super) fn validate_args(&self, args: &[String]) -> Result<(), String> {
        if args.len() < self.min_args {
            return Err(format!("至少需要 {} 个参数", self.min_args));
        }
        if args.len() > self.max_args {
            return Err(format!("最多允许 {} 个参数", self.max_args));
        }
        for (index, value) in args.iter().enumerate() {
            if value.is_empty()
                || value.len() > MAX_ARG_BYTES
                || value.chars().any(|ch| ch == '\0' || ch.is_control())
            {
                return Err(format!("第 {} 个参数包含不允许的内容", index + 1));
            }
            let Some(rule) = self.args.get(&index) else {
                continue;
            };
            if !rule.allowed_values.is_empty()
                && !rule.allowed_values.iter().any(|allowed| allowed == value)
            {
                return Err(format!("第 {} 个参数不在允许值中", index + 1));
            }
            if let Some(pattern) = rule.pattern.as_deref() {
                let regex = full_match_regex(pattern)
                    .expect("ops argument regex is validated when config is loaded");
                if !regex.is_match(value) {
                    return Err(format!("第 {} 个参数格式不正确", index + 1));
                }
            }
        }
        Ok(())
    }
}

impl Default for OpsCodexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            program: String::new(),
            working_directory: String::new(),
            timeout_seconds: 1800,
            max_prompt_bytes: 8000,
            max_stdout_bytes: 32 * 1024,
            max_stderr_bytes: 16 * 1024,
            profile: "qq-maid-ops".to_owned(),
            sandbox: "workspace-write".to_owned(),
            cancellable: true,
            max_concurrent_tasks: 1,
        }
    }
}

impl OpsArgRule {
    fn validate(&self, name: &str, index: usize) -> Result<(), String> {
        if self.allowed_values.is_empty() && self.pattern.is_none() {
            return Err(format!(
                "commands.{name}.args.{index} requires allowed_values or pattern"
            ));
        }
        for value in &self.allowed_values {
            if value.is_empty()
                || value.len() > MAX_ARG_BYTES
                || value.chars().any(|ch| ch == '\0' || ch.is_control())
            {
                return Err(format!(
                    "commands.{name}.args.{index}.allowed_values contains an invalid value"
                ));
            }
        }
        if let Some(pattern) = self.pattern.as_deref() {
            if pattern.len() > 512 {
                return Err(format!("commands.{name}.args.{index}.pattern is too long"));
            }
            full_match_regex(pattern)
                .map_err(|err| format!("commands.{name}.args.{index}.pattern is invalid: {err}"))?;
        }
        Ok(())
    }
}

fn validate_id_list(name: &str, values: &[String]) -> Result<(), String> {
    for value in values {
        if value.trim().is_empty()
            || value != value.trim()
            || value.len() > 256
            || value.chars().any(char::is_control)
        {
            return Err(format!("{name} contains an invalid stable ID"));
        }
    }
    Ok(())
}

fn validate_capture_limit(name: &str, value: usize) -> Result<(), String> {
    if value > MAX_CAPTURE_BYTES {
        return Err(format!("{name} must not exceed {MAX_CAPTURE_BYTES}"));
    }
    Ok(())
}

fn validate_absolute_file(name: &str, value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim() != value || !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    if !path.is_file() {
        return Err(format!("{name} must point to an existing file"));
    }
    Ok(())
}

fn validate_absolute_directory(name: &str, value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim() != value || !path.is_absolute() {
        return Err(format!("{name} must be an absolute path"));
    }
    if !path.is_dir() {
        return Err(format!("{name} must point to an existing directory"));
    }
    Ok(())
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn full_match_regex(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("\\A(?:{pattern})\\z"))
}

const fn default_timeout_seconds() -> u64 {
    30
}

const fn default_stdout_bytes() -> usize {
    4096
}

const fn default_stderr_bytes() -> usize {
    4096
}
