use super::*;

fn embedded_snapshot() -> CatalogSnapshot {
    parse_snapshot(EMBEDDED_SNAPSHOT_BYTES).expect("嵌入式快照必须可解析")
}

fn local_entry(
    provider: &str,
    id: &str,
    mut patch: impl FnMut(&mut LocalModelEntry),
) -> LocalModelEntry {
    let mut entry = LocalModelEntry {
        provider: provider.to_owned(),
        id: id.to_owned(),
        display_name: None,
        context_window: None,
        max_output_tokens: None,
        modalities: None,
        capabilities: None,
        status: None,
        enabled: None,
        price: None,
    };
    patch(&mut entry);
    entry
}

fn overrides(entries: Vec<LocalModelEntry>) -> LocalModelOverrides {
    LocalModelOverrides {
        schema_version: LOCAL_SCHEMA_VERSION,
        models: entries,
    }
}

#[test]
fn embedded_snapshot_parses_with_metadata() {
    let snapshot = embedded_snapshot();
    assert_eq!(snapshot.schema_version, CATALOG_SCHEMA_VERSION);
    assert_eq!(snapshot.source.name, "models.dev");
    assert_eq!(snapshot.source.converter_version, CONVERTER_VERSION);
    assert!(!snapshot.source.source_hash.is_empty());
    assert!(!snapshot.source.fetched_at.is_empty());
    assert!(!snapshot.providers.is_empty());
    for provider in &snapshot.providers {
        assert!(!provider.models.is_empty());
        for model in &provider.models {
            assert!(!model.display_name.is_empty());
            // 远程目录声明能力时必须保持 verified=unknown，不伪造验证结果。
            for claim in [
                &model.capabilities.reasoning,
                &model.capabilities.tool_calling,
                &model.capabilities.vision,
            ] {
                if claim.advertised {
                    assert_eq!(claim.verified, Verified::Unknown);
                }
            }
        }
    }
}

#[test]
fn snapshot_rejects_unknown_fields_and_wrong_schema_version() {
    let broken = br#"{"schema_version":1,"source":{"name":"x","upstream_license":"MIT",
        "fetched_at":"2025-01-01","source_hash":"h","converter_version":"v1"},
        "providers":[],"mystery_field":1}"#;
    assert!(parse_snapshot(broken).is_err());

    let wrong_version = br#"{"schema_version":99,"source":{"name":"x","upstream_license":"MIT",
        "fetched_at":"2025-01-01","source_hash":"h","converter_version":"v1"},"providers":[]}"#;
    let error = parse_snapshot(wrong_version).unwrap_err();
    assert!(error.message.contains("schema_version 不受支持"));
}

#[test]
fn corrupted_snapshot_returns_error_not_panic() {
    let corrupted = br#"{"schema_version":1,"source":"broken-json""#;
    let error = parse_snapshot(corrupted).unwrap_err();
    assert!(error.message.contains("内置模型目录快照解析失败"));
}

#[test]
fn local_overrides_can_append_and_disable_models() {
    let catalog = EffectiveModelCatalog::from_embedded(Some(&overrides(vec![
        local_entry("custom_mimo", "mimo-vision-large", |entry| {
            entry.display_name = Some("MiMo Vision".to_owned());
            entry.context_window = Some(32768);
        }),
        local_entry("openai", "gpt-4o", |entry| {
            entry.enabled = Some(false);
        }),
    ])))
    .expect("合并应当成功");

    let appended = catalog
        .find(Some("custom_mimo"), "mimo-vision-large")
        .expect("不存在的模型应被本地追加");
    assert_eq!(appended.display_name, "MiMo Vision");
    assert_eq!(appended.context_window, Some(32768));
    assert_eq!(appended.origin, CatalogOrigin::Local);

    let disabled = catalog
        .find(Some("openai"), "gpt-4o")
        .expect("已有模型应保留");
    assert!(!disabled.enabled);
    assert_eq!(
        catalog.is_model_enabled(Some("openai"), "gpt-4o"),
        Some(false)
    );
    // 禁用模型在目录中仍可见，但不出现在 active 集合中。
    assert!(catalog.active_models().all(|model| model.enabled));
}

#[test]
fn local_overrides_patch_existing_fields_and_inherit_rest() {
    let catalog = EffectiveModelCatalog::from_embedded(Some(&overrides(vec![local_entry(
        "deepseek",
        "deepseek-chat",
        |entry| {
            entry.context_window = Some(64000);
        },
    )])))
    .expect("合并应当成功");

    let model = catalog
        .find(Some("deepseek"), "deepseek-chat")
        .expect("覆盖目标应存在");
    assert_eq!(model.context_window, Some(64000));
    // 未覆盖字段继续继承目录值。
    assert_eq!(model.max_output_tokens, Some(8192));
    assert!(model.capabilities.tool_calling.advertised);
    assert_eq!(model.capabilities.tool_calling.verified, Verified::Unknown);
}

#[test]
fn local_config_rejects_secrets_and_connection_fields() {
    let cases = [
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","base_url":"https://evil"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","api_key":"sk-1"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","auth_header":"X-Key"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","credential":"a"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","adapter":"openai_compatible"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","route":"main"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","enabled_tools":["save_memory"]}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","shell":"/bin/sh"}]}"#,
    ];
    for raw in cases {
        let error = parse_local_overrides(raw.as_bytes()).unwrap_err();
        assert!(
            error.message.contains("禁止的字段"),
            "应拒绝 {raw}，实际：{}",
            error.message
        );
    }
}

#[test]
fn local_config_rejects_unknown_fields_and_bad_provider() {
    let bad_field =
        br#"{"schema_version":1,"models":[{"provider":"openai","id":"x","color":"red"}]}"#;
    assert!(parse_local_overrides(bad_field).is_err());
    let bad_provider = br#"{"schema_version":1,"models":[{"provider":"!!","id":"x"}]}"#;
    let error = parse_local_overrides(bad_provider).unwrap_err();
    assert!(error.message.contains("provider 非法"));
}

#[test]
fn unknown_models_do_not_block_and_query_reports_unknown() {
    let catalog = EffectiveModelCatalog::from_embedded(None).expect("合并应当成功");
    // Route 中可能存在但目录未收录的模型：查询返回 None，不报错、不 panic。
    assert!(catalog.find(Some("openai"), "never-seen-model").is_none());
    assert!(catalog.find(Some("custom_unknown"), "nope").is_none());
    assert_eq!(
        catalog.is_model_enabled(Some("openai"), "never-seen-model"),
        None
    );
    // find 未显式指定 provider 时按模型 id 全局匹配。
    assert!(catalog.find(None, "deepseek-chat").is_some());
}

#[test]
fn merge_is_deterministic_and_stable() {
    let patch = CatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        source: CatalogSource {
            name: "official-compat".to_owned(),
            upstream_license: "MIT".to_owned(),
            fetched_at: "2025-06-02T00:00:00Z".to_owned(),
            source_hash: "sha256:patch".to_owned(),
            converter_version: CONVERTER_VERSION.to_owned(),
        },
        providers: vec![CatalogProvider {
            id: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            models: vec![CatalogModel {
                id: "gpt-4o".to_owned(),
                display_name: "GPT-4o (updated)".to_owned(),
                context_window: Some(256000),
                max_output_tokens: None,
                modalities: ModelModalities::default(),
                capabilities: ModelCapabilities::default(),
                status: ModelStatus::Active,
                enabled: true,
                price: None,
                origin: CatalogOrigin::Embedded,
            }],
        }],
    };
    let local = overrides(vec![local_entry("gemini", "gemini-2.5-flash", |entry| {
        entry.max_output_tokens = Some(8192);
    })]);
    let first = EffectiveModelCatalog::build(
        &embedded_snapshot(),
        std::slice::from_ref(&patch),
        Some(&local),
    )
    .expect("合并应当成功");
    let second = EffectiveModelCatalog::build(&embedded_snapshot(), &[patch], Some(&local))
        .expect("合并应当成功");
    assert_eq!(first, second);

    // 结果顺序稳定：provider 与 model 均按 id 字典序。
    let provider_ids: Vec<&str> = first.providers().iter().map(|p| p.id.as_str()).collect();
    let mut sorted = provider_ids.clone();
    sorted.sort();
    assert_eq!(provider_ids, sorted);
    for provider in first.providers() {
        let ids: Vec<&str> = provider.models.iter().map(|m| m.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    // 官方补丁先于本地覆盖生效：本地值最终胜出。
    let gemini = first
        .find(Some("gemini"), "gemini-2.5-flash")
        .expect("gemini 模型应存在");
    assert_eq!(gemini.max_output_tokens, Some(8192));
    let openai = first.find(Some("openai"), "gpt-4o").expect("gpt-4o 应存在");
    assert_eq!(openai.display_name, "GPT-4o (updated)");
    assert_eq!(openai.origin, CatalogOrigin::OfficialPatch);
}

#[test]
fn capability_effective_requires_verification() {
    let mut claim = CapabilityClaim {
        advertised: true,
        adapter_supported: None,
        verified: Verified::Unknown,
    };
    assert_eq!(claim.effective(), None);
    claim.verified = Verified::Yes;
    assert_eq!(claim.effective(), Some(true));
    claim.adapter_supported = Some(false);
    assert_eq!(claim.effective(), Some(false));
    claim.verified = Verified::No;
    assert_eq!(claim.effective(), Some(false));
}
