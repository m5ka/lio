//! Client errors, mapped from transport failures and Subsonic error codes.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// DNS failure, connection refused or timeout.
    #[error("can't reach the server")]
    Unreachable,
    /// TLS verification failed. The fingerprint lets the user decide to trust it.
    #[error("the server's certificate isn't trusted")]
    Certificate { fingerprint_sha256: String },
    /// The response isn't a Subsonic response.
    #[error("this doesn't look like a Navidrome server")]
    Incompatible,
    /// Error codes 40, 41 and 44.
    #[error("wrong username or password")]
    BadCredentials,
    /// Error codes 20 and 30.
    #[error("server version not supported")]
    ServerTooOld,
    /// Error code 50.
    #[error("not allowed")]
    NotAuthorized,
    /// Error code 70.
    #[error("not found")]
    NotFound,
    /// Any other Subsonic error.
    #[error("server error {code}: {message}")]
    Server { code: i32, message: String },
    /// The response was a valid envelope but its payload didn't parse.
    #[error("unexpected response: {0}")]
    Decode(String),
    /// The server address can't be used.
    #[error("invalid server address")]
    InvalidUrl,
    /// Unexpected HTTP status on a non-API request (streams, covers).
    #[error("HTTP status {0}")]
    Http(u16),
}

impl Error {
    /// Maps a Subsonic `error.code`.
    pub fn from_code(code: i32, message: String) -> Self {
        match code {
            40 | 41 | 44 => Self::BadCredentials,
            20 | 30 => Self::ServerTooOld,
            50 => Self::NotAuthorized,
            70 => Self::NotFound,
            _ => Self::Server { code, message },
        }
    }

    /// Whether the error means the server can't be reached at all (used for offline detection).
    pub fn is_connectivity(&self) -> bool {
        matches!(self, Self::Unreachable)
    }

    pub fn is_auth(&self) -> bool {
        matches!(self, Self::BadCredentials)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
