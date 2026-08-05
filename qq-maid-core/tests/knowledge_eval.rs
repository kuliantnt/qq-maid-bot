use std::{path::PathBuf, process::Command, time::SystemTime};

#[test]
fn failed_knowledge_eval_logs_once_and_exits_nonzero() {
    let missing_dataset = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "target/qq-maid-knowledge-eval-missing-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_nanos()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_knowledge-eval"))
        .env_clear()
        .env("RUST_LOG", "info")
        .arg(missing_dataset)
        .output()
        .expect("knowledge-eval should run");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("knowledge eval failed").count(),
        1,
        "{stderr}"
    );
    assert!(!stderr.contains("Error:"), "{stderr}");
}
