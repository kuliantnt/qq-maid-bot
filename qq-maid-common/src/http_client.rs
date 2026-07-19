//! 统一的 reqwest rustls 客户端配置。

use std::sync::Arc;

use rustls_platform_verifier::BuilderVerifierExt;

/// 创建显式使用 ring 和系统证书校验器的 reqwest builder。
///
/// 这里传入预配置的 `ClientConfig`，避免 `rustls-no-provider` 要求调用全局
/// `install_default()`，也不会让其他依赖改变本项目的 CryptoProvider 选择。
pub fn try_builder() -> Result<reqwest::ClientBuilder, rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(reqwest::Client::builder().tls_backend_preconfigured(tls))
}

/// 与 `reqwest::Client::new()` 一致，构建失败时直接暴露错误而不伪造可用客户端。
pub fn client() -> reqwest::Client {
    try_builder()
        .expect("ring-backed rustls configuration must be valid")
        .build()
        .expect("ring-backed reqwest client must initialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ring_client_without_installing_global_provider() {
        let had_no_default = rustls::crypto::CryptoProvider::get_default().is_none();
        try_builder().unwrap().build().unwrap();

        if had_no_default {
            assert!(rustls::crypto::CryptoProvider::get_default().is_none());
        }
    }
}
