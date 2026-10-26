//! `IdListModel`: a `gio::ListModel` over SQLite rowids.
//!
//! Only the ID list lives in memory (8 bytes per row). GObjects are created lazily in
//! [`BATCH`]-row chunks when the list view asks for them, and kept in a bounded cache, so
//! a 100k-track list costs about a megabyte until scrolled.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use lio_store::{Db, queries};

use super::{Album, Artist, Track};

/// Rows hydrated per database query.
const BATCH: usize = 200;
/// Most GObjects kept alive by the model.
const CACHE_LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ItemKind {
    #[default]
    Album,
    Artist,
    Track,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct IdListModel {
        pub kind: Cell<ItemKind>,
        pub db: RefCell<Option<Db>>,
        pub ids: RefCell<Vec<i64>>,
        /// position → object (positions, not rowids: playlists may repeat a track).
        pub cache: RefCell<HashMap<u32, glib::Object>>,
        pub order: RefCell<VecDeque<u32>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IdListModel {
        const NAME: &'static str = "LioIdListModel";
        type Type = super::IdListModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for IdListModel {}

    impl ListModelImpl for IdListModel {
        fn item_type(&self) -> glib::Type {
            match self.kind.get() {
                ItemKind::Album => Album::static_type(),
                ItemKind::Artist => Artist::static_type(),
                ItemKind::Track => Track::static_type(),
            }
        }

        fn n_items(&self) -> u32 {
            self.ids.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.n_items() {
                return None;
            }
            if let Some(obj) = self.cache.borrow().get(&position) {
                return Some(obj.clone());
            }
            self.obj().hydrate(position);
            self.cache.borrow().get(&position).cloned()
        }
    }
}

glib::wrapper! {
    pub struct IdListModel(ObjectSubclass<imp::IdListModel>) @implements gio::ListModel;
}

impl IdListModel {
    pub fn new(kind: ItemKind, db: &Db) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().kind.set(kind);
        obj.imp().db.replace(Some(db.clone()));
        obj
    }

    pub fn kind(&self) -> ItemKind {
        self.imp().kind.get()
    }

    pub fn ids(&self) -> Vec<i64> {
        self.imp().ids.borrow().clone()
    }

    pub fn id_at(&self, position: u32) -> Option<i64> {
        self.imp().ids.borrow().get(position as usize).copied()
    }

    /// Replaces the contents with a single `items-changed`.
    pub fn set_ids(&self, ids: Vec<i64>) {
        let imp = self.imp();
        let removed = imp.ids.borrow().len() as u32;
        let added = ids.len() as u32;
        imp.ids.replace(ids);
        imp.cache.borrow_mut().clear();
        imp.order.borrow_mut().clear();
        self.items_changed(0, removed, added);
    }

    /// Creates objects for the batch containing `position`.
    fn hydrate(&self, position: u32) {
        let imp = self.imp();
        let Some(db) = imp.db.borrow().clone() else {
            return;
        };
        let start = (position as usize / BATCH) * BATCH;
        let ids: Vec<i64> = {
            let all = imp.ids.borrow();
            all[start..(start + BATCH).min(all.len())].to_vec()
        };
        let objects: Vec<glib::Object> = match imp.kind.get() {
            ItemKind::Album => self.load(
                &db,
                &ids,
                queries::album_rows,
                |r| Album::new(r).upcast(),
                |r| r.rowid,
            ),
            ItemKind::Artist => self.load(
                &db,
                &ids,
                queries::artist_rows,
                |r| Artist::new(r).upcast(),
                |r| r.rowid,
            ),
            ItemKind::Track => self.load(
                &db,
                &ids,
                queries::track_rows,
                |r| Track::new(r).upcast(),
                |r| r.rowid,
            ),
        };
        let mut cache = imp.cache.borrow_mut();
        let mut order = imp.order.borrow_mut();
        for (offset, obj) in objects.into_iter().enumerate() {
            let pos = (start + offset) as u32;
            if cache.insert(pos, obj).is_none() {
                order.push_back(pos);
            }
        }
        while cache.len() > CACHE_LIMIT {
            let Some(oldest) = order.pop_front() else {
                break;
            };
            cache.remove(&oldest);
        }
    }

    /// Loads rows for `ids` (which may repeat) and maps them to objects in `ids` order.
    fn load<R: Clone>(
        &self,
        db: &Db,
        ids: &[i64],
        query: fn(&rusqlite::Connection, &[i64]) -> lio_store::Result<Vec<R>>,
        make: fn(R) -> glib::Object,
        key: fn(&R) -> i64,
    ) -> Vec<glib::Object> {
        let mut unique = ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let rows = db.read(|c| query(c, &unique)).unwrap_or_else(|e| {
            tracing::error!("list model query failed: {e}");
            Vec::new()
        });
        let by_id: HashMap<i64, R> = rows.into_iter().map(|r| (key(&r), r)).collect();
        // Rows deleted meanwhile become placeholders so positions stay stable.
        ids.iter()
            .map(|id| match by_id.get(id) {
                Some(row) => make(row.clone()),
                None => match self.imp().kind.get() {
                    ItemKind::Album => Album::new(Default::default()).upcast(),
                    ItemKind::Artist => Artist::new(Default::default()).upcast(),
                    ItemKind::Track => Track::new(Default::default()).upcast(),
                },
            })
            .collect()
    }
}
