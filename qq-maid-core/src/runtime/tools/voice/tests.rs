use std::collections::HashMap;

use qq_maid_common::identity_context::ConversationKind;

use super::*;
use crate::{config::VoiceFeatureConfig, storage::database::SqliteDatabase};

fn request(account_id: &str, target_id: &str, group_role: Option<&str>) -> RespondRequest {
    let is_group = group_role.is_some();
    RespondRequest {
        platform: "qq_official".to_owned(),
        account_id: Some(account_id.to_owned()),
        conversation_kind: if is_group {
            ConversationKind::Group
        } else {
            ConversationKind::Private
        },
        conversation_id: Some(target_id.to_owned()),
        group_id: is_group.then(|| target_id.to_owned()),
        group_member_role: group_role.map(str::to_owned),
        ..Default::default()
    }
}

fn available_config() -> VoiceFeatureConfig {
    VoiceFeatureConfig::from_environment(&HashMap::from([
        ("TTS_PROVIDER".to_owned(), "qwen".to_owned()),
        ("QWEN_TTS_API_KEY".to_owned(), "test-key".to_owned()),
    ]))
}

#[test]
fn private_preferences_persist_and_isolate_account_and_peer() {
    let database = SqliteDatabase::open_temp("voice-pref", &[VOICE_PREFERENCE_SCHEMA_V1]).unwrap();
    let service = VoicePreferenceService::new(
        VoicePreferenceStore::new(database.clone()),
        available_config(),
    );
    let first = request("bot-a", "user-a", None);
    service.execute(VoiceCommand::Enable, &first).unwrap();
    assert!(service.enabled_for_request(&first).unwrap());
    assert!(
        !service
            .enabled_for_request(&request("bot-b", "user-a", None))
            .unwrap()
    );
    assert!(
        !service
            .enabled_for_request(&request("bot-a", "user-b", None))
            .unwrap()
    );

    let reopened =
        VoicePreferenceService::new(VoicePreferenceStore::new(database), available_config());
    assert!(reopened.enabled_for_request(&first).unwrap());
}

#[test]
fn group_modification_requires_owner_or_admin_but_query_is_open() {
    let database = SqliteDatabase::open_temp("voice-role", &[VOICE_PREFERENCE_SCHEMA_V1]).unwrap();
    let service =
        VoicePreferenceService::new(VoicePreferenceStore::new(database), available_config());

    for role in [Some("member"), Some("unknown"), None] {
        let mut request = request("bot-a", "group-a", role);
        if role.is_none() {
            request.group_id = Some("group-a".to_owned());
            request.conversation_kind = ConversationKind::Group;
        }
        let result = service.execute(VoiceCommand::Enable, &request).unwrap();
        assert_eq!(result.text, "只有群主或管理员可以修改群聊语音设置");
        assert!(!service.enabled_for_request(&request).unwrap());
        assert!(service.execute(VoiceCommand::Query, &request).is_ok());
    }

    for role in ["owner", "admin"] {
        let request = request("bot-a", &format!("group-{role}"), Some(role));
        service.execute(VoiceCommand::Enable, &request).unwrap();
        assert!(service.enabled_for_request(&request).unwrap());
    }
}

#[test]
fn unavailable_tts_rejects_enable_without_overwriting_existing_state() {
    let database =
        SqliteDatabase::open_temp("voice-unavailable", &[VOICE_PREFERENCE_SCHEMA_V1]).unwrap();
    let store = VoicePreferenceStore::new(database);
    let request = request("bot-a", "user-a", None);
    VoicePreferenceService::new(store.clone(), available_config())
        .execute(VoiceCommand::Enable, &request)
        .unwrap();

    let disabled = VoicePreferenceService::new(store.clone(), VoiceFeatureConfig::default());
    let result = disabled.execute(VoiceCommand::Enable, &request).unwrap();
    assert!(result.text.contains("TTS_PROVIDER=qwen"));
    assert!(disabled.enabled_for_request(&request).unwrap());

    disabled.execute(VoiceCommand::Disable, &request).unwrap();
    let missing_key = VoicePreferenceService::new(
        store,
        VoiceFeatureConfig::from_environment(&HashMap::from([(
            "TTS_PROVIDER".to_owned(),
            "qwen".to_owned(),
        )])),
    );
    let result = missing_key.execute(VoiceCommand::Enable, &request).unwrap();
    assert!(result.text.contains("QWEN_TTS_API_KEY"));
    assert!(!missing_key.enabled_for_request(&request).unwrap());
}

#[test]
fn command_parser_accepts_only_the_declared_shape() {
    assert_eq!(parse_voice_command("语音"), Some(VoiceCommand::Query));
    assert_eq!(parse_voice_command("语音 开启"), Some(VoiceCommand::Enable));
    assert_eq!(
        parse_voice_command("语音 关闭"),
        Some(VoiceCommand::Disable)
    );
    assert_eq!(
        parse_voice_command("语音 开启 多余"),
        Some(VoiceCommand::Invalid)
    );
    assert_eq!(parse_voice_command("声音 开启"), None);
    for alias in ["开启", "打开", "on"] {
        assert_eq!(
            parse_voice_command(&format!("语音 {alias}")),
            Some(VoiceCommand::Enable)
        );
    }
    for alias in ["关闭", "关掉", "off"] {
        assert_eq!(
            parse_voice_command(&format!("语音 {alias}")),
            Some(VoiceCommand::Disable)
        );
    }
}

#[test]
fn incomplete_identity_or_group_context_never_reads_or_writes_preferences() {
    // 故意不创建语音表：若边界校验失效，下面任一调用都会返回存储错误。
    let database = SqliteDatabase::open_temp("voice-incomplete", &[]).unwrap();
    let service =
        VoicePreferenceService::new(VoicePreferenceStore::new(database), available_config());

    let mut missing_account = request("bot-a", "user-a", None);
    missing_account.account_id = None;
    assert!(!service.enabled_for_request(&missing_account).unwrap());
    assert!(
        service
            .execute(VoiceCommand::Enable, &missing_account)
            .unwrap()
            .text
            .contains("暂不支持")
    );

    let mut incomplete_group = request("bot-a", "group-a", Some("owner"));
    incomplete_group.group_id = None;
    assert!(!service.enabled_for_request(&incomplete_group).unwrap());

    let mut mismatched_group = request("bot-a", "group-a", Some("owner"));
    mismatched_group.group_id = Some("group-b".to_owned());
    assert!(!service.enabled_for_request(&mismatched_group).unwrap());

    let mut unknown = request("bot-a", "user-a", None);
    unknown.conversation_kind = ConversationKind::Unknown;
    assert!(!service.enabled_for_request(&unknown).unwrap());
}

#[test]
fn conversation_kind_is_authoritative_for_private_and_group_keys() {
    let database = SqliteDatabase::open_temp("voice-kind", &[VOICE_PREFERENCE_SCHEMA_V1]).unwrap();
    let service =
        VoicePreferenceService::new(VoicePreferenceStore::new(database), available_config());
    let mut private = request("bot-a", "same-target", None);
    // 即使携带了非权威 group_id，Private 仍按私聊 key 和私聊权限处理。
    private.group_id = Some("same-target".to_owned());
    private.group_member_role = Some("member".to_owned());
    service.execute(VoiceCommand::Enable, &private).unwrap();
    assert!(service.enabled_for_request(&private).unwrap());

    let group = request("bot-a", "same-target", Some("owner"));
    assert!(!service.enabled_for_request(&group).unwrap());
}
