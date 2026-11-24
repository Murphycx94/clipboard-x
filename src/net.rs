//! What the app's connections to servers share: how TLS is set up under
//! the HTTP clients (reqwest) and the sync events' WebSocket (tungstenite),
//! both over rustls.

use std::sync::Once;

use reqwest::blocking::{Client, ClientBuilder};

/// A builder for an HTTP client. Every client starts here, not from
/// `Client::builder()` (clippy.toml refuses that): that one fails on
/// Android, see [`trust`].
pub fn http_client() -> ClientBuilder {
    #[expect(clippy::disallowed_methods, reason = "where every client starts")]
    let builder = Client::builder();
    trust(builder)
}

/// Whose certificates the client accepts: the system's verifier decides,
/// reqwest's default.
#[cfg(not(target_os = "android"))]
fn trust(builder: ClientBuilder) -> ClientBuilder {
    builder
}

/// Whose certificates the client accepts: on Android, those of the
/// certificate authorities compiled into the app.
///
/// reqwest's default, the system's verifier, needs a Java class in the APK
/// and a call at startup there. NSH's package has neither, and without
/// them the first HTTPS handshake panics: no request ever left the phone.
/// A server whose certificate comes from an authority the user installed
/// on the phone is therefore not accepted.
#[cfg(target_os = "android")]
fn trust(builder: ClientBuilder) -> ClientBuilder {
    trust_bundled_roots(builder)
}

/// Accepts only certificates from Mozilla's list of authorities, as the
/// events' WebSocket does on every platform.
#[cfg(any(target_os = "android", test))]
fn trust_bundled_roots(builder: ClientBuilder) -> ClientBuilder {
    // Kept as bytes here and read when the client is built, where one that
    // rustls cannot read fails the whole client: the test below is what
    // keeps a new list of authorities from doing that.
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    builder.tls_certs_only(roots)
}

/// Chooses rustls's crypto provider for the process, once.
///
/// Both of its providers are compiled in (reqwest brings aws-lc-rs, NSH
/// ring), and rustls then refuses to pick one by itself: a
/// `ClientConfig::builder()` before this, such as tungstenite's for
/// `wss://`, panics. reqwest names its own and is not concerned.
pub fn choose_crypto_provider() {
    static CHOSEN: Once = Once::new();
    CHOSEN.call_once(|| {
        // Refused only when the process already has one: as good.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_trusting_the_bundled_authorities_builds() {
        // Android's client. One certificate rustls cannot read fails it.
        assert!(webpki_root_certs::TLS_SERVER_ROOT_CERTS.len() > 100);
        #[expect(clippy::disallowed_methods, reason = "the builder under test")]
        let client = trust_bundled_roots(Client::builder()).build();
        assert!(client.is_ok(), "{:?}", client.err());
    }

    // `choose_crypto_provider` is tested where it matters, by the events
    // connection over TLS (cloud.rs): a test of it here would choose for
    // the whole test process and hide a call missing there.
}
