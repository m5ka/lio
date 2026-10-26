//! MPRIS 2: media keys, shell widgets,
//! lock screen, `playerctl`.

use std::future::Future;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};

use crate::application::LioApplication;
use crate::config;
use crate::covers::SIZE_LARGE;
use crate::playback_controller::PlaybackController;

fn track_path(rowid: i64) -> String {
    format!("/dev/m5ka/Lio/track/{rowid}")
}

fn loop_status(repeat: &str) -> LoopStatus {
    match repeat {
        "all" => LoopStatus::Playlist,
        "one" => LoopStatus::Track,
        _ => LoopStatus::None,
    }
}

fn repeat_for(status: LoopStatus) -> &'static str {
    match status {
        LoopStatus::Playlist => "all",
        LoopStatus::Track => "one",
        LoopStatus::None => "off",
    }
}

/// Starts the MPRIS server on bus name `org.mpris.MediaPlayer2.lio` (`.lio.Devel` for
/// development builds).
pub fn setup(app: &LioApplication, controller: &PlaybackController) {
    let app = app.clone();
    let controller = controller.clone();
    glib::spawn_future_local(async move {
        tracing::debug!("starting MPRIS server");
        let player = match Player::builder(config::mpris_name())
            .identity("Lio")
            .desktop_entry(config::APP_ID)
            .can_quit(true)
            .can_raise(true)
            .can_control(true)
            .has_track_list(false)
            .build()
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("MPRIS unavailable: {e}");
                return;
            }
        };
        connect_methods(&player, &app, &controller);
        glib::spawn_future_local(player.run());
        let player = Rc::new(player);
        connect_properties(&player, &controller);
        sync_all(&player, &controller).await;
        tracing::debug!(name = config::mpris_name(), "MPRIS server running");
    });
}

fn connect_methods(player: &Player, app: &LioApplication, c: &PlaybackController) {
    player.connect_raise(glib::clone!(
        #[weak]
        app,
        move |_| app.activate()
    ));
    player.connect_quit(glib::clone!(
        #[weak]
        app,
        move |_| {
            app.playback().save_now();
            app.quit();
        }
    ));
    player.connect_play_pause(glib::clone!(
        #[weak]
        c,
        move |_| c.play_pause()
    ));
    player.connect_play(glib::clone!(
        #[weak]
        c,
        move |_| c.play()
    ));
    player.connect_pause(glib::clone!(
        #[weak]
        c,
        move |_| c.pause()
    ));
    player.connect_stop(glib::clone!(
        #[weak]
        c,
        move |_| c.stop()
    ));
    player.connect_next(glib::clone!(
        #[weak]
        c,
        move |_| c.next()
    ));
    player.connect_previous(glib::clone!(
        #[weak]
        c,
        move |_| c.previous()
    ));
    player.connect_seek(glib::clone!(
        #[weak]
        c,
        move |_, offset| {
            let target = c.position_ms() as i64 + offset.as_millis();
            c.seek_ms(target.clamp(0, c.duration_ms() as i64) as u64);
        }
    ));
    player.connect_set_position(glib::clone!(
        #[weak]
        c,
        move |_, track_id, position| {
            let current = c.current_track().map(|t| track_path(t.rowid()));
            if current.as_deref() == Some(track_id.as_str()) && position.as_millis() >= 0 {
                c.seek_ms(position.as_millis() as u64);
            }
        }
    ));
    player.connect_set_loop_status(glib::clone!(
        #[weak]
        c,
        move |_, status| c.set_repeat(repeat_for(status))
    ));
    player.connect_set_shuffle(glib::clone!(
        #[weak]
        c,
        move |_, shuffle| c.set_shuffle(shuffle)
    ));
    player.connect_set_volume(glib::clone!(
        #[weak]
        c,
        move |_, volume| c.set_volume(volume.clamp(0.0, 1.0))
    ));
}

/// Wraps an async property update as a notify handler.
fn on<F, Fut>(player: &Rc<Player>, f: F) -> impl Fn(&PlaybackController) + 'static
where
    F: Fn(Rc<Player>, PlaybackController) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let player = player.clone();
    move |c| {
        glib::spawn_future_local(f(player.clone(), c.clone()));
    }
}

fn connect_properties(player: &Rc<Player>, c: &PlaybackController) {
    c.connect_playing_notify(on(player, |p, c| async move { set_status(&p, &c).await }));
    c.connect_active_notify(on(player, |p, c| async move { set_status(&p, &c).await }));
    c.connect_current_track_notify(on(player, |p, c| async move { set_metadata(&p, &c).await }));
    c.connect_duration_ms_notify(on(player, |p, c| async move { set_metadata(&p, &c).await }));
    c.connect_volume_notify(on(player, |p, c| async move {
        let _ = p.set_volume(c.volume()).await;
    }));
    c.connect_shuffle_notify(on(player, |p, c| async move {
        let _ = p.set_shuffle(c.shuffle()).await;
    }));
    c.connect_repeat_notify(on(player, |p, c| async move {
        let _ = p.set_loop_status(loop_status(&c.repeat())).await;
    }));
    c.connect_has_next_notify(on(player, |p, c| async move {
        let _ = p.set_can_go_next(c.has_next()).await;
    }));
    c.connect_has_previous_notify(on(player, |p, c| async move {
        let _ = p.set_can_go_previous(c.has_previous()).await;
    }));
    c.connect_seekable_notify(on(player, |p, c| async move {
        let _ = p.set_can_seek(c.seekable()).await;
    }));
    // Position is polled by clients; keep it current without emitting signals.
    let p = player.clone();
    c.connect_position_ms_notify(move |c| {
        p.set_position(Time::from_millis(c.position_ms() as i64))
    });
}

async fn sync_all(player: &Player, c: &PlaybackController) {
    set_status(player, c).await;
    set_metadata(player, c).await;
    let _ = player.set_volume(c.volume()).await;
    let _ = player.set_shuffle(c.shuffle()).await;
    let _ = player.set_loop_status(loop_status(&c.repeat())).await;
    let _ = player.set_can_go_next(c.has_next()).await;
    let _ = player.set_can_go_previous(c.has_previous()).await;
    let _ = player.set_can_seek(c.seekable()).await;
}

async fn set_status(player: &Player, c: &PlaybackController) {
    let status = if c.playing() {
        PlaybackStatus::Playing
    } else if c.active() {
        PlaybackStatus::Paused
    } else {
        PlaybackStatus::Stopped
    };
    let has_track = c.current_track().is_some();
    let _ = player.set_can_play(has_track).await;
    let _ = player.set_can_pause(has_track).await;
    let _ = player.set_playback_status(status).await;
}

/// MPRIS metadata (art as a `file://` cover-cache path).
async fn set_metadata(player: &Player, c: &PlaybackController) {
    let Some(track) = c.current_track() else {
        let _ = player.set_metadata(Metadata::new()).await;
        return;
    };
    let row = track.row().clone();
    let mut builder = Metadata::builder()
        .title(track.title())
        .artist([track.artist()])
        .length(Time::from_millis(c.duration_ms() as i64));
    if let Ok(id) = TrackId::try_from(track_path(row.rowid)) {
        builder = builder.trackid(id);
    }
    if let Some(album) = &row.album_name {
        builder = builder.album(album.clone());
    }
    if let Some(album_artist) = &row.album_artist {
        builder = builder.album_artist([album_artist.clone()]);
    }
    if let Some(n) = row.track_no {
        builder = builder.track_number(n as i32);
    }
    if let Some(n) = row.disc_no {
        builder = builder.disc_number(n as i32);
    }
    if let Some(r) = row.rating {
        builder = builder.user_rating(r as f64 / 5.0);
    }
    if let (Some(cover), Some(covers)) =
        (row.cover_art_id.clone(), LioApplication::default().covers())
    {
        // Make sure the file exists before advertising it.
        if covers.load(&cover, SIZE_LARGE, None).await.is_some() {
            let path = covers.path(&cover, SIZE_LARGE);
            if let Ok(uri) = glib::filename_to_uri(&path, None) {
                builder = builder.art_url(uri.to_string());
            }
        }
    }
    // The track may have changed while the cover loaded.
    if c.current_track().is_some_and(|t| t.rowid() == row.rowid) {
        let _ = player.set_metadata(builder.build()).await;
    }
}
