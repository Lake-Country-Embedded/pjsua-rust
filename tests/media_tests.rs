#![cfg(feature = "integration-tests")]

use pjsua_rust::*;

fn init_app() -> (PjsuaApp, tokio::sync::mpsc::UnboundedReceiver<SipEvent>) {
    let config = PjsuaConfig {
        log_level: 0,
        clock_rate: 16000,
        null_audio: true,
    };
    PjsuaApp::new(config).expect("Failed to init PJSUA")
}

#[test]
fn test_conf_enum_ports() {
    let (app, _rx) = init_app();
    let ports = app.conf_enum_ports().expect("Failed to enumerate ports");
    assert!(!ports.is_empty(), "Should have at least one port (master)");
    drop(app);
}

#[test]
fn test_conf_get_port_info() {
    let (app, _rx) = init_app();
    let info = app
        .conf_get_port_info(ConfPort::MASTER)
        .expect("Failed to get port info");
    assert_eq!(info.port, ConfPort::MASTER);
    assert!(info.clock_rate > 0);
    assert!(info.channel_count > 0);
    drop(app);
}

#[test]
fn test_tone_generator_lifecycle() {
    let (app, _rx) = init_app();
    let mut tonegen =
        ToneGenerator::new(&app, 8000, 1, 160).expect("Failed to create tone generator");
    assert!(!tonegen.is_busy());

    let port = tonegen
        .add_to_conference(&app)
        .expect("Failed to add tonegen to conference");
    assert!(port.0 > 0);

    tonegen
        .play_digits(&[
            DtmfDigit {
                digit: '1',
                on_ms: 100,
                off_ms: 50,
                volume: 0,
            },
            DtmfDigit {
                digit: '2',
                on_ms: 100,
                off_ms: 50,
                volume: 0,
            },
        ])
        .expect("Failed to play digits");

    assert!(tonegen.is_busy());
    tonegen.stop().expect("Failed to stop");
    assert!(!tonegen.is_busy());

    tonegen
        .remove_from_conference(&app)
        .expect("Failed to remove tonegen");
    drop(tonegen);
    drop(app);
}

#[test]
fn test_tone_generator_custom_tones() {
    let (app, _rx) = init_app();
    let mut tonegen =
        ToneGenerator::new(&app, 8000, 1, 160).expect("Failed to create tone generator");
    tonegen
        .add_to_conference(&app)
        .expect("Failed to add to conf");

    tonegen
        .play_tones(&[ToneDesc {
            freq1: 350,
            freq2: 440,
            on_ms: 500,
            off_ms: 0,
            volume: 0,
        }])
        .expect("Failed to play tone");

    assert!(tonegen.is_busy());
    drop(tonegen);
    drop(app);
}

#[test]
fn test_custom_port_lifecycle() {
    let (app, _rx) = init_app();

    struct SilencePort;
    impl MediaPort for SilencePort {}

    let mut port = CustomPort::new(&app, "test-silence", 8000, 1, 160, Box::new(SilencePort))
        .expect("Failed to create custom port");

    let conf = port
        .add_to_conference(&app)
        .expect("Failed to add to conference");
    assert!(conf.0 > 0);

    let ports = app.conf_enum_ports().expect("Failed to enumerate");
    assert!(ports.contains(&conf));

    let info = app
        .conf_get_port_info(conf)
        .expect("Failed to get port info");
    assert_eq!(info.clock_rate, 8000);
    assert_eq!(info.channel_count, 1);

    port.remove_from_conference(&app).expect("Failed to remove");
    drop(port);
    drop(app);
}

#[test]
fn test_set_no_sound_device() {
    let (app, _rx) = init_app();
    app.set_no_sound_device()
        .expect("Failed to set no sound device");
    drop(app);
}

#[test]
fn test_custom_port_bidirectional() {
    use std::sync::{Arc, Mutex};

    let (app, _rx) = init_app();

    struct CountPort {
        gets: Arc<Mutex<u32>>,
        puts: Arc<Mutex<u32>>,
    }
    impl MediaPort for CountPort {
        fn get_frame(&mut self, _frame: &mut AudioFrame) -> pjsua_rust::Result<()> {
            *self.gets.lock().unwrap() += 1;
            Ok(())
        }
        fn put_frame(&mut self, _frame: &AudioFrame) -> pjsua_rust::Result<()> {
            *self.puts.lock().unwrap() += 1;
            Ok(())
        }
    }

    let gets = Arc::new(Mutex::new(0u32));
    let puts = Arc::new(Mutex::new(0u32));

    let mut port = CustomPort::new(
        &app,
        "test-counter",
        8000,
        1,
        160,
        Box::new(CountPort {
            gets: gets.clone(),
            puts: puts.clone(),
        }),
    )
    .expect("Failed to create port");

    let conf = port.add_to_conference(&app).expect("Failed to add");
    app.conf_connect(conf, ConfPort::MASTER).ok();
    app.conf_connect(ConfPort::MASTER, conf).ok();

    std::thread::sleep(std::time::Duration::from_millis(100));

    app.conf_disconnect(conf, ConfPort::MASTER).ok();
    app.conf_disconnect(ConfPort::MASTER, conf).ok();

    drop(port);
    drop(app);
}

#[test]
fn test_sound_device_query() {
    let (app, _rx) = init_app();
    // get_sound_device should succeed; with null audio the IDs are implementation-defined
    let (_capture, _playback) = app.get_sound_device().unwrap();
    drop(app);
}

#[test]
fn test_tonegen_double_add_to_conference() {
    let (app, _rx) = init_app();
    let mut tg = ToneGenerator::new(&app, 8000, 1, 160).unwrap();
    tg.add_to_conference(&app).unwrap();
    let err = tg.add_to_conference(&app).unwrap_err();
    // Should get AlreadyInUse, not AlreadyInitialized (after Task 6)
    assert!(format!("{err}").contains("Already in use"));
    drop(tg);
    drop(app);
}

#[test]
fn test_custom_port_double_add_to_conference() {
    struct Dummy;
    impl MediaPort for Dummy {}
    let (app, _rx) = init_app();
    let mut port = CustomPort::new(&app, "test", 8000, 1, 160, Box::new(Dummy)).unwrap();
    port.add_to_conference(&app).unwrap();
    let err = port.add_to_conference(&app).unwrap_err();
    assert!(format!("{err}").contains("Already in use"));
    drop(port);
    drop(app);
}

#[test]
fn test_codec_priority() {
    let (app, _rx) = init_app();
    // Valid codec
    app.set_codec_priority("PCMA/8000", 255).unwrap();
    app.set_codec_priority("PCMU/8000", 0).unwrap();
    drop(app);
}
