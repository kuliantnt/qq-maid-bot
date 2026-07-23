use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-config-check-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("test directory should be created");
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

fn run_config_check(directory: &Path, extra_environment: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qq-maid-bot"));
    command
        .current_dir(directory)
        .env_clear()
        .args(["config", "check"]);
    for (name, value) in extra_environment {
        command.env(name, value);
    }
    command.output().expect("config check should run")
}

#[test]
fn fresh_install_config_check_uses_embedded_agent_template_without_writes() {
    let directory = TestDirectory::new("fresh");

    let output = run_config_check(directory.path(), &[]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("agent_config=valid"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!directory.path().join("config/agent.toml").exists());
    assert!(!directory.path().join("config").exists());
    assert!(!directory.path().join("data/storage/app.db").exists());
}

#[test]
fn config_check_rejects_missing_explicit_agent_config() {
    let directory = TestDirectory::new("explicit-missing");
    let missing = directory.path().join("custom/missing-agent.toml");

    let output = run_config_check(
        directory.path(),
        &[("AGENT_CONFIG_FILE", missing.as_path())],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Agent 配置无效"), "stderr: {stderr}");
    assert!(stderr.contains("missing file"), "stderr: {stderr}");
    assert!(!missing.exists());
}

#[test]
fn config_check_validates_existing_default_agent_config() {
    let directory = TestDirectory::new("existing-invalid");
    let config_directory = directory.path().join("config");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(config_directory.join("agent.toml"), "not valid toml = [").unwrap();

    let output = run_config_check(directory.path(), &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Agent 配置无效"), "stderr: {stderr}");
    assert!(
        stderr.contains("failed to parse agent config"),
        "stderr: {stderr}"
    );
}
