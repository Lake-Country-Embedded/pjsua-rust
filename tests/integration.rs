#![cfg(feature = "integration-tests")]

use pjsua_rust::config::Config;
use pjsua_rust::*;
use std::time::Duration;

fn load_test_config() -> Config {
    Config::load("config/test-accounts.toml").expect("Missing config/test-accounts.toml")
}

#[tokio::test]
async fn test_registration() {
    let config = load_test_config();
    let pj_cfg = config.to_pjsua_config();

    let (app, mut rx) = PjsuaApp::new(pj_cfg).expect("Failed to init PJSUA");

    // Create UDP transport
    app.create_transport(TransportType::Udp, None, 0, None, None)
        .expect("Failed to create transport");

    // Register the caller account
    let caller = config.find_account("caller").expect("No caller account");
    let caller_cfg = caller.to_account_config();
    let _caller_id = app.add_account(&caller_cfg).expect("Failed to add caller");

    // Wait for registration event
    let timeout = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if let SipEvent::RegistrationState { is_registered, .. } = &event {
                if *is_registered {
                    return true;
                }
            }
        }
        false
    })
    .await;

    assert!(
        timeout.unwrap_or(false),
        "Registration did not succeed within timeout"
    );

    drop(app);
}

#[tokio::test]
async fn test_basic_call() {
    let config = load_test_config();
    let pj_cfg = config.to_pjsua_config();

    let (app, mut rx) = PjsuaApp::new(pj_cfg).expect("Failed to init PJSUA");

    app.create_transport(TransportType::Udp, None, 0, None, None)
        .expect("Failed to create transport");

    // Register both accounts
    let caller = config.find_account("caller").expect("No caller account");
    let receiver = config
        .find_account("receiver")
        .expect("No receiver account");
    let caller_cfg = caller.to_account_config();
    let receiver_cfg = receiver.to_account_config();
    let caller_id = app.add_account(&caller_cfg).expect("Failed to add caller");
    let _receiver_id = app
        .add_account(&receiver_cfg)
        .expect("Failed to add receiver");

    // Wait for both registrations
    let mut registrations = 0u32;
    let reg_result = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = rx.recv().await {
            if let SipEvent::RegistrationState { is_registered, .. } = &event {
                if *is_registered {
                    registrations += 1;
                    if registrations >= 2 {
                        return true;
                    }
                }
            }
        }
        false
    })
    .await;
    assert!(
        reg_result.unwrap_or(false),
        "Both registrations did not succeed"
    );

    // Make call from caller to receiver via PBX
    let dest_uri = format!("sip:{}@{}", receiver_cfg.username, receiver_cfg.server);
    let _call_id = app
        .make_call(caller_id, &dest_uri)
        .expect("Failed to make call");

    // Process events: auto-answer incoming, verify Confirmed, then hangup
    let call_result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut confirmed = false;
        while let Some(event) = rx.recv().await {
            match &event {
                SipEvent::IncomingCall { call_id, .. } => {
                    app.answer_call(*call_id, 200).ok();
                }
                SipEvent::CallState { state, .. } => {
                    if *state == CallState::Confirmed {
                        confirmed = true;
                        // Send DTMF, wait briefly, then hangup
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        app.hangup_all().ok();
                    }
                    if *state == CallState::Disconnected && confirmed {
                        return true;
                    }
                }
                SipEvent::CallMediaState {
                    media_status,
                    conf_port,
                    ..
                } => {
                    if *media_status == MediaStatus::Active {
                        if let Some(port) = conf_port {
                            app.conf_connect(*port, ConfPort::MASTER).ok();
                            app.conf_connect(ConfPort::MASTER, *port).ok();
                        }
                    }
                }
                _ => {}
            }
        }
        false
    })
    .await;

    assert!(
        call_result.unwrap_or(false),
        "Call did not complete successfully"
    );

    drop(app);
}

// ---------------------------------------------------------------------------
// RingCentral integration tests
// ---------------------------------------------------------------------------
//
// These tests register two real RingCentral accounts in a single PjsuaApp and
// drive a caller → callee call through RC's SBC. They reproduce the exact
// failure path that downstream consumers hit in production (PJSIP auto-cancels
// on PJMEDIA_SDPNEG_ENOMEDIA when RC's 183 SDP advertises `m=audio 0` /
// `a=inactive`) without any target redeploy.
//
// Credentials live in `<repo>/testing/config/environment.json` under
// `sip_accounts.ringcentral` (gitignored; see environment.json.template for
// the schema). When the block is missing or `enabled=false`, every test in
// this section skips with a log line — so the integration suite stays usable
// for developers who only have the local-Asterisk creds.
//
// Run:
//   cargo test --features integration-tests --test integration \
//     test_ringcentral -- --test-threads=1 --nocapture

mod ringcentral_env {
    use pjsua_rust::{AccountConfig, SrtpMode, TransportType};
    use serde_json::Value;
    use std::path::PathBuf;

    pub struct AccountSpec {
        /// Ready-to-add `pjsua_rust::AccountConfig`. `transport_id` is left
        /// `None`; the test fills it after creating the TLS transport.
        pub config: AccountConfig,
        /// E.164 DID — exposed so a test could dial the callee directly even
        /// when `test_dial_target` points elsewhere.
        #[allow(dead_code)]
        pub did: String,
    }

    pub struct RingCentralEnv {
        pub caller: AccountSpec,
        pub callee: AccountSpec,
        /// What `caller` should dial to reach `callee` through the PSTN/SBC.
        /// Usually equal to `callee.did`, but kept separate so tests can
        /// route through a different number when RC's plan requires it.
        pub test_dial_target: String,
    }

    pub fn load() -> Option<RingCentralEnv> {
        let path = match std::env::var("VK_TESTING_ENV_JSON") {
            Ok(p) => PathBuf::from(p),
            Err(_) => find_default_env_json()?,
        };
        let raw = std::fs::read_to_string(&path).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        let rc = v.get("sip_accounts")?.get("ringcentral")?;
        if !rc
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
        {
            return None;
        }

        let server = rc.get("server")?.as_str()?.to_string();
        let port = rc.get("port").and_then(|x| x.as_u64()).unwrap_or(5060) as u16;
        let transport = match rc.get("transport").and_then(|x| x.as_str()).unwrap_or("tls") {
            "udp" => TransportType::Udp,
            "tcp" => TransportType::Tcp,
            "tls" => TransportType::Tls,
            other => {
                eprintln!("ringcentral: unknown transport {other:?}, defaulting to tls");
                TransportType::Tls
            }
        };
        let use_sips = rc
            .get("use_sips")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let reg_timeout = rc
            .get("reg_timeout_sec")
            .and_then(|x| x.as_u64())
            .map(|v| v as u32);
        let outbound_proxy = rc
            .get("outbound_proxy")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty() && !s.starts_with('<'))
            .map(str::to_string);

        let accounts = rc.get("accounts")?;
        let caller = parse_account(
            accounts.get("caller")?,
            "rc-caller",
            &server,
            port,
            transport,
            use_sips,
            reg_timeout,
            outbound_proxy.as_deref(),
        )?;
        let callee = parse_account(
            accounts.get("callee")?,
            "rc-callee",
            &server,
            port,
            transport,
            use_sips,
            reg_timeout,
            outbound_proxy.as_deref(),
        )?;
        let test_dial_target = rc
            .get("test_dial_target")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| callee.did.clone());

        Some(RingCentralEnv {
            caller,
            callee,
            test_dial_target,
        })
    }

    fn parse_account(
        node: &Value,
        name: &str,
        server: &str,
        port: u16,
        transport: TransportType,
        use_sips: bool,
        reg_timeout: Option<u32>,
        outbound_proxy: Option<&str>,
    ) -> Option<AccountSpec> {
        let username = node.get("username")?.as_str()?.to_string();
        let password = node.get("password")?.as_str()?.to_string();
        let auth_username = node
            .get("auth_username")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty() && !s.starts_with('<'))
            .map(str::to_string);
        let did = node
            .get("did")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_default();

        Some(AccountSpec {
            config: AccountConfig {
                name: name.to_string(),
                username,
                password,
                server: server.to_string(),
                port,
                transport,
                srtp: SrtpMode::Disabled,
                use_sips,
                auth_username,
                reg_timeout,
                proxy: outbound_proxy.map(str::to_string),
                ..Default::default()
            },
            did,
        })
    }

    fn find_default_env_json() -> Option<PathBuf> {
        // Try CARGO_MANIFEST_DIR first (works when pjsua-rust lives inside
        // the monorepo workspace, e.g. devtool's
        // `build/vkz-01/build/workspace/sources/pjsua-rust`). Then fall
        // back to walking up from the current working directory — this
        // covers the case where pjsua-rust is checked out as a sibling of
        // the monorepo (e.g. `~/projects/pjsua-rust` next to
        // `~/projects/z-series-monorepo`) and `cargo test` is run from
        // the workspace mount with cwd inside the monorepo. If neither
        // works, set VK_TESTING_ENV_JSON to point directly at the file.
        let mut roots: Vec<PathBuf> = Vec::new();
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }

        for start in &roots {
            let mut dir = start.clone();
            loop {
                let candidate = dir.join("testing/config/environment.json");
                if candidate.exists() {
                    return Some(candidate);
                }
                if !dir.pop() {
                    break;
                }
            }
        }

        eprintln!(
            "ringcentral: no testing/config/environment.json found by walking up from {} \
             (set VK_TESTING_ENV_JSON to override)",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        );
        None
    }
}

/// Skip the surrounding `#[tokio::test]` if RingCentral env config isn't
/// present or has `enabled=false`. Returns the loaded `RingCentralEnv`.
macro_rules! rc_env_or_skip {
    () => {
        match crate::ringcentral_env::load() {
            Some(env) => env,
            None => {
                eprintln!(
                    "[skip] no RingCentral env config — \
                     set sip_accounts.ringcentral.enabled=true in \
                     <repo>/testing/config/environment.json (or point \
                     VK_TESTING_ENV_JSON at the file) to enable these tests"
                );
                return;
            }
        }
    };
}

/// Common setup: build PjsuaApp with TLS transport, add both RC accounts
/// (pinned to the TLS transport so PJSUA doesn't fail auto-resolution per the
/// AccountConfig::transport_id docs), wait for both REGISTERs to succeed.
async fn rc_setup_and_register(
    env: &ringcentral_env::RingCentralEnv,
) -> (
    PjsuaApp,
    tokio::sync::mpsc::UnboundedReceiver<SipEvent>,
    AccountId,
    AccountId,
) {
    let pj_cfg = PjsuaConfig {
        log_level: 4,
        clock_rate: 16000,
        null_audio: true,
        user_agent: Some("pjsua-rust-integration-test/1.0".into()),
        nameservers: Vec::new(),
    };
    let (app, mut rx) = PjsuaApp::new(pj_cfg).expect("PjsuaApp::new failed");

    // RC requires TLS. Bind to ephemeral port; verify_server=false so we
    // don't need a custom CA bundle — RC's cert chain validation isn't what
    // these tests are about.
    let tls_id = app
        .create_transport(
            TransportType::Tls,
            None,
            0,
            Some(&TlsConfig {
                verify_server: false,
                ..Default::default()
            }),
            None,
        )
        .expect("create TLS transport");

    let mut caller_cfg = env.caller.config.clone();
    let mut callee_cfg = env.callee.config.clone();
    caller_cfg.transport_id = Some(tls_id.0);
    callee_cfg.transport_id = Some(tls_id.0);

    // 100rel experiment notes (kept for reference, but left at the
    // PJSIP default `NotUsed`):
    //
    // * `Optional` — pjsua's UAC code already always advertises
    //   `Supported: 100rel`; this option only changes UAS behavior.
    //   Verified: RC ignores the `Supported` header and still sends
    //   unreliable `183 ... m=audio 0 a=inactive`.
    // * `Mandatory` — adds `Require: 100rel`. Verified: RC responds
    //   `500 Internal Server Error`. They don't speak PRACK.
    //
    // Conclusion: 100rel is not the lever. The actual workaround for
    // the unreliable-18x-with-inactive-SDP path lives elsewhere — see
    // `test_ringcentral_outbound_sip_trace` for the bug repro.

    let caller_id = app.add_account(&caller_cfg).expect("add caller");
    let callee_id = app.add_account(&callee_cfg).expect("add callee");

    // Wait for both 200 OK REGISTERs (or any failure).
    let mut caller_done = false;
    let mut callee_done = false;
    let reg = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = rx.recv().await {
            if let SipEvent::RegistrationState {
                account_id,
                is_registered,
                code,
                reason,
            } = &event
            {
                if *account_id == caller_id {
                    assert!(
                        *is_registered,
                        "caller registration failed: code={code} reason={reason}"
                    );
                    caller_done = true;
                }
                if *account_id == callee_id {
                    assert!(
                        *is_registered,
                        "callee registration failed: code={code} reason={reason}"
                    );
                    callee_done = true;
                }
                if caller_done && callee_done {
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert!(
        reg.unwrap_or(false),
        "REGISTER did not complete for both accounts within 30s"
    );

    (app, rx, caller_id, callee_id)
}

#[tokio::test]
async fn test_ringcentral_registration() {
    let env = rc_env_or_skip!();
    let (app, _rx, _caller, _callee) = rc_setup_and_register(&env).await;
    drop(app);
}

#[tokio::test]
async fn test_ringcentral_outbound_call_lifecycle() {
    let env = rc_env_or_skip!();
    let (app, mut rx, caller_id, _callee_id) = rc_setup_and_register(&env).await;

    // Plain `sip:` — let the AccountConfig.transport_id pin route via TLS.
    let dest_uri = format!("sip:{}@{}", env.test_dial_target, env.caller.config.server);
    let outbound_call = app
        .make_call(caller_id, &dest_uri)
        .expect("make_call failed");
    eprintln!("rc test: placed outbound call_id={outbound_call} -> {dest_uri}");

    // Drain events: auto-answer the incoming leg on the callee side, wait
    // for the outbound call to reach Confirmed, then hangup and require a
    // clean Disconnected for both ends.
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let mut confirmed_outbound = false;
        while let Some(event) = rx.recv().await {
            match &event {
                SipEvent::IncomingCall { call_id, .. } => {
                    eprintln!("rc test: incoming call (callee leg), auto-answering");
                    app.answer_call(*call_id, 200).ok();
                }
                SipEvent::CallState {
                    call_id,
                    state,
                    last_code,
                    last_reason,
                    ..
                } => {
                    eprintln!(
                        "rc test: CallState call_id={} state={:?} code={} reason={}",
                        call_id, state, last_code, last_reason
                    );
                    if *state == CallState::Confirmed && *call_id == outbound_call {
                        confirmed_outbound = true;
                        // Hold the call briefly, then tear down.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        app.hangup_all().ok();
                    }
                    if *state == CallState::Disconnected && *call_id == outbound_call {
                        return Ok::<_, String>(confirmed_outbound);
                    }
                }
                SipEvent::CallMediaState {
                    media_status,
                    conf_port,
                    ..
                } => {
                    if *media_status == MediaStatus::Active {
                        if let Some(port) = conf_port {
                            // Loop into master so the call is fully active.
                            app.conf_connect(*port, ConfPort::MASTER).ok();
                            app.conf_connect(ConfPort::MASTER, *port).ok();
                        }
                    }
                }
                _ => {}
            }
        }
        Err("event channel closed".into())
    })
    .await;

    drop(app);

    match outcome {
        Ok(Ok(true)) => { /* pass */ }
        Ok(Ok(false)) => panic!(
            "outbound call disconnected without ever reaching Confirmed — \
             see the CallState log lines above for the last code/reason \
             observed (e.g. 487 Request Terminated when PJSUA auto-cancels \
             on PJMEDIA_SDPNEG_ENOMEDIA after RC's port-0 / inactive 183 SDP)"
        ),
        Ok(Err(e)) => panic!("call lifecycle aborted: {e}"),
        Err(_) => panic!("timeout waiting for outbound call lifecycle"),
    }
}

#[tokio::test]
async fn test_ringcentral_outbound_sip_trace() {
    let env = rc_env_or_skip!();
    let (app, mut rx, caller_id, _callee_id) = rc_setup_and_register(&env).await;

    let dest_uri = format!("sip:{}@{}", env.test_dial_target, env.caller.config.server);
    let outbound_call = app
        .make_call(caller_id, &dest_uri)
        .expect("make_call failed");

    // The SipMessageTrace events route by SIP Call-ID (the C trace module
    // doesn't know the pjsua call_id). Pull our outbound call's SIP Call-ID
    // from CallInfo so we can filter.
    let outbound_sip_call_id = app
        .get_call_info(outbound_call)
        .expect("get_call_info")
        .sip_call_id;
    eprintln!("rc test: outbound SIP Call-ID = {outbound_sip_call_id:?}");

    // Collect (direction, method_or_status) pairs for our outbound call
    // until it disconnects (or 60s elapses).
    let collected = tokio::time::timeout(Duration::from_secs(60), async {
        let mut events: Vec<(TraceDirection, String)> = Vec::new();
        while let Some(event) = rx.recv().await {
            match &event {
                SipEvent::IncomingCall { call_id, .. } => {
                    app.answer_call(*call_id, 200).ok();
                }
                SipEvent::CallState {
                    call_id,
                    state,
                    last_code,
                    ..
                } => {
                    if *call_id == outbound_call {
                        if *state == CallState::Confirmed {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            app.hangup_all().ok();
                        }
                        if *state == CallState::Disconnected {
                            eprintln!(
                                "rc test: outbound disconnected, last_code={last_code}, \
                                 captured {} trace events",
                                events.len()
                            );
                            return events;
                        }
                    }
                }
                SipEvent::SipMessageTrace { info, .. } => {
                    if info.sip_call_id == outbound_sip_call_id {
                        events.push((info.direction, info.method_or_status.clone()));
                    }
                }
                SipEvent::CallMediaState {
                    media_status,
                    conf_port,
                    ..
                } => {
                    if *media_status == MediaStatus::Active {
                        if let Some(port) = conf_port {
                            app.conf_connect(*port, ConfPort::MASTER).ok();
                            app.conf_connect(ConfPort::MASTER, *port).ok();
                        }
                    }
                }
                _ => {}
            }
        }
        events
    })
    .await
    .expect("timeout while collecting SIP trace events");

    drop(app);

    eprintln!("rc test: full trace sequence:");
    for (i, (dir, mos)) in collected.iter().enumerate() {
        eprintln!("  [{i:02}] {dir:?} {mos}");
    }

    // Minimum expectations that hold for both the broken and fixed cases:
    // we always send INVITE and always receive 100 Trying. Stronger
    // assertions are split into "broken" and "fixed" branches below.
    assert!(
        collected
            .iter()
            .any(|(d, m)| *d == TraceDirection::Sent && m == "INVITE"),
        "expected to have sent at least one INVITE"
    );
    assert!(
        collected
            .iter()
            .any(|(d, m)| *d == TraceDirection::Received && m.starts_with("100 ")),
        "expected to receive 100 Trying"
    );

    // Did we ever send a CANCEL? That's the symptom of the
    // PJMEDIA_SDPNEG_ENOMEDIA auto-cancel on RC's port-0 183.
    let we_sent_cancel = collected
        .iter()
        .any(|(d, m)| *d == TraceDirection::Sent && m == "CANCEL");
    // Did we reach 200 OK on the INVITE?
    let we_got_200_invite = collected
        .iter()
        .any(|(d, m)| *d == TraceDirection::Received && m.starts_with("200 "));

    if we_sent_cancel {
        // Today's broken behaviour. Pin the exact shape so the fix can be
        // verified by inverting these assertions.
        eprintln!(
            "rc test: detected current-broken trace (CANCEL after 183). \
             This will become an inverted assertion once the ENOMEDIA fix lands."
        );
        assert!(
            collected
                .iter()
                .any(|(d, m)| *d == TraceDirection::Received && m.starts_with("183 ")),
            "broken path: expected 183 before our CANCEL"
        );
        assert!(
            collected
                .iter()
                .any(|(d, m)| *d == TraceDirection::Received && m.starts_with("487 ")),
            "broken path: expected 487 Request Terminated after CANCEL"
        );
        panic!(
            "BUG REPRODUCED: pjsua sent CANCEL after RC's 183 SDP \
             (PJMEDIA_SDPNEG_ENOMEDIA on m=audio 0 / a=inactive). \
             When the fix lands, this test should pass without entering this branch."
        );
    } else {
        // Fixed behaviour: the call goes 18x (with no auto-cancel) and then 200 OK.
        assert!(
            we_got_200_invite,
            "fixed path: expected 200 OK on INVITE (we didn't send CANCEL but never saw 200 either)"
        );
    }
}

#[tokio::test]
async fn test_ringcentral_outbound_audio_loop() {
    let env = rc_env_or_skip!();
    let (app, mut rx, caller_id, _callee_id) = rc_setup_and_register(&env).await;

    let dest_uri = format!("sip:{}@{}", env.test_dial_target, env.caller.config.server);
    let outbound_call = app
        .make_call(caller_id, &dest_uri)
        .expect("make_call failed");
    eprintln!("rc test: placed outbound call_id={outbound_call}");

    // Cross-connect both call legs through the conf bridge once both are
    // active, push a tone from the caller side, and assert the callee
    // sees non-zero RX signal level — proving RTP traversed RC's SBC and
    // came back to us on the second leg.
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let mut caller_port: Option<ConfPort> = None;
        let mut callee_port: Option<ConfPort> = None;
        let mut tonegen: Option<ToneGenerator> = None;
        let mut audio_observed = false;

        while let Some(event) = rx.recv().await {
            match &event {
                SipEvent::IncomingCall { call_id, .. } => {
                    app.answer_call(*call_id, 200).ok();
                }
                SipEvent::CallMediaState {
                    call_id,
                    media_status,
                    conf_port,
                    account_id: _,
                } => {
                    if *media_status == MediaStatus::Active {
                        if let Some(port) = conf_port {
                            if *call_id == outbound_call {
                                caller_port = Some(*port);
                            } else {
                                callee_port = Some(*port);
                            }
                        }
                    }
                    // Once both legs are active, wire them: caller TX → callee RX
                    // via tone generator → caller; check the callee's RX level.
                    if let (Some(c), Some(r)) = (caller_port, callee_port) {
                        if tonegen.is_none() {
                            let mut tg = ToneGenerator::new(&app, 16000, 1, 320)
                                .expect("ToneGenerator::new");
                            let tg_port = tg.add_to_conference(&app)
                                .expect("ToneGenerator add_to_conference");
                            // Route the tone into the outbound (caller) leg's TX.
                            app.conf_connect(tg_port, c).ok();
                            // Loop both legs to master so audio is "live".
                            app.conf_connect(c, ConfPort::MASTER).ok();
                            app.conf_connect(r, ConfPort::MASTER).ok();
                            // Play a 440 Hz tone for 4 seconds (one element).
                            tg.play_tones(&[ToneDesc {
                                freq1: 440,
                                freq2: 0,
                                on_ms: 4000,
                                off_ms: 0,
                                volume: 0,
                            }])
                            .ok();
                            tonegen = Some(tg);
                            eprintln!(
                                "rc test: tone playing into caller leg, sampling callee RX level"
                            );

                            // Sample the callee port RX level for ~5 seconds; pass on
                            // any sample reading > 0 (defensive: PJSIP returns level as
                            // 0..=255 EAR-weighted RMS and SBC routing introduces a
                            // small delay).
                            let deadline = tokio::time::Instant::now()
                                + Duration::from_secs(5);
                            while tokio::time::Instant::now() < deadline {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                if let Ok((_tx, rx_level)) = app.conf_get_signal_level(r) {
                                    if rx_level > 0 {
                                        eprintln!(
                                            "rc test: callee RX level={} (non-zero — audio observed)",
                                            rx_level
                                        );
                                        audio_observed = true;
                                        break;
                                    }
                                }
                            }
                            // Tear down: disconnect tone source, drop the
                            // ToneGenerator (Drop removes it from the conf
                            // bridge and releases the pool), then hangup.
                            app.conf_disconnect(tg_port, c).ok();
                            if let Some(mut tg) = tonegen.take() {
                                tg.stop().ok();
                                tg.remove_from_conference(&app).ok();
                                drop(tg);
                            }
                            app.hangup_all().ok();
                        }
                    }
                }
                SipEvent::CallState {
                    call_id, state, ..
                } => {
                    if *state == CallState::Disconnected && *call_id == outbound_call {
                        return audio_observed;
                    }
                }
                _ => {}
            }
        }
        audio_observed
    })
    .await;

    drop(app);

    let ok = outcome.unwrap_or(false);
    assert!(
        ok,
        "did not observe non-zero callee RX signal level within timeout — \
         either the outbound call never confirmed (see \
         test_ringcentral_outbound_call_lifecycle for the protocol-level \
         repro) or RTP didn't traverse RC's SBC"
    );
}
