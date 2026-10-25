#![cfg(feature = "video")]

//! H.264 passthrough loopback tests.
//!
//! One PJSUA instance calls its own UDP transport, so the same process is
//! both caller and callee. The caller sends access units through the
//! passthrough; the callee's render device hands them back.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pjsua_rust::*;
use tokio::sync::mpsc::UnboundedReceiver;

/// PJSUA is a process-wide singleton; run these one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `PJSIP_LOG=<level>` prints PJSIP's log to stderr.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("{} {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

fn pjsip_log_level() -> u32 {
    let level = std::env::var("PJSIP_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if level > 0 && log::set_logger(&StderrLog).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
    level
}

#[derive(Default)]
struct Recorder {
    received: Mutex<Vec<(Vec<u8>, bool)>>,
    keyframe_requests: AtomicUsize,
    opened: Mutex<Option<VideoStreamInfo>>,
}

impl VideoHandler for Recorder {
    fn on_keyframe_request(&self) {
        self.keyframe_requests.fetch_add(1, Ordering::SeqCst);
    }
    fn on_encoder_open(&self, info: VideoStreamInfo) {
        *self.opened.lock().unwrap() = Some(info);
    }
    fn on_frame_received(&self, frame: &[u8], keyframe: bool) {
        self.received
            .lock()
            .unwrap()
            .push((frame.to_vec(), keyframe));
    }
}

/// Bytes with no zero, so no accidental start code or emulation sequence.
fn payload(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x % 255) as u8 + 1
        })
        .collect()
}

fn nal(out: &mut Vec<u8>, header: u8, body: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1, header]);
    out.extend_from_slice(body);
}

/// An access unit: SPS + PPS + IDR for a keyframe, else one P slice. The
/// slices are larger than the MTU so they go out as FU-A fragments.
fn access_unit(n: u32, keyframe: bool) -> Vec<u8> {
    let mut au = Vec::new();
    if keyframe {
        nal(&mut au, 0x67, &[0x42, 0xe0, 0x1f, 0x8d, 0x68, 0x14, 0x1f]);
        nal(&mut au, 0x68, &[0xce, 0x3c, 0x80]);
        nal(&mut au, 0x65, &payload(n, 9000));
    } else {
        nal(&mut au, 0x41, &payload(n, 2500 + (n as usize % 7) * 300));
    }
    au
}

struct Loopback {
    app: PjsuaApp,
    rx: UnboundedReceiver<SipEvent>,
    account: AccountId,
    uri: String,
    video: VideoPassthrough,
    recorder: Arc<Recorder>,
}

fn setup(port: u16) -> Loopback {
    setup_with(port, PassthroughConfig::default())
}

fn setup_with(port: u16, config: PassthroughConfig) -> Loopback {
    let (app, rx) = PjsuaApp::new(PjsuaConfig {
        log_level: pjsip_log_level(),
        clock_rate: 16000,
        null_audio: true,
        user_agent: None,
        nameservers: Vec::new(),
    })
    .expect("init");

    let recorder = Arc::new(Recorder::default());
    let (video, devices) =
        VideoPassthrough::install(&app, config, recorder.clone()).expect("install passthrough");

    app.create_transport(TransportType::Udp, Some("127.0.0.1"), port, None, None)
        .expect("transport");

    let account = app
        .add_account(&AccountConfig {
            name: "loopback".into(),
            username: "loopback".into(),
            server: "127.0.0.1".into(),
            port,
            video_devices: Some(devices),
            ..Default::default()
        })
        .expect("account");
    // Nothing to register with; keep REGISTER off the wire.
    let _ = app.set_registration(account, false);

    Loopback {
        app,
        rx,
        account,
        uri: format!("sip:callee@127.0.0.1:{port}"),
        video,
        recorder,
    }
}

/// Run the call until both legs are confirmed, answering the incoming leg
/// with `answer_video`. Returns the last video state seen per leg.
async fn connect(
    lb: &mut Loopback,
    offer: Option<VideoDirection>,
    answer_video: Option<VideoDirection>,
) -> (
    CallId,
    CallId,
    Option<VideoMediaState>,
    Option<VideoMediaState>,
) {
    let caller = lb
        .app
        .make_call_with_setting(
            lb.account,
            &lb.uri,
            &CallSetting {
                video: offer,
                ..Default::default()
            },
        )
        .expect("make_call");

    let mut callee = None;
    let mut confirmed = 0;
    let mut caller_video = None;
    let mut callee_video = None;

    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(ev) = lb.rx.recv().await {
            match ev {
                SipEvent::IncomingCall { call_id, .. } => {
                    callee = Some(call_id);
                    lb.app
                        .answer_call_with_video(call_id, 180, answer_video)
                        .expect("180");
                    lb.app
                        .answer_call_with_video(call_id, 200, answer_video)
                        .expect("200");
                }
                SipEvent::CallState {
                    state: CallState::Confirmed,
                    ..
                } => {
                    confirmed += 1;
                }
                SipEvent::CallVideoState { call_id, video, .. } => {
                    if call_id == caller {
                        caller_video = Some(video);
                    } else {
                        callee_video = Some(video);
                    }
                }
                _ => {}
            }
            if confirmed == 2 {
                // Media state follows CONFIRMED closely; collect it too.
                let drain = tokio::time::sleep(Duration::from_millis(300));
                tokio::pin!(drain);
                loop {
                    tokio::select! {
                        _ = &mut drain => break,
                        Some(ev) = lb.rx.recv() => {
                            if let SipEvent::CallVideoState { call_id, video, .. } = ev {
                                if call_id == caller {
                                    caller_video = Some(video);
                                } else {
                                    callee_video = Some(video);
                                }
                            }
                        }
                    }
                }
                break;
            }
        }
    })
    .await
    .expect("call did not connect");

    (
        caller,
        callee.expect("no incoming call"),
        caller_video,
        callee_video,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn sendonly_video_arrives_intact() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup(25_160);

    let (caller, _callee, caller_video, callee_video) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;

    assert_eq!(
        caller_video.and_then(|v| v.direction),
        Some(VideoDirection::SendOnly)
    );
    assert_eq!(
        callee_video.and_then(|v| v.direction),
        Some(VideoDirection::RecvOnly)
    );
    let opened = lb
        .recorder
        .opened
        .lock()
        .unwrap()
        .expect("encoder not opened");
    assert_eq!((opened.width, opened.height), (640, 480));
    assert!(
        lb.recorder.keyframe_requests.load(Ordering::SeqCst) >= 1,
        "opening the encoder should ask for a keyframe"
    );

    // A P frame first: undecodable for a new receiver, so never sent.
    lb.video.push_frame(&access_unit(0, false), false);

    let mut sent = Vec::new();
    let mut depth = Vec::new();
    for n in 1..=45u32 {
        let key = n % 15 == 1;
        let au = access_unit(n, key);
        assert!(lb.video.push_frame(&au, key));
        sent.push(au);
        tokio::time::sleep(Duration::from_millis(66)).await;
        depth.push(lb.video.queued());
    }
    // Once the stream runs, each frame is taken within a tick.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        depth[10..].iter().all(|&d| d <= 1),
        "send queue backs up in steady state: {depth:?}"
    );
    assert_eq!(lb.video.queued(), 0, "send queue did not drain");
    // The receiving jitter buffer holds the newest few frames until more
    // arrive; push filler to flush the ones under test.
    for n in 100..120u32 {
        lb.video.push_frame(&access_unit(n, false), false);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let received: Vec<(Vec<u8>, bool)> = lb
        .recorder
        .received
        .lock()
        .unwrap()
        .iter()
        .filter(|(f, _)| sent.contains(f))
        .cloned()
        .collect();
    assert!(!received.is_empty(), "no video received");
    assert!(received[0].1, "first received frame is not a keyframe");

    // Every received frame is one we sent, in order, byte for byte.
    let mut next = 0;
    for (frame, key) in &received {
        let pos = sent[next..]
            .iter()
            .position(|s| s == frame)
            .unwrap_or_else(|| {
                panic!("received a frame that was not sent ({} bytes)", frame.len())
            });
        next += pos + 1;
        assert_eq!(*key, (next - 1) % 15 == 0, "keyframe flag mismatch");
    }
    assert_eq!(received.len(), sent.len(), "frames were lost");

    lb.app.hangup_call(caller, 200).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_only_by_default() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup(25_162);

    // Plain call and plain answer: no video m-line at all.
    let (caller, _, caller_video, callee_video) = connect(&mut lb, None, None).await;
    assert_eq!(caller_video, None);
    assert_eq!(callee_video, None);
    lb.app.hangup_call(caller, 200).ok();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Video offered, answered audio only: the stream is rejected.
    let (caller, _, caller_video, callee_video) =
        connect(&mut lb, Some(VideoDirection::SendOnly), None).await;
    assert_eq!(
        caller_video.map(|v| (v.status, v.direction)),
        Some((MediaStatus::None, None))
    );
    assert_eq!(
        callee_video.map(|v| (v.status, v.direction)),
        Some((MediaStatus::None, None))
    );
    assert!(lb.recorder.opened.lock().unwrap().is_none());
    lb.app.hangup_call(caller, 200).ok();
}

/// A slice-limited source, as an encoder capped to the MTU produces.
fn small_access_unit(n: u32, keyframe: bool) -> Vec<u8> {
    let mut au = Vec::new();
    if keyframe {
        nal(&mut au, 0x67, &[0x42, 0x80, 0x1f, 0x8d, 0x68, 0x14, 0x1f]);
        nal(&mut au, 0x68, &[0xce, 0x3c, 0x80]);
        for i in 0..4 {
            nal(&mut au, 0x65, &payload(n * 10 + i, 900));
        }
    } else {
        for i in 0..2 {
            nal(&mut au, 0x41, &payload(n * 10 + i, 700));
        }
    }
    au
}

/// packetization-mode=0: NAL units within the MTU arrive intact; an access
/// unit with one over it is dropped, with a keyframe request.
#[tokio::test(flavor = "multi_thread")]
async fn single_nal_mode() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup_with(
        25_164,
        PassthroughConfig {
            packetization_mode: 0,
            ..PassthroughConfig::default()
        },
    );
    let (caller, _, caller_video, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    assert_eq!(
        caller_video.and_then(|v| v.direction),
        Some(VideoDirection::SendOnly)
    );

    let mut sent = Vec::new();
    for n in 1..=30u32 {
        let key = n % 15 == 1;
        let au = small_access_unit(n, key);
        lb.video.push_frame(&au, key);
        sent.push(au);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    let before = lb.recorder.keyframe_requests.load(Ordering::SeqCst);
    // A keyframe whose last slice is over the MTU: none of it may go out.
    let mut oversized = small_access_unit(99, true);
    nal(&mut oversized, 0x65, &payload(990, 3000));
    let partial = small_access_unit(99, true);
    lb.video.push_frame(&oversized, true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        lb.recorder.keyframe_requests.load(Ordering::SeqCst) > before,
        "an unsendable frame should ask for a keyframe"
    );
    // After a drop nothing is sent until a keyframe, so the source's next
    // one restarts the stream (and flushes the receiver's jitter buffer).
    for n in 100..115u32 {
        let key = n == 100;
        lb.video.push_frame(&small_access_unit(n, key), key);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let received = lb.recorder.received.lock().unwrap().clone();
    let got = received.iter().filter(|(f, _)| sent.contains(f)).count();
    assert_eq!(got, sent.len(), "frames were lost in single-NAL mode");
    assert!(
        !received
            .iter()
            .any(|(f, _)| f == &partial || f == &oversized),
        "part of an unsendable access unit was sent"
    );
    lb.app.hangup_call(caller, 200).ok();
}

/// Reconfiguring takes effect from the next call, with no reinstall: size,
/// bitrate and packetization-mode all change.
#[tokio::test(flavor = "multi_thread")]
async fn reconfigure_applies_to_the_next_call() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup(25_166);

    let (caller, _, _, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    let first = lb
        .recorder
        .opened
        .lock()
        .unwrap()
        .take()
        .expect("first stream");
    assert_eq!((first.width, first.height), (640, 480));
    lb.app.hangup_call(caller, 200).ok();
    tokio::time::sleep(Duration::from_millis(500)).await;

    lb.video
        .reconfigure(&PassthroughConfig {
            width: 320,
            height: 240,
            fps: 10,
            avg_bps: 300_000,
            max_bps: 450_000,
            packetization_mode: 0,
            ..PassthroughConfig::default()
        })
        .expect("reconfigure");
    // What stays as installed cannot change.
    assert!(lb
        .video
        .reconfigure(&PassthroughConfig {
            payload_type: 98,
            ..PassthroughConfig::default()
        })
        .is_err());

    let (caller, _, caller_video, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    assert_eq!(
        caller_video.and_then(|v| v.direction),
        Some(VideoDirection::SendOnly)
    );
    let second = lb.recorder.opened.lock().unwrap().expect("second stream");
    assert_eq!((second.width, second.height), (320, 240));
    assert_eq!((second.avg_bps, second.max_bps), (300_000, 450_000));

    // Mode 0 now: a NAL unit over the MTU cannot be sent, the small ones can.
    lb.video.push_frame(&small_access_unit(1, true), true);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let before = lb.recorder.keyframe_requests.load(Ordering::SeqCst);
    let mut oversized = small_access_unit(2, true);
    nal(&mut oversized, 0x65, &payload(20, 3000));
    lb.video.push_frame(&oversized, true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        lb.recorder.keyframe_requests.load(Ordering::SeqCst) > before,
        "packetization-mode 0 did not apply"
    );
    for n in 3..20u32 {
        let key = n == 3;
        lb.video.push_frame(&small_access_unit(n, key), key);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let received = lb.recorder.received.lock().unwrap().clone();
    assert!(
        received
            .iter()
            .any(|(f, _)| f == &small_access_unit(3, true)),
        "no video after reconfiguring"
    );
    assert!(!received.iter().any(|(f, _)| f == &oversized));
    lb.app.hangup_call(caller, 200).ok();
}

/// The sender learns what the receiver can take: its level (with the
/// level's limits folded in), explicit fmtp limits and its b=TIAS.
#[tokio::test(flavor = "multi_thread")]
async fn remote_receive_caps_are_reported() {
    let _guard = SERIAL.lock().await;
    // Both legs share one passthrough, so the callee advertises this.
    let mut lb = setup_with(
        25_168,
        PassthroughConfig {
            profile: H264Profile::constrained_baseline(21),
            receive: H264ReceiveLimits {
                max_fs: Some(1000),
                ..H264ReceiveLimits::default()
            },
            avg_bps: 400_000,
            max_bps: 600_000,
            ..PassthroughConfig::default()
        },
    );
    let (caller, _, caller_video, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    let remote = caller_video
        .and_then(|v| v.remote)
        .expect("sender has the remote's caps");
    assert_eq!(remote.profile_level_id, Some((0x42, 0xe0, 21)));
    assert_eq!(remote.packetization_mode, 1);
    // max-fs above the level's 792 raises it; the rest are level 2.1's.
    assert_eq!(remote.max_fs, 1000);
    assert_eq!(remote.max_mbps, 19_800);
    assert_eq!(remote.max_br_kbps, 4_000);
    assert_eq!(remote.tias_bps, Some(600_000));
    assert_eq!(remote.as_kbps, None);
    lb.app.hangup_call(caller, 200).ok();
}

/// A re-INVITE with new receive limits reaches the sender as a new video
/// state, so it can change what it sends mid-call.
#[tokio::test(flavor = "multi_thread")]
async fn reinvite_reports_the_new_remote_caps() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup(25_170);
    let (caller, callee, caller_video, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    let before = caller_video.and_then(|v| v.remote).expect("remote caps");
    assert_eq!(before.tias_bps, Some(PassthroughConfig::default().max_bps));

    lb.video
        .reconfigure(&PassthroughConfig {
            profile: H264Profile::constrained_baseline(21),
            avg_bps: 300_000,
            max_bps: 400_000,
            ..PassthroughConfig::default()
        })
        .expect("reconfigure");
    lb.app.reinvite_call(callee).expect("reinvite");

    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match lb.rx.recv().await {
                Some(SipEvent::CallVideoState { call_id, video, .. }) if call_id == caller => {
                    if let Some(r) = video.remote.filter(|r| r.tias_bps != before.tias_bps) {
                        return r;
                    }
                }
                Some(_) => {}
                None => panic!("event channel closed"),
            }
        }
    })
    .await
    .expect("no video state with the new caps");
    assert_eq!(after.profile_level_id, Some((0x42, 0xe0, 21)));
    assert_eq!(after.max_fs, 792);
    assert_eq!(after.tias_bps, Some(400_000));
    lb.app.hangup_call(caller, 200).ok();
}

/// A stream PJSIP reopens mid-call carries on from the source's last
/// keyframe, since the remote still has it; after `clear()`, the source has
/// restarted and nothing goes until its next keyframe.
#[tokio::test(flavor = "multi_thread")]
async fn reopened_stream_carries_on_until_the_source_restarts() {
    let _guard = SERIAL.lock().await;
    let mut lb = setup(25_172);
    let (caller, callee, _, _) = connect(
        &mut lb,
        Some(VideoDirection::SendOnly),
        Some(VideoDirection::RecvOnly),
    )
    .await;
    let send = |lb: &Loopback, n: u32, key: bool| lb.video.push_frame(&access_unit(n, key), key);
    let arrived = |lb: &Loopback, n: u32, key: bool| {
        let au = access_unit(n, key);
        lb.recorder
            .received
            .lock()
            .unwrap()
            .iter()
            .any(|(f, _)| f == &au)
    };
    // The receiving jitter buffer holds the newest few frames until more
    // arrive, so each check is on frames well before the last pushed.
    for n in 0..15u32 {
        send(&lb, n, n == 0);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(arrived(&lb, 5, false), "no video before the re-INVITE");

    // A new b=TIAS changes the media, so PJSIP reopens the sender's stream.
    lb.video
        .reconfigure(&PassthroughConfig {
            avg_bps: 300_000,
            max_bps: 400_000,
            ..PassthroughConfig::default()
        })
        .expect("reconfigure");
    let opens = || lb.recorder.opened.lock().unwrap().map(|i| i.max_bps);
    lb.app.reinvite_call(callee).expect("reinvite");
    tokio::time::timeout(Duration::from_secs(5), async {
        while opens() != Some(400_000) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the sender's stream was not reopened");
    tokio::time::sleep(Duration::from_millis(300)).await;
    for n in 15..35u32 {
        send(&lb, n, false);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        (15..25).all(|n| arrived(&lb, n, false)),
        "the reopened stream waited for a keyframe the remote did not need"
    );

    lb.video.clear();
    for n in 35..60u32 {
        send(&lb, n, n == 38);
        tokio::time::sleep(Duration::from_millis(66)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        (35..38).all(|n| !arrived(&lb, n, false)),
        "frames before the restarted source's keyframe were sent"
    );
    assert!(
        arrived(&lb, 38, true) && (39..45).all(|n| arrived(&lb, n, false)),
        "the restarted source's keyframe and what follows were not sent"
    );
    lb.app.hangup_call(caller, 200).ok();
}
