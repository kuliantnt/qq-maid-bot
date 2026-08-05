//! 启动配置文件的安全诊断。
//!
//! Agent 与 Ops 文件按契约不得保存 secret，因此可以报告文件路径以及校验错误的首行；
//! TOML 的源码片段可能包含稳定 ID，必须丢弃后续行，不能直接打印完整解析错误。

use std::collections::HashMap;

use qq_maid_core::{
    config::{
        AgentRuntimeConfig,
        agent::{AGENT_CONFIG_FILE_ENV, DEFAULT_AGENT_CONFIG_PATH},
    },
    runtime::tools::ops::{OPS_CONFIG_FILE_ENV, OpsConfig},
};

const DEFAULT_OPS_CONFIG_PATH: &str = "config/ops.toml";
const MAX_SAFE_REASON_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigFileIssue {
    pub component: &'static str,
    pub path: String,
    pub reason: String,
}

/// 独立校验会阻断 Core 构造的文件配置，以便一次启动同时报告多个坏文件。
pub(crate) fn collect_config_file_issues(
    environment: &HashMap<String, String>,
) -> Vec<ConfigFileIssue> {
    let mut issues = Vec::new();
    if let Err(error) = AgentRuntimeConfig::validate_for_read_only_check(environment) {
        issues.push(ConfigFileIssue {
            component: "agent",
            path: configured_path(
                environment,
                AGENT_CONFIG_FILE_ENV,
                DEFAULT_AGENT_CONFIG_PATH,
            ),
            reason: safe_file_error_reason(&error.message),
        });
    }
    if let Err(error) = OpsConfig::load_from_environment(environment) {
        issues.push(ConfigFileIssue {
            component: "ops",
            path: configured_path(environment, OPS_CONFIG_FILE_ENV, DEFAULT_OPS_CONFIG_PATH),
            reason: safe_file_error_reason(&error.message),
        });
    }
    issues
}

fn configured_path(environment: &HashMap<String, String>, variable: &str, default: &str) -> String {
    environment
        .get(variable)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_owned()
}

fn safe_file_error_reason(message: &str) -> String {
    let first_line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("configuration file is invalid");
    let mut chars = first_line.chars();
    let mut reason = chars
        .by_ref()
        .take(MAX_SAFE_REASON_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        reason.push('…');
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "qq-maid-config-diagnostics-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reports_missing_agent_and_invalid_ops_files_without_stable_ids() {
        let directory = TestDirectory::new("agent-ops");
        let missing_agent = directory.path().join("missing-agent.toml");
        let ops_file = directory.path().join("ops.toml");
        fs::write(
            &ops_file,
            r#"
enabled = true

[private]
enabled = true
allowed_user_ids = ["do-not-log-this-stable-id"]

[codex]
enabled = true
program = "/definitely/missing/codex"
working_directory = "/definitely/missing/workspace"
"#,
        )
        .unwrap();
        let environment = HashMap::from([
            (
                AGENT_CONFIG_FILE_ENV.to_owned(),
                missing_agent.to_string_lossy().into_owned(),
            ),
            (
                OPS_CONFIG_FILE_ENV.to_owned(),
                ops_file.to_string_lossy().into_owned(),
            ),
        ]);

        let issues = collect_config_file_issues(&environment);

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].component, "agent");
        assert_eq!(issues[0].path, missing_agent.to_string_lossy());
        assert!(issues[0].reason.contains("missing file"));
        assert_eq!(issues[1].component, "ops");
        assert_eq!(issues[1].path, ops_file.to_string_lossy());
        assert!(issues[1].reason.contains("codex.program"));
        assert!(issues[1].reason.contains("existing file"));
        assert!(!issues[1].reason.contains("do-not-log-this-stable-id"));
    }

    #[test]
    fn keeps_only_the_safe_first_line_of_toml_parse_errors() {
        let message = "invalid OPS_CONFIG_FILE: TOML parse error at line 2, column 1\n  |\n2 | allowed_user_ids = [\"private-id\"]";

        let reason = safe_file_error_reason(message);

        assert_eq!(
            reason,
            "invalid OPS_CONFIG_FILE: TOML parse error at line 2, column 1"
        );
        assert!(!reason.contains("private-id"));
    }
}
