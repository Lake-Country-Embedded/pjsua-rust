//! SIP event types and the C callback bridge.
//!
//! PJSIP delivers events via C callbacks registered on `pjsua_config::cb`.
//! Each callback converts raw FFI data into a [`SipEvent`] and sends it
//! through a `tokio::sync::mpsc::UnboundedSender`.

use std::sync::Mutex;
use tokio::sync::mpsc;
use tracing::{error, trace};

use crate::ffi;
use crate::ffi_helpers::pj_str_to_string;
use crate::types::{AccountId, CallId, CallState, ConfPort, MediaStatus};

// ---------------------------------------------------------------------------
// SipEvent enum
// ---------------------------------------------------------------------------

/// A high-level SIP event produced by the PJSUA callback bridge.
#[derive(Debug, Clone)]
pub enum SipEvent {
    /// Account registration state changed.
    RegistrationState {
        account_id: AccountId,
        code: u16,
        reason: String,
        is_registered: bool,
    },
    /// An incoming call has arrived.
    IncomingCall {
        account_id: AccountId,
        call_id: CallId,
        remote_uri: String,
    },
    /// The state of a call has changed.
    CallState {
        call_id: CallId,
        state: CallState,
        last_code: u16,
        last_reason: String,
    },
    /// The media state of a call has changed.
    CallMediaState {
        call_id: CallId,
        media_status: MediaStatus,
        conf_port: Option<ConfPort>,
    },
    /// A DTMF digit was received on a call.
    DtmfDigit {
        call_id: CallId,
        digit: char,
    },
    /// Transfer (REFER) status update.
    TransferStatus {
        call_id: CallId,
        status_code: u16,
        status_text: String,
        is_final: bool,
    },
}

// ---------------------------------------------------------------------------
// Global event channel (Mutex-based so it can be reset on Drop)
// ---------------------------------------------------------------------------

static EVENT_TX: Mutex<Option<mpsc::UnboundedSender<SipEvent>>> = Mutex::new(None);

/// Store the sender half of the event channel. Called during
/// [`PjsuaApp::new`](crate::PjsuaApp::new).
pub(crate) fn set_event_sender(tx: mpsc::UnboundedSender<SipEvent>) {
    let mut guard = EVENT_TX.lock().unwrap();
    *guard = Some(tx);
}

/// Clear the event sender. Called during [`PjsuaApp::drop`].
pub(crate) fn clear_event_sender() {
    let mut guard = EVENT_TX.lock().unwrap();
    *guard = None;
}

/// Send an event through the global channel. Silently drops if the
/// receiver has been closed or the sender was never installed.
fn send_event(event: SipEvent) {
    if let Ok(guard) = EVENT_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            if let Err(e) = tx.send(event) {
                trace!("Event channel closed, dropping event: {}", e);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// C callback functions
// ---------------------------------------------------------------------------

/// `on_reg_state` callback — registration state changed.
pub(crate) unsafe extern "C" fn on_reg_state(acc_id: ffi::pjsua_acc_id) {
    let mut info: ffi::pjsua_acc_info = Default::default();
    let status = unsafe { ffi::pjsua_acc_get_info(acc_id, &mut info) };
    if status != 0 {
        error!("pjsua_acc_get_info failed: {status}");
        return;
    }

    let code = info.status as u16;
    let reason = pj_str_to_string(&info.status_text);
    let is_registered = code >= 200 && code < 300;

    send_event(SipEvent::RegistrationState {
        account_id: AccountId(acc_id),
        code,
        reason,
        is_registered,
    });
}

/// `on_incoming_call` callback — new incoming call.
pub(crate) unsafe extern "C" fn on_incoming_call(
    acc_id: ffi::pjsua_acc_id,
    call_id: ffi::pjsua_call_id,
    _rdata: *mut ffi::pjsip_rx_data,
) {
    let mut info: ffi::pjsua_call_info = Default::default();
    let status = unsafe { ffi::pjsua_call_get_info(call_id, &mut info) };
    if status != 0 {
        error!("pjsua_call_get_info failed: {status}");
        return;
    }

    let remote_uri = pj_str_to_string(&info.remote_info);

    send_event(SipEvent::IncomingCall {
        account_id: AccountId(acc_id),
        call_id: CallId(call_id),
        remote_uri,
    });
}

/// `on_call_state` callback — call state changed.
pub(crate) unsafe extern "C" fn on_call_state(
    call_id: ffi::pjsua_call_id,
    _e: *mut ffi::pjsip_event,
) {
    let mut info: ffi::pjsua_call_info = Default::default();
    let status = unsafe { ffi::pjsua_call_get_info(call_id, &mut info) };
    if status != 0 {
        error!("pjsua_call_get_info failed: {status}");
        return;
    }

    let state = CallState::from_pjsip(info.state);
    let last_code = info.last_status as u16;
    let last_reason = pj_str_to_string(&info.last_status_text);

    send_event(SipEvent::CallState {
        call_id: CallId(call_id),
        state,
        last_code,
        last_reason,
    });
}

/// `on_call_media_state` callback — call media state changed.
pub(crate) unsafe extern "C" fn on_call_media_state(call_id: ffi::pjsua_call_id) {
    let mut info: ffi::pjsua_call_info = Default::default();
    let status = unsafe { ffi::pjsua_call_get_info(call_id, &mut info) };
    if status != 0 {
        error!("pjsua_call_get_info failed: {status}");
        return;
    }

    let media_status = MediaStatus::from_pjsip(info.media_status);
    let conf_port = if media_status == MediaStatus::Active {
        Some(ConfPort(info.conf_slot))
    } else {
        None
    };

    send_event(SipEvent::CallMediaState {
        call_id: CallId(call_id),
        media_status,
        conf_port,
    });
}

/// `on_dtmf_digit2` callback — DTMF digit received.
pub(crate) unsafe extern "C" fn on_dtmf_digit2(
    call_id: ffi::pjsua_call_id,
    info: *const ffi::pjsua_dtmf_info,
) {
    if info.is_null() {
        return;
    }
    let info = unsafe { &*info };
    // digit is c_uint; cast to u8 then char
    let digit = char::from(info.digit as u8);

    send_event(SipEvent::DtmfDigit {
        call_id: CallId(call_id),
        digit,
    });
}

/// `on_call_transfer_status` callback — call transfer (REFER) status.
pub(crate) unsafe extern "C" fn on_call_transfer_status(
    call_id: ffi::pjsua_call_id,
    st_code: ::std::os::raw::c_int,
    st_text: *const ffi::pj_str_t,
    final_: ffi::pj_bool_t,
    p_cont: *mut ffi::pj_bool_t,
) {
    let status_text = if st_text.is_null() {
        String::new()
    } else {
        pj_str_to_string(unsafe { &*st_text })
    };
    let is_final = final_ != 0;

    // Continue receiving notifications unless this is the final one
    if !p_cont.is_null() {
        unsafe { *p_cont = if is_final { 0 } else { 1 } };
    }

    send_event(SipEvent::TransferStatus {
        call_id: CallId(call_id),
        status_code: st_code as u16,
        status_text,
        is_final,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sip_event_debug() {
        let event = SipEvent::RegistrationState {
            account_id: AccountId(0),
            code: 200,
            reason: "OK".into(),
            is_registered: true,
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("RegistrationState"));
        assert!(debug.contains("200"));
    }

    #[test]
    fn sip_event_clone() {
        let event = SipEvent::IncomingCall {
            account_id: AccountId(1),
            call_id: CallId(0),
            remote_uri: "sip:alice@example.com".into(),
        };
        let cloned = event.clone();
        let debug_orig = format!("{:?}", event);
        let debug_clone = format!("{:?}", cloned);
        assert_eq!(debug_orig, debug_clone);
    }

    #[test]
    fn sip_event_call_state_debug() {
        let event = SipEvent::CallState {
            call_id: CallId(42),
            state: CallState::Confirmed,
            last_code: 200,
            last_reason: "OK".into(),
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("CallState"));
        assert!(debug.contains("Confirmed"));
    }

    #[test]
    fn sip_event_media_state_debug() {
        let event = SipEvent::CallMediaState {
            call_id: CallId(0),
            media_status: MediaStatus::Active,
            conf_port: Some(ConfPort(1)),
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("CallMediaState"));
        assert!(debug.contains("Active"));
    }

    #[test]
    fn sip_event_dtmf_debug() {
        let event = SipEvent::DtmfDigit {
            call_id: CallId(0),
            digit: '5',
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("DtmfDigit"));
        assert!(debug.contains('5'));
    }

    #[test]
    fn sip_event_transfer_status_debug() {
        let event = SipEvent::TransferStatus {
            call_id: CallId(1),
            status_code: 200,
            status_text: "OK".into(),
            is_final: true,
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("TransferStatus"));
        assert!(debug.contains("200"));
        assert!(debug.contains("true"));
    }
}
