//! Helpers for the narrow layout.

use gtk::glib;
use gtk::prelude::*;

use crate::application::LioApplication;

/// Lays `boxed` out vertically (and centred) while the window is narrow.
pub fn stack_when_narrow(boxed: &gtk::Box) {
    let Some(win) = LioApplication::default().window() else {
        return;
    };
    win.bind_property("narrow", boxed, "orientation")
        .transform_to(|_, narrow: bool| {
            Some(if narrow {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            })
        })
        .sync_create()
        .build();
    let apply = glib::clone!(
        #[weak]
        boxed,
        move |narrow: bool| {
            // Centre children (cover, buttons) when stacked.
            let mut child = boxed.first_child();
            while let Some(c) = child {
                c.set_halign(if narrow {
                    gtk::Align::Center
                } else {
                    gtk::Align::Fill
                });
                if let Some(inner) = c.downcast_ref::<gtk::Box>() {
                    let mut grand = inner.first_child();
                    while let Some(g) = grand {
                        if let Some(label) = g.downcast_ref::<gtk::Label>() {
                            label.set_xalign(if narrow { 0.5 } else { 0.0 });
                            label.set_justify(if narrow {
                                gtk::Justification::Center
                            } else {
                                gtk::Justification::Left
                            });
                        } else {
                            g.set_halign(if narrow {
                                gtk::Align::Center
                            } else {
                                gtk::Align::Start
                            });
                            // Wrapped lines (e.g. the album page's buttons) centre too.
                            if let Some(wrap) = g.downcast_ref::<adw::WrapBox>() {
                                wrap.set_align(if narrow { 0.5 } else { 0.0 });
                            }
                        }
                        grand = g.next_sibling();
                    }
                }
                child = c.next_sibling();
            }
        }
    );
    apply(win.narrow());
    win.connect_narrow_notify(move |w| apply(w.narrow()));
}
