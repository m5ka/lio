//! Per-account file locations.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// First 16 hex chars of `sha256(server_url_normalised + "\n" + username)`.
pub fn account_hash(server_url_normalised: &str, username: &str) -> String {
    let digest = Sha256::digest(format!("{server_url_normalised}\n{username}").as_bytes());
    hex::encode(digest)[..16].to_owned()
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/".into());
            home.join(fallback)
        })
}

pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

#[derive(Debug, Clone)]
pub struct AccountPaths {
    pub account: String,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AccountPaths {
    /// Paths under the current `XDG_DATA_HOME` / `XDG_CACHE_HOME`.
    pub fn new(server_url_normalised: &str, username: &str) -> Self {
        Self::with_roots(server_url_normalised, username, &data_home(), &cache_home())
    }

    pub fn with_roots(server_url: &str, username: &str, data: &Path, cache: &Path) -> Self {
        let account = account_hash(server_url, username);
        Self {
            data_dir: data.join("lio").join(&account),
            cache_dir: cache.join("lio").join(&account),
            account,
        }
    }

    /// `$XDG_DATA_HOME/lio/<acct>/library.db`
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("library.db")
    }

    /// `$XDG_DATA_HOME/lio/<acct>/downloads/`
    pub fn downloads(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    /// `$XDG_CACHE_HOME/lio/<acct>/audio/`
    pub fn audio_cache(&self) -> PathBuf {
        self.cache_dir.join("audio")
    }

    /// `$XDG_CACHE_HOME/lio/<acct>/covers/`
    pub fn covers(&self) -> PathBuf {
        self.cache_dir.join("covers")
    }

    /// Creates the account's directories, readable only by the user (they hold
    /// listening history and downloaded music).
    pub fn create_all(&self) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        for dir in [
            &self.data_dir,
            &self.downloads(),
            &self.audio_cache(),
            &self.covers(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        for dir in [&self.data_dir, &self.cache_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Removes every file belonging to this account (sign-out).
    pub fn wipe(&self) -> std::io::Result<()> {
        for dir in [&self.data_dir, &self.cache_dir] {
            match std::fs::remove_dir_all(dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Removes all data and cache directories of an account.
pub fn wipe_account(paths: &AccountPaths) -> std::io::Result<()> {
    paths.wipe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_short() {
        let h = account_hash("https://music.example.org", "alice");
        assert_eq!(h.len(), 16);
        assert_eq!(h, account_hash("https://music.example.org", "alice"));
        assert_ne!(h, account_hash("https://music.example.org", "bob"));
    }

    #[test]
    fn layout_and_wipe() {
        let tmp = tempfile::tempdir().unwrap();
        let p = AccountPaths::with_roots(
            "https://x",
            "u",
            &tmp.path().join("data"),
            &tmp.path().join("cache"),
        );
        p.create_all().unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(p.db().parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "account data is private");
        }
        assert!(p.db().starts_with(tmp.path().join("data/lio")));
        assert!(
            p.audio_cache()
                .ends_with(format!("lio/{}/audio", p.account))
        );
        std::fs::write(p.downloads().join("x"), b"1").unwrap();
        wipe_account(&p).unwrap();
        assert!(!p.downloads().exists());
        assert!(!p.audio_cache().exists());
        wipe_account(&p).unwrap();
    }
}
