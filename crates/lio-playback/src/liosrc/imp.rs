use std::io;
use std::sync::{Arc, LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_src::CreateSuccess;
use gst_base::subclass::prelude::*;

use crate::source::{TrackSource, parse_track_uri, resolver};

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "liosrc",
        gst::DebugColorFlags::empty(),
        Some("Lio track source"),
    )
});

#[derive(Default)]
struct State {
    source: Option<Arc<dyn TrackSource>>,
}

#[derive(Default)]
pub struct LioSrc {
    uri: Mutex<Option<String>>,
    state: Mutex<State>,
}

impl LioSrc {
    fn with_source<T>(&self, f: impl FnOnce(&dyn TrackSource) -> T) -> Option<T> {
        self.state.lock().unwrap().source.as_deref().map(f)
    }

    fn read_error(&self, e: io::Error) -> gst::FlowError {
        if e.kind() == io::ErrorKind::Interrupted {
            gst::debug!(CAT, imp = self, "read interrupted");
            return gst::FlowError::Flushing;
        }
        gst::element_imp_error!(self, gst::ResourceError::Read, ["{e}"]);
        gst::FlowError::Error
    }
}

#[glib::object_subclass]
impl ObjectSubclass for LioSrc {
    const NAME: &'static str = "LioSrc";
    type Type = super::LioSrc;
    type ParentType = gst_base::BaseSrc;
    type Interfaces = (gst::URIHandler,);
}

impl ObjectImpl for LioSrc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecString::builder("location")
                    .nick("Location")
                    .blurb("lio:// URI of the track")
                    .build(),
            ]
        });
        PROPS.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        if pspec.name() == "location" {
            let uri: Option<String> = value.get().unwrap();
            if let Some(uri) = uri {
                if let Err(e) = self.set_uri(&uri) {
                    gst::warning!(CAT, imp = self, "invalid location: {e}");
                }
            } else {
                *self.uri.lock().unwrap() = None;
            }
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "location" => self.uri.lock().unwrap().to_value(),
            _ => unreachable!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.set_format(gst::Format::Bytes);
        obj.set_automatic_eos(false);
    }
}

impl GstObjectImpl for LioSrc {}

impl ElementImpl for LioSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Lio track source",
                "Source/File",
                "Reads tracks from Lio's download store, audio cache or server",
                "Lio",
            )
        });
        Some(&*META)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            vec![
                gst::PadTemplate::new(
                    "src",
                    gst::PadDirection::Src,
                    gst::PadPresence::Always,
                    &gst::Caps::new_any(),
                )
                .unwrap(),
            ]
        });
        TEMPLATES.as_ref()
    }
}

impl BaseSrcImpl for LioSrc {
    fn start(&self) -> Result<(), gst::ErrorMessage> {
        let uri =
            self.uri.lock().unwrap().clone().ok_or_else(|| {
                gst::error_msg!(gst::ResourceError::NotFound, ["no lio:// URI set"])
            })?;
        let (track_id, quality) = parse_track_uri(&uri).ok_or_else(|| {
            gst::error_msg!(
                gst::ResourceError::NotFound,
                ["not a lio:// track URI: {uri}"]
            )
        })?;
        let resolver = resolver().ok_or_else(|| {
            gst::error_msg!(gst::ResourceError::Failed, ["no track resolver installed"])
        })?;
        let source = resolver
            .open(&track_id, &quality)
            .map_err(|e| gst::error_msg!(gst::ResourceError::OpenRead, ["{e}"]))?;
        gst::debug!(CAT, imp = self, "opened {uri} (size {:?})", source.size());
        self.state.lock().unwrap().source = Some(Arc::from(source));
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        if let Some(source) = self.state.lock().unwrap().source.take() {
            source.cancel();
        }
        Ok(())
    }

    fn is_seekable(&self) -> bool {
        self.with_source(|s| s.size().is_some()).unwrap_or(false)
    }

    fn size(&self) -> Option<u64> {
        self.with_source(|s| s.size()).flatten()
    }

    fn create(
        &self,
        offset: u64,
        buffer: Option<&mut gst::BufferRef>,
        length: u32,
    ) -> Result<CreateSuccess, gst::FlowError> {
        if let Some(size) = self.size()
            && offset >= size
        {
            return Err(gst::FlowError::Eos);
        }
        // Clone the source so a blocking read doesn't hold the state lock that
        // `unlock()` needs to reach it.
        let source = self
            .state
            .lock()
            .unwrap()
            .source
            .clone()
            .ok_or(gst::FlowError::Flushing)?;
        // Downstream may pass its own buffer (pull mode): fill that one, since a shorter
        // new buffer can't be copied into it.
        if let Some(buffer) = buffer {
            let read = {
                let mut map = buffer.map_writable().map_err(|_| gst::FlowError::Error)?;
                let len = map.len().min(length.max(1) as usize);
                source.read_at(offset, &mut map[..len])
            };
            return match read {
                Ok(0) => Err(gst::FlowError::Eos),
                Ok(n) => {
                    buffer.set_size(n);
                    buffer.set_offset(offset);
                    buffer.set_offset_end(offset + n as u64);
                    Ok(CreateSuccess::FilledBuffer)
                }
                Err(e) => Err(self.read_error(e)),
            };
        }
        let mut data = vec![0u8; length.max(1) as usize];
        let read = source.read_at(offset, &mut data);
        match read {
            Ok(0) => Err(gst::FlowError::Eos),
            Ok(n) => {
                data.truncate(n);
                let mut buffer = gst::Buffer::from_mut_slice(data);
                {
                    let buffer = buffer.get_mut().unwrap();
                    buffer.set_offset(offset);
                    buffer.set_offset_end(offset + n as u64);
                }
                Ok(CreateSuccess::NewBuffer(buffer))
            }
            Err(e) => Err(self.read_error(e)),
        }
    }

    fn unlock(&self) -> Result<(), gst::ErrorMessage> {
        gst::debug!(CAT, imp = self, "unlock");
        self.with_source(|s| s.cancel());
        Ok(())
    }

    fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
        gst::debug!(CAT, imp = self, "unlock_stop");
        self.with_source(|s| s.resume());
        Ok(())
    }
}

impl URIHandlerImpl for LioSrc {
    const URI_TYPE: gst::URIType = gst::URIType::Src;

    fn protocols() -> &'static [&'static str] {
        &["lio"]
    }

    fn uri(&self) -> Option<String> {
        self.uri.lock().unwrap().clone()
    }

    fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
        if parse_track_uri(uri).is_none() {
            return Err(glib::Error::new(
                gst::URIError::BadUri,
                &format!("not a lio:// track URI: {uri}"),
            ));
        }
        *self.uri.lock().unwrap() = Some(uri.to_owned());
        Ok(())
    }
}
