//! Account session: sign-in, restore, sign-out, secrets and connectivity state.
//!
//! State machine:
//! `SignedOut → SigningIn → SignedIn ⇄ Offline`, `SignedIn → ReauthRequired → SignedIn | SignedOut`.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::{AccountPaths, Db, TrackStore};
use lio_subsonic::types::EXT_API_KEY;
use lio_subsonic::{Client, Credentials, Error, TlsConfig};

use crate::runtime;
use crate::settings::Settings;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "LioSessionState")]
pub enum SessionState {
    #[default]
    SignedOut,
    SigningIn,
    SignedIn,
    Offline,
    ReauthRequired,
}

/// Everything that exists while an account is signed in.
#[derive(Clone)]
pub struct Account {
    pub client: Client,
    pub paths: AccountPaths,
    pub db: Db,
    pub tracks: TrackStore,
    pub server_url: String,
    pub username: String,
    /// `downloadRole` from `getUser`; gates downloads.
    pub can_download: bool,
}

/// Details to show after a successful sign-in.
#[derive(Debug, Clone, Default)]
pub struct SignInInfo {
    /// Server isn't Navidrome with OpenSubsonic: show a "may not be fully compatible" notice.
    pub limited_compatibility: bool,
}

mod imp {
    use std::cell::Cell;

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::LioSession)]
    pub struct LioSession {
        #[property(get, builder(SessionState::SignedOut))]
        pub state: Cell<SessionState>,
        pub account: RefCell<Option<Account>>,
        pub settings: RefCell<Option<Settings>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioSession {
        const NAME: &'static str = "LioSession";
        type Type = super::LioSession;
    }

    #[glib::derived_properties]
    impl ObjectImpl for LioSession {}
}

glib::wrapper! {
    pub struct LioSession(ObjectSubclass<imp::LioSession>);
}

impl LioSession {
    pub fn new(settings: &Settings) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().settings.replace(Some(settings.clone()));
        obj
    }

    fn settings(&self) -> Settings {
        self.imp()
            .settings
            .borrow()
            .clone()
            .expect("settings set in new()")
    }

    pub fn account(&self) -> Option<Account> {
        self.imp().account.borrow().clone()
    }

    pub fn client(&self) -> Option<Client> {
        self.account().map(|a| a.client)
    }

    pub fn set_state(&self, state: SessionState) {
        if self.imp().state.replace(state) != state {
            tracing::info!(?state, "session state");
            if let Some(account) = self.account() {
                account.tracks.set_online(state == SessionState::SignedIn);
            }
            self.notify_state();
        }
    }

    pub fn is_online(&self) -> bool {
        self.state() == SessionState::SignedIn
    }

    /// Signs in with a password. On `Error::Certificate` the caller may ask the user and
    /// retry with `trusted_cert` set to the fingerprint.
    pub async fn sign_in(
        &self,
        server_url: &str,
        username: &str,
        password: &str,
        trusted_cert: Option<String>,
    ) -> Result<SignInInfo, Error> {
        self.set_state(SessionState::SigningIn);
        let result = self
            .try_sign_in(server_url, username, password, trusted_cert, true)
            .await;
        if result.is_err() {
            let state = if self.account().is_some() {
                SessionState::ReauthRequired
            } else {
                SessionState::SignedOut
            };
            self.set_state(state);
        }
        result
    }

    async fn try_sign_in(
        &self,
        server_url: &str,
        username: &str,
        password: &str,
        trusted_cert: Option<String>,
        store_secret: bool,
    ) -> Result<SignInInfo, Error> {
        let base = lio_subsonic::normalize_base_url(server_url)?;
        let base_str = base.as_str().trim_end_matches('/').to_owned();
        let tls = trusted_cert
            .clone()
            .map(TlsConfig::Pinned)
            .unwrap_or_default();
        let client = Client::new(&base_str, Credentials::token(username, password), tls)?;

        let (info, extensions, user) = {
            let client = client.clone();
            let username = username.to_owned();
            runtime::run(async move {
                let info = client.ping().await?;
                let extensions = client
                    .get_open_subsonic_extensions()
                    .await
                    .unwrap_or_default();
                let user = client.get_user(&username).await.ok();
                Ok::<_, Error>((info, extensions, user))
            })
            .await?
        };
        let extension_names: Vec<String> = extensions.iter().map(|e| e.name.clone()).collect();
        if extension_names.iter().any(|e| e == EXT_API_KEY) {
            tracing::info!(
                "server supports API keys; Lio signs in with a password and uses token auth"
            );
        }

        if store_secret {
            let (server, user_name, secret) =
                (base_str.clone(), username.to_owned(), password.to_owned());
            if let Err(e) =
                runtime::run(async move { secrets::store(&server, &user_name, &secret).await })
                    .await
            {
                tracing::error!("could not store the password in the keyring: {e}");
            }
        }

        let settings = self.settings();
        let _ = settings.set_string("server-url", &base_str);
        let _ = settings.set_string("username", username);
        let _ = settings.set_string("auth-mode", "token");
        let _ = settings.set_string("pinned-cert-sha256", trusted_cert.as_deref().unwrap_or(""));
        let _ = settings.set_string("server-type", &info.server_type);
        let _ = settings.set_string("server-version", &info.server_version);
        let _ = settings.set_string("api-version", &info.version);
        let ext_refs: Vec<&str> = extension_names.iter().map(String::as_str).collect();
        let _ = settings.set_strv("extensions", ext_refs);

        let can_download = user.as_ref().is_none_or(|u| u.download_role);
        self.open_account(client, &base_str, username, can_download)?;
        let _ = settings.set_boolean("offline-mode", false);
        self.set_state(SessionState::SignedIn);
        Ok(SignInInfo {
            limited_compatibility: !info.is_fully_compatible(),
        })
    }

    fn open_account(
        &self,
        client: Client,
        server_url: &str,
        username: &str,
        can_download: bool,
    ) -> Result<(), Error> {
        // Keep the existing account (and its queue) when re-authenticating the same user.
        if let Some(existing) = self.account()
            && existing.server_url == server_url
            && existing.username == username
        {
            existing.tracks.set_client(Some(client.clone()));
            self.imp().account.replace(Some(Account {
                client,
                can_download,
                ..existing
            }));
            return Ok(());
        }
        let paths = AccountPaths::new(server_url, username);
        paths
            .create_all()
            .map_err(|e| Error::Decode(format!("cannot create data directories: {e}")))?;
        let db = Db::open(&paths.db()).map_err(|e| Error::Decode(e.to_string()))?;
        let tracks = TrackStore::new(
            db.clone(),
            paths.clone(),
            runtime::runtime().handle().clone(),
        );
        tracks.set_client(Some(client.clone()));
        self.imp().account.replace(Some(Account {
            client,
            paths,
            db,
            tracks,
            server_url: server_url.to_owned(),
            username: username.to_owned(),
            can_download,
        }));
        Ok(())
    }

    /// Restores the saved account without network access (instant launch), then checks
    /// the server in the background. Returns `false` when there is nothing to restore.
    pub async fn restore(&self) -> bool {
        if let Some(debug) = debug_credentials() {
            tracing::warn!("signing in from LIO_TEST_* environment variables (debug build)");
            if std::env::var("LIO_TEST_OFFLINE").is_ok() {
                // Open the saved mirror without contacting the server.
                let Ok(base) = lio_subsonic::normalize_base_url(&debug.0) else {
                    return false;
                };
                let base = base.as_str().trim_end_matches('/').to_owned();
                let Ok(client) = Client::new(
                    &base,
                    Credentials::token(&debug.1, &debug.2),
                    TlsConfig::System,
                ) else {
                    return false;
                };
                if self.open_account(client, &base, &debug.1, true).is_err() {
                    return false;
                }
                self.set_state(SessionState::Offline);
                return true;
            }
            return self
                .try_sign_in(&debug.0, &debug.1, &debug.2, None, false)
                .await
                .is_ok();
        }
        let settings = self.settings();
        let (server, username) = (settings.server_url(), settings.username());
        if server.is_empty() || username.is_empty() {
            return false;
        }
        let password = {
            let (server, username) = (server.clone(), username.clone());
            runtime::run(async move { secrets::load(&server, &username).await }).await
        };
        let password = match password {
            Ok(Some(p)) => p,
            Ok(None) => {
                tracing::warn!("no password in the keyring for the saved account");
                return false;
            }
            Err(e) => {
                tracing::error!("keyring unavailable: {e}");
                return false;
            }
        };
        let tls = settings
            .pinned_cert()
            .map(TlsConfig::Pinned)
            .unwrap_or_default();
        let Ok(client) = Client::new(&server, Credentials::token(&username, &password), tls) else {
            return false;
        };
        if self.open_account(client, &server, &username, true).is_err() {
            return false;
        }
        // Show the mirrored library right away; the connection check follows.
        let state = if settings.offline_mode() {
            SessionState::Offline
        } else {
            SessionState::SignedIn
        };
        self.set_state(state);
        self.check_connection();
        true
    }

    /// Pings the server and moves between `SignedIn`, `Offline` and `ReauthRequired`.
    pub fn check_connection(&self) {
        let Some(client) = self.client() else { return };
        if self.settings().offline_mode()
            || (cfg!(debug_assertions) && std::env::var("LIO_TEST_OFFLINE").is_ok())
        {
            self.set_state(SessionState::Offline);
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = session)]
            self,
            async move {
                let result = runtime::run(async move { client.ping().await }).await;
                match result {
                    Ok(_) => session.set_state(SessionState::SignedIn),
                    Err(e) if e.is_auth() => session.set_state(SessionState::ReauthRequired),
                    Err(e) => {
                        tracing::info!("server unreachable: {e}");
                        session.set_state(SessionState::Offline);
                    }
                }
            }
        ));
    }

    /// Deletes the secret, settings and every local file of the account.
    pub async fn sign_out(&self) {
        let settings = self.settings();
        let (server, username) = (settings.server_url(), settings.username());
        if let Some(account) = self.imp().account.take() {
            account.tracks.cancel_all();
            let paths = account.paths.clone();
            drop(account);
            if let Err(e) = runtime::run(async move { lio_store::wipe_account(&paths) }).await {
                tracing::error!("could not remove account data: {e}");
            }
        }
        if !server.is_empty() {
            let _ = runtime::run(async move { secrets::delete(&server, &username).await }).await;
        }
        settings.reset_account();
        self.set_state(SessionState::SignedOut);
    }
}

/// `(server, user, password)` from `LIO_TEST_SERVER` / `LIO_TEST_USER` / `LIO_TEST_PASSWORD`,
/// honoured only in debug builds (integration tests, development).
fn debug_credentials() -> Option<(String, String, String)> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let server = std::env::var("LIO_TEST_SERVER").ok()?;
    let user = std::env::var("LIO_TEST_USER").unwrap_or_else(|_| "lio".into());
    let password = std::env::var("LIO_TEST_PASSWORD").unwrap_or_else(|_| "lio-test".into());
    Some((server, user, password))
}

/// Secret Service / Secret portal storage via oo7. Must run on Tokio.
mod secrets {
    use super::HashMap;

    /// oo7's error type is large; callers only log it.
    type Result<T> = std::result::Result<T, String>;

    fn attributes<'a>(server: &'a str, user: &'a str) -> HashMap<&'static str, &'a str> {
        HashMap::from([("app", "lio"), ("server", server), ("user", user)])
    }

    pub async fn store(server: &str, user: &str, secret: &str) -> Result<()> {
        let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
        keyring.unlock().await.map_err(|e| e.to_string())?;
        let label = format!("Lio: {user} on {server}");
        keyring
            .create_item(&label, &attributes(server, user), secret, true)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn load(server: &str, user: &str) -> Result<Option<String>> {
        let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
        keyring.unlock().await.map_err(|e| e.to_string())?;
        let items = keyring
            .search_items(&attributes(server, user))
            .await
            .map_err(|e| e.to_string())?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        item.unlock().await.map_err(|e| e.to_string())?;
        let secret = item.secret().await.map_err(|e| e.to_string())?;
        Ok(Some(
            String::from_utf8_lossy(secret.as_bytes()).into_owned(),
        ))
    }

    pub async fn delete(server: &str, user: &str) -> Result<()> {
        let keyring = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
        keyring
            .delete(&attributes(server, user))
            .await
            .map_err(|e| e.to_string())
    }
}
