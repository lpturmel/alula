use rustls::crypto::CryptoProvider;

/// Selects one process-wide Rustls provider before any TLS client is built.
///
/// Alula's UI stack and networking stack can enable different Rustls provider
/// features. Installing `ring` explicitly keeps that feature unification from
/// making provider selection ambiguous. A provider installed earlier by an
/// embedding process is respected.
pub fn install_tls_crypto_provider() {
    if CryptoProvider::get_default().is_none() {
        // Losing a race to another caller is harmless: Rustls has a provider
        // after either successful installation.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_installation_is_early_and_idempotent() {
        install_tls_crypto_provider();
        install_tls_crypto_provider();

        assert!(CryptoProvider::get_default().is_some());
        let _client = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}
