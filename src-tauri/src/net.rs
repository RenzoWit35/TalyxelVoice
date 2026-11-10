//! One HTTP client for model downloads and both servers.
//!
//! reqwest checks certificates with the OS verifier, which on Android needs a
//! JVM handle and a Kotlin helper set up first; without them the first HTTPS
//! request panics. There the client trusts Mozilla's root list instead.

use std::sync::OnceLock;

pub fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(build).clone()
}

#[cfg(not(target_os = "android"))]
fn build() -> reqwest::Client {
    reqwest::Client::new()
}

#[cfg(target_os = "android")]
fn build() -> reqwest::Client {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    reqwest::Client::builder()
        .tls_certs_only(roots)
        .build()
        .unwrap_or_else(|e| {
            log::error!("HTTP client: {e}");
            reqwest::Client::new()
        })
}
