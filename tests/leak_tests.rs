#![cfg(feature = "integration-tests")]

//! Memory leak stress tests for PjsuaApp lifecycle.
//!
//! These tests exercise repeated init/destroy cycles to catch memory leaks
//! in the RAII singleton and FFI cleanup paths. Run under Valgrind for
//! full leak detection:
//!
//! ```bash
//! ./scripts/valgrind-test.sh
//! ```

use pjsua_rust::*;

fn default_config() -> PjsuaConfig {
    PjsuaConfig {
        log_level: 0, // suppress PJSIP output during stress tests
        clock_rate: 16000,
        null_audio: true,
        user_agent: None,
        nameservers: Vec::new(),
    }
}

/// Create and destroy PjsuaApp multiple times.
/// If pjsua_destroy() doesn't fully clean up, Valgrind will report
/// "definitely lost" or "possibly lost" bytes growing each iteration.
#[test]
fn test_repeated_init_destroy() {
    for i in 0..5 {
        let (app, _rx) = PjsuaApp::new(default_config())
            .unwrap_or_else(|e| panic!("Init failed on iteration {i}: {e}"));

        // Create a transport to exercise more allocation paths
        app.create_transport(TransportType::Udp, None, 0, None, None)
            .unwrap_or_else(|e| panic!("Transport failed on iteration {i}: {e}"));

        drop(app);
    }
}

/// Add accounts repeatedly within a single PJSUA session.
/// Each add_account allocates PjStrings and FFI config structs.
/// Leaks in the conversion/allocation path will show up in Valgrind
/// as memory growing with each iteration.
#[test]
fn test_account_add_cycle() {
    let (app, _rx) = PjsuaApp::new(default_config()).expect("Failed to init PJSUA");

    app.create_transport(TransportType::Udp, None, 0, None, None)
        .expect("Failed to create transport");

    // PJSUA has a limited number of account slots (typically 8-32).
    // We add a few accounts with different configs to exercise the
    // string conversion paths, then let pjsua_destroy clean them up.
    for i in 0..5 {
        let account_cfg = AccountConfig {
            name: format!("leak-test-{i}"),
            username: format!("user-{i}"),
            password: format!("pass-{i}"),
            server: "127.0.0.1".into(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: Some(format!("{i}{i}{i}")),
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            registrar: None,
            proxy: None,
            reg_retry_interval: None,
            rtp_port_start: None,
            rtp_port_range: None,
            transport_id: None,
        };

        let acc_id = app
            .add_account(&account_cfg)
            .unwrap_or_else(|e| panic!("add_account failed on iteration {i}: {e}"));

        // Query account info to exercise the read/conversion path
        let _info = app.get_account_info(acc_id).ok();
    }

    // pjsua_destroy() in Drop cleans up all accounts
    drop(app);
}

/// Create transports across multiple init/destroy cycles.
/// Transports allocate sockets and internal PJSIP structures.
#[test]
fn test_transport_lifecycle() {
    for i in 0..3 {
        let (app, _rx) = PjsuaApp::new(default_config())
            .unwrap_or_else(|e| panic!("Init failed on iteration {i}: {e}"));

        // Create UDP and TCP transports
        let _udp = app
            .create_transport(TransportType::Udp, None, 0, None, None)
            .unwrap_or_else(|e| panic!("UDP transport failed on iteration {i}: {e}"));

        let _tcp = app
            .create_transport(TransportType::Tcp, None, 0, None, None)
            .unwrap_or_else(|e| panic!("TCP transport failed on iteration {i}: {e}"));

        drop(app);
    }
}

/// Exercise codec priority changes to catch any string-related leaks
/// in the codec API path.
#[test]
fn test_codec_priority_cycle() {
    let (app, _rx) = PjsuaApp::new(default_config()).expect("Failed to init PJSUA");

    for _ in 0..20 {
        // These codec strings get converted to PjString each call
        app.set_codec_priority("PCMA/8000", 255).ok();
        app.set_codec_priority("PCMU/8000", 200).ok();
        app.set_codec_priority("G722/16000", 0).ok();
        app.set_codec_priority("PCMA/8000", 128).ok();
    }

    drop(app);
}

/// Create and destroy ToneGenerator instances repeatedly.
#[test]
fn test_tonegen_lifecycle_leak() {
    for i in 0..3 {
        let (app, _rx) = PjsuaApp::new(default_config())
            .unwrap_or_else(|e| panic!("Init failed on iteration {i}: {e}"));

        let mut tonegen = ToneGenerator::new(&app, 8000, 1, 160)
            .unwrap_or_else(|e| panic!("Tonegen create failed on iteration {i}: {e}"));

        tonegen
            .add_to_conference(&app)
            .unwrap_or_else(|e| panic!("Tonegen conf add failed on iteration {i}: {e}"));

        tonegen
            .play_digits(&[
                DtmfDigit {
                    digit: '1',
                    on_ms: 50,
                    off_ms: 25,
                    volume: 0,
                },
                DtmfDigit {
                    digit: '2',
                    on_ms: 50,
                    off_ms: 25,
                    volume: 0,
                },
            ])
            .ok();

        tonegen.stop().ok();
        drop(tonegen);
        drop(app);
    }
}

/// Create and destroy CustomPort instances repeatedly.
#[test]
fn test_custom_port_lifecycle_leak() {
    struct DummyPort;
    impl MediaPort for DummyPort {}

    for i in 0..3 {
        let (app, _rx) = PjsuaApp::new(default_config())
            .unwrap_or_else(|e| panic!("Init failed on iteration {i}: {e}"));

        let mut port = CustomPort::new(&app, "leak-test", 8000, 1, 160, Box::new(DummyPort))
            .unwrap_or_else(|e| panic!("Port create failed on iteration {i}: {e}"));

        port.add_to_conference(&app)
            .unwrap_or_else(|e| panic!("Port conf add failed on iteration {i}: {e}"));

        drop(port);
        drop(app);
    }
}
