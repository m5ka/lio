//! HTTP client: base URL handling, TLS (system trust + optional pinned fingerprint),
//! envelope unwrapping, error mapping and secret redaction for logs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use url::Url;

use crate::auth::Credentials;
use crate::error::{Error, Result};

/// Query parameters for one request. Keys may repeat (e.g. `songId`).
pub type Params = Vec<(&'static str, String)>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const API_TIMEOUT: Duration = Duration::from_secs(30);
const SECRET_PARAMS: [&str; 4] = ["t", "s", "apiKey", "p"];

/// Which certificates to accept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TlsConfig {
    /// System trust store only.
    #[default]
    System,
    /// System trust store, or a leaf certificate with exactly this SHA-256 fingerprint
    /// (lowercase hex), after the user explicitly trusted it.
    Pinned(String),
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base: Url,
    credentials: Credentials,
    /// Fingerprint of the most recent leaf certificate seen during a handshake.
    last_cert: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.inner.base.as_str())
            .field("credentials", &self.inner.credentials)
            .finish()
    }
}

impl Client {
    pub fn new(base_url: &str, credentials: Credentials, tls: TlsConfig) -> Result<Self> {
        let base = normalize_base_url(base_url)?;
        let last_cert = Arc::new(Mutex::new(None));
        let tls_config = rustls_config(&tls, last_cert.clone())?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("Lio/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_backend_preconfigured(tls_config)
            .build()
            .map_err(|e| Error::Decode(format!("HTTP client setup failed: {e}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base,
                credentials,
                last_cert,
            }),
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    pub fn credentials(&self) -> &Credentials {
        &self.inner.credentials
    }

    /// The underlying HTTP client, for streaming and cover downloads.
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// Full URL for `endpoint`, including fresh authentication parameters.
    pub fn url(&self, endpoint: &str, params: &[(&'static str, String)]) -> Url {
        let mut url = self.inner.base.clone();
        url.path_segments_mut()
            .expect("http(s) URL")
            .extend(["rest", endpoint]);
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in self.inner.credentials.query_params() {
                q.append_pair(k, &v);
            }
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        url
    }

    /// Calls `endpoint` and returns the inner `subsonic-response` object.
    pub async fn get_envelope(
        &self,
        endpoint: &str,
        params: &[(&'static str, String)],
    ) -> Result<serde_json::Value> {
        let url = self.url(endpoint, params);
        tracing::debug!(url = %redact(&url), "GET");
        let response = self
            .inner
            .http
            .get(url.clone())
            .timeout(API_TIMEOUT)
            .send()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| self.map_transport_error(e))?;
        parse_envelope(status.as_u16(), &body).inspect_err(|e| {
            tracing::debug!(url = %redact(&url), %status, error = %e, "request failed");
        })
    }

    /// Calls `endpoint` and deserializes the payload under `key`
    /// (e.g. `"albumList2"`). A missing key yields `T::default()`.
    pub async fn get<T: DeserializeOwned + Default>(
        &self,
        endpoint: &str,
        params: &[(&'static str, String)],
        key: &str,
    ) -> Result<T> {
        let mut envelope = self.get_envelope(endpoint, params).await?;
        match envelope.get_mut(key).map(serde_json::Value::take) {
            Some(value) => serde_json::from_value(value)
                .map_err(|e| Error::Decode(format!("{endpoint}.{key}: {e}"))),
            None => Ok(T::default()),
        }
    }

    /// Calls an endpoint whose payload is empty (star, scrobble, ...).
    pub async fn call(&self, endpoint: &str, params: &[(&'static str, String)]) -> Result<()> {
        self.get_envelope(endpoint, params).await.map(drop)
    }

    /// Maps a transport-level failure to an [`Error`].
    pub fn map_transport_error(&self, error: reqwest::Error) -> Error {
        // `without_url` keeps the token and salt out of the log.
        let error = error.without_url();
        if tracing::enabled!(tracing::Level::DEBUG) {
            let mut chain = vec![error.to_string()];
            let mut source = std::error::Error::source(&error);
            while let Some(e) = source {
                chain.push(e.to_string());
                source = e.source();
            }
            tracing::debug!(chain = %chain.join(" ← "), "transport error");
        }
        if is_certificate_error(&error) {
            let fingerprint_sha256 = self
                .inner
                .last_cert
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default();
            return Error::Certificate { fingerprint_sha256 };
        }
        if error.is_connect() || error.is_timeout() || error.is_request() {
            return Error::Unreachable;
        }
        if let Some(status) = error.status() {
            return Error::Http(status.as_u16());
        }
        Error::Unreachable
    }
}

fn parse_envelope(status: u16, body: &[u8]) -> Result<serde_json::Value> {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let Some(mut inner) =
        parsed.and_then(|mut v| v.get_mut("subsonic-response").map(serde_json::Value::take))
    else {
        return Err(if status >= 500 {
            Error::Server {
                code: i32::from(status),
                message: "server error".into(),
            }
        } else {
            Error::Incompatible
        });
    };
    if inner.get("status").and_then(|s| s.as_str()) == Some("ok") {
        return Ok(inner);
    }
    let error = inner
        .get_mut("error")
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0) as i32;
    let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_owned();
    Err(Error::from_code(code, message))
}

/// Normalises user input: scheme required (`https://` default), no trailing `/`,
/// path prefix allowed (for servers behind a reverse proxy subpath).
pub fn normalize_base_url(input: &str) -> Result<Url> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidUrl);
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|_| Error::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::InvalidUrl);
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url)
}

/// The URL as a string with secret query values (`t`, `s`, `apiKey`, `p`) masked.
pub fn redact(url: &Url) -> String {
    let mut redacted = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if SECRET_PARAMS.contains(&k.as_ref()) {
                "***".into()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    if pairs.is_empty() {
        return redacted.into();
    }
    redacted.query_pairs_mut().clear().extend_pairs(pairs);
    redacted.into()
}

/// Whether plain HTTP to this host should trigger a warning: anything other than
/// loopback, link-local or private (RFC 1918 / ULA) addresses and `.local` names.
pub fn is_insecure_remote(url: &Url) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => !(ip.is_loopback() || ip.is_private() || ip.is_link_local()),
        Some(url::Host::Ipv6(ip)) => {
            !(ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local())
        }
        Some(url::Host::Domain(d)) => !(d == "localhost" || d.ends_with(".local")),
        None => true,
    }
}

fn is_certificate_error(error: &reqwest::Error) -> bool {
    find_rustls_error(error).is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(_)))
}

/// Finds a rustls error anywhere in the chain. `io::Error`s are unwrapped explicitly:
/// their `source()` skips the error they wrap, and hyper nests them two deep.
fn find_rustls_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a rustls::Error> {
    if let Some(tls) = error.downcast_ref::<rustls::Error>() {
        return Some(tls);
    }
    if let Some(io) = error.downcast_ref::<std::io::Error>()
        && let Some(inner) = io.get_ref()
        && let Some(tls) = find_rustls_error(inner)
    {
        return Some(tls);
    }
    error.source().and_then(find_rustls_error)
}

pub fn fingerprint(cert: &[u8]) -> String {
    hex::encode(Sha256::digest(cert))
}

pub(crate) fn rustls_config(
    tls: &TlsConfig,
    last_cert: Arc<Mutex<Option<String>>>,
) -> Result<rustls::ClientConfig> {
    let setup = |e: rustls::Error| Error::Decode(format!("TLS setup failed: {e}"));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let platform = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(setup)?;
    let pinned = match tls {
        TlsConfig::System => None,
        TlsConfig::Pinned(fp) => Some(fp.to_ascii_lowercase()),
    };
    let verifier = PinningVerifier {
        platform,
        pinned,
        last_cert,
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(setup)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// System verification first; on failure accept only an exact pinned fingerprint match.
#[derive(Debug)]
struct PinningVerifier {
    platform: rustls_platform_verifier::Verifier,
    pinned: Option<String>,
    last_cert: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());
        *self.last_cert.lock().unwrap() = Some(fp.clone());
        match self.platform.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(ok) => Ok(ok),
            Err(_) if self.pinned.as_deref() == Some(fp.as_str()) => {
                Ok(ServerCertVerified::assertion())
            }
            Err(e) => Err(e),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.platform.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.platform.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        let n = |s| normalize_base_url(s).unwrap().to_string();
        assert_eq!(n("music.example.org"), "https://music.example.org/");
        assert_eq!(n("http://localhost:4533/"), "http://localhost:4533/");
        assert_eq!(
            n(" https://example.org/navidrome/ "),
            "https://example.org/navidrome"
        );
        assert_eq!(n("https://example.org/nd?x=1#y"), "https://example.org/nd");
        assert!(normalize_base_url("").is_err());
        assert!(normalize_base_url("ftp://example.org").is_err());
    }

    #[test]
    fn builds_endpoint_url_under_path_prefix() {
        let client = Client::new(
            "https://example.org/navidrome/",
            Credentials::token("u", "p"),
            TlsConfig::System,
        )
        .unwrap();
        let url = client.url("ping", &[]);
        assert_eq!(url.path(), "/navidrome/rest/ping");
        let url = client.url("ping", &[]);
        let root = Client::new(
            "https://example.org",
            Credentials::token("u", "p"),
            TlsConfig::System,
        )
        .unwrap()
        .url("ping", &[]);
        assert_eq!(root.path(), "/rest/ping");
        assert!(url.query().unwrap().contains("c=lio"));
    }

    #[test]
    fn redacts_secrets() {
        let url = Url::parse("https://x/rest/ping?u=bob&t=abc&s=salt&apiKey=k&p=pw&v=1").unwrap();
        let r = redact(&url);
        assert!(r.contains("u=bob"));
        for secret in ["abc", "salt", "=k&", "pw"] {
            assert!(!r.contains(secret), "{r} leaks {secret}");
        }
    }

    #[test]
    fn insecure_remote_detection() {
        let u = |s| Url::parse(s).unwrap();
        assert!(is_insecure_remote(&u("http://music.example.org")));
        assert!(!is_insecure_remote(&u("http://localhost:4533")));
        assert!(!is_insecure_remote(&u("http://192.168.1.5")));
        assert!(!is_insecure_remote(&u("http://10.0.0.2")));
        assert!(!is_insecure_remote(&u("http://nas.local")));
        assert!(!is_insecure_remote(&u("https://music.example.org")));
    }

    #[test]
    fn envelope_errors() {
        assert!(matches!(
            parse_envelope(200, b"<html>"),
            Err(Error::Incompatible)
        ));
        assert!(matches!(
            parse_envelope(502, b"bad gateway"),
            Err(Error::Server { .. })
        ));
        let failed =
            br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"x"}}}"#;
        assert!(matches!(
            parse_envelope(200, failed),
            Err(Error::BadCredentials)
        ));
        let ok = br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
        assert!(parse_envelope(200, ok).is_ok());
    }
}
