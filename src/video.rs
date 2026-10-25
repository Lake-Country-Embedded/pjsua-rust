//! H.264 passthrough video.
//!
//! PJSUA normally captures raw video, encodes it and decodes what it
//! receives. [`VideoPassthrough`] replaces both ends with the application:
//! it pushes already-encoded H.264 access units (Annex-B, one picture each)
//! with [`VideoPassthrough::push_frame`], and receives the remote's access
//! units through [`VideoHandler::on_frame_received`]. PJSUA still does the
//! SDP negotiation, RTP packetization, RTCP and SRTP.
//!
//! Typical use:
//!
//! 1. [`VideoPassthrough::install`] once, after [`PjsuaApp::new`].
//! 2. Put the returned [`VideoDevices`] in
//!    [`AccountConfig::video_devices`](crate::AccountConfig::video_devices).
//! 3. Request video per call with [`CallSetting::video`](crate::CallSetting)
//!    or [`PjsuaApp::answer_call_with_video`].
//! 4. Feed access units from the encoder; answer
//!    [`VideoHandler::on_keyframe_request`] by forcing an IDR.
//!
//! There is a single source, shared by every call that sends video.

use std::collections::VecDeque;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tracing::{debug, warn};

use crate::error::{check_status, Result};
use crate::types::{CallId, VideoDevices};
use crate::PjsuaApp;

#[repr(C)]
struct pjrs_vpt_config {
    width: c_uint,
    height: c_uint,
    fps: c_uint,
    poll_fps: c_uint,
    max_frame_size: c_uint,
    avg_bps: c_uint,
    max_bps: c_uint,
    pt: c_uint,
    packetization_mode: c_uint,
    profile_idc: u8,
    profile_iop: u8,
    level_idc: u8,
    max_fs: c_uint,
    max_mbps: c_uint,
    max_br: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct pjrs_vid_remote_caps {
    has_profile_level_id: c_int,
    profile_idc: u8,
    profile_iop: u8,
    level_idc: u8,
    packetization_mode: c_uint,
    max_fs: c_uint,
    max_mbps: c_uint,
    max_br: c_uint,
    tias_bps: c_uint,
    as_kbps: c_uint,
}

#[repr(C)]
struct pjrs_vpt_stream_info {
    width: c_uint,
    height: c_uint,
    fps_num: c_uint,
    fps_denum: c_uint,
    avg_bps: c_uint,
    max_bps: c_uint,
}

#[repr(C)]
struct pjrs_vpt_callbacks {
    user_data: *mut c_void,
    capture_peek:
        Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut c_int, *mut u32) -> usize>,
    capture_consumed: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    keyframe_request: Option<unsafe extern "C" fn(*mut c_void)>,
    encoder_open: Option<unsafe extern "C" fn(*mut c_void, *const pjrs_vpt_stream_info)>,
    encoder_close: Option<unsafe extern "C" fn(*mut c_void)>,
    render_push: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, c_int)>,
}

#[repr(C)]
#[derive(Default)]
struct pjrs_vpt_devices {
    capture: c_int,
    render: c_int,
}

extern "C" {
    fn pjrs_vpt_init(
        cfg: *const pjrs_vpt_config,
        cb: *const pjrs_vpt_callbacks,
        devs: *mut pjrs_vpt_devices,
    ) -> i32;
    fn pjrs_vpt_reconfigure(cfg: *const pjrs_vpt_config) -> i32;
    fn pjrs_vpt_source_restarted();
    fn pjrs_vid_remote_caps_get(
        call_id: c_int,
        med_idx: c_uint,
        caps: *mut pjrs_vid_remote_caps,
    ) -> i32;
    fn pjrs_vid_set_transmit(call_id: c_int, enable: c_int) -> i32;
}

/// The remote's receive capabilities for media `med_idx` of a call.
pub(crate) fn remote_caps(call_id: c_int, med_idx: usize) -> Option<crate::types::H264RemoteCaps> {
    let mut c = pjrs_vid_remote_caps::default();
    let status = unsafe { pjrs_vid_remote_caps_get(call_id, med_idx as c_uint, &mut c) };
    if status != 0 {
        return None;
    }
    Some(crate::types::H264RemoteCaps {
        profile_level_id: (c.has_profile_level_id != 0).then_some((
            c.profile_idc,
            c.profile_iop,
            c.level_idc,
        )),
        packetization_mode: c.packetization_mode as u8,
        max_fs: c.max_fs,
        max_mbps: c.max_mbps,
        max_br_kbps: c.max_br,
        tias_bps: (c.tias_bps != 0).then_some(c.tias_bps),
        as_kbps: (c.as_kbps != 0).then_some(c.as_kbps),
    })
}

/// H.264 `profile-level-id`, as advertised in SDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264Profile {
    pub profile_idc: u8,
    pub profile_iop: u8,
    pub level_idc: u8,
}

impl H264Profile {
    /// Constrained Baseline, level 3.1 (`42e01f`): the profile every H.264
    /// video phone and softphone decodes.
    pub const CONSTRAINED_BASELINE_3_1: H264Profile = H264Profile::constrained_baseline(31);

    /// Constrained Baseline at `level_idc` (tenths: 31 is level 3.1).
    pub const fn constrained_baseline(level_idc: u8) -> H264Profile {
        H264Profile {
            profile_idc: 0x42,
            profile_iop: 0xe0,
            level_idc,
        }
    }
}

/// Receive limits to advertise beyond what the level implies (RFC 6184
/// `max-fs`, `max-mbps`, `max-br`). `None` leaves a parameter out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct H264ReceiveLimits {
    /// Macroblocks.
    pub max_fs: Option<u32>,
    /// Macroblocks per second.
    pub max_mbps: Option<u32>,
    pub max_br_kbps: Option<u32>,
}

/// Passthrough parameters. These describe the source. All but
/// `max_frame_size` and `payload_type` can be changed later with
/// [`VideoPassthrough::reconfigure`].
#[derive(Debug, Clone)]
pub struct PassthroughConfig {
    pub width: u32,
    pub height: u32,
    /// Nominal frame rate of the source.
    pub fps: u32,
    /// How often PJSUA polls for a new access unit. `None` is twice `fps`,
    /// which keeps the added latency under half a frame.
    pub poll_fps: Option<u32>,
    /// Largest access unit, in bytes. Larger ones are dropped.
    pub max_frame_size: usize,
    /// Bitrates in bits per second, for SDP and send pacing.
    pub avg_bps: u32,
    pub max_bps: u32,
    /// Dynamic RTP payload type for H.264.
    pub payload_type: u8,
    /// `packetization-mode` advertised in SDP. 1 lets large NAL units be
    /// fragmented; 0 needs every NAL unit to fit one RTP packet (limit the
    /// encoder's slice size). The remote's choice of either is accepted.
    pub packetization_mode: u8,
    pub profile: H264Profile,
    /// What this end can receive, beyond its level.
    pub receive: H264ReceiveLimits,
    /// Most access units held waiting for PJSUA. On overflow, frames before
    /// the newest queued keyframe are dropped; with no keyframe queued, the
    /// queue is flushed and a keyframe requested.
    pub max_queue: usize,
}

impl Default for PassthroughConfig {
    fn default() -> Self {
        PassthroughConfig {
            width: 640,
            height: 480,
            fps: 15,
            poll_fps: None,
            max_frame_size: 256 * 1024,
            avg_bps: 1_000_000,
            max_bps: 1_500_000,
            payload_type: 97,
            packetization_mode: 1,
            profile: H264Profile::CONSTRAINED_BASELINE_3_1,
            receive: H264ReceiveLimits::default(),
            max_queue: 8,
        }
    }
}

/// Parameters of a sending video stream after negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_denum: u32,
    pub avg_bps: u32,
    pub max_bps: u32,
}

/// Events from the passthrough. Called on PJSIP media threads: return
/// quickly and do not call back into PJSUA.
pub trait VideoHandler: Send + Sync + 'static {
    /// A keyframe is needed (stream start, RTCP PLI/FIR, queue overflow).
    fn on_keyframe_request(&self) {}
    /// A sending stream opened, or its parameters changed.
    fn on_encoder_open(&self, info: VideoStreamInfo) {
        let _ = info;
    }
    /// A sending stream closed.
    fn on_encoder_close(&self) {}
    /// An access unit arrived from the remote.
    fn on_frame_received(&self, frame: &[u8], keyframe: bool) {
        let _ = (frame, keyframe);
    }
}

struct Queued {
    id: u32,
    data: Vec<u8>,
    keyframe: bool,
}

struct Queue {
    frames: VecDeque<Queued>,
    /// Id for the next pushed frame; never 0, which C reads as "none".
    next_id: u32,
    /// Set after a flush: drop everything until the next keyframe, which is
    /// the first frame the remote can decode again.
    need_keyframe: bool,
}

impl Queue {
    /// Drop frames up to and including `id`. Ids wrap, so "at or before" is
    /// a non-negative wrapping distance.
    fn consume_through(&mut self, id: u32) {
        while self
            .frames
            .front()
            .is_some_and(|f| (id.wrapping_sub(f.id) as i32) >= 0)
        {
            self.frames.pop_front();
        }
    }
}

struct Shared {
    queue: Mutex<Queue>,
    max_queue: AtomicUsize,
    max_frame_size: usize,
    handler: Arc<dyn VideoHandler>,
}

/// The installed passthrough. PJSIP calls back through a process-wide slot
/// rather than a pointer it owns, so a callback racing `pjsua_destroy` never
/// sees freed memory.
static SHARED: RwLock<Option<Arc<Shared>>> = RwLock::new(None);

fn shared() -> Option<Arc<Shared>> {
    SHARED.read().ok().and_then(|s| s.clone())
}

/// Handle to the installed H.264 passthrough. Cheap to clone.
#[derive(Clone)]
pub struct VideoPassthrough {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for VideoPassthrough {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPassthrough")
            .field("max_queue", &self.shared.max_queue.load(Ordering::Relaxed))
            .finish()
    }
}

impl VideoPassthrough {
    /// Register the passthrough codec and devices with PJSUA.
    ///
    /// Call once per [`PjsuaApp`], from a thread registered with PJLIB. The
    /// registration ends with `pjsua_destroy`.
    pub fn install(
        _app: &PjsuaApp,
        config: PassthroughConfig,
        handler: Arc<dyn VideoHandler>,
    ) -> Result<(Self, VideoDevices)> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                frames: VecDeque::new(),
                next_id: 1,
                need_keyframe: false,
            }),
            max_queue: AtomicUsize::new(config.max_queue.max(1)),
            max_frame_size: config.max_frame_size,
            handler,
        });

        let cfg = c_config(&config);
        let cb = pjrs_vpt_callbacks {
            user_data: std::ptr::null_mut(),
            capture_peek: Some(capture_peek),
            capture_consumed: Some(capture_consumed),
            keyframe_request: Some(keyframe_request),
            encoder_open: Some(encoder_open),
            encoder_close: Some(encoder_close),
            render_push: Some(render_push),
        };
        let mut devs = pjrs_vpt_devices::default();

        *SHARED.write().unwrap_or_else(|e| e.into_inner()) = Some(shared.clone());
        let status = unsafe { pjrs_vpt_init(&cfg, &cb, &mut devs) };
        if let Err(e) = check_status(status) {
            *SHARED.write().unwrap_or_else(|e| e.into_inner()) = None;
            return Err(e);
        }

        debug!(
            "video passthrough installed: capture={} render={}",
            devs.capture, devs.render
        );
        Ok((
            VideoPassthrough { shared },
            VideoDevices {
                capture: devs.capture,
                render: devs.render,
            },
        ))
    }

    /// Queue one encoded access unit (Annex-B) for sending.
    ///
    /// Returns `false` if it was dropped: empty or too large, waiting for a
    /// keyframe after a flush, or the queue overflowed with no keyframe to
    /// keep (which flushes it and requests one).
    pub fn push_frame(&self, frame: &[u8], keyframe: bool) -> bool {
        if frame.is_empty() || frame.len() > self.shared.max_frame_size {
            return false;
        }
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.need_keyframe {
            if !keyframe {
                return false;
            }
            q.need_keyframe = false;
        }
        if q.frames.len() >= self.shared.max_queue.load(Ordering::Relaxed) {
            // Dropping one frame would break every frame after it until the
            // next keyframe. If a keyframe is waiting, what came before it is
            // all that need go: the remote can start again from it.
            let newest_key = q.frames.iter().rposition(|f| f.keyframe);
            match newest_key {
                Some(i) if i > 0 && !keyframe => {
                    q.frames.drain(..i);
                    debug!(
                        "video passthrough queue overflow; dropped {i} frames before a keyframe"
                    );
                }
                // Otherwise drop them all and ask for a keyframe now; one
                // arriving here starts the queue again itself.
                _ => {
                    q.frames.clear();
                    q.need_keyframe = !keyframe;
                    if !keyframe {
                        drop(q);
                        warn!("video passthrough queue overflow; waiting for a keyframe");
                        self.shared.handler.on_keyframe_request();
                        return false;
                    }
                }
            }
        }
        let id = q.next_id;
        q.next_id = q.next_id.wrapping_add(1).max(1);
        q.frames.push_back(Queued {
            id,
            data: frame.to_vec(),
            keyframe,
        });
        true
    }

    /// Change the source's parameters for the next offer or answer; a stream
    /// already sending keeps the ones it opened with. `max_frame_size` and
    /// `payload_type` must stay as installed.
    ///
    /// Call from a thread registered with PJLIB.
    pub fn reconfigure(&self, config: &PassthroughConfig) -> Result<()> {
        let cfg = c_config(config);
        check_status(unsafe { pjrs_vpt_reconfigure(&cfg) })?;
        self.shared
            .max_queue
            .store(config.max_queue.max(1), Ordering::Relaxed);
        debug!(
            "video passthrough reconfigured: {}x{} @{} fps, {}/{} bps",
            config.width, config.height, config.fps, config.avg_bps, config.max_bps
        );
        Ok(())
    }

    /// Discard queued access units, when the source restarts or before a
    /// new call: nothing more is sent until the source's next keyframe.
    ///
    /// Without this, a stream PJSIP reopens mid-call (a re-INVITE that
    /// changes the media) carries on without waiting for a keyframe.
    pub fn clear(&self) {
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.frames.clear();
        q.need_keyframe = false;
        unsafe { pjrs_vpt_source_restarted() };
    }

    /// Start or stop sending video on `call`, without a re-INVITE.
    ///
    /// Starting attaches the capture device if the stream has none. PJSUA
    /// detaches it on local hold and, if the video stream is unchanged when
    /// the hold ends, leaves it detached, so nothing is sent until this is
    /// called.
    ///
    /// Call from a PJLIB-registered thread, outside PJSUA callbacks.
    pub fn set_transmit(&self, call: CallId, enable: bool) -> Result<()> {
        check_status(unsafe { pjrs_vid_set_transmit(call.0, c_int::from(enable)) })
    }

    /// Number of access units waiting to be sent.
    pub fn queued(&self) -> usize {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frames
            .len()
    }
}

fn c_config(config: &PassthroughConfig) -> pjrs_vpt_config {
    pjrs_vpt_config {
        width: config.width,
        height: config.height,
        fps: config.fps,
        poll_fps: config.poll_fps.unwrap_or(config.fps * 2),
        max_frame_size: config.max_frame_size as c_uint,
        avg_bps: config.avg_bps,
        max_bps: config.max_bps,
        pt: config.payload_type as c_uint,
        packetization_mode: c_uint::from(config.packetization_mode),
        profile_idc: config.profile.profile_idc,
        profile_iop: config.profile.profile_iop,
        level_idc: config.profile.level_idc,
        max_fs: config.receive.max_fs.unwrap_or(0),
        max_mbps: config.receive.max_mbps.unwrap_or(0),
        max_br: config.receive.max_br_kbps.unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Callbacks from C. None of these may unwind into PJSIP.
// ---------------------------------------------------------------------------

unsafe extern "C" fn capture_peek(
    _user: *mut c_void,
    buf: *mut u8,
    buf_size: usize,
    keyframe: *mut c_int,
    id: *mut u32,
) -> usize {
    std::panic::catch_unwind(|| {
        let Some(shared) = shared() else { return 0 };
        let q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        let Some(front) = q.frames.front() else {
            return 0;
        };
        if front.data.len() > buf_size {
            return 0;
        }
        // SAFETY: C passes a buffer of buf_size bytes and valid keyframe and
        // id pointers, all live for this call.
        unsafe {
            std::ptr::copy_nonoverlapping(front.data.as_ptr(), buf, front.data.len());
            *keyframe = c_int::from(front.keyframe);
            *id = front.id;
        }
        front.data.len()
    })
    .unwrap_or(0)
}

unsafe extern "C" fn capture_consumed(_user: *mut c_void, id: u32) {
    let _ = std::panic::catch_unwind(|| {
        let Some(shared) = shared() else { return };
        shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .consume_through(id);
    });
}

unsafe extern "C" fn keyframe_request(_user: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if let Some(shared) = shared() {
            shared.handler.on_keyframe_request();
        }
    });
}

unsafe extern "C" fn encoder_open(_user: *mut c_void, info: *const pjrs_vpt_stream_info) {
    if info.is_null() {
        return;
    }
    // SAFETY: non-null, valid for this call.
    let info = unsafe { &*info };
    let info = VideoStreamInfo {
        width: info.width,
        height: info.height,
        fps_num: info.fps_num,
        fps_denum: info.fps_denum,
        avg_bps: info.avg_bps,
        max_bps: info.max_bps,
    };
    let _ = std::panic::catch_unwind(|| {
        if let Some(shared) = shared() {
            shared.handler.on_encoder_open(info);
        }
    });
}

unsafe extern "C" fn encoder_close(_user: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if let Some(shared) = shared() {
            shared.handler.on_encoder_close();
        }
    });
}

unsafe extern "C" fn render_push(_user: *mut c_void, buf: *const u8, len: usize, keyframe: c_int) {
    if buf.is_null() || len == 0 {
        return;
    }
    // SAFETY: C passes len readable bytes, valid for this call.
    let frame = unsafe { std::slice::from_raw_parts(buf, len) };
    let _ = std::panic::catch_unwind(|| {
        if let Some(shared) = shared() {
            shared.handler.on_frame_received(frame, keyframe != 0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Counter(std::sync::atomic::AtomicUsize);

    impl VideoHandler for Counter {
        fn on_keyframe_request(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn passthrough(max_queue: usize) -> (VideoPassthrough, Arc<Counter>) {
        let counter = Arc::new(Counter::default());
        let vp = VideoPassthrough {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue {
                    frames: VecDeque::new(),
                    next_id: 1,
                    need_keyframe: false,
                }),
                max_queue: AtomicUsize::new(max_queue),
                max_frame_size: 16,
                handler: counter.clone(),
            }),
        };
        (vp, counter)
    }

    fn ids(vp: &VideoPassthrough) -> Vec<u32> {
        vp.shared
            .queue
            .lock()
            .unwrap()
            .frames
            .iter()
            .map(|f| f.id)
            .collect()
    }

    #[test]
    fn rejects_empty_and_oversized_frames() {
        let (vp, _) = passthrough(4);
        assert!(!vp.push_frame(&[], true));
        assert!(!vp.push_frame(&[1; 17], true));
        assert!(vp.push_frame(&[1; 16], true));
    }

    #[test]
    fn overflow_flushes_and_waits_for_keyframe() {
        let (vp, counter) = passthrough(2);
        assert!(vp.push_frame(b"k", true));
        assert!(vp.push_frame(b"p", false));
        // Full: a P frame flushes everything and asks for a keyframe.
        assert!(!vp.push_frame(b"p", false));
        assert_eq!(vp.queued(), 0);
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Until it comes, P frames are useless to the remote.
        assert!(!vp.push_frame(b"p", false));
        assert!(vp.push_frame(b"k", true));
        assert!(vp.push_frame(b"p", false));
        assert_eq!(vp.queued(), 2);
    }

    #[test]
    fn overflow_keeps_a_queued_keyframe() {
        let (vp, counter) = passthrough(4);
        for (f, k) in [(b"k", true), (b"p", false), (b"k", true), (b"p", false)] {
            assert!(vp.push_frame(f, k));
        }
        // Full: only what precedes the newest keyframe goes.
        assert!(vp.push_frame(b"p", false));
        assert_eq!(ids(&vp), vec![3, 4, 5]);
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn overflow_on_keyframe_keeps_it() {
        let (vp, counter) = passthrough(1);
        assert!(vp.push_frame(b"p", false));
        assert!(vp.push_frame(b"k", true));
        assert_eq!(vp.queued(), 1);
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(vp.push_frame(b"k", true));
    }

    #[test]
    fn consume_through_drops_up_to_id() {
        let (vp, _) = passthrough(8);
        for _ in 0..4 {
            vp.push_frame(b"x", true);
        }
        assert_eq!(ids(&vp), [1, 2, 3, 4]);
        vp.shared.queue.lock().unwrap().consume_through(2);
        assert_eq!(ids(&vp), [3, 4]);
        // A stale id consumes nothing.
        vp.shared.queue.lock().unwrap().consume_through(1);
        assert_eq!(ids(&vp), [3, 4]);
    }

    #[test]
    fn ids_wrap_and_skip_zero() {
        let (vp, _) = passthrough(8);
        vp.shared.queue.lock().unwrap().next_id = u32::MAX;
        vp.push_frame(b"a", true);
        vp.push_frame(b"b", true);
        assert_eq!(ids(&vp), [u32::MAX, 1]);
        vp.shared.queue.lock().unwrap().consume_through(u32::MAX);
        assert_eq!(ids(&vp), [1]);
        vp.shared.queue.lock().unwrap().consume_through(1);
        assert!(ids(&vp).is_empty());
    }
}
