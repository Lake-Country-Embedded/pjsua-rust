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
    app.create_transport(TransportType::Udp, None, 0, None)
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

    app.create_transport(TransportType::Udp, None, 0, None)
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
                    call_id,
                    media_status,
                    conf_port,
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
