//! Cover art loading: server-resized images (`getCoverArt?size=N`), decoded
//! off the main thread with glycin (falling back to the `image` crate when the glycin
//! sandbox is unavailable), cached in memory (byte-bounded LRU) and on disk.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use lio_subsonic::Client;

use crate::runtime;

/// In-memory texture budget.
const MEMORY_BUDGET: usize = 64 * 1024 * 1024;

pub const SIZE_ROW: u32 = 64;
pub const SIZE_GRID: u32 = 192;
pub const SIZE_LARGE: u32 = 512;
/// Full-window Now Playing cover.
pub const SIZE_FULL: u32 = 1024;

type Key = (String, u32);

struct Entry {
    texture: gdk::Texture,
    bytes: usize,
    tick: u64,
}

#[derive(Default)]
struct Lru {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    tick: u64,
}

impl Lru {
    fn get(&mut self, key: &Key) -> Option<gdk::Texture> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|e| {
            e.tick = tick;
            e.texture.clone()
        })
    }

    fn insert(&mut self, key: Key, texture: gdk::Texture) {
        let bytes = (texture.width() * texture.height() * 4) as usize;
        self.tick += 1;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                texture,
                bytes,
                tick: self.tick,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > MEMORY_BUDGET && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.tick)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest
                && let Some(e) = self.entries.remove(&k)
            {
                self.bytes -= e.bytes;
            }
        }
    }
}

/// Per-account cover cache. Main-thread only; cheap to clone.
#[derive(Clone)]
pub struct Covers {
    inner: Rc<Inner>,
}

struct Inner {
    client: RefCell<Option<Client>>,
    dir: PathBuf,
    memory: RefCell<Lru>,
    inflight: RefCell<HashMap<Key, Vec<async_channel::Sender<Option<gdk::Texture>>>>>,
}

impl Covers {
    pub fn new(client: Option<Client>, dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            inner: Rc::new(Inner {
                client: RefCell::new(client),
                dir,
                memory: RefCell::default(),
                inflight: RefCell::default(),
            }),
        }
    }

    pub fn set_client(&self, client: Option<Client>) {
        self.inner.client.replace(client);
    }

    /// Disk location of a cover; it exists once loaded (used for MPRIS `artUrl`).
    pub fn path(&self, id: &str, size: u32) -> PathBuf {
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.inner.dir.join(format!("{safe}-{size}.img"))
    }

    /// The cover if it's already in memory.
    pub fn cached(&self, id: &str, size: u32) -> Option<gdk::Texture> {
        self.inner.memory.borrow_mut().get(&(id.to_owned(), size))
    }

    /// Loads a cover. Concurrent requests for the same cover share one load.
    /// Returns `None` if the cover can't be loaded or `cancellable` was cancelled.
    pub async fn load(
        &self,
        id: &str,
        size: u32,
        cancellable: Option<&gio::Cancellable>,
    ) -> Option<gdk::Texture> {
        let key = (id.to_owned(), size);
        if let Some(texture) = self.inner.memory.borrow_mut().get(&key) {
            return Some(texture);
        }
        let (tx, rx) = async_channel::bounded(1);
        let first = {
            let mut inflight = self.inner.inflight.borrow_mut();
            let waiters = inflight.entry(key.clone()).or_default();
            waiters.push(tx);
            waiters.len() == 1
        };
        if first {
            let this = self.clone();
            let key = key.clone();
            glib::spawn_future_local(async move {
                let texture = this.fetch_and_decode(&key.0, key.1).await;
                if let Some(t) = &texture {
                    this.inner
                        .memory
                        .borrow_mut()
                        .insert(key.clone(), t.clone());
                }
                for waiter in this
                    .inner
                    .inflight
                    .borrow_mut()
                    .remove(&key)
                    .unwrap_or_default()
                {
                    let _ = waiter.try_send(texture.clone());
                }
            });
        }
        let texture = rx.recv().await.ok().flatten();
        if cancellable.is_some_and(|c| c.is_cancelled()) {
            return None;
        }
        texture
    }

    async fn fetch_and_decode(&self, id: &str, size: u32) -> Option<gdk::Texture> {
        let path = self.path(id, size);
        let disk = {
            let path = path.clone();
            runtime::run(async move { tokio::fs::read(&path).await.ok() }).await
        };
        let bytes = match disk {
            Some(bytes) => bytes,
            None => {
                let client = self.inner.client.borrow().clone()?;
                let url = client.cover_art_url(id, size);
                runtime::run(async move {
                    let response = client.http().get(url).send().await.ok()?;
                    let is_image = response
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|ct| ct.starts_with("image/"));
                    if !response.status().is_success() || !is_image {
                        return None;
                    }
                    let bytes = response.bytes().await.ok()?.to_vec();
                    let _ = tokio::fs::write(&path, &bytes).await;
                    Some(bytes)
                })
                .await?
            }
        };
        decode(bytes, size).await
    }
}

/// GLib main context for glycin's internal work, on a thread that has entered the
/// Tokio runtime (zbus runs in Tokio mode in this app, so glycin must too).
fn glycin_context() -> glib::MainContext {
    static CONTEXT: OnceLock<glib::MainContext> = OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("lio-glycin".into())
                .spawn(move || {
                    let _tokio = runtime::runtime().enter();
                    let context = glib::MainContext::new();
                    let main_loop = glib::MainLoop::new(Some(&context), false);
                    let _ = tx.send(context.clone());
                    let _default = context.acquire();
                    main_loop.run();
                })
                .expect("spawn glycin thread");
            rx.recv().expect("glycin thread started")
        })
        .clone()
}

/// Decodes on a worker thread: glycin first, `image` as fallback.
async fn decode(bytes: Vec<u8>, size: u32) -> Option<gdk::Texture> {
    let fallback_bytes = bytes.clone();
    let glycin = runtime::run(async move {
        let mut loader = glycin::Loader::new_vec(bytes);
        loader.apply_transformations(true);
        loader.main_context_selector(glycin::MainContextSelector::Specific(glycin_context()));
        let mut image = loader.load().await.ok()?;
        let frame = image
            .specific_frame(glycin::FrameRequest::new().scale(size, size))
            .await
            .ok()?;
        Some(frame.texture())
    })
    .await;
    if glycin.is_some() {
        return glycin;
    }
    tracing::debug!("glycin decode failed; using image crate fallback");
    let decoded = runtime::run(async move {
        tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&fallback_bytes).ok()?;
            let img = if img.width() > size * 3 / 2 || img.height() > size * 3 / 2 {
                img.resize(size, size, image::imageops::FilterType::Triangle)
            } else {
                img
            };
            let rgba = img.to_rgba8();
            Some((rgba.width(), rgba.height(), rgba.into_raw()))
        })
        .await
        .ok()
        .flatten()
    })
    .await?;
    let (w, h, raw) = decoded;
    let bytes = glib::Bytes::from_owned(raw);
    Some(
        gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            w as usize * 4,
        )
        .upcast(),
    )
}
