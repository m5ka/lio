use crate::Client;
use crate::error::Result;
use crate::types::{Extension, ServerInfo, User};

impl Client {
    /// `ping`: checks reachability and credentials; returns the envelope fields.
    pub async fn ping(&self) -> Result<ServerInfo> {
        let envelope = self.get_envelope("ping", &[]).await?;
        serde_json::from_value(envelope).map_err(|e| crate::Error::Decode(e.to_string()))
    }

    /// `getOpenSubsonicExtensions`. Servers without OpenSubsonic return an empty list.
    pub async fn get_open_subsonic_extensions(&self) -> Result<Vec<Extension>> {
        match self
            .get("getOpenSubsonicExtensions", &[], "openSubsonicExtensions")
            .await
        {
            Err(crate::Error::NotFound) => Ok(Vec::new()),
            other => other,
        }
    }

    /// `getUser` for `username` (permissions such as `downloadRole`).
    pub async fn get_user(&self, username: &str) -> Result<User> {
        self.get("getUser", &[("username", username.to_owned())], "user")
            .await
    }
}
