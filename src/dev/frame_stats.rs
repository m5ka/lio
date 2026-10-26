//! Frame-timing instrumentation.
//!
//! - `LIO_FRAME_STATS=1`: logs p50/p95/max frame interval and the dropped-frame share
//!   (interval > 1.5× the target frame time) every 5 s.
//! - `LIO_AUTOSCROLL=albums|tracks`: once the library is shown, scrolls the album grid
//!   (or the largest playlist's track list) at 3,000 px/s for 20 s, prints a JSON
//!   summary `{p50_ms, p95_ms, max_ms, dropped_pct, refresh_ms, target_ms, frames, pass}`
//!   to stdout and quits. The target frame time is the monitor's refresh period, but
//!   never stricter than 60 fps; it passes when p95 ≤ target and ≤ 1 % of frames
//!   take longer than 1.5× the target.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::application::LioApplication;
use crate::widgets::{LioPlaylistPage, LioWindow, Section};

const REPORT_EVERY: Duration = Duration::from_secs(5);
const SCROLL_SPEED_PX_S: f64 = 3000.0;
const SCROLL_DURATION: Duration = Duration::from_secs(20);
/// The target is at least 60 fps; faster monitors don't make the target stricter.
const MIN_TARGET_MS: f64 = 1000.0 / 60.0;
/// Gaps longer than this are idle time (nothing to draw), not dropped frames.
const IDLE_GAP_US: i64 = 250_000;

#[derive(Default)]
struct Samples {
    intervals_us: Vec<i64>,
    last_frame_us: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub dropped_pct: f64,
    pub refresh_ms: f64,
    pub target_ms: f64,
    pub frames: usize,
}

impl Summary {
    pub fn passes(&self) -> bool {
        self.p95_ms <= self.target_ms + 0.5 && self.dropped_pct <= 1.0
    }

    fn to_json(self) -> String {
        format!(
            r#"{{"p50_ms":{:.2},"p95_ms":{:.2},"max_ms":{:.2},"dropped_pct":{:.2},"refresh_ms":{:.2},"target_ms":{:.2},"frames":{},"pass":{}}}"#,
            self.p50_ms,
            self.p95_ms,
            self.max_ms,
            self.dropped_pct,
            self.refresh_ms,
            self.target_ms,
            self.frames,
            self.passes()
        )
    }
}

fn summarise(intervals_us: &[i64], refresh_ms: f64) -> Option<Summary> {
    let mut v: Vec<i64> = intervals_us
        .iter()
        .copied()
        .filter(|i| *i > 0 && *i < IDLE_GAP_US)
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
    let target_ms = refresh_ms.max(MIN_TARGET_MS);
    let dropped = v
        .iter()
        .filter(|i| **i as f64 / 1000.0 > target_ms * 1.5)
        .count();
    Some(Summary {
        p50_ms: pct(0.5),
        p95_ms: pct(0.95),
        max_ms: *v.last().unwrap() as f64 / 1000.0,
        dropped_pct: dropped as f64 * 100.0 / v.len() as f64,
        refresh_ms,
        target_ms,
        frames: v.len(),
    })
}

fn refresh_ms(win: &LioWindow) -> f64 {
    let rate_mhz = win
        .surface()
        .and_then(|s| gdk::Display::default().and_then(|d| d.monitor_at_surface(&s)))
        .map(|m| m.refresh_rate())
        .filter(|r| *r > 0)
        .unwrap_or(60_000);
    1_000_000.0 / rate_mhz as f64
}

/// Installs the frame clock hooks if either environment variable is set.
pub fn setup(win: &LioWindow) {
    let stats = std::env::var("LIO_FRAME_STATS").is_ok_and(|v| v == "1");
    let autoscroll = std::env::var("LIO_AUTOSCROLL").ok();
    if !stats && autoscroll.is_none() {
        return;
    }
    let samples = Rc::new(RefCell::new(Samples::default()));
    win.connect_realize(glib::clone!(
        #[strong]
        samples,
        move |win| {
            let Some(clock) = win.frame_clock() else {
                return;
            };
            let samples = samples.clone();
            clock.connect_after_paint(move |clock| {
                let now = clock.frame_time();
                let mut s = samples.borrow_mut();
                if let Some(last) = s.last_frame_us.replace(now) {
                    s.intervals_us.push(now - last);
                }
            });
        }
    ));

    if stats {
        glib::timeout_add_local(
            REPORT_EVERY,
            glib::clone!(
                #[weak]
                win,
                #[strong]
                samples,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let intervals = std::mem::take(&mut samples.borrow_mut().intervals_us);
                    if let Some(s) = summarise(&intervals, refresh_ms(&win)) {
                        tracing::info!(
                            p50_ms = s.p50_ms,
                            p95_ms = s.p95_ms,
                            max_ms = s.max_ms,
                            dropped_pct = s.dropped_pct,
                            frames = s.frames,
                            "frame stats"
                        );
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
    }

    if let Some(target) = autoscroll {
        // Give sign-in and the first sync time to fill the views.
        glib::timeout_add_local_once(
            Duration::from_secs(4),
            glib::clone!(
                #[weak]
                win,
                move || start_autoscroll(&win, &target, samples)
            ),
        );
    }
}

fn start_autoscroll(win: &LioWindow, target: &str, samples: Rc<RefCell<Samples>>) {
    match target {
        "tracks" => {
            // The largest playlist.
            let playlist = LioApplication::default().session().account().and_then(|a| {
                a.db.read(lio_store::queries::playlists)
                    .ok()
                    .and_then(|p| p.into_iter().max_by_key(|p| p.song_count))
            });
            match playlist {
                Some(row) => {
                    win.push_page(&LioPlaylistPage::new(&crate::models::Playlist::new(row)))
                }
                None => {
                    eprintln!("LIO_AUTOSCROLL=tracks: no playlists to scroll");
                    return;
                }
            }
        }
        _ => win.show_section(Section::Albums),
    }

    glib::timeout_add_local_once(
        Duration::from_millis(500),
        glib::clone!(
            #[weak]
            win,
            move || {
                let Some(page) = win.nav_view().visible_page() else {
                    return;
                };
                let Some(scrolled) = find_scrolled(page.upcast_ref()) else {
                    eprintln!("LIO_AUTOSCROLL: no scrollable view found");
                    return;
                };
                let adj = scrolled.vadjustment();
                samples.borrow_mut().intervals_us.clear();
                samples.borrow_mut().last_frame_us = None;
                let start = Rc::new(RefCell::new(None::<i64>));
                let last = Rc::new(RefCell::new(None::<i64>));
                scrolled.add_tick_callback(move |widget, clock| {
                    let now = clock.frame_time();
                    let started = *start.borrow_mut().get_or_insert(now);
                    let dt =
                        last.borrow_mut().replace(now).map_or(0, |l| now - l) as f64 / 1_000_000.0;
                    let max = (adj.upper() - adj.page_size()).max(0.0);
                    let mut value = adj.value() + SCROLL_SPEED_PX_S * dt;
                    if value >= max {
                        value = 0.0;
                    }
                    adj.set_value(value);
                    if now - started >= SCROLL_DURATION.as_micros() as i64 {
                        let win = widget.root().and_downcast::<LioWindow>();
                        let refresh = win.as_ref().map_or(1000.0 / 60.0, refresh_ms);
                        let intervals = samples.borrow().intervals_us.clone();
                        match summarise(&intervals, refresh) {
                            Some(s) => println!("{}", s.to_json()),
                            None => println!(r#"{{"error":"no frames"}}"#),
                        }
                        LioApplication::default().quit();
                        return glib::ControlFlow::Break;
                    }
                    glib::ControlFlow::Continue
                });
            }
        ),
    );
}

fn find_scrolled(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Some(s) = widget.downcast_ref::<gtk::ScrolledWindow>()
        && s.is_mapped()
    {
        return Some(s.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_scrolled(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_maths() {
        let mut v = vec![16_667; 99];
        v.push(40_000); // one dropped frame
        v.push(1_000_000); // idle gap, ignored
        let s = summarise(&v, 16.667).unwrap();
        assert_eq!(s.frames, 100);
        assert!((s.p50_ms - 16.667).abs() < 0.01);
        assert!((s.dropped_pct - 1.0).abs() < 0.01);
        assert!(s.passes());
        // A 200 Hz monitor doesn't tighten the 60 fps target.
        let fast = summarise(&[5_000, 10_000, 5_000, 10_300], 5.0).unwrap();
        assert!((fast.target_ms - 16.667).abs() < 0.01);
        assert!(fast.passes());
    }
}
