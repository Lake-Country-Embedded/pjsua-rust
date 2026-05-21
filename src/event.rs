//! SIP event types and the C callback bridge.
//!
//! PJSIP delivers events via C callbacks registered on `pjsua_config::cb`.
//! Each callback converts raw FFI data into a [`SipEvent`] and sends it
//! through a `tokio::sync::mpsc::UnboundedSender`.

use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::mpsc;
use tracing::{error, trace};

use crate::ffi;
use crate::ffi_helpers::{pj_str_to_string, pj_strerror_to_string};
use crate::types::{
    AccountId, CallId, CallState, ConfPort, MediaStatus, SipMessageInfo, TraceDirection,
};

// ---------------------------------------------------------------------------
// SipEvent enum
// ---------------------------------------------------------------------------

/// A `pj_status_t` plus its `pj_strerror` text. Distinct from the
/// crate-root `crate::error::PjError` enum (the `Result` error type) —
/// downstream consumers import this as `pjsua_rust::event::PjError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PjError {
    pub code: u32,
    pub text: String,
}

/// A high-level SIP event produced by the PJSUA callback bridge.
#[derive(Debug, Clone)]
pub enum SipEvent {
    /// Account registration state changed.
    RegistrationState {
        account_id: AccountId,
        code: u16,
        reason: String,
        is_registered: bool,
        /// `pjsua_acc_info.reg_last_err` rendered via `pj_strerror`. `None`
        /// when the registration succeeded or no transport-level error was
        /// recorded. The SIP `code` may still indicate failure when this is
        /// `None` (e.g. a real 4xx/5xx from the server).
        reg_last_error: Option<PjError>,
    },
    /// An incoming call has arrived.
    IncomingCall {
        account_id: AccountId,
        call_id: CallId,
        /// Remote party URI from the From: header (e.g. caller).
        remote_uri: String,
        /// Local party URI from the To: header (e.g. callee) — i.e. the
        /// user the incoming INVITE was addressed to. Useful for
        /// overriding PJSUA's default account matching when a P2P
        /// account with server=local_bound_ip would otherwise swallow
        /// PBX-forwarded calls for registered accounts.
        local_uri: String,
        sip_call_id: String,
    },
    /// The state of a call has changed.
    CallState {
        call_id: CallId,
        account_id: AccountId,
        state: CallState,
        last_code: u16,
        last_reason: String,
    },
    /// The media state of a call has changed.
    CallMediaState {
        call_id: CallId,
        account_id: AccountId,
        media_status: MediaStatus,
        conf_port: Option<ConfPort>,
    },
    /// A DTMF digit was received on a call.
    DtmfDigit { call_id: CallId, digit: char },
    /// Transfer (REFER) status update.
    TransferStatus {
        call_id: CallId,
        status_code: u16,
        status_text: String,
        is_final: bool,
    },
    /// A SIP message was sent or received for a call (for tracing).
    SipMessageTrace {
        call_id: CallId,
        account_id: AccountId,
        info: SipMessageInfo,
    },
}

// ---------------------------------------------------------------------------
// Global event channel (Mutex-based so it can be reset on Drop)
// ---------------------------------------------------------------------------

static EVENT_TX: Mutex<Option<mpsc::UnboundedSender<SipEvent>>> = Mutex::new(None);

/// Store the sender half of the event channel. Called during
/// [`PjsuaApp::new`](crate::PjsuaApp::new).
pub(crate) fn set_event_sender(tx: mpsc::UnboundedSender<SipEvent>) {
    let mut guard = EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(tx);
}

/// Clear the event sender. Called during [`PjsuaApp::drop`].
pub(crate) fn clear_event_sender() {
    let mut guard = EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
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

/// Registration state changed. Uses `on_reg_state2` (not `on_reg_state`)
/// because `cbparam.reason` is the only PJSIP-exposed path to transport-
/// error text — `pjsua_acc_info.status_text` is hard-wired to the canonical
/// SIP reason phrase, and `acc->reg_last_err` stays 0 for transport
/// failures (regc reports `param->status = PJ_SUCCESS`; the underlying
/// tsx/transport error lives only in `tsx->status_text`).
pub(crate) unsafe extern "C" fn on_reg_state2(
    acc_id: ffi::pjsua_acc_id,
    info: *mut ffi::pjsua_reg_info,
) {
    let mut acc_info: ffi::pjsua_acc_info = Default::default();
    let status = unsafe { ffi::pjsua_acc_get_info(acc_id, &mut acc_info) };
    if status != 0 {
        error!("pjsua_acc_get_info failed: {status}");
        return;
    }

    let code = acc_info.status as u16;
    let reason = pj_str_to_string(&acc_info.status_text);
    let is_registered = (200..300).contains(&code);

    // First try acc_info.reg_last_err (set when regc itself reports a
    // pj_status_t failure — auth retry exhaustion, 423 re-send failure, etc).
    let reg_last_err = acc_info.reg_last_err;
    let mut reg_last_error = if reg_last_err != 0 {
        Some(PjError {
            code: reg_last_err as u32,
            text: pj_strerror_to_string(reg_last_err),
        })
    } else {
        None
    };

    // For transport-level failures (TLS verify, ECONNREFUSED, etc.),
    // `acc->reg_last_err` is 0 (PJSIP reports transport errors via
    // tsx->status_text, not via the regc status field). The actual text is
    // in `cbparam.reason`. When that differs from the canonical SIP reason
    // phrase, it's a transport-layer error worth surfacing.
    if reg_last_error.is_none() && !is_registered && !info.is_null() {
        let cbparam = unsafe { (*info).cbparam };
        if !cbparam.is_null() {
            let cb_reason = pj_str_to_string(unsafe { &(*cbparam).reason });
            if !cb_reason.is_empty() && cb_reason != reason {
                reg_last_error = Some(PjError {
                    code: 0,
                    text: cb_reason,
                });
            }
        }
    }

    send_event(SipEvent::RegistrationState {
        account_id: AccountId(acc_id),
        code,
        reason,
        is_registered,
        reg_last_error,
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
    let local_uri = pj_str_to_string(&info.local_info);
    let sip_call_id = pj_str_to_string(&info.call_id);

    send_event(SipEvent::IncomingCall {
        account_id: AccountId(acc_id),
        call_id: CallId(call_id),
        remote_uri,
        local_uri,
        sip_call_id,
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
        account_id: AccountId(info.acc_id),
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
        account_id: AccountId(info.acc_id),
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
// PJSIP trace module (C helper) — sees ALL SIP messages
// ---------------------------------------------------------------------------

extern "C" {
    fn sip_trace_module_register() -> ffi::pj_status_t;
    fn sip_trace_module_unregister();
}

/// Register the PJSIP trace module. Call after `pjsua_start()`.
pub(crate) fn register_trace_module() {
    let status = unsafe { sip_trace_module_register() };
    if status != 0 {
        error!("sip_trace_module_register failed: {status}");
    }
}

/// Unregister the PJSIP trace module. Call before `pjsua_destroy()`.
pub(crate) fn unregister_trace_module() {
    unsafe { sip_trace_module_unregister() };
}

/// FFI callback invoked from `trace_module.c` for every SIP message.
///
/// Converts the C data into a [`SipEvent::SipMessageTrace`] and sends it
/// through the event channel. The `call_id` is set to `CallId(-1)` since
/// the PJSIP module doesn't know the PJSUA call index — the consumer
/// correlates by `sip_call_id` string.
#[no_mangle]
pub extern "C" fn rust_sip_trace_on_msg(
    is_outgoing: std::os::raw::c_int,
    method_or_status: *const std::os::raw::c_char,
    method_or_status_len: std::os::raw::c_int,
    sip_call_id: *const std::os::raw::c_char,
    sip_call_id_len: std::os::raw::c_int,
    sdp_body: *const std::os::raw::c_char,
    sdp_body_len: std::os::raw::c_int,
) {
    let mos = if method_or_status.is_null() || method_or_status_len <= 0 {
        String::new()
    } else {
        let slice = unsafe {
            std::slice::from_raw_parts(method_or_status as *const u8, method_or_status_len as usize)
        };
        String::from_utf8_lossy(slice).to_string()
    };

    let call_id_str = if sip_call_id.is_null() || sip_call_id_len <= 0 {
        String::new()
    } else {
        let slice = unsafe {
            std::slice::from_raw_parts(sip_call_id as *const u8, sip_call_id_len as usize)
        };
        String::from_utf8_lossy(slice).to_string()
    };

    let sdp = if sdp_body.is_null() || sdp_body_len <= 0 {
        None
    } else {
        let slice =
            unsafe { std::slice::from_raw_parts(sdp_body as *const u8, sdp_body_len as usize) };
        Some(String::from_utf8_lossy(slice).to_string())
    };

    let direction = if is_outgoing != 0 {
        TraceDirection::Sent
    } else {
        TraceDirection::Received
    };

    send_event(SipEvent::SipMessageTrace {
        call_id: CallId(-1),
        account_id: AccountId(-1),
        info: SipMessageInfo {
            direction,
            method_or_status: mos,
            sip_call_id: call_id_str,
            sdp,
            headers: HashMap::new(),
        },
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
            reg_last_error: None,
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
            local_uri: "sip:bob@example.com".into(),
            sip_call_id: "test-call-id".into(),
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
            account_id: AccountId(0),
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
            account_id: AccountId(0),
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

    #[test]
    fn pj_error_debug_clone() {
        let err = PjError {
            code: 171173,
            text: "SSL certificate verification error (PJSIP_TLS_ECERTVERIF)".into(),
        };
        let cloned = err.clone();
        assert_eq!(format!("{:?}", err), format!("{:?}", cloned));
        assert!(format!("{:?}", err).contains("171173"));
    }

    #[test]
    fn registration_state_carries_reg_last_error() {
        let event = SipEvent::RegistrationState {
            account_id: AccountId(0),
            code: 503,
            reason: "Service Unavailable".into(),
            is_registered: false,
            reg_last_error: Some(PjError {
                code: 171173,
                text: "SSL certificate verification error (PJSIP_TLS_ECERTVERIF)".into(),
            }),
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("171173"));
        assert!(debug.contains("RegistrationState"));
    }
}
