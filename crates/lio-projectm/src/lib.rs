//! The parts of libprojectM's C API that Lio's MilkDrop visualizer uses, and a safe
//! [`ProjectM`] handle over them. Declarations follow the `projectM-4/*.h` headers of
//! libprojectM 4.1.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Whether libprojectM was built for OpenGL ES 3 (shaders `#version 300 es`) rather than
/// desktop OpenGL 3.3, so which kind of context it needs.
pub const GLES: bool = cfg!(projectm_gles);

mod ffi {
    use std::ffi::{c_char, c_uint, c_void};

    #[repr(C)]
    pub struct projectm {
        _private: [u8; 0],
    }
    #[allow(non_camel_case_types)]
    pub type projectm_handle = *mut projectm;
    #[allow(non_camel_case_types)]
    pub type projectm_preset_switch_failed_event =
        Option<unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void)>;

    unsafe extern "C" {
        pub fn projectm_create() -> projectm_handle;
        pub fn projectm_destroy(instance: projectm_handle);
        pub fn projectm_load_preset_file(
            instance: projectm_handle,
            filename: *const c_char,
            smooth_transition: bool,
        );
        pub fn projectm_set_preset_switch_failed_event_callback(
            instance: projectm_handle,
            callback: projectm_preset_switch_failed_event,
            user_data: *mut c_void,
        );
        pub fn projectm_set_hard_cut_enabled(instance: projectm_handle, enabled: bool);
        pub fn projectm_set_soft_cut_duration(instance: projectm_handle, seconds: f64);
        pub fn projectm_set_preset_duration(instance: projectm_handle, seconds: f64);
        pub fn projectm_set_mesh_size(instance: projectm_handle, width: usize, height: usize);
        pub fn projectm_set_fps(instance: projectm_handle, fps: i32);
        pub fn projectm_set_aspect_correction(instance: projectm_handle, enabled: bool);
        pub fn projectm_set_window_size(instance: projectm_handle, width: usize, height: usize);
        pub fn projectm_pcm_get_max_samples() -> c_uint;
        /// `count` is per channel; `channels` is `PROJECTM_MONO` (1) or `PROJECTM_STEREO` (2).
        pub fn projectm_pcm_add_float(
            instance: projectm_handle,
            samples: *const f32,
            count: c_uint,
            channels: c_uint,
        );
        pub fn projectm_opengl_render_frame(instance: projectm_handle);
    }
}

type FailedCallback = Box<dyn Fn(&str, &str)>;

/// A projectM instance. Create, use and drop it with its OpenGL context current.
pub struct ProjectM {
    handle: ffi::projectm_handle,
    /// Boxed twice so libprojectM holds a thin pointer; freed after the instance.
    failed: Option<Box<FailedCallback>>,
}

impl ProjectM {
    /// A new instance for the current OpenGL context; `None` if libprojectM can't use it.
    pub fn new() -> Option<Self> {
        // SAFETY: no arguments; a null handle means failure.
        let handle = unsafe { ffi::projectm_create() };
        (!handle.is_null()).then_some(Self {
            handle,
            failed: None,
        })
    }

    /// Switches to the preset at `path`, crossfading if `smooth`. Failures are reported
    /// to the callback from [`Self::on_preset_failed`].
    pub fn load_preset_file(&self, path: &Path, smooth: bool) {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return;
        };
        // SAFETY: a live handle and a NUL-terminated string that outlives the call.
        unsafe { ffi::projectm_load_preset_file(self.handle, path.as_ptr(), smooth) }
    }

    /// Calls `callback(file, message)` when a preset fails to load.
    pub fn on_preset_failed(&mut self, callback: impl Fn(&str, &str) + 'static) {
        unsafe extern "C" fn trampoline(
            file: *const c_char,
            message: *const c_char,
            user_data: *mut c_void,
        ) {
            let text = |s: *const c_char| {
                // SAFETY: libprojectM passes NUL-terminated strings valid for the call.
                (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) }.to_string_lossy())
            };
            // SAFETY: `user_data` is the `FailedCallback` box in `ProjectM::failed`, which
            // lives as long as the instance.
            let callback = unsafe { &*(user_data as *const FailedCallback) };
            callback(
                text(file).as_deref().unwrap_or_default(),
                text(message).as_deref().unwrap_or_default(),
            );
        }
        let callback: Box<FailedCallback> = Box::new(Box::new(callback));
        let user_data = &*callback as *const FailedCallback as *mut c_void;
        // SAFETY: a live handle; `user_data` stays valid until replaced or dropped, and the
        // callback is replaced before the old box is freed.
        unsafe {
            ffi::projectm_set_preset_switch_failed_event_callback(
                self.handle,
                Some(trampoline),
                user_data,
            )
        };
        self.failed = Some(callback);
    }

    pub fn set_hard_cut_enabled(&self, enabled: bool) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_hard_cut_enabled(self.handle, enabled) }
    }

    pub fn set_soft_cut_duration(&self, seconds: f64) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_soft_cut_duration(self.handle, seconds) }
    }

    pub fn set_preset_duration(&self, seconds: f64) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_preset_duration(self.handle, seconds) }
    }

    pub fn set_mesh_size(&self, width: usize, height: usize) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_mesh_size(self.handle, width, height) }
    }

    pub fn set_fps(&self, fps: i32) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_fps(self.handle, fps) }
    }

    pub fn set_aspect_correction(&self, enabled: bool) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_aspect_correction(self.handle, enabled) }
    }

    pub fn set_window_size(&self, width: usize, height: usize) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_set_window_size(self.handle, width, height) }
    }

    /// Adds interleaved stereo samples (-1 to 1), in pieces libprojectM takes whole.
    pub fn pcm_add_stereo(&self, samples: &[f32]) {
        // SAFETY: no arguments.
        let max = unsafe { ffi::projectm_pcm_get_max_samples() } as usize;
        for chunk in samples.chunks(max.max(1) * 2) {
            let frames = (chunk.len() / 2) as c_uint;
            // SAFETY: a live handle, and `frames` stereo frames are in `chunk`.
            unsafe { ffi::projectm_pcm_add_float(self.handle, chunk.as_ptr(), frames, 2) }
        }
    }

    /// Draws a frame into framebuffer 0 of the current context.
    pub fn render_frame(&self) {
        // SAFETY: a live handle.
        unsafe { ffi::projectm_opengl_render_frame(self.handle) }
    }
}

impl Drop for ProjectM {
    fn drop(&mut self) {
        // SAFETY: the handle is live and not used again; the callback box outlives it.
        unsafe { ffi::projectm_destroy(self.handle) }
    }
}
