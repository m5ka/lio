//! `liosrc`: GStreamer source for `lio://track/<id>?q=<quality>` URIs.
//!
//! Reads through the installed [`crate::TrackResolver`], so playbin3 only ever sees
//! `lio://` URIs and its gapless switching keeps working.

mod imp;

use gst::glib;
use gst::prelude::*;

glib::wrapper! {
    pub struct LioSrc(ObjectSubclass<imp::LioSrc>)
        @extends gst_base::BaseSrc, gst::Element, gst::Object,
        @implements gst::URIHandler;
}

/// Registers `liosrc` with the GStreamer registry. Call once after `gst::init`.
pub fn register_static() -> Result<(), glib::BoolError> {
    gst::Element::register(None, "liosrc", gst::Rank::PRIMARY, LioSrc::static_type())
}
