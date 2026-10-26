#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Subsonic(#[from] lio_subsonic::Error),
    /// Waited [`crate::track_store::READ_TIMEOUT`] for audio bytes that never arrived.
    #[error("timed out waiting for audio data")]
    Timeout,
    /// Offline and no local copy exists.
    #[error("not available offline")]
    UnavailableOffline,
    #[error("cancelled")]
    Cancelled,
    #[error("disk full")]
    DiskFull,
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Maps `ENOSPC` to [`Error::DiskFull`].
    pub fn from_io(e: std::io::Error) -> Self {
        if e.raw_os_error() == Some(libc::ENOSPC) {
            Self::DiskFull
        } else {
            Self::Io(e)
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
