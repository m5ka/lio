//! A box for inside a scrolled window that fades out at the window's top and bottom
//! where there is more to scroll to, instead of being cut off there.

use std::cell::RefCell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// Height of the fade at an edge with more content beyond it.
const FADE: f64 = 48.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LioFadeBox {
        /// The scrolled window's vertical adjustment, while rooted.
        pub adjustment: RefCell<Option<(gtk::Adjustment, Vec<glib::SignalHandlerId>)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioFadeBox {
        const NAME: &'static str = "LioFadeBox";
        type Type = super::LioFadeBox;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for LioFadeBox {}

    impl WidgetImpl for LioFadeBox {
        fn root(&self) {
            self.parent_root();
            self.obj().track_scrolling();
        }

        fn unroot(&self) {
            if let Some((adj, handlers)) = self.adjustment.take() {
                for id in handlers {
                    adj.disconnect(id);
                }
            }
            self.parent_unroot();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some((rect, top, bottom)) = self.obj().fades() else {
                self.parent_snapshot(snapshot);
                return;
            };
            let height = rect.height() as f64;
            let opaque = gdk::RGBA::BLACK;
            let clear = gdk::RGBA::TRANSPARENT;
            let stops = [
                gsk::ColorStop::new(0.0, if top > 0.0 { clear } else { opaque }),
                gsk::ColorStop::new((top / height) as f32, opaque),
                gsk::ColorStop::new(1.0 - (bottom / height) as f32, opaque),
                gsk::ColorStop::new(1.0, if bottom > 0.0 { clear } else { opaque }),
            ];
            snapshot.push_mask(gsk::MaskMode::Alpha);
            snapshot.append_linear_gradient(
                &rect,
                &graphene::Point::new(rect.x(), rect.y()),
                &graphene::Point::new(rect.x(), rect.y() + rect.height()),
                &stops,
            );
            snapshot.pop();
            self.parent_snapshot(snapshot);
            snapshot.pop();
        }
    }

    impl BoxImpl for LioFadeBox {}
}

glib::wrapper! {
    pub struct LioFadeBox(ObjectSubclass<imp::LioFadeBox>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl LioFadeBox {
    /// Redraws on scrolling: the viewport only moves the box, which would otherwise
    /// keep its fades where they were.
    fn track_scrolling(&self) {
        let Some(scroller) = self
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
        else {
            return;
        };
        let adj = scroller.vadjustment();
        let redraw = glib::clone!(
            #[weak(rename_to = b)]
            self,
            move |_: &gtk::Adjustment| b.queue_draw()
        );
        let handlers = vec![
            adj.connect_value_changed(redraw.clone()),
            adj.connect_changed(redraw),
        ];
        self.imp().adjustment.replace(Some((adj, handlers)));
    }

    /// The visible part of the box (in its own coordinates) and how far to fade at
    /// its top and bottom; `None` when everything fits.
    fn fades(&self) -> Option<(graphene::Rect, f64, f64)> {
        let adj = self.imp().adjustment.borrow().as_ref()?.0.clone();
        let viewport = self.parent()?;
        let origin = viewport.compute_point(self, &graphene::Point::zero())?;
        let height = viewport.height() as f64;
        if height <= 0.0 {
            return None;
        }
        // Fade in over the first FADE px of scrolling, out over the last, at most half
        // the visible height each.
        let limit = FADE.min(height / 2.0);
        let top = (adj.value() - adj.lower()).clamp(0.0, limit);
        let bottom = (adj.upper() - adj.value() - adj.page_size()).clamp(0.0, limit);
        if top <= 0.0 && bottom <= 0.0 {
            return None;
        }
        let rect = graphene::Rect::new(
            origin.x(),
            origin.y(),
            viewport.width() as f32,
            height as f32,
        );
        Some((rect, top, bottom))
    }
}
