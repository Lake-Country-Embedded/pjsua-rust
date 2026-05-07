//! Custom audio port support via the `MediaPort` trait.

use crate::error::{make_pj_error, PjError, Result};
use crate::ffi;
use crate::ffi_helpers::PjString;
use crate::types::ConfPort;
use crate::PjsuaApp;
use tracing::debug;

/// An audio frame exchanged between media ports and the conference bridge.
pub struct AudioFrame<'a> {
    pub samples: &'a mut [i16],
    pub timestamp: u64,
}

impl<'a> std::fmt::Debug for AudioFrame<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFrame")
            .field("sample_count", &self.samples.len())
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

/// Trait for implementing custom audio ports.
///
/// Implement this trait to create a custom audio source/sink that can be
/// registered with the PJSUA conference bridge via [`CustomPort`].
pub trait MediaPort: Send + 'static {
    /// Called when the conference bridge needs audio data from this port.
    fn get_frame(&mut self, frame: &mut AudioFrame) -> Result<()> {
        let _ = frame;
        Ok(())
    }
    /// Called when the conference bridge delivers audio data to this port.
    fn put_frame(&mut self, frame: &AudioFrame) -> Result<()> {
        let _ = frame;
        Ok(())
    }
    /// Called when the port is being destroyed.
    fn on_destroy(&mut self) {}
}

/// Heap-allocated context owned by a `CustomPort`'s `pjmedia_port` and freed
/// in [`trampoline_on_destroy`]. PJSIP reaches it through `port_data.pdata`.
///
/// Ownership of these resources is transferred to PJSIP for the duration of
/// the port's lifetime so they outlive `CustomPort::drop`. PJSIP's queued
/// port-removal (`pjmedia_conf_remove_port`) runs `op_remove_port` later on
/// the audio clock thread and dereferences the `pjmedia_port` struct (to read
/// `grp_lock`); the `pjmedia_port_info.name` pj_str_t aliases bytes inside
/// `_name`. Freeing any of this in `Drop` is therefore a use-after-free.
struct PortContext {
    impl_box: Box<dyn MediaPort>,
    /// Backs the `pjmedia_port_info.name` pj_str_t pointer. Must outlive the
    /// `pjmedia_port` struct.
    _name: PjString,
}

/// A custom audio port wrapping a MediaPort implementation.
pub struct CustomPort {
    raw_port: *mut ffi::pjmedia_port,
    conf_port: Option<ConfPort>,
    /// Parent pool we passed to `pjsua_conf_add_port`. Only its factory is used
    /// by PJSIP (the conf_port has its own child pool), so this is safe to
    /// release any time after `conf_add_port` returns.
    conf_pool: Option<*mut ffi::pj_pool_t>,
}

// SAFETY: Once registered with the conference bridge, the MediaPort impl is
// exclusively accessed by PJSIP's worker thread via the trampoline callbacks.
// The CustomPort struct methods (add_to_conference, remove_from_conference, drop)
// synchronise with PJSIP through conf_add_port/conf_remove_port, which acquire
// PJSIP's internal mutex. Before registration and after removal, no concurrent
// access occurs. The raw pointers are heap-stable (Box::into_raw).
unsafe impl Send for CustomPort {}

impl std::fmt::Debug for CustomPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomPort")
            .field("conf_port", &self.conf_port)
            .finish()
    }
}

impl CustomPort {
    /// Create a new custom audio port wrapping a [`MediaPort`] implementation.
    pub fn new(
        _app: &PjsuaApp,
        name: &str,
        clock_rate: u32,
        channel_count: u32,
        samples_per_frame: u32,
        port_impl: Box<dyn MediaPort>,
    ) -> Result<Self> {
        let raw_port = Box::into_raw(Box::new(ffi::pjmedia_port::default()));
        let port_name = PjString::new(name);
        let pj_name = port_name.as_pj_str();
        let status = unsafe {
            ffi::pjmedia_port_info_init(
                &mut (*raw_port).info,
                &pj_name,
                0x4352_5354, // "CRST"
                clock_rate,
                channel_count,
                16,
                samples_per_frame,
            )
        };
        if status != 0 {
            let _ = unsafe { Box::from_raw(raw_port) };
            return Err(make_pj_error(status));
        }

        let context = Box::into_raw(Box::new(PortContext {
            impl_box: port_impl,
            _name: port_name,
        }));
        unsafe {
            (*raw_port).port_data.pdata = context as *mut std::ffi::c_void;
            (*raw_port).get_frame = Some(trampoline_get_frame);
            (*raw_port).put_frame = Some(trampoline_put_frame);
            (*raw_port).on_destroy = Some(trampoline_on_destroy);
        }

        debug!("custom port created: name={name}");
        Ok(CustomPort {
            raw_port,
            conf_port: None,
            conf_pool: None,
        })
    }

    /// Add this port to the conference bridge.
    pub fn add_to_conference(&mut self, app: &PjsuaApp) -> Result<ConfPort> {
        if self.conf_port.is_some() {
            return Err(PjError::AlreadyInUse("conference port".into()));
        }
        let (port_id, pool) = unsafe { app.conf_add_port(self.raw_port)? };
        self.conf_port = Some(port_id);
        self.conf_pool = Some(pool);
        Ok(port_id)
    }

    /// Remove this port from the conference bridge.
    ///
    /// Does *not* destroy the underlying `pjmedia_port` — that happens in
    /// `Drop` (or when the port's `grp_lock` refcount hits zero, whichever
    /// comes last). Calling this and then keeping the `CustomPort` alive is
    /// fine: PJSIP still holds a `grp_lock` reference so the port struct
    /// stays valid until `Drop` calls `pjmedia_port_destroy`.
    pub fn remove_from_conference(&mut self, app: &PjsuaApp) -> Result<()> {
        if let Some(port) = self.conf_port.take() {
            app.conf_remove_port(port)?;
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        Ok(())
    }

    /// Get the conference bridge port ID, if registered.
    #[must_use]
    pub fn conf_port(&self) -> Option<ConfPort> {
        self.conf_port
    }
    /// Get the raw `pjmedia_port` pointer.
    #[must_use]
    pub fn as_raw_port(&self) -> *mut ffi::pjmedia_port {
        self.raw_port
    }
}

impl Drop for CustomPort {
    fn drop(&mut self) {
        // Release our resources in the order PJSIP expects:
        //
        //   1. `pjsua_conf_remove_port` queues a REMOVE_PORT op. PJSIP's audio
        //      clock thread eventually runs `op_remove_port`, which calls
        //      `pj_grp_lock_dec_ref` on `port->grp_lock` (the conf bridge's
        //      reference, added when `create_conf_port` ran during add).
        //
        //   2. `pj_pool_release(conf_pool)` releases the parent pool we
        //      handed `pjsua_conf_add_port`. PJSIP only used its factory to
        //      spawn `conf_port`'s own child pool, so this is safe at any
        //      point after add.
        //
        //   3. `pjmedia_port_destroy(raw_port)` releases *our* reference on
        //      `port->grp_lock`. With `grp_lock` set (the normal case after
        //      conf add), it just `dec_ref`s — when refcount reaches zero
        //      (after step 1's queued op also dec'd), the registered handlers
        //      fire: `port_on_destroy` invokes our `trampoline_on_destroy`
        //      (which frees `raw_port` and the `PortContext`), and
        //      `conf_port_on_destroy` releases the conf_port's child pool.
        //      With no `grp_lock` (port was never added to a conference),
        //      `pjmedia_port_destroy` invokes `on_destroy` directly.
        //
        // Critically, *we do not* `Box::from_raw(self.raw_port)` here — that
        // was the bug: PJSIP still needs to read `port->grp_lock` from the
        // queued op when the audio thread runs.
        if let Some(port) = self.conf_port.take() {
            unsafe { ffi::pjsua_conf_remove_port(port.0) };
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        unsafe { ffi::pjmedia_port_destroy(self.raw_port) };
    }
}

/// Trampoline for `get_frame` — called by PJSIP's conference bridge from a
/// worker thread. Wrapped in `catch_unwind` to prevent panics in user
/// `MediaPort` implementations from unwinding across C frames (which is UB).
unsafe extern "C" fn trampoline_get_frame(
    this_port: *mut ffi::pjmedia_port,
    frame: *mut ffi::pjmedia_frame,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let context = &mut *((*this_port).port_data.pdata as *mut PortContext);
        let port_impl = &mut *context.impl_box;
        let buf = (*frame).buf as *mut i16;
        let sample_count = (*frame).size / 2;
        let samples = std::slice::from_raw_parts_mut(buf, sample_count);
        samples.fill(0);
        let mut audio_frame = AudioFrame {
            samples,
            timestamp: (*frame).timestamp.u64_,
        };
        match port_impl.get_frame(&mut audio_frame) {
            Ok(()) => {
                // Signal to PJSUA's conference bridge that this frame contains
                // audio data. Without this, the bridge treats every frame as
                // PJMEDIA_FRAME_TYPE_NONE (0) and zero-fills the sample buffer,
                // silencing all audio even when get_frame() wrote real PCM data.
                (*frame).type_ =
                    ffi::pjmedia_frame_type_PJMEDIA_FRAME_TYPE_AUDIO;
                0
            }
            Err(_) => -1,
        }
    }));
    result.unwrap_or(-1)
}

/// Trampoline for `put_frame` — called by PJSIP's conference bridge from a
/// worker thread. Wrapped in `catch_unwind` to prevent panics in user
/// `MediaPort` implementations from unwinding across C frames (which is UB).
unsafe extern "C" fn trampoline_put_frame(
    this_port: *mut ffi::pjmedia_port,
    frame: *mut ffi::pjmedia_frame,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let context = &mut *((*this_port).port_data.pdata as *mut PortContext);
        let port_impl = &mut *context.impl_box;
        let buf = (*frame).buf as *mut i16;
        let size_bytes = (*frame).size;
        if buf.is_null() || size_bytes == 0 {
            return 0;
        }
        let sample_count = size_bytes / 2;
        let samples = std::slice::from_raw_parts_mut(buf, sample_count);
        let audio_frame = AudioFrame {
            samples,
            timestamp: (*frame).timestamp.u64_,
        };
        match port_impl.put_frame(&audio_frame) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }));
    result.unwrap_or(-1)
}

/// PJSIP's port-destroy callback. Invoked when the port's `grp_lock` refcount
/// reaches zero (via the `port_on_destroy` handler PJSIP registered during
/// `pjmedia_port_init_grp_lock`), or directly from `pjmedia_port_destroy` when
/// the port has no `grp_lock` (i.e., was never added to a conference).
///
/// This is the only place we free the `PortContext` and the `pjmedia_port`
/// itself — by the time PJSIP calls us, no further trampoline callbacks or
/// queued conf-bridge operations will touch them.
unsafe extern "C" fn trampoline_on_destroy(this_port: *mut ffi::pjmedia_port) -> i32 {
    if this_port.is_null() {
        return 0;
    }
    let context_ptr = (*this_port).port_data.pdata as *mut PortContext;
    if !context_ptr.is_null() {
        // Deliberately scoped: drop `context` (which calls `impl_box.on_destroy`
        // via `MediaPort::on_destroy` only if we explicitly invoke it; the
        // `Drop` of `Box<dyn MediaPort>` does not).
        let mut context = Box::from_raw(context_ptr);
        // Catch unwind so a panic in user `on_destroy` doesn't propagate
        // across the FFI boundary.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            context.impl_box.on_destroy();
        }));
        (*this_port).port_data.pdata = std::ptr::null_mut();
        // `context` drops here, freeing `impl_box` and the `PjString` name.
    }
    let _ = Box::from_raw(this_port);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SilencePort;
    impl MediaPort for SilencePort {}

    #[test]
    fn audio_frame_debug() {
        let mut buf = [0i16; 4];
        let frame = AudioFrame {
            samples: &mut buf,
            timestamp: 12345,
        };
        let debug = format!("{:?}", frame);
        assert!(debug.contains("AudioFrame"));
        assert!(debug.contains("4")); // sample count
        assert!(debug.contains("12345")); // timestamp
    }

    #[test]
    fn custom_port_is_debug() {
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<CustomPort>();
    }

    #[test]
    fn audio_frame_size() {
        let mut buf = [0i16; 160];
        let frame = AudioFrame {
            samples: &mut buf,
            timestamp: 0,
        };
        assert_eq!(frame.samples.len(), 160);
    }

    #[test]
    fn silence_port_default_impls() {
        let mut port = SilencePort;
        let mut buf = [0i16; 160];
        let mut frame = AudioFrame {
            samples: &mut buf,
            timestamp: 0,
        };
        assert!(port.get_frame(&mut frame).is_ok());
        assert!(port.put_frame(&frame).is_ok());
        port.on_destroy();
    }
}
