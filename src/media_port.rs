//! Custom audio port support via the `MediaPort` trait.

use tracing::debug;
use crate::error::{check_status, make_pj_error, PjError, Result};
use crate::ffi;
use crate::ffi_helpers::PjString;
use crate::types::ConfPort;
use crate::PjsuaApp;

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
pub trait MediaPort: Send + 'static {
    fn get_frame(&mut self, frame: &mut AudioFrame) -> Result<()> {
        let _ = frame;
        Ok(())
    }
    fn put_frame(&mut self, frame: &AudioFrame) -> Result<()> {
        let _ = frame;
        Ok(())
    }
    fn on_destroy(&mut self) {}
}

/// A custom audio port wrapping a MediaPort implementation.
pub struct CustomPort {
    raw_port: *mut ffi::pjmedia_port,
    _impl_box: *mut Box<dyn MediaPort>,
    /// Keep the port name alive — pjmedia_port_info.name is a pj_str_t
    /// pointing into this buffer.
    _name: PjString,
    conf_port: Option<ConfPort>,
    /// Pool allocated by conf_add_port, released on remove/drop.
    conf_pool: Option<*mut ffi::pj_pool_t>,
}

// SAFETY: CustomPort's raw pointers (raw_port, _impl_box, conf_pool) are only
// accessed from &self/&mut self methods. The underlying PJSIP objects are not
// accessed concurrently — they are used from a single owner at a time.
unsafe impl Send for CustomPort {}

impl std::fmt::Debug for CustomPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomPort")
            .field("conf_port", &self.conf_port)
            .finish()
    }
}

impl CustomPort {
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

        let impl_box = Box::into_raw(Box::new(port_impl));
        unsafe {
            (*raw_port).port_data.pdata = impl_box as *mut std::ffi::c_void;
            (*raw_port).get_frame = Some(trampoline_get_frame);
            (*raw_port).put_frame = Some(trampoline_put_frame);
            (*raw_port).on_destroy = Some(trampoline_on_destroy);
        }

        debug!("custom port created: name={name}");
        Ok(CustomPort { raw_port, _impl_box: impl_box, _name: port_name, conf_port: None, conf_pool: None })
    }

    pub fn add_to_conference(&mut self, app: &PjsuaApp) -> Result<ConfPort> {
        if self.conf_port.is_some() {
            return Err(PjError::AlreadyInUse("conference port".into()));
        }
        let (port_id, pool) = unsafe { app.conf_add_port(self.raw_port)? };
        self.conf_port = Some(port_id);
        self.conf_pool = Some(pool);
        Ok(port_id)
    }

    pub fn remove_from_conference(&mut self, app: &PjsuaApp) -> Result<()> {
        if let Some(port) = self.conf_port.take() {
            app.conf_remove_port(port)?;
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        Ok(())
    }

    pub fn conf_port(&self) -> Option<ConfPort> { self.conf_port }
    pub fn as_raw_port(&self) -> *mut ffi::pjmedia_port { self.raw_port }
}

impl Drop for CustomPort {
    fn drop(&mut self) {
        if let Some(port) = self.conf_port.take() {
            unsafe { ffi::pjsua_conf_remove_port(port.0) };
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        let impl_box = unsafe { &mut *self._impl_box };
        impl_box.on_destroy();
        let _ = unsafe { Box::from_raw(self._impl_box) };
        let _ = unsafe { Box::from_raw(self.raw_port) };
    }
}

unsafe extern "C" fn trampoline_get_frame(
    this_port: *mut ffi::pjmedia_port,
    frame: *mut ffi::pjmedia_frame,
) -> i32 {
    let impl_ptr = (*this_port).port_data.pdata as *mut Box<dyn MediaPort>;
    let port_impl = &mut *impl_ptr;
    let buf = (*frame).buf as *mut i16;
    let sample_count = (*frame).size as usize / 2;
    let samples = std::slice::from_raw_parts_mut(buf, sample_count);
    samples.fill(0);
    let mut audio_frame = AudioFrame { samples, timestamp: (*frame).timestamp.u64_ };
    match port_impl.get_frame(&mut audio_frame) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn trampoline_put_frame(
    this_port: *mut ffi::pjmedia_port,
    frame: *mut ffi::pjmedia_frame,
) -> i32 {
    let impl_ptr = (*this_port).port_data.pdata as *mut Box<dyn MediaPort>;
    let port_impl = &mut *impl_ptr;
    let buf = (*frame).buf as *mut i16;
    let size_bytes = (*frame).size as usize;
    if buf.is_null() || size_bytes == 0 {
        return 0;
    }
    let sample_count = size_bytes / 2;
    let samples = std::slice::from_raw_parts_mut(buf, sample_count);
    let audio_frame = AudioFrame { samples, timestamp: (*frame).timestamp.u64_ };
    match port_impl.put_frame(&audio_frame) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// No-op: actual cleanup is done in `CustomPort::drop`, which removes the port
/// from the conference bridge, calls `MediaPort::on_destroy`, and frees memory.
unsafe extern "C" fn trampoline_on_destroy(_this_port: *mut ffi::pjmedia_port) -> i32 {
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
        let frame = AudioFrame { samples: &mut buf, timestamp: 12345 };
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
        let frame = AudioFrame { samples: &mut buf, timestamp: 0 };
        assert_eq!(frame.samples.len(), 160);
    }

    #[test]
    fn silence_port_default_impls() {
        let mut port = SilencePort;
        let mut buf = [0i16; 160];
        let mut frame = AudioFrame { samples: &mut buf, timestamp: 0 };
        assert!(port.get_frame(&mut frame).is_ok());
        assert!(port.put_frame(&frame).is_ok());
        port.on_destroy();
    }
}
