//! Shared helpers for integration tests.

#![allow(dead_code)]

use std::path::PathBuf;

/// Connection details for the test Navidrome.
pub struct TestServer {
    pub url: String,
    pub user: String,
    pub password: String,
}

/// Server from `LIO_TEST_SERVER` / `LIO_TEST_USER` / `LIO_TEST_PASSWORD`,
/// defaulting to the instance started by `tests/test-server.sh up`.
pub fn server() -> TestServer {
    TestServer {
        url: std::env::var("LIO_TEST_SERVER").unwrap_or_else(|_| "http://localhost:4533".into()),
        user: std::env::var("LIO_TEST_USER").unwrap_or_else(|_| "lio".into()),
        password: std::env::var("LIO_TEST_PASSWORD").unwrap_or_else(|_| "lio-test".into()),
    }
}

/// Per-test XDG data and cache directories, removed when dropped.
pub struct TempXdg {
    dir: tempfile::TempDir,
}

impl TempXdg {
    pub fn data_home(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn cache_home(&self) -> PathBuf {
        self.dir.path().join("cache")
    }

    /// Environment variables for spawning the app with these directories.
    pub fn envs(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("XDG_DATA_HOME", self.data_home()),
            ("XDG_CACHE_HOME", self.cache_home()),
            ("XDG_CONFIG_HOME", self.dir.path().join("config")),
        ]
    }
}

pub fn temp_xdg() -> TempXdg {
    let dir = tempfile::tempdir().expect("create temp dir");
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    TempXdg { dir }
}
