use super::*;

#[test]
fn connection_target_accepts_configured_custom_https_hosts() {
    let environment = std::collections::HashMap::from([
        ("OPENAI_API_KEY".to_owned(), "secret-value".to_owned()),
        (
            "OPENAI_BASE_URLS".to_owned(),
            "https://api.openai.com/v1".to_owned(),
        ),
    ]);
    let (url, key) = connection_test_target("openai", &environment).unwrap();
    assert_eq!(url.as_str(), "https://api.openai.com/v1/models");
    assert_eq!(key, "secret-value");

    let mut custom = environment;
    custom.insert(
        "OPENAI_BASE_URLS".to_owned(),
        "https://provider.example.com/openai/v1".to_owned(),
    );
    let (url, _) = connection_test_target("openai", &custom).unwrap();
    assert_eq!(
        url.as_str(),
        "https://provider.example.com/openai/v1/models"
    );

    custom.insert(
        "OPENAI_BASE_URLS".to_owned(),
        "http://127.0.0.1:8080/v1".to_owned(),
    );
    assert!(connection_test_target("openai", &custom).is_err());
}

#[test]
fn connection_target_rejects_non_public_addresses() {
    for value in [
        "127.0.0.1",
        "10.0.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.0.1",
        "198.18.0.1",
        "203.0.113.1",
        "::1",
        "fc00::1",
        "fe80::1",
        "fec0::1",
        "64:ff9b::a00:1",
        "2001:db8::1",
    ] {
        assert!(is_non_public_ip(value.parse().unwrap()), "{value}");
    }
    for value in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(!is_non_public_ip(value.parse().unwrap()), "{value}");
    }
    assert!(is_blocked_connection_hostname("metadata.google.internal"));
}

#[test]
fn opencode_connection_target_uses_official_side_effect_free_models_endpoint() {
    let environment = std::collections::HashMap::from([(
        "OPENCODE_API_KEY".to_owned(),
        "shared-secret".to_owned(),
    )]);
    let (url, key) = connection_test_target("opencode", &environment).unwrap();
    assert_eq!(url.as_str(), "https://opencode.ai/zen/v1/models");
    assert_eq!(key, "shared-secret");
}

#[test]
fn connection_status_has_stable_safe_classifications() {
    assert!(classify_connection_status(reqwest::StatusCode::OK).0);
    assert_eq!(
        classify_connection_status(reqwest::StatusCode::UNAUTHORIZED).1,
        "authentication_failed"
    );
    assert_eq!(
        classify_connection_status(reqwest::StatusCode::TOO_MANY_REQUESTS).1,
        "upstream_rate_limited"
    );
    assert_eq!(
        classify_connection_status(reqwest::StatusCode::NOT_FOUND).1,
        "endpoint_unsupported"
    );
}
