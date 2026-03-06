//! Tone generator for DTMF and custom tone playback.

use std::ffi::CString;
use std::ptr;
use tracing::debug;
use crate::error::{check_status, make_pj_error, PjError, Result};
use crate::ffi;
use crate::ffi_helpers::PjString;
use crate::types::ConfPort;
use crate::PjsuaApp;

/// Describes a single tone (dual-frequency) for playback.
#[derive(Debug, Clone, Copy)]
pub struct ToneDesc {
    /// First frequency in Hz (0 to disable).
    pub freq1: i16,
    /// Second frequency in Hz (0 to disable).
    pub freq2: i16,
    /// Duration of the tone in milliseconds.
    pub on_ms: i16,
    /// Silence after the tone in milliseconds.
    pub off_ms: i16,
    /// Volume (0 = default).
    pub volume: i16,
}

/// Describes a single DTMF digit for playback.
#[derive(Debug, Clone, Copy)]
pub struct DtmfDigit {
    /// The digit character ('0'-'9', '*', '#', 'A'-'D').
    pub digit: char,
    /// Duration of the digit tone in milliseconds.
    pub on_ms: i16,
    /// Silence after the digit in milliseconds.
    pub off_ms: i16,
    /// Volume (0 = default).
    pub volume: i16,
}

/// A tone generator media port for DTMF and custom tone playback.
pub struct ToneGenerator {
    port: *mut ffi::pjmedia_port,
    pool: *mut ffi::pj_pool_t,
    /// Keep the port name alive — pjmedia_port_info.name is a pj_str_t
    /// pointing into this buffer.
    _name: PjString,
    conf_port: Option<ConfPort>,
    /// Pool allocated by conf_add_port, released on remove/drop.
    conf_pool: Option<*mut ffi::pj_pool_t>,
}

// SAFETY: ToneGenerator's raw pointers (port, pool, conf_pool) are only
// accessed from &self/&mut self methods. The underlying PJSIP objects are not
// accessed concurrently — they are used from a single owner at a time.
unsafe impl Send for ToneGenerator {}

impl std::fmt::Debug for ToneGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToneGenerator")
            .field("conf_port", &self.conf_port)
            .finish()
    }
}

impl ToneGenerator {
    /// Create a new tone generator.
    pub fn new(_app: &PjsuaApp, clock_rate: u32, channel_count: u32, samples_per_frame: u32) -> Result<Self> {
        let pool_name = CString::new("tonegen").unwrap();
        let pool = unsafe { ffi::pjsua_pool_create(pool_name.as_ptr(), 4096, 4096) };
        if pool.is_null() {
            return Err(PjError::Pjsip { status: -1, message: "Failed to create pool for ToneGenerator".into() });
        }
        let mut port: *mut ffi::pjmedia_port = ptr::null_mut();
        let name = PjString::new("tonegen");
        let pj_name = name.as_pj_str();
        let status = unsafe {
            ffi::pjmedia_tonegen_create2(pool, &pj_name, clock_rate, channel_count, samples_per_frame, 16, 0, &mut port)
        };
        if status != 0 {
            unsafe { ffi::pj_pool_release(pool) };
            return Err(make_pj_error(status));
        }
        debug!("tone generator created: rate={clock_rate} ch={channel_count} spf={samples_per_frame}");
        Ok(ToneGenerator { port, pool, _name: name, conf_port: None, conf_pool: None })
    }

    /// Add this tone generator to the conference bridge.
    pub fn add_to_conference(&mut self, app: &PjsuaApp) -> Result<ConfPort> {
        if self.conf_port.is_some() {
            return Err(PjError::AlreadyInUse("conference port".into()));
        }
        let (port_id, pool) = unsafe { app.conf_add_port(self.port)? };
        self.conf_port = Some(port_id);
        self.conf_pool = Some(pool);
        debug!("tone generator added to conference: id={}", port_id.0);
        Ok(port_id)
    }

    /// Remove this tone generator from the conference bridge.
    pub fn remove_from_conference(&mut self, app: &PjsuaApp) -> Result<()> {
        if let Some(port) = self.conf_port.take() {
            app.conf_remove_port(port)?;
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        Ok(())
    }

    /// Queue DTMF digits for playback.
    pub fn play_digits(&self, digits: &[DtmfDigit]) -> Result<()> {
        if digits.is_empty() { return Ok(()); }
        let c_digits: Vec<ffi::pjmedia_tone_digit> = digits.iter().map(|d| ffi::pjmedia_tone_digit {
            digit: d.digit as i8,
            on_msec: d.on_ms,
            off_msec: d.off_ms,
            volume: d.volume,
        }).collect();
        let status = unsafe { ffi::pjmedia_tonegen_play_digits(self.port, c_digits.len() as u32, c_digits.as_ptr(), 0) };
        check_status(status)
    }

    /// Queue custom tones for playback.
    pub fn play_tones(&self, tones: &[ToneDesc]) -> Result<()> {
        if tones.is_empty() { return Ok(()); }
        let c_tones: Vec<ffi::pjmedia_tone_desc> = tones.iter().map(|t| ffi::pjmedia_tone_desc {
            freq1: t.freq1,
            freq2: t.freq2,
            on_msec: t.on_ms,
            off_msec: t.off_ms,
            volume: t.volume,
            flags: 0,
        }).collect();
        let status = unsafe { ffi::pjmedia_tonegen_play(self.port, c_tones.len() as u32, c_tones.as_ptr(), 0) };
        check_status(status)
    }

    /// Check if the tone generator is currently playing.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        unsafe { ffi::pjmedia_tonegen_is_busy(self.port) != 0 }
    }

    /// Stop playback immediately.
    pub fn stop(&self) -> Result<()> {
        let status = unsafe { ffi::pjmedia_tonegen_stop(self.port) };
        check_status(status)
    }

    /// Get the conference bridge port ID, if registered.
    #[must_use]
    pub fn conf_port(&self) -> Option<ConfPort> { self.conf_port }
    /// Get the raw `pjmedia_port` pointer.
    #[must_use]
    pub fn as_raw_port(&self) -> *mut ffi::pjmedia_port { self.port }
}

impl Drop for ToneGenerator {
    fn drop(&mut self) {
        if let Some(port) = self.conf_port.take() {
            unsafe { ffi::pjsua_conf_remove_port(port.0) };
        }
        if let Some(pool) = self.conf_pool.take() {
            unsafe { ffi::pj_pool_release(pool) };
        }
        unsafe { ffi::pjmedia_port_destroy(self.port) };
        unsafe { ffi::pj_pool_release(self.pool) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_generator_is_debug() {
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<ToneGenerator>();
    }

    #[test]
    fn tone_desc_construction() {
        let tone = ToneDesc { freq1: 440, freq2: 0, on_ms: 200, off_ms: 100, volume: 0 };
        assert_eq!(tone.freq1, 440);
    }

    #[test]
    fn dtmf_digit_construction() {
        let digit = DtmfDigit { digit: '5', on_ms: 100, off_ms: 50, volume: 0 };
        assert_eq!(digit.digit, '5');
    }

    #[test]
    fn dtmf_digit_all_valid_chars() {
        for ch in "0123456789*#ABCD".chars() {
            let d = DtmfDigit { digit: ch, on_ms: 100, off_ms: 50, volume: 0 };
            assert_eq!(d.digit, ch);
        }
    }
}
