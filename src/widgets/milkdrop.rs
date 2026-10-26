//! MilkDrop presets rendered by projectM from the audio being heard (the "Visualizer"
//! view's picture; [`super::milkdrop_view`] adds its controls). With `cycle` on, every
//! `PRESET_S` seconds of playback it crossfades to a random other preset. While paused
//! the last frame stays.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use lio_projectm::ProjectM;
use rand::seq::IndexedRandom;

use crate::application::LioApplication;
use crate::config;

/// Seconds of playback per preset, and the crossfade between presets.
const PRESET_S: f64 = 30.0;
const CROSSFADE_S: f64 = 3.0;
/// Detail of projectM's warp mesh: plenty for a cover-sized view.
const MESH: (usize, usize) = (64, 48);

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::LioMilkdrop)]
    pub struct LioMilkdrop {
        /// File name of the preset showing (or about to show).
        #[property(get)]
        pub preset: RefCell<String>,
        /// Move on to a random preset every `PRESET_S` seconds of playback.
        #[property(get, set)]
        pub cycle: Cell<bool>,
        pub projectm: RefCell<Option<ProjectM>>,
        pub presets: RefCell<Vec<PathBuf>>,
        /// Playback time on the current preset, in seconds.
        pub preset_time: Cell<f64>,
        /// Presets that failed to load in a row; past the number of presets, Lio stops
        /// trying until the next scheduled switch.
        pub failures: Cell<usize>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub last_frame_us: Cell<i64>,
    }

    impl Default for LioMilkdrop {
        fn default() -> Self {
            Self {
                preset: RefCell::default(),
                cycle: Cell::new(true),
                projectm: RefCell::default(),
                presets: RefCell::new(find_presets()),
                preset_time: Cell::default(),
                failures: Cell::default(),
                tick: RefCell::default(),
                last_frame_us: Cell::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LioMilkdrop {
        const NAME: &'static str = "LioMilkdrop";
        type Type = super::LioMilkdrop;
        type ParentType = gtk::GLArea;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for LioMilkdrop {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.update_property(&[gtk::accessible::Property::Label(&gettext(
                "Music visualizer",
            ))]);
            // An OpenGL ES 3 build of libprojectM (the bundled one) works in the GLES
            // context GTK gives GL areas by default, and desktop GL drivers run its
            // shaders too; a desktop GL build needs OpenGL 3.3.
            if lio_projectm::GLES {
                obj.set_allowed_apis(gdk::GLAPI::GLES | gdk::GLAPI::GL);
                obj.set_required_version(3, 0);
            } else {
                obj.set_allowed_apis(gdk::GLAPI::GL);
                obj.set_required_version(3, 3);
            }
            obj.set_auto_render(false);
        }
    }

    impl WidgetImpl for LioMilkdrop {
        fn realize(&self) {
            self.parent_realize();
            let obj = self.obj();
            obj.make_current();
            if let Some(e) = obj.error() {
                tracing::warn!("no OpenGL for the visualizer: {e}");
                return;
            }
            let Some(mut projectm) = ProjectM::new() else {
                tracing::warn!("libprojectM can't use this OpenGL context");
                return;
            };
            projectm.set_mesh_size(MESH.0, MESH.1);
            projectm.set_fps(60);
            projectm.set_soft_cut_duration(CROSSFADE_S);
            // Lio picks presets itself (see `advance`).
            projectm.set_preset_duration(f64::MAX);
            projectm.set_hard_cut_enabled(false);
            projectm.set_aspect_correction(true);
            // A preset that fails to load (e.g. a shader the driver rejects) is skipped.
            let weak = obj.downgrade();
            projectm.on_preset_failed(move |file, message| {
                tracing::warn!(%file, "MilkDrop preset failed: {message}");
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    let Some(obj) = weak.upgrade() else { return };
                    let imp = obj.imp();
                    imp.failures.set(imp.failures.get() + 1);
                    if imp.failures.get() <= imp.presets.borrow().len() {
                        obj.next_preset(false);
                    }
                });
            });
            self.projectm.replace(Some(projectm));
            // A preset chosen before the GL context existed, or a random one.
            let chosen = self.preset.borrow().clone();
            if chosen.is_empty() || !obj.load(&chosen, false) {
                obj.next_preset(false);
            }
        }

        fn unrealize(&self) {
            let obj = self.obj();
            if let Some(projectm) = self.projectm.take() {
                obj.make_current();
                drop(projectm);
            }
            self.parent_unrealize();
        }

        fn map(&self) {
            self.parent_map();
            LioApplication::default().playback().want_pcm(true);
            self.last_frame_us.set(0);
            let id = self.obj().add_tick_callback(|w, clock| {
                w.advance(clock.frame_time());
                glib::ControlFlow::Continue
            });
            self.tick.replace(Some(id));
        }

        fn unmap(&self) {
            if let Some(id) = self.tick.take() {
                id.remove();
            }
            LioApplication::default().playback().want_pcm(false);
            self.parent_unmap();
        }
    }

    impl GLAreaImpl for LioMilkdrop {
        fn resize(&self, width: i32, height: i32) {
            self.parent_resize(width, height);
            if let Some(projectm) = self.projectm.borrow().as_ref() {
                projectm.set_window_size(width.max(1) as usize, height.max(1) as usize);
            }
        }

        fn render(&self, _context: &gdk::GLContext) -> glib::Propagation {
            let projectm = self.projectm.borrow();
            let Some(projectm) = projectm.as_ref() else {
                return glib::Propagation::Proceed;
            };
            projectm.pcm_add_stereo(&LioApplication::default().playback().pcm());
            framebuffer::render_into_current(|| projectm.render_frame());
            glib::Propagation::Stop
        }
    }
}

glib::wrapper! {
    pub struct LioMilkdrop(ObjectSubclass<imp::LioMilkdrop>)
        @extends gtk::GLArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LioMilkdrop {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LioMilkdrop {
    /// Renders a frame while music plays (holding the last one while paused) and moves
    /// on to another preset after `PRESET_S` seconds of playback.
    fn advance(&self, frame_us: i64) {
        let imp = self.imp();
        let last = imp.last_frame_us.replace(frame_us);
        let dt = if last == 0 {
            0.0
        } else {
            ((frame_us - last) as f64 / 1e6).clamp(0.0, 0.25)
        };
        if !LioApplication::default().playback().playing() {
            return;
        }
        let time = imp.preset_time.get() + dt;
        if time >= PRESET_S && imp.cycle.get() {
            imp.preset_time.set(0.0);
            imp.failures.set(0);
            self.next_preset(true);
        } else {
            imp.preset_time.set(time);
        }
        self.queue_render();
    }

    /// File names of the presets, in order.
    pub fn preset_names(&self) -> Vec<String> {
        self.imp()
            .presets
            .borrow()
            .iter()
            .filter_map(|p| Some(p.file_name()?.to_str()?.to_owned()))
            .collect()
    }

    /// Shows the preset with file name `name`, crossfading to it. Before the GL context
    /// exists, it becomes the first one shown.
    pub fn set_preset(&self, name: &str) {
        let imp = self.imp();
        if imp.projectm.borrow().is_none() {
            if imp.preset.replace(name.to_owned()) != name {
                self.notify_preset();
            }
            return;
        }
        imp.preset_time.set(0.0);
        self.load(name, true);
    }

    /// Switches to a random preset other than the current one, crossfading if `smooth`.
    fn next_preset(&self, smooth: bool) {
        let imp = self.imp();
        // Debug builds: `LIO_TEST_PRESET=<file name>` keeps to one preset.
        let forced = cfg!(debug_assertions)
            .then(|| std::env::var("LIO_TEST_PRESET").ok())
            .flatten();
        let name = forced.or_else(|| {
            let current = imp.preset.borrow().clone();
            let names = self.preset_names();
            let others: Vec<&String> = names.iter().filter(|n| **n != current).collect();
            others
                .choose(&mut rand::rng())
                .map(|n| (*n).clone())
                .or_else(|| names.first().cloned())
        });
        if let Some(name) = name {
            self.load(&name, smooth);
        }
    }

    /// Loads the preset with file name `name`; `false` if there is no such preset.
    fn load(&self, name: &str, smooth: bool) -> bool {
        let imp = self.imp();
        let Some(path) = imp
            .presets
            .borrow()
            .iter()
            .find(|p| p.file_name().is_some_and(|f| f == name))
            .cloned()
        else {
            return false;
        };
        tracing::debug!(preset = %path.display(), "MilkDrop preset");
        if let Some(projectm) = imp.projectm.borrow().as_ref() {
            self.make_current();
            projectm.load_preset_file(&path, smooth);
        }
        if imp.preset.replace(name.to_owned()) != name {
            self.notify_preset();
        }
        true
    }
}

/// The `.milk` files in the presets folder.
fn find_presets() -> Vec<PathBuf> {
    let dir = config::presets_dir();
    let mut presets: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "milk"))
                .collect()
        })
        .unwrap_or_default();
    presets.sort();
    if presets.is_empty() {
        tracing::warn!(dir = %dir.display(), "no MilkDrop presets found");
    }
    presets
}

/// Makes projectM draw into the `GtkGLArea`'s framebuffer.
///
/// libprojectM 4.1 always draws its final image to framebuffer 0, the window's own
/// (`ToDo: Allow external apps to provide a custom target framebuffer`), but a
/// `GtkGLArea` renders into a framebuffer of its own. libprojectM's `glBindFramebuffer`
/// calls resolve to the function below (at link time when it is linked statically, and
/// through the symbol `build.rs` exports when it is a shared library), which swaps 0 for
/// the framebuffer that was bound when the frame started. GTK loads its GL functions at
/// run time (libepoxy) and keeps calling the real one.
///
/// TODO: use `projectm_opengl_render_frame_fbo` (libprojectM 4.2) and drop this.
#[allow(unsafe_code)]
mod framebuffer {
    use std::cell::Cell;
    use std::sync::OnceLock;

    const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

    type BindFramebuffer = unsafe extern "C" fn(u32, u32);
    type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);

    thread_local! {
        /// Framebuffer to use instead of 0 while rendering a frame.
        static TARGET: Cell<u32> = const { Cell::new(0) };
    }

    /// The next definition of `name` after this executable's (libGL's).
    fn real<T: Copy>(name: &'static std::ffi::CStr, slot: &OnceLock<usize>) -> Option<T> {
        let address = *slot.get_or_init(|| {
            // SAFETY: dlsym with RTLD_NEXT and a NUL-terminated name.
            unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr()) as usize }
        });
        // SAFETY: a non-null address of the GL function `T` describes.
        (address != 0).then(|| unsafe { std::mem::transmute_copy::<usize, T>(&address) })
    }

    fn real_bind() -> Option<BindFramebuffer> {
        static SLOT: OnceLock<usize> = OnceLock::new();
        real(c"glBindFramebuffer", &SLOT)
    }

    /// Runs `render` with framebuffer 0 standing for the one bound right now.
    pub fn render_into_current(render: impl FnOnce()) {
        static SLOT: OnceLock<usize> = OnceLock::new();
        let mut bound = 0;
        if let Some(get) = real::<GetIntegerv>(c"glGetIntegerv", &SLOT) {
            // SAFETY: a GL context is current (GtkGLArea's render).
            unsafe { get(GL_DRAW_FRAMEBUFFER_BINDING, &mut bound) };
        }
        TARGET.set(bound as u32);
        render();
        TARGET.set(0);
    }

    /// Replaces libGL's `glBindFramebuffer` for libprojectM.
    #[unsafe(no_mangle)]
    pub extern "C" fn glBindFramebuffer(target: u32, framebuffer: u32) {
        let framebuffer = match framebuffer {
            0 => TARGET.get(),
            other => other,
        };
        if let Some(bind) = real_bind() {
            // SAFETY: forwarding the call unchanged apart from the framebuffer.
            unsafe { bind(target, framebuffer) };
        }
    }
}
