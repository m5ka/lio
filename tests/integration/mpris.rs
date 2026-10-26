//! The built binary exposes MPRIS correctly.
//!
//! Spawns `lio` (debug build) signed in through `LIO_TEST_*`, autoplaying the gapless
//! fixture album into a fakesink, then checks it over the session bus.

use std::collections::HashMap;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedValue, Value};

use crate::common;

const BUS_NAME: &str = "org.mpris.MediaPlayer2.lio.Devel";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

struct App(Child);

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn album_id(srv: &common::TestServer) -> String {
    let client = lio_subsonic::Client::new(
        &srv.url,
        lio_subsonic::Credentials::token(&srv.user, &srv.password),
        lio_subsonic::TlsConfig::System,
    )
    .unwrap();
    client
        .get_all_albums(&lio_subsonic::AlbumListType::AlphabeticalByName)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Gapless Sweep")
        .expect("fixture album")
        .id
}

async fn get(conn: &zbus::Connection, prop: &str) -> OwnedValue {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)
        .unwrap()
        .path("/org/mpris/MediaPlayer2")
        .unwrap()
        .build()
        .await
        .unwrap();
    proxy.get(PLAYER.try_into().unwrap(), prop).await.unwrap()
}

async fn wait_for<F: AsyncFnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs tests/test-server.sh up and a session bus"]
async fn mpris_reflects_playback() {
    let srv = common::server();
    let album = album_id(&srv).await;
    let xdg = common::temp_xdg();
    let app = App(Command::new(env!("CARGO_BIN_EXE_lio"))
        .envs(xdg.envs())
        .env("GSETTINGS_BACKEND", "memory")
        // GTK and libadwaita would otherwise start the desktop portals (and GIO gvfs) on
        // the test's session bus; they outlive the test, holding its output open.
        .env("GDK_DEBUG", "no-portals")
        .env("ADW_DISABLE_PORTAL", "1")
        .env("GIO_USE_VFS", "local")
        .env("LIO_AUDIO_SINK", "fakesink")
        .env("LIO_TEST_SERVER", &srv.url)
        .env("LIO_TEST_USER", &srv.user)
        .env("LIO_TEST_PASSWORD", &srv.password)
        .env("LIO_TEST_AUTOPLAY_ALBUM", &album)
        .spawn()
        .expect("spawn lio"));

    let conn = zbus::Connection::session().await.unwrap();
    let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
    assert!(
        wait_for(Duration::from_secs(15), async || dbus
            .name_has_owner(BUS_NAME.try_into().unwrap())
            .await
            .unwrap_or(false))
        .await,
        "{BUS_NAME} never appeared"
    );

    assert!(
        wait_for(Duration::from_secs(10), async || {
            let status: String = get(&conn, "PlaybackStatus").await.try_into().unwrap();
            status == "Playing"
        })
        .await,
        "never started playing"
    );

    let metadata: HashMap<String, OwnedValue> = get(&conn, "Metadata").await.try_into().unwrap();
    let trackid = metadata["mpris:trackid"]
        .downcast_ref::<zbus::zvariant::ObjectPath<'_>>()
        .unwrap();
    assert!(
        trackid.as_str().starts_with("/dev/m5ka/Lio/track/"),
        "{trackid}"
    );
    let title: String = metadata["xesam:title"].clone().try_into().unwrap();
    assert_eq!(title, "Sweep Part 1");
    let art: String = metadata["mpris:artUrl"].clone().try_into().unwrap();
    assert!(art.starts_with("file://"), "{art}");

    // LoopStatus ↔ repeat mapping, written over D-Bus.
    let props = zbus::fdo::PropertiesProxy::builder(&conn)
        .destination(BUS_NAME)
        .unwrap()
        .path("/org/mpris/MediaPlayer2")
        .unwrap()
        .build()
        .await
        .unwrap();
    for status in ["Playlist", "Track", "None"] {
        props
            .set(
                PLAYER.try_into().unwrap(),
                "LoopStatus",
                Value::from(status),
            )
            .await
            .unwrap();
        assert!(
            wait_for(Duration::from_secs(3), async || {
                let now: String = get(&conn, "LoopStatus").await.try_into().unwrap();
                now == status
            })
            .await,
            "LoopStatus {status} not applied"
        );
    }

    // PlayPause → Paused; Next → second track.
    conn.call_method(
        Some(BUS_NAME),
        "/org/mpris/MediaPlayer2",
        Some(PLAYER),
        "PlayPause",
        &(),
    )
    .await
    .unwrap();
    assert!(
        wait_for(Duration::from_secs(3), async || {
            let s: String = get(&conn, "PlaybackStatus").await.try_into().unwrap();
            s == "Paused"
        })
        .await
    );
    conn.call_method(
        Some(BUS_NAME),
        "/org/mpris/MediaPlayer2",
        Some(PLAYER),
        "Next",
        &(),
    )
    .await
    .unwrap();
    assert!(
        wait_for(Duration::from_secs(5), async || {
            let m: HashMap<String, OwnedValue> = get(&conn, "Metadata").await.try_into().unwrap();
            m.get("xesam:title")
                .and_then(|t| String::try_from(t.clone()).ok())
                .as_deref()
                == Some("Sweep Part 2")
        })
        .await,
        "Next didn't move to track 2"
    );
    drop(app);
}
