use std::process::Command;

#[test]
fn invalid_cli_command_logs_once_and_exits_nonzero() {
    let output = Command::new(env!("CARGO_BIN_EXE_qq-maid-bot"))
        .env_clear()
        .args(["definitely-not-a-command"])
        .output()
        .expect("qq-maid-bot should run");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("qq-maid-bot 执行失败").count(),
        1,
        "{stderr}"
    );
    assert_eq!(stderr.matches("未知命令").count(), 1, "{stderr}");
    assert!(!stderr.contains("Error:"), "{stderr}");
}
