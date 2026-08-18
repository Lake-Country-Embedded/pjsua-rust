use pjsua_rust::*;
use std::collections::HashSet;

#[test]
fn test_account_id_equality() {
    let a = AccountId(1);
    let b = AccountId(1);
    let c = AccountId(2);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn test_call_id_hash() {
    let mut set = HashSet::new();
    set.insert(CallId(0));
    set.insert(CallId(1));
    set.insert(CallId(0)); // duplicate
    assert_eq!(set.len(), 2);
}

#[test]
fn test_conf_port_master() {
    assert_eq!(ConfPort::MASTER, ConfPort(0));
    assert_eq!(ConfPort::MASTER.0, 0);
}

#[test]
fn test_sip_event_variants() {
    // Verify that all SipEvent variants can be constructed and debug-printed.
    let events: Vec<SipEvent> = vec![
        SipEvent::RegistrationState {
            account_id: AccountId(0),
            code: 200,
            reason: "OK".into(),
            is_registered: true,
            reg_last_error: None,
        },
        SipEvent::IncomingCall {
            account_id: AccountId(0),
            call_id: CallId(0),
            remote_uri: "sip:alice@example.com".into(),
            local_uri: "sip:bob@example.com".into(),
            sip_call_id: "test-id".into(),
        },
        SipEvent::CallState {
            call_id: CallId(0),
            account_id: AccountId(0),
            state: CallState::Confirmed,
            last_code: 200,
            last_reason: "OK".into(),
        },
        SipEvent::CallMediaState {
            call_id: CallId(0),
            account_id: AccountId(0),
            media_status: MediaStatus::Active,
            conf_port: Some(ConfPort(1)),
        },
        SipEvent::DtmfDigit {
            call_id: CallId(0),
            digit: '5',
        },
        SipEvent::TransferStatus {
            call_id: CallId(0),
            status_code: 200,
            status_text: "OK".into(),
            is_final: true,
        },
    ];
    for event in &events {
        let debug = format!("{:?}", event);
        assert!(!debug.is_empty());
    }
    assert_eq!(events.len(), 6);
}
