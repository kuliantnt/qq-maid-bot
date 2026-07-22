//! 统一程序入口。
//!
//! 该入口一次性完成 dotenv / tracing 初始化，组装 CoreHandle、Gateway 和主动推送
//! sink。Core 与 Gateway 之间只走进程内强类型调用，不再通过 localhost HTTP 探活或通信。

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use qq_maid_core::{
    app::{LlmRuntime as CoreRuntime, ManagementRuntime},
    config::{
        AppConfig as CoreConfig, center::ConfigCenter, center::ConfigCenterPaths,
        database_bootstrap_from_environment, ensure_default_agent_config,
        install_resolved_environment, management_bootstrap_from_environment,
    },
    management::AdminAuth,
    storage::identity_rebaseline::rebaseline_qq_official_identity,
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};
use qq_maid_gateway_rs::{
    config::AppConfig as GatewayConfig,
    gateway::{
        console::GatewayConsoleStatusSource, ping::GatewayRuntimeStatus, push::GatewayPushSink,
    },
    respond::RespondClient,
};
use time::{UtcOffset, macros::format_description};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const OPS_HTTP_SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
const BUILD_COMMIT: &str = match option_env!("QQ_MAID_BUILD_COMMIT") {
    Some(value) => value,
    None => "unknown",
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    qq_maid_core::app::load_dotenv_files();
    init_tracing()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = BUILD_COMMIT,
        "qq-maid-bot starting"
    );

    let external_environment = std::env::vars().collect::<HashMap<_, _>>();
    let (database_file, database_pool_size) =
        database_bootstrap_from_environment(&external_environment)?;
    let database =
        SqliteDatabase::open_with_pool_size(&database_file, APP_MIGRATIONS, database_pool_size)?;
    let mut managed_fields = qq_maid_core::config::managed_config_fields();
    managed_fields.extend(qq_maid_gateway_rs::config::managed_config_fields());
    let config_center_paths = ConfigCenterPaths::from_environment(&external_environment);
    let bootstrap_token_file = config_center_paths
        .master_key_file
        .with_file_name("bootstrap.token");
    let config_center = ConfigCenter::open(managed_fields, config_center_paths, database.clone())?
        .with_external_environment(external_environment.clone());
    let resolved_environment = config_center.resolved_environment(&external_environment)?;
    ensure_default_agent_config(&resolved_environment)?;
    install_resolved_environment(resolved_environment.clone())?;
    let management_bootstrap = management_bootstrap_from_environment(&resolved_environment)?;
    let admin_auth = AdminAuth::open_if_enabled(
        database.clone(),
        bootstrap_token_file,
        management_bootstrap.web_console_enabled,
    )?;
    let core_config = CoreConfig::from_env();
    let mut config_center = config_center.with_startup_preflight(preflight_candidate_startup);
    if let Ok(config) = core_config.as_ref() {
        config_center = config_center.with_running_agent_config(config.agent_config.clone())?;
    }
    let gateway_config = GatewayConfig::from_map(&resolved_environment);
    let runtime_ready = core_config.is_ok()
        && gateway_config
            .as_ref()
            .is_ok_and(GatewayConfig::has_enabled_channel)
        && preflight_candidate_startup(&resolved_environment, None).is_ok();
    if !runtime_ready {
        warn!(
            state = "setup_required",
            core_config_valid = core_config.is_ok(),
            gateway_config_valid = gateway_config.is_ok(),
            "业务配置尚未完成，仅启动受保护管理入口"
        );
        return ManagementRuntime::new(
            management_bootstrap,
            config_center.with_incomplete_setup_writes(),
            admin_auth,
            env!("CARGO_PKG_VERSION"),
        )?
        .serve_with_shutdown(shutdown_signal())
        .await;
    }
    let core_config = core_config.expect("runtime readiness checked Core configuration");
    let gateway_config = gateway_config.expect("runtime readiness checked Gateway configuration");
    if let Some(app_id) = gateway_config.app_id.as_deref() {
        let rebaseline_report = rebaseline_qq_official_identity(&core_config.app_db_file, app_id)?;
        if rebaseline_report.changed() {
            info!(
                sessions = rebaseline_report.sessions,
                session_active = rebaseline_report.session_active,
                memories = rebaseline_report.memories,
                todos = rebaseline_report.todos,
                rss_subscriptions = rebaseline_report.rss_subscriptions,
                rss_duplicates_removed = rebaseline_report.rss_duplicates_removed,
                "已完成旧 QQ 业务归属键归一"
            );
        }
    }

    let push_sink = GatewayPushSink::unbound();
    let gateway_runtime = GatewayRuntimeStatus::new();
    let console_status_source = Arc::new(GatewayConsoleStatusSource::new(
        gateway_config.clone(),
        gateway_runtime.clone(),
    ));
    let core_runtime = CoreRuntime::from_config_with_database_push_sink_and_console_source(
        core_config,
        database,
        Some(config_center),
        admin_auth,
        Some(Arc::new(push_sink.clone())),
        console_status_source,
        env!("CARGO_PKG_VERSION"),
    )?;
    let core_handle = core_runtime.core_handle();
    let (core_shutdown_tx, core_shutdown_rx) = oneshot::channel::<()>();
    let mut core_http_handle = tokio::spawn(async move {
        core_runtime
            .serve_with_shutdown(async move {
                let _ = core_shutdown_rx.await;
            })
            .await
    });
    let respond = match gateway_config.app_id.as_deref() {
        Some(app_id) => {
            RespondClient::new(Arc::new(core_handle)).with_qq_official_account_id(app_id)
        }
        None => RespondClient::new(Arc::new(core_handle)),
    };
    info!("Core 已完成进程内初始化，开始启动 Gateway");
    let shutdown_token = CancellationToken::new();
    let gateway_shutdown = shutdown_token.clone();
    let mut gateway_handle = tokio::spawn(async move {
        qq_maid_gateway_rs::app::run_with_config_with_shutdown_and_status(
            gateway_config,
            respond,
            push_sink,
            gateway_runtime,
            gateway_shutdown,
        )
        .await
    });

    tokio::select! {
        _ = shutdown_signal() => {
            info!("收到退出信号，准备停止统一进程");
            shutdown_token.cancel();
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut gateway_handle).await;
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut core_http_handle).await;
            Ok(())
        }
        result = &mut core_http_handle => {
            shutdown_token.cancel();
            let _ = gateway_handle.await;
            Err(task_exit_error("qq-maid-core-ops-http", result))
        }
        result = &mut gateway_handle => {
            shutdown_token.cancel();
            let _ = core_shutdown_tx.send(());
            let _ = tokio::time::timeout(OPS_HTTP_SHUTDOWN_WAIT, &mut core_http_handle).await;
            Err(task_exit_error("qq-maid-gateway-rs", result))
        }
    }
}

/// Docker 与 systemd 使用 SIGTERM，交互式终端使用 SIGINT；两者必须进入同一收尾链路。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// 根程序同时依赖 Core、LLM 与 Gateway；在这里组合真实启动预检可保持依赖方向，
/// 同时避免配置中心复制跨字段和 Provider 路由规则。错误不拼接候选值，防止 secret 泄漏。
fn preflight_candidate_startup(
    environment: &HashMap<String, String>,
    candidate_agent: Option<&qq_maid_core::config::AgentRuntimeConfig>,
) -> Result<(), String> {
    CoreConfig::preflight_environment(environment, candidate_agent)
        .map_err(|_| "candidate Core/LLM configuration is invalid".to_owned())?;
    let gateway = GatewayConfig::from_map(environment)
        .map_err(|_| "candidate Gateway configuration is invalid".to_owned())?;
    if !gateway.has_enabled_channel() {
        return Err("candidate configuration has no enabled Gateway channel".to_owned());
    }
    Ok(())
}

fn task_exit_error(
    task_name: &str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Error {
    match result {
        Ok(Ok(())) => anyhow!("{task_name} 意外退出"),
        Ok(Err(err)) => err.context(format!("{task_name} 运行失败")),
        Err(err) => anyhow!("{task_name} 任务结束异常: {err}"),
    }
}

fn init_tracing() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(false)
                .with_timer(shanghai_log_timer()),
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info")
        }))
        .try_init()?;
    Ok(())
}

fn shanghai_log_timer() -> impl tracing_subscriber::fmt::time::FormatTime {
    fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).expect("valid Asia/Shanghai UTC offset"),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_core::config::center::{
        ManagedConfigChange, SECRET_MISSING_REVISION, SecretConfigChange,
    };
    use std::path::{Path, PathBuf};
    use toml::Value;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "qq-maid-root-config-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_config_center(
        name: &str,
        additional_external: &[(&str, &str)],
    ) -> (ConfigCenter, SqliteDatabase, TestDirectory) {
        let directory = TestDirectory::new(name);
        let database =
            SqliteDatabase::open_with_pool_size(directory.path().join("app.db"), APP_MIGRATIONS, 1)
                .unwrap();
        let mut fields = qq_maid_core::config::managed_config_fields();
        fields.extend(qq_maid_gateway_rs::config::managed_config_fields());
        let mut external = HashMap::from([
            (
                "AGENT_CONFIG_FILE".to_owned(),
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("runtime/config/agent.toml")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("OPENAI_API_KEY".to_owned(), "test-provider-key".to_owned()),
        ]);
        external.extend(
            additional_external
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        let center = ConfigCenter::open(
            fields,
            ConfigCenterPaths {
                managed_config_file: directory.path().join("runtime.toml"),
                master_key_file: directory.path().join("secrets/master.key"),
            },
            database.clone(),
        )
        .unwrap()
        .with_external_environment(external)
        .with_startup_preflight(preflight_candidate_startup);
        (center, database, directory)
    }

    fn provider_config_center(
        name: &str,
    ) -> (ConfigCenter, SqliteDatabase, TestDirectory, PathBuf) {
        let directory = TestDirectory::new(name);
        let database =
            SqliteDatabase::open_with_pool_size(directory.path().join("app.db"), APP_MIGRATIONS, 1)
                .unwrap();
        let agent_path = directory.path().join("config/agent.toml");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        std::fs::write(&agent_path, include_str!("../runtime/config/agent.toml")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // 配置中心会拒绝组或其他用户可写的 agent 配置。测试不能依赖宿主机
            // umask，否则 umask 0002 会把夹具创建成 0775/0664 并提前触发安全拒绝。
            std::fs::set_permissions(
                agent_path.parent().unwrap(),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            std::fs::set_permissions(&agent_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let external = HashMap::from([
            (
                "AGENT_CONFIG_FILE".to_owned(),
                agent_path.to_string_lossy().into_owned(),
            ),
            ("QQ_BOT_APP_ID".to_owned(), "test-qq-app".to_owned()),
            ("QQ_BOT_APP_SECRET".to_owned(), "test-qq-secret".to_owned()),
        ]);
        let running_agent =
            qq_maid_core::config::AgentRuntimeConfig::load_from_environment(&external).unwrap();
        let mut fields = qq_maid_core::config::managed_config_fields();
        fields.extend(qq_maid_gateway_rs::config::managed_config_fields());
        let center = ConfigCenter::open(
            fields,
            ConfigCenterPaths {
                managed_config_file: directory.path().join("runtime.toml"),
                master_key_file: directory.path().join("secrets/master.key"),
            },
            database.clone(),
        )
        .unwrap()
        .with_external_environment(external)
        .with_running_agent_config(running_agent)
        .unwrap();
        (center, database, directory, agent_path)
    }

    #[test]
    fn core_and_gateway_managed_fields_form_one_valid_registry() {
        let mut fields = qq_maid_core::config::managed_config_fields();
        fields.extend(qq_maid_gateway_rs::config::managed_config_fields());
        qq_maid_core::config::center::ConfigRegistry::new(fields).unwrap();
    }

    #[test]
    fn clearing_last_provider_key_is_rejected_and_secret_is_unchanged() {
        let (center, _database, _directory, _agent_path) =
            provider_config_center("provider-last-key");
        let original_revision = center
            .replace_secret(
                "provider.openai.api_key",
                "original-openai-key",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        let center = center.with_startup_preflight(preflight_candidate_startup);

        let error = center
            .clear_secret("provider.openai.api_key", &original_revision)
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        let snapshot = center.current_snapshot().unwrap();
        let secret = snapshot
            .fields
            .iter()
            .find(|field| field.key == "provider.openai.api_key")
            .unwrap();
        assert!(secret.configured);
        assert_eq!(secret.revision.as_deref(), Some(original_revision.as_str()));
    }

    #[test]
    fn setup_mode_persists_partial_valid_fields_without_claiming_ready() {
        let (center, _database, _directory) = test_config_center("setup-partial", &[]);
        let center = center
            .with_startup_preflight(preflight_candidate_startup)
            .with_incomplete_setup_writes();

        center
            .replace_secret(
                "platform.qq_official.app_id",
                "partial-qq-app-id",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        let snapshot = center.current_snapshot().unwrap();
        let field = snapshot
            .fields
            .iter()
            .find(|field| field.key == "platform.qq_official.app_id")
            .unwrap();
        assert!(field.configured);
        assert!(!field.valid, "partial setup must not be reported ready");
    }

    #[test]
    fn clearing_one_provider_key_succeeds_when_every_route_has_another_provider() {
        let (center, _database, _directory, _agent_path) =
            provider_config_center("provider-fallback-key");
        let openai_revision = center
            .replace_secret(
                "provider.openai.api_key",
                "openai-key",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        center
            .replace_secret(
                "provider.deepseek.api_key",
                "deepseek-key",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        let center = center.with_startup_preflight(preflight_candidate_startup);

        let revision = center
            .clear_secret("provider.openai.api_key", &openai_revision)
            .unwrap();
        assert_eq!(revision, SECRET_MISSING_REVISION);
        let environment = center.resolved_environment(&HashMap::new()).unwrap();
        assert!(!environment.contains_key("OPENAI_API_KEY"));
        assert!(environment.contains_key("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn undeclared_custom_provider_route_is_rejected_before_agent_replace() {
        let (center, _database, _directory, agent_path) =
            provider_config_center("agent-undeclared-provider");
        center
            .replace_secret(
                "provider.openai.api_key",
                "openai-key",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        let center = center.with_startup_preflight(preflight_candidate_startup);
        let initial = center.current_snapshot().unwrap().agent.unwrap();
        let before = std::fs::read(&agent_path).unwrap();

        let error = center
            .update_agent(
                &initial.revision,
                &[
                    qq_maid_core::config::center::AgentConfigChange::SetModelRoute {
                        name: "private_main".to_owned(),
                        candidates: vec!["custom_provider:model".to_owned()],
                    },
                ],
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        assert_eq!(std::fs::read(agent_path).unwrap(), before);
    }

    #[test]
    fn declared_custom_provider_route_with_key_saves_successfully() {
        let (center, _database, _directory, agent_path) =
            provider_config_center("agent-declared-provider");
        center
            .replace_secret(
                "provider.openai.api_key",
                "openai-key",
                SECRET_MISSING_REVISION,
            )
            .unwrap();
        center
            .replace_secret("provider.mimo.api_key", "mimo-key", SECRET_MISSING_REVISION)
            .unwrap();
        let center = center.with_startup_preflight(preflight_candidate_startup);
        let initial = center.current_snapshot().unwrap().agent.unwrap();

        center
            .update_agent(
                &initial.revision,
                &[
                    qq_maid_core::config::center::AgentConfigChange::SetModelRoute {
                        name: "private_main".to_owned(),
                        candidates: vec!["mimo:mimo-v2.5".to_owned()],
                    },
                ],
            )
            .unwrap();
        let text = std::fs::read_to_string(agent_path).unwrap();
        assert!(text.contains("candidates = [\"mimo:mimo-v2.5\"]"));
    }

    #[test]
    fn onebot_requires_token_but_accepts_external_override() {
        let (center, _database, _directory) = test_config_center("onebot-invalid", &[]);
        let initial_revision = center.current_snapshot().unwrap().revision;
        let error = center
            .update_managed(
                &initial_revision,
                &[ManagedConfigChange::Set {
                    key: "platform.onebot11.enabled".to_owned(),
                    value: Value::Boolean(true),
                }],
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        assert_eq!(
            center.current_snapshot().unwrap().revision,
            initial_revision
        );

        let (center, _database, _directory) = test_config_center(
            "onebot-external",
            &[("ONEBOT11_ACCESS_TOKEN", "external-token")],
        );
        let initial_revision = center.current_snapshot().unwrap().revision;
        center
            .update_managed(
                &initial_revision,
                &[ManagedConfigChange::Set {
                    key: "platform.onebot11.enabled".to_owned(),
                    value: Value::Boolean(true),
                }],
            )
            .unwrap();
    }

    #[test]
    fn wechat_aes_uses_gateway_completeness_and_key_validation() {
        let (center, _database, _directory) = test_config_center(
            "wechat-aes-missing",
            &[("WECHAT_SERVICE_TOKEN", "external-token")],
        );
        let initial_revision = center.current_snapshot().unwrap().revision;
        let error = center
            .update_managed(
                &initial_revision,
                &[
                    ManagedConfigChange::Set {
                        key: "platform.wechat_service.enabled".to_owned(),
                        value: Value::Boolean(true),
                    },
                    ManagedConfigChange::Set {
                        key: "platform.wechat_service.encryption_mode".to_owned(),
                        value: Value::String("aes".to_owned()),
                    },
                ],
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        assert_eq!(
            center.current_snapshot().unwrap().revision,
            initial_revision
        );

        let error = center
            .replace_secret(
                "platform.wechat_service.encoding_aes_key",
                "invalid-key-must-not-appear-in-error",
                SECRET_MISSING_REVISION,
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");
        assert!(
            !error
                .message()
                .contains("invalid-key-must-not-appear-in-error")
        );
    }

    #[test]
    fn qq_credentials_require_atomic_batch_and_snapshot_stays_redacted() {
        let (center, _database, _directory) = test_config_center("qq-batch", &[]);
        let error = center
            .replace_secret(
                "platform.qq_official.app_id",
                "qq-app-id-plaintext",
                SECRET_MISSING_REVISION,
            )
            .unwrap_err();
        assert_eq!(error.code(), "invalid_config");

        center
            .update_secrets(&[
                SecretConfigChange::Replace {
                    key: "platform.qq_official.app_id".to_owned(),
                    value: "qq-app-id-plaintext".to_owned(),
                    expected_revision: SECRET_MISSING_REVISION.to_owned(),
                },
                SecretConfigChange::Replace {
                    key: "platform.qq_official.app_secret".to_owned(),
                    value: "qq-app-secret-plaintext".to_owned(),
                    expected_revision: SECRET_MISSING_REVISION.to_owned(),
                },
            ])
            .unwrap();
        let snapshot = center.current_snapshot().unwrap();
        assert!(snapshot.fields.iter().all(|field| field.valid));
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("qq-app-id-plaintext"));
        assert!(!json.contains("qq-app-secret-plaintext"));
    }
}
