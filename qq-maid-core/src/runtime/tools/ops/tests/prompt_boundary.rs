use super::*;

#[cfg(unix)]
#[test]
fn codex_prompt_is_always_after_option_terminator() {
    let script = write_script("exit 0");
    let working_directory = write_working_directory();
    let config = codex_ops_config(&script, &working_directory, 3, 1);
    let long_chinese = "检查并修复构建、测试和文档一致性。".repeat(100);
    let prompts = [
        long_chinese.as_str(),
        "review",
        "resume",
        "--help",
        "--json",
        "--ignore-rules",
        "--dangerously-bypass-approvals-and-sandbox",
        "-仍然只是任务描述",
        " 保留空格、\"引号\"、换行\n以及 ; | $() `反引号` ",
    ];

    for prompt in prompts {
        let argv = codex_argv(&config.codex, prompt);
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(argv.last().map(String::as_str), Some(prompt));
        assert_eq!(argv.iter().filter(|arg| arg.as_str() == prompt).count(), 1);
        assert_eq!(argv[0], "exec");
        assert_eq!(argv[1], "--skip-git-repo-check");
        assert_eq!(argv[2], "--ephemeral");
        assert_eq!(argv[4], "qq-maid-ops");
        assert_eq!(argv[6], "workspace-write");
        assert_eq!(argv[8], working_directory.to_str().unwrap());
    }
}

#[cfg(unix)]
#[test]
fn codex_rejects_exact_stdin_prompt_marker() {
    let script = write_script("exit 0");
    let store = test_store();
    let service = OpsService::new(
        codex_ops_config(&script, &write_working_directory(), 3, 1),
        store.clone(),
    );

    let reply = service.accept(
        parse_ops_command("/ops codex -").unwrap(),
        private_context(Some("admin-1")),
    );

    assert!(reply.contains("任务描述不合法"));
    assert!(store.list_all_for_test().unwrap().is_empty());
}
