use std::{collections::HashMap, ffi::OsString, path::PathBuf};

use anyhow::{anyhow, bail};
use qq_maid_core::{
    config::{
        AgentRuntimeConfig, AppConfig as CoreConfig,
        center::{ConfigCenter, ConfigCenterPaths},
        database_bootstrap_from_environment,
    },
    maintenance::{
        BackupOptions, ConfigMigrationAction, ConfigMigrationKind, apply_config_migration,
        create_backup, plan_config_migration, plan_restore, restore_backup, verify_backup,
    },
    storage::{
        APP_MIGRATIONS,
        database::{SqliteDatabase, SqliteMigrationPlan},
    },
};
use qq_maid_gateway_rs::config::AppConfig as GatewayConfig;

const HELP: &str = r#"qq-maid-bot

用法：
  qq-maid-bot [run]
  qq-maid-bot config check
  qq-maid-bot config sources
  qq-maid-bot config migrate [--env-file PATH ...] [--apply]
  qq-maid-bot migration status
  qq-maid-bot backup create --output DIR [--include-secrets]
  qq-maid-bot backup verify --from DIR
  qq-maid-bot backup restore --from DIR --target DIR [--apply]

config migrate 和 backup restore 默认只预检；只有显式 --apply 才写入。
恢复只接受不存在或为空的新实例目录，不覆盖当前运行目录。
"#;

pub fn dispatch(args: Vec<OsString>) -> anyhow::Result<bool> {
    let args = args
        .into_iter()
        .map(|value| {
            value
                .into_string()
                .map_err(|_| anyhow!("CLI 参数必须是有效 UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match args.first().map(String::as_str) {
        None | Some("run") => {
            if args.len() > 1 {
                bail!("run 不接受额外参数");
            }
            Ok(false)
        }
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(true)
        }
        Some("-V" | "--version" | "version") => {
            println!("qq-maid-bot {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        Some("config") => {
            run_config(&args[1..])?;
            Ok(true)
        }
        Some("migration") => {
            run_migration(&args[1..])?;
            Ok(true)
        }
        Some("backup") => {
            run_backup(&args[1..])?;
            Ok(true)
        }
        Some(command) => bail!("未知命令 '{command}'；运行 --help 查看用法"),
    }
}

fn run_config(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("check") if args.len() == 1 => config_check(),
        Some("sources") if args.len() == 1 => config_sources(),
        Some("migrate") => config_migrate(&args[1..]),
        _ => bail!("用法：qq-maid-bot config <check|sources|migrate>"),
    }
}

fn config_check() -> anyhow::Result<()> {
    let environment = current_environment();
    let (database_file, _) = database_bootstrap_from_environment(&environment)?;
    let migration = SqliteDatabase::inspect_migrations(&database_file, APP_MIGRATIONS)?;
    println!("database={database_file}");
    print_migration_plan(&migration);
    if !migration.unknown.is_empty() {
        bail!("数据库包含当前二进制不认识的 migration，禁止继续预检或降级启动");
    }
    let agent = AgentRuntimeConfig::load_from_environment(&environment);
    println!(
        "agent_config={}",
        if agent.is_ok() { "valid" } else { "invalid" }
    );

    let paths = ConfigCenterPaths::from_environment(&environment);
    let prerequisites_exist = migration.database_exists
        && migration.pending.is_empty()
        && paths.managed_config_file.is_file()
        && paths.master_key_file.is_file();
    if !prerequisites_exist {
        println!("startup_preflight=not_run");
        println!(
            "提示：当前实例尚需初始化或执行 migration；本次 check 保持只读，未创建文件或更新数据库。"
        );
        if let Err(error) = agent {
            bail!("Agent 配置无效：{error}");
        }
        return Ok(());
    }
    let (_, pool_size) = database_bootstrap_from_environment(&environment)?;
    let database = SqliteDatabase::open_with_pool_size(&database_file, APP_MIGRATIONS, pool_size)?;
    let center = ConfigCenter::open(all_managed_fields(), paths, database)?
        .with_external_environment(environment.clone());
    let resolved = center.resolved_environment(&environment)?;
    let core = CoreConfig::preflight_environment(&resolved, None);
    let gateway = GatewayConfig::from_map(&resolved);
    let gateway_valid = gateway
        .as_ref()
        .is_ok_and(GatewayConfig::has_enabled_channel);
    println!(
        "core_preflight={}",
        if core.is_ok() { "valid" } else { "invalid" }
    );
    println!(
        "gateway_preflight={}",
        if gateway_valid { "valid" } else { "invalid" }
    );
    core.map_err(|_| anyhow!("Core/Provider 配置预检失败（敏感值已隐藏）"))?;
    if !gateway_valid {
        bail!("Gateway 配置预检失败或没有启用入口（敏感值已隐藏）");
    }
    Ok(())
}

fn config_sources() -> anyhow::Result<()> {
    let environment = current_environment();
    let (database_file, pool_size) = database_bootstrap_from_environment(&environment)?;
    let paths = ConfigCenterPaths::from_environment(&environment);
    let migration = SqliteDatabase::inspect_migrations(&database_file, APP_MIGRATIONS)?;
    if !migration.database_exists
        || !migration.pending.is_empty()
        || !paths.managed_config_file.is_file()
        || !paths.master_key_file.is_file()
    {
        println!("配置中心尚未完整初始化；以下为旧配置迁移来源盘点：");
        let plan = plan_config_migration(
            all_managed_fields(),
            &paths.managed_config_file,
            PathBuf::from(&database_file).as_path(),
            &default_dotenv_files(),
        )?;
        print_config_migration_plan(&plan);
        return Ok(());
    }
    let database = SqliteDatabase::open_with_pool_size(&database_file, APP_MIGRATIONS, pool_size)?;
    let center = ConfigCenter::open(all_managed_fields(), paths, database)?
        .with_external_environment(environment.clone());
    let snapshot = center.current_snapshot()?;
    println!("managed_revision={}", snapshot.revision);
    for field in snapshot.fields {
        println!(
            "{} source={:?} configured={} overridden={} editable={} valid={} pending_restart={} sensitivity={:?}",
            field.key,
            field.source,
            field.configured,
            field.overridden,
            field.editable,
            field.valid,
            field.pending_restart,
            field.sensitivity,
        );
    }
    Ok(())
}

fn config_migrate(args: &[String]) -> anyhow::Result<()> {
    let mut apply = false;
    let mut dotenv_files = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--apply" => apply = true,
            "--env-file" => {
                index += 1;
                dotenv_files.push(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--env-file 缺少路径"))?,
                ));
            }
            value => bail!("config migrate 未知参数 '{value}'"),
        }
        index += 1;
    }
    if dotenv_files.is_empty() {
        dotenv_files = default_dotenv_files();
    }
    let environment = current_environment();
    let (database_file, pool_size) = database_bootstrap_from_environment(&environment)?;
    let paths = ConfigCenterPaths::from_environment(&environment);
    let plan = plan_config_migration(
        all_managed_fields(),
        &paths.managed_config_file,
        PathBuf::from(&database_file).as_path(),
        &dotenv_files,
    )?;
    print_config_migration_plan(&plan);
    print_external_file_status(&environment);
    if !apply {
        println!("dry_run=true；原文件与数据库均未修改。确认后追加 --apply。");
        return Ok(());
    }
    if plan
        .entries
        .iter()
        .any(|entry| entry.action == ConfigMigrationAction::InvalidValue)
    {
        bail!("存在无效旧配置，拒绝导入");
    }
    let database = SqliteDatabase::open_with_pool_size(&database_file, APP_MIGRATIONS, pool_size)?;
    let report = apply_config_migration(
        all_managed_fields(),
        paths,
        database,
        PathBuf::from(&database_file).as_path(),
        &dotenv_files,
    )?;
    println!(
        "applied=true managed_imported={} secrets_imported={} unchanged={}",
        report.managed_imported, report.secrets_imported, report.unchanged
    );
    println!("旧 dotenv/TOML 未修改、未删除；可重复执行检查幂等结果。");
    Ok(())
}

fn run_migration(args: &[String]) -> anyhow::Result<()> {
    if args != ["status"] {
        bail!("用法：qq-maid-bot migration status");
    }
    let environment = current_environment();
    let (database_file, _) = database_bootstrap_from_environment(&environment)?;
    let plan = SqliteDatabase::inspect_migrations(&database_file, APP_MIGRATIONS)?;
    println!("database={database_file}");
    print_migration_plan(&plan);
    if !plan.unknown.is_empty() {
        bail!("检测到新版本 schema，当前二进制不能安全降级读取");
    }
    Ok(())
}

fn run_backup(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("create") => backup_create(&args[1..]),
        Some("verify") => backup_verify(&args[1..]),
        Some("restore") => backup_restore(&args[1..]),
        _ => bail!("用法：qq-maid-bot backup <create|verify|restore>"),
    }
}

fn backup_create(args: &[String]) -> anyhow::Result<()> {
    let mut output = None;
    let mut include_secrets = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                index += 1;
                output = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--output 缺少路径"))?,
                ));
            }
            "--include-secrets" => include_secrets = true,
            value => bail!("backup create 未知参数 '{value}'"),
        }
        index += 1;
    }
    let output = output.ok_or_else(|| anyhow!("backup create 需要 --output DIR"))?;
    let environment = current_environment();
    let (database_file, _) = database_bootstrap_from_environment(&environment)?;
    let paths = ConfigCenterPaths::from_environment(&environment);
    let config_directory = paths
        .managed_config_file
        .parent()
        .unwrap_or_else(|| std::path::Path::new("config"))
        .to_path_buf();
    let report = create_backup(
        &BackupOptions {
            database_file: database_file.into(),
            config_directory,
            output_directory: output,
            include_secrets,
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        APP_MIGRATIONS,
    )?;
    println!(
        "backup={} files={} includes_secret_material={}",
        report.output_directory.display(),
        report.file_count,
        report.includes_secret_material
    );
    for warning in report.warnings {
        println!("warning: {warning}");
    }
    Ok(())
}

fn backup_verify(args: &[String]) -> anyhow::Result<()> {
    reject_unknown_options(args, &["--from"])?;
    let bundle = required_path_option(args, "--from")?;
    let manifest = verify_backup(&bundle, APP_MIGRATIONS)?;
    println!(
        "valid=true format={} app_version={} files={} includes_secret_material={}",
        manifest.format_version,
        manifest.application_version,
        manifest.files.len(),
        manifest.includes_secret_material
    );
    Ok(())
}

fn backup_restore(args: &[String]) -> anyhow::Result<()> {
    reject_unknown_options(args, &["--from", "--target", "--apply"])?;
    let bundle = required_path_option(args, "--from")?;
    let target = required_path_option(args, "--target")?;
    let apply = args.iter().any(|value| value == "--apply");
    let plan = plan_restore(&bundle, &target, APP_MIGRATIONS)?;
    println!(
        "target={} database={} config={} files={} includes_secret_material={}",
        plan.target_root.display(),
        plan.database_destination.display(),
        plan.config_destination.display(),
        plan.file_count,
        plan.includes_secret_material
    );
    for warning in &plan.warnings {
        println!("warning: {warning}");
    }
    if !apply {
        println!("dry_run=true；恢复目标未修改。确认服务已停止后追加 --apply。");
        return Ok(());
    }
    restore_backup(&bundle, &target, APP_MIGRATIONS)?;
    println!(
        "restored=true；已恢复数据库与包内配置，并非完整部署；请补齐同期主密钥、外部 secret 和部署文件，随后运行 config check 再启动实例。"
    );
    Ok(())
}

fn required_path_option(args: &[String], name: &str) -> anyhow::Result<PathBuf> {
    let index = args
        .iter()
        .position(|value| value == name)
        .ok_or_else(|| anyhow!("缺少 {name} PATH"))?;
    args.get(index + 1)
        .filter(|value| !value.starts_with('-'))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{name} 缺少路径"))
}

fn reject_unknown_options(args: &[String], allowed: &[&str]) -> anyhow::Result<()> {
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        if value.starts_with('-') {
            if !allowed.contains(&value.as_str()) {
                bail!("未知参数 '{value}'");
            }
            if value != "--apply" {
                index += 1;
                if index >= args.len() || args[index].starts_with('-') {
                    bail!("{value} 缺少路径");
                }
            }
        }
        index += 1;
    }
    Ok(())
}

fn print_migration_plan(plan: &SqliteMigrationPlan) {
    println!("database_exists={}", plan.database_exists);
    println!("migrations_applied={}", plan.applied.len());
    println!("migrations_pending={}", plan.pending.len());
    println!("migrations_unknown={}", plan.unknown.len());
    println!(
        "last_successful_migration_at={}",
        plan.last_applied_at.as_deref().unwrap_or("none")
    );
    if !plan.pending.is_empty() {
        println!("pending={}", plan.pending.join(","));
    }
    if !plan.unknown.is_empty() {
        println!("unknown={}", plan.unknown.join(","));
    }
}

fn print_config_migration_plan(plan: &qq_maid_core::maintenance::ConfigMigrationPlan) {
    println!("managed_revision={}", plan.managed_revision);
    for entry in plan
        .entries
        .iter()
        .filter(|entry| entry.action != ConfigMigrationAction::NotPresent)
    {
        println!(
            "{} kind={} action={} source={} name={}",
            entry.key,
            migration_kind(entry.kind),
            migration_action(entry.action),
            entry
                .source_file
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_owned()),
            entry.source_name.as_deref().unwrap_or("none"),
        );
    }
}

fn migration_kind(kind: ConfigMigrationKind) -> &'static str {
    match kind {
        ConfigMigrationKind::Public => "public",
        ConfigMigrationKind::Secret => "secret",
        ConfigMigrationKind::Restricted => "restricted",
    }
}

fn migration_action(action: ConfigMigrationAction) -> &'static str {
    match action {
        ConfigMigrationAction::ImportManaged => "import_managed",
        ConfigMigrationAction::ImportSecret => "import_secret_redacted",
        ConfigMigrationAction::AlreadyManaged => "conflict_keep_managed",
        ConfigMigrationAction::KeepExternal => "keep_external",
        ConfigMigrationAction::InvalidValue => "invalid_redacted",
        ConfigMigrationAction::NotPresent => "not_present",
    }
}

fn print_external_file_status(environment: &HashMap<String, String>) {
    for (name, default) in [
        ("AGENT_CONFIG_FILE", "config/agent.toml"),
        ("OPS_CONFIG_FILE", "config/ops.toml"),
    ] {
        let path = environment.get(name).map(String::as_str).unwrap_or(default);
        println!(
            "{}={} status={} action=retain_external",
            name,
            path,
            if PathBuf::from(path).is_file() {
                "present"
            } else {
                "missing"
            }
        );
    }
}

fn current_environment() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn default_dotenv_files() -> Vec<PathBuf> {
    vec![PathBuf::from("config/.env"), PathBuf::from(".env")]
}

fn all_managed_fields() -> Vec<qq_maid_core::config::center::ManagedConfigField> {
    let mut fields = qq_maid_core::config::managed_config_fields();
    fields.extend(qq_maid_gateway_rs::config::managed_config_fields());
    fields
}
