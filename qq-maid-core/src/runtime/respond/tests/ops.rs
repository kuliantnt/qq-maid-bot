use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use qq_maid_common::identity_context::ConversationKind;

use crate::runtime::tools::ops::{OpsConfig, OpsService};

use super::{super::common::empty_respond_request, support::*};

#[tokio::test]
async fn ops_is_deterministic_and_never_calls_llm() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (mut service, _) =
        test_service_with_provider_and_base(MockProvider::with_counter(calls.clone()));
    service.ops_service = OpsService::new(OpsConfig::default(), service.notification_store.clone());
    let response = service
        .respond(crate::runtime::respond::RespondRequest {
            content: "/ops status".to_owned(),
            scope_key: "platform:qq_official:account:app:private:user".to_owned(),
            conversation_kind: ConversationKind::Private,
            conversation_id: Some("user".to_owned()),
            user_id: Some("user".to_owned()),
            user_identity_source: Some(qq_maid_common::identity_context::IdentitySource::Event),
            platform: "qq_official".to_owned(),
            account_id: Some("app".to_owned()),
            ..empty_respond_request()
        })
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("ops"));
    assert!(response.text.unwrap().contains("未启用"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        service
            .notification_store
            .list_all_for_test()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn custom_prefix_keeps_ops_deterministic_permission_boundary() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (mut service, _) =
        test_service_with_provider_and_base(MockProvider::with_counter(calls.clone()));
    service.command_prefix = qq_maid_common::command_prefix::CommandPrefix::parse("*").unwrap();
    service.ops_service = OpsService::new(OpsConfig::default(), service.notification_store.clone());

    let response = service
        .respond(crate::runtime::respond::RespondRequest {
            content: "*ops status".to_owned(),
            scope_key: "platform:qq_official:account:app:private:user".to_owned(),
            conversation_kind: ConversationKind::Private,
            conversation_id: Some("user".to_owned()),
            user_id: Some("user".to_owned()),
            user_identity_source: Some(qq_maid_common::identity_context::IdentitySource::Event),
            platform: "qq_official".to_owned(),
            account_id: Some("app".to_owned()),
            ..empty_respond_request()
        })
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("ops"));
    assert!(response.text.unwrap().contains("未启用"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
