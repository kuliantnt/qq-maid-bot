use super::*;
use crate::model_catalog::converter::{ModelsDevSourceManifest, convert_snapshot};

const UPSTREAM_FIXTURE: &[u8] = include_bytes!("../../assets/models-dev/upstream.json");
const SOURCE_MANIFEST: &[u8] = include_bytes!("../../assets/models-dev/manifest.json");

fn embedded_snapshot() -> CatalogSnapshot {
    parse_snapshot(EMBEDDED_SNAPSHOT_BYTES).expect("嵌入式快照必须可解析")
}

fn catalog_provider_id(value: &str) -> CatalogProviderId {
    CatalogProviderId::parse(value).expect("测试 provider id 应合法")
}

/// 构造仅含给定 provider id、每个 provider 带一个合法模型的快照 JSON；
/// 用于验证反序列化阶段的 provider id canonical 不变量。
fn snapshot_with_provider_ids(provider_ids: &[&str]) -> String {
    let providers = provider_ids
        .iter()
        .map(|id| {
            format!(
                r#"{{"id":"{id}","name":"Provider","models":[
                    {{"id":"model","display_name":"Model"}}]}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"schema_version":1,"source":{{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"}},"providers":[{providers}]}}"#
    )
}

fn provider<'a>(snapshot: &'a CatalogSnapshot, id: &str) -> &'a CatalogProvider {
    snapshot
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == id)
        .unwrap_or_else(|| panic!("快照应包含 provider `{id}`"))
}

fn first_catalog_model(snapshot: &CatalogSnapshot, id: &str) -> CatalogModel {
    provider(snapshot, id).models[0].clone()
}

fn local_entry(
    provider: &str,
    id: &str,
    mut patch: impl FnMut(&mut LocalModelEntry),
) -> LocalModelEntry {
    let mut entry = LocalModelEntry {
        provider: catalog_provider_id(provider),
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

fn source(name: &str, source_version: &str) -> CatalogSource {
    CatalogSource {
        name: name.to_owned(),
        upstream_license: "MIT".to_owned(),
        source_url: "https://models.dev/api.json".to_owned(),
        source_version: source_version.to_owned(),
        fetched_at: "2026-01-01T00:00:00Z".to_owned(),
        source_hash: "sha256:test".to_owned(),
        converter_version: CONVERTER_VERSION.to_owned(),
    }
}

#[test]
fn embedded_snapshot_metadata_is_real_and_regeneratable() {
    let snapshot = embedded_snapshot();
    assert_eq!(snapshot.schema_version, CATALOG_SCHEMA_VERSION);
    assert_eq!(snapshot.source.name, "models.dev");
    assert!(
        snapshot
            .source
            .source_url
            .starts_with("https://models.dev/")
    );
    assert_eq!(snapshot.source.converter_version, CONVERTER_VERSION);
    assert!(!snapshot.source.source_hash.is_empty());
    assert!(!snapshot.source.fetched_at.is_empty());
    assert!(!snapshot.source.source_version.is_empty());
    assert!(!snapshot.providers.is_empty());
    for provider in &snapshot.providers {
        assert!(!provider.models.is_empty());
        for model in &provider.models {
            assert!(!model.display_name.is_empty());
        }
    }

    // 转换器必须能从仓库内固定上游输入原样复现已提交快照。
    let manifest: ModelsDevSourceManifest =
        serde_json::from_slice(SOURCE_MANIFEST).expect("来源清单应可解析");
    assert_eq!(manifest.source_version, snapshot.source.source_version);
    let generated = convert_snapshot(UPSTREAM_FIXTURE, &manifest).expect("转换应当成功");
    assert_eq!(generated, EMBEDDED_SNAPSHOT_BYTES);

    // Catalog 基础层不得携带 Connection/Adapter 判定或伪 verified 状态。
    let raw = std::str::from_utf8(EMBEDDED_SNAPSHOT_BYTES).expect("快照应为 UTF-8");
    assert!(!raw.contains("adapter_supported"));
    assert!(!raw.contains("verified"));
    assert!(!raw.contains("\"enabled\""));
}

#[test]
fn catalog_provider_ids_do_not_apply_runtime_connection_aliases() {
    let google = catalog_provider_id("google");
    let gemini = catalog_provider_id("gemini");
    assert_ne!(google, gemini);

    let zhipuai = catalog_provider_id("zhipuai");
    assert_ne!(zhipuai, catalog_provider_id("bigmodel"));
    assert_ne!(zhipuai, catalog_provider_id("glm"));
    assert_ne!(zhipuai, catalog_provider_id("zhipu"));

    // Catalog 内实际保留 Models.dev Provider ID，而不是改写为运行时 Connection 名。
    let snapshot = embedded_snapshot();
    assert!(
        provider(&snapshot, "google")
            .models
            .iter()
            .any(|m| m.id == "gemini-2.5-flash")
    );
    assert!(
        provider(&snapshot, "zhipuai")
            .models
            .iter()
            .any(|m| m.id == "glm-4.5")
    );
    assert!(!snapshot.providers.iter().any(|p| p.id.as_str() == "gemini"));
    assert!(
        !snapshot
            .providers
            .iter()
            .any(|p| p.id.as_str() == "bigmodel")
    );

    let catalog = EffectiveModelCatalog::from_embedded(None).expect("合并应当成功");
    assert!(
        catalog
            .find_by_provider(&google, "gemini-2.5-flash")
            .is_some()
    );
    assert!(
        catalog
            .find_by_provider(&gemini, "gemini-2.5-flash")
            .is_none()
    );
    assert!(
        catalog
            .find_by_provider(&catalog_provider_id("zhipuai"), "glm-4.5")
            .is_some()
    );
    assert!(
        catalog
            .find_by_provider(&catalog_provider_id("bigmodel"), "glm-4.5")
            .is_none()
    );
}

#[test]
fn deserialized_catalog_provider_ids_are_canonical() {
    let canonical: CatalogProviderId =
        serde_json::from_str(r#""google""#).expect("canonical 应可解析");
    assert_eq!(canonical, catalog_provider_id("google"));
    assert_eq!(canonical.as_str(), "google");

    // `Google` 与带首尾空白的 ID 在反序列化时必须归一为同一个 canonical ID，
    // 不能以非 canonical 外观进入 Catalog 领域模型。
    for raw_id in ["Google", " google", "google ", "  Google  "] {
        let parsed: CatalogProviderId =
            serde_json::from_str(&format!(r#""{raw_id}""#)).expect("应归一为合法 provider id");
        assert_eq!(parsed, canonical, "raw id `{raw_id}` 应归一为 google");
    }

    // 快照入口同样只产出 canonical provider id；分别验证大写与首尾空白两种输入。
    for raw_id in ["Google", " google "] {
        let snapshot = parse_snapshot(snapshot_with_provider_ids(&[raw_id]).as_bytes())
            .expect("provider id 应被归一且模型条目合法");
        assert!(
            snapshot
                .providers
                .iter()
                .all(|provider| provider.id == canonical),
            "raw id `{raw_id}` 的快照应只包含 google provider"
        );
    }
}

#[test]
fn canonical_aliases_cannot_coexist_as_separate_providers() {
    let error =
        parse_snapshot(snapshot_with_provider_ids(&["Google", "google"]).as_bytes()).unwrap_err();
    assert!(
        error.message.contains("provider id 重复"),
        "`Google` 与 `google` 应被视为同一 provider，不能共存，实际：{}",
        error.message
    );
}

#[test]
fn local_overrides_share_catalog_provider_identity_rules() {
    let raw = br#"{"schema_version":1,"models":[
        {"provider":"Google","id":"catalog-identity-model","display_name":"First"},
        {"provider":" google ","id":"catalog-identity-model","display_name":"Second"}
    ]}"#;
    let local = parse_local_overrides(raw).expect("本地覆盖中的 provider id 应归一化");
    let google = catalog_provider_id("google");
    assert!(local.models.iter().all(|entry| entry.provider == google));

    // 本地 override 与 embedded/cache/patch 使用同一 identity：两个写法只会命中
    // 同一个 Catalog Provider，而不会额外创建 `Google` Provider。
    let catalog = EffectiveModelCatalog::from_embedded(Some(&local)).expect("合并应当成功");
    assert_eq!(
        catalog
            .providers()
            .iter()
            .filter(|provider| provider.id == google)
            .count(),
        1
    );
    let model = catalog
        .find_by_provider(&google, "catalog-identity-model")
        .expect("两条本地条目应合并到同一个 google Provider");
    assert_eq!(model.display_name, "Second");
}

#[test]
fn snapshot_rejects_unknown_fields_wrong_schema_and_remote_enabled() {
    let broken = br#"{"schema_version":1,"source":{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"},
        "providers":[],"mystery_field":1}"#;
    assert!(parse_snapshot(broken).is_err());

    let wrong_version = br#"{"schema_version":99,"source":{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"},"providers":[]}"#;
    let error = parse_snapshot(wrong_version).unwrap_err();
    assert!(error.message.contains("schema_version 不受支持"));

    // 远程/规范化快照不允许携带本地 enabled 开关；enabled 只能由用户本地覆盖产生。
    let remote_enabled = br#"{"schema_version":1,"source":{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"},
        "providers":[{"id":"openai","name":"OpenAI","models":[
          {"id":"m","display_name":"M","enabled":false}
        ]}]}"#;
    assert!(parse_snapshot(remote_enabled).is_err());
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
        .find_by_provider(&catalog_provider_id("custom_mimo"), "mimo-vision-large")
        .expect("不存在的模型应被本地追加");
    assert_eq!(appended.display_name, "MiMo Vision");
    assert_eq!(appended.context_window, Some(32768));
    assert_eq!(appended.provenance.display_name, FieldSource::LocalOverride);

    let disabled = catalog
        .find_by_provider(&catalog_provider_id("openai"), "gpt-4o")
        .expect("已有模型应保留");
    assert!(!disabled.enabled);
    assert_eq!(
        catalog.is_model_enabled(Some(&catalog_provider_id("openai")), "gpt-4o"),
        Some(false)
    );
    // 禁用模型在目录中仍可见，但不出现在 active 集合中。
    assert!(catalog.active_models().all(|model| model.enabled));
}

#[test]
fn local_context_override_keeps_catalog_sources_for_unpatched_fields() {
    let snapshot = embedded_snapshot();
    let base_model = first_catalog_model(&snapshot, "deepseek");
    let catalog = EffectiveModelCatalog::build(
        &snapshot,
        &[],
        Some(&overrides(vec![local_entry(
            "deepseek",
            &base_model.id,
            |entry| {
                entry.context_window = Some(999999);
            },
        )])),
    )
    .expect("合并应当成功");

    let model = catalog
        .find_by_provider(&catalog_provider_id("deepseek"), &base_model.id)
        .expect("覆盖目标应存在");
    assert_eq!(model.context_window, Some(999999));
    assert_eq!(model.provenance.context_window, FieldSource::LocalOverride);
    // 只覆盖 context_window 时，其余元数据仍保留 Catalog 来源。
    assert_eq!(model.provenance.display_name, FieldSource::Catalog);
    assert_eq!(model.provenance.max_output_tokens, FieldSource::Catalog);
    assert_eq!(model.provenance.modalities, FieldSource::Catalog);
    assert_eq!(
        model.provenance.capabilities.tool_calling,
        FieldSource::Catalog
    );
    assert_eq!(
        model.provenance.capabilities.reasoning,
        FieldSource::Catalog
    );
    assert_eq!(model.capabilities, base_model.capabilities);
    assert_eq!(model.max_output_tokens, base_model.max_output_tokens);
}

#[test]
fn local_capability_override_only_changes_that_capability_source() {
    let snapshot = embedded_snapshot();
    let base_model = first_catalog_model(&snapshot, "openai");
    let catalog = EffectiveModelCatalog::build(
        &snapshot,
        &[],
        Some(&overrides(vec![local_entry(
            "openai",
            &base_model.id,
            |entry| {
                entry.capabilities = Some(LocalCapabilities {
                    tool_calling: Some(CapabilityClaim { advertised: false }),
                    ..LocalCapabilities::default()
                });
            },
        )])),
    )
    .expect("合并应当成功");

    let model = catalog
        .find_by_provider(&catalog_provider_id("openai"), &base_model.id)
        .expect("覆盖目标应存在");
    assert!(!model.capabilities.tool_calling.advertised);
    assert_eq!(
        model.provenance.capabilities.tool_calling,
        FieldSource::LocalOverride
    );
    // 未覆盖的 reasoning / vision 能力声明来源仍是 Catalog。
    assert_eq!(
        model.provenance.capabilities.reasoning,
        FieldSource::Catalog
    );
    assert_eq!(model.provenance.capabilities.vision, FieldSource::Catalog);
    assert_eq!(
        model.capabilities.reasoning,
        base_model.capabilities.reasoning
    );
}

#[test]
fn local_config_rejects_secrets_connection_fields_and_adapter_capabilities() {
    let cases = [
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","base_url":"https://evil"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","api_key":"sk-1"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","auth_header":"X-Key"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","credential":"a"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","adapter":"openai_compatible"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","route":"main"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","enabled_tools":["save_memory"]}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","shell":"/bin/sh"}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","capabilities":{"tool_calling":{"advertised":true,"adapter_supported":true}}}]}"#,
        r#"{"schema_version":1,"models":[{"provider":"openai","id":"x","capabilities":{"tool_calling":{"advertised":true,"verified":"yes"}}}]}"#,
    ];
    for raw in cases {
        let error = parse_local_overrides(raw.as_bytes()).unwrap_err();
        assert!(
            error.message.contains("禁止的字段") || error.message.contains("Schema 校验失败"),
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
    assert!(error.message.contains("provider id `!!` 非法"));
}

#[test]
fn unknown_models_do_not_block_and_query_reports_unknown() {
    let catalog = EffectiveModelCatalog::from_embedded(None).expect("合并应当成功");
    assert!(
        catalog
            .find_by_provider(&catalog_provider_id("openai"), "never-seen-model")
            .is_none()
    );
    assert_eq!(
        catalog.is_model_enabled(Some(&catalog_provider_id("openai")), "never-seen-model"),
        None
    );
}

#[test]
fn providerless_lookup_is_ambiguous_when_model_id_repeats() {
    let catalog = EffectiveModelCatalog::build(
        &embedded_snapshot(),
        &[],
        Some(&overrides(vec![
            local_entry("alpha_provider", "shared-model", |_| {}),
            local_entry("beta_provider", "shared-model", |_| {}),
        ])),
    )
    .expect("合并应当成功");

    // 多 Provider 同 model id 时不得隐式选择字典序第一个。
    assert_eq!(catalog.find(None, "shared-model"), None);
    assert_eq!(catalog.is_model_enabled(None, "shared-model"), None);
    assert!(
        catalog
            .find_by_provider(&catalog_provider_id("alpha_provider"), "shared-model")
            .is_some()
    );
    assert!(
        catalog
            .find_by_provider(&catalog_provider_id("beta_provider"), "shared-model")
            .is_some()
    );

    // 全局唯一 id 才允许无 Provider 查询返回。
    assert!(catalog.find_unique("gpt-4o").is_some());
}

#[test]
fn merge_is_deterministic_and_patch_precedes_local_override() {
    let patch_model = CatalogModel {
        id: "gpt-4o".to_owned(),
        display_name: "GPT-4o (patched)".to_owned(),
        context_window: Some(200000),
        max_output_tokens: Some(20000),
        modalities: ModelModalities {
            input: vec![Modality::Text, Modality::Image],
            output: vec![Modality::Text],
        },
        capabilities: ModelCapabilities {
            reasoning: CapabilityClaim { advertised: false },
            tool_calling: CapabilityClaim { advertised: true },
            vision: CapabilityClaim { advertised: true },
        },
        status: ModelStatus::Active,
        price: None,
    };
    let patch = CatalogSnapshot {
        schema_version: CATALOG_SCHEMA_VERSION,
        source: source("official-compat", "v1"),
        providers: vec![CatalogProvider {
            id: catalog_provider_id("openai"),
            name: "OpenAI".to_owned(),
            models: vec![patch_model],
        }],
    };
    let local = overrides(vec![local_entry("openai", "gpt-4o", |entry| {
        entry.max_output_tokens = Some(65536);
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

    let provider_ids: Vec<&str> = first
        .providers()
        .iter()
        .map(|provider| provider.id.as_str())
        .collect();
    let mut sorted = provider_ids.clone();
    sorted.sort();
    assert_eq!(provider_ids, sorted);
    for provider in first.providers() {
        let ids: Vec<&str> = provider
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    // 官方补丁先于本地覆盖生效：本地 max_output_tokens 最终胜出，未覆盖的显示名
    // 保留官方补丁来源。
    let openai = first
        .find_by_provider(&catalog_provider_id("openai"), "gpt-4o")
        .expect("gpt-4o 应存在");
    assert_eq!(openai.display_name, "GPT-4o (patched)");
    assert_eq!(openai.context_window, Some(200000));
    assert_eq!(openai.max_output_tokens, Some(65536));
    assert_eq!(openai.provenance.display_name, FieldSource::OfficialPatch);
    assert_eq!(openai.provenance.context_window, FieldSource::OfficialPatch);
    assert_eq!(
        openai.provenance.max_output_tokens,
        FieldSource::LocalOverride
    );
    assert_eq!(
        openai.provenance.capabilities.tool_calling,
        FieldSource::OfficialPatch
    );
}

#[test]
fn snapshot_rejects_adapter_and_verified_capability_fields() {
    let with_adapter = br#"{"schema_version":1,"source":{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"},
        "providers":[{"id":"openai","name":"OpenAI","models":[{
          "id":"gpt-x","display_name":"GPT X",
          "capabilities":{"tool_calling":{"advertised":true,"adapter_supported":true}}
        }]}]}"#;
    assert!(parse_snapshot(with_adapter).is_err());

    let with_verified = br#"{"schema_version":1,"source":{"name":"x","upstream_license":"MIT",
        "source_url":"u","source_version":"v","fetched_at":"2025-01-01",
        "source_hash":"h","converter_version":"v1"},
        "providers":[{"id":"openai","name":"OpenAI","models":[{
          "id":"gpt-x","display_name":"GPT X",
          "capabilities":{"reasoning":{"advertised":true,"verified":"yes"}}
        }]}]}"#;
    assert!(parse_snapshot(with_verified).is_err());
}
