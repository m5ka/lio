use crate::Client;
use crate::error::Result;

/// What to star or unstar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StarTarget {
    Song(String),
    Album(String),
    Artist(String),
}

impl StarTarget {
    fn param(&self) -> (&'static str, String) {
        match self {
            Self::Song(id) => ("id", id.clone()),
            Self::Album(id) => ("albumId", id.clone()),
            Self::Artist(id) => ("artistId", id.clone()),
        }
    }
}

impl Client {
    pub async fn star(&self, target: &StarTarget) -> Result<()> {
        self.call("star", &[target.param()]).await
    }

    pub async fn unstar(&self, target: &StarTarget) -> Result<()> {
        self.call("unstar", &[target.param()]).await
    }

    /// `setRating`: 1–5, or 0 to clear.
    pub async fn set_rating(&self, id: &str, rating: u8) -> Result<()> {
        self.call(
            "setRating",
            &[("id", id.to_owned()), ("rating", rating.min(5).to_string())],
        )
        .await
    }

    /// `scrobble`: `submission = false` reports "now playing", `true` a completed play
    /// at `time_ms` (Unix milliseconds).
    pub async fn scrobble(&self, id: &str, submission: bool, time_ms: Option<i64>) -> Result<()> {
        let mut params = vec![
            ("id", id.to_owned()),
            ("submission", submission.to_string()),
        ];
        if let Some(t) = time_ms {
            params.push(("time", t.to_string()));
        }
        self.call("scrobble", &params).await
    }
}
