//! Self-signed TLS: the first contact reports the
//! certificate's fingerprint; after the user trusts it, the pinned fingerprint connects.
//!
//! Needs `LIO_TEST_TLS_SERVER` (an https Navidrome with a self-signed certificate, e.g.
//! `tests/test-server.sh up --tls-self-signed`); skipped otherwise.

use lio_subsonic::{Client, Credentials, Error, TlsConfig};

use crate::common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs an https test server (LIO_TEST_TLS_SERVER)"]
async fn self_signed_certificate_can_be_pinned() {
    let Ok(url) = std::env::var("LIO_TEST_TLS_SERVER") else {
        eprintln!("LIO_TEST_TLS_SERVER not set; skipping");
        return;
    };
    let srv = common::server();
    let creds = Credentials::token(&srv.user, &srv.password);

    let untrusted = Client::new(&url, creds.clone(), TlsConfig::System).unwrap();
    let fingerprint = match untrusted.ping().await {
        Err(Error::Certificate { fingerprint_sha256 }) => fingerprint_sha256,
        other => panic!("expected a certificate error, got {other:?}"),
    };
    assert_eq!(fingerprint.len(), 64, "SHA-256 hex");
    if let Ok(expected) = std::env::var("LIO_TEST_TLS_FINGERPRINT") {
        assert_eq!(fingerprint, expected);
    }

    let pinned = Client::new(&url, creds.clone(), TlsConfig::Pinned(fingerprint)).unwrap();
    assert_eq!(
        pinned
            .ping()
            .await
            .expect("pinned certificate connects")
            .server_type,
        "navidrome"
    );

    let wrong = Client::new(&url, creds, TlsConfig::Pinned("00".repeat(32))).unwrap();
    assert!(
        matches!(wrong.ping().await, Err(Error::Certificate { .. })),
        "only the exact fingerprint is trusted"
    );
}
