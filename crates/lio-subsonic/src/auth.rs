//! Request authentication (Subsonic token auth or OpenSubsonic API keys).

use md5::{Digest, Md5};
use rand::Rng;
use rand::distr::Alphanumeric;

/// Subsonic API version Lio speaks.
pub const API_VERSION: &str = "1.16.1";
/// Client name sent as `c`.
pub const CLIENT_NAME: &str = "lio";

const SALT_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// `u` + `t = md5(password + s)` + random `s` per request.
    #[default]
    Token,
    /// OpenSubsonic `apiKeyAuthentication` extension.
    ApiKey,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::ApiKey => "apikey",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        if s == "apikey" {
            Self::ApiKey
        } else {
            Self::Token
        }
    }
}

/// Account credentials. `secret` is the password (token mode) or the API key.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub secret: String,
    pub mode: AuthMode,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .field("mode", &self.mode)
            .finish()
    }
}

impl Credentials {
    pub fn token(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            secret: password.into(),
            mode: AuthMode::Token,
        }
    }

    pub fn api_key(username: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            secret: key.into(),
            mode: AuthMode::ApiKey,
        }
    }

    /// Authentication and protocol parameters for one request. A fresh salt is
    /// generated on every call. The plain password (`p`) is never emitted.
    pub fn query_params(&self) -> Vec<(&'static str, String)> {
        let mut params = Vec::with_capacity(6);
        match self.mode {
            AuthMode::Token => {
                let salt = random_salt();
                params.push(("u", self.username.clone()));
                params.push(("t", token(&self.secret, &salt)));
                params.push(("s", salt));
            }
            AuthMode::ApiKey => params.push(("apiKey", self.secret.clone())),
        }
        params.push(("v", API_VERSION.to_owned()));
        params.push(("c", CLIENT_NAME.to_owned()));
        params.push(("f", "json".to_owned()));
        params
    }
}

fn random_salt() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(SALT_LEN)
        .map(char::from)
        .collect()
}

/// `md5(password + salt)` as lowercase hex.
pub fn token(password: &str, salt: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_subsonic_example() {
        // Example from the Subsonic API documentation.
        assert_eq!(
            token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn token_mode_never_sends_password() {
        let creds = Credentials::token("alice", "sesame");
        let params = creds.query_params();
        assert!(params.iter().all(|(k, v)| *k != "p" && v != "sesame"));
        let keys: Vec<_> = params.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["u", "t", "s", "v", "c", "f"]);
    }

    #[test]
    fn salt_changes_between_calls() {
        let creds = Credentials::token("alice", "sesame");
        let s1 = creds.query_params()[2].1.clone();
        let s2 = creds.query_params()[2].1.clone();
        assert_eq!(s1.len(), SALT_LEN);
        assert_ne!(s1, s2);
    }

    #[test]
    fn api_key_mode() {
        let creds = Credentials::api_key("alice", "k3y");
        let params = creds.query_params();
        assert_eq!(params[0], ("apiKey", "k3y".to_owned()));
        assert!(
            params
                .iter()
                .all(|(k, _)| !matches!(*k, "u" | "t" | "s" | "p"))
        );
    }
}
