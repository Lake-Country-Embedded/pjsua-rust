use std::collections::HashMap;
use std::ffi::CString;

use crate::ffi;
use crate::types::{SipMessageInfo, TraceDirection};

/// A helper that owns a CString and provides a pj_str_t pointing into it.
///
/// The pj_str_t is valid for the lifetime of this struct. Use `.as_pj_str()`
/// to get the raw pj_str_t to pass to PJSIP functions.
pub struct PjString {
    _cstring: CString,
    pj_str: ffi::pj_str_t,
}

impl PjString {
    /// Create a new PjString from a Rust string.
    ///
    /// Panics if the input contains interior null bytes.
    pub fn new(s: &str) -> Self {
        let cstring = CString::new(s).expect("PjString::new: interior null byte");
        // SAFETY: pj_str_t.ptr is *mut but PJSIP reads through it without
        // modification. The CString is kept alive by this struct.
        let pj_str = ffi::pj_str_t {
            ptr: cstring.as_ptr() as *mut std::ffi::c_char,
            slen: s.len() as ffi::pj_ssize_t,
        };
        PjString {
            _cstring: cstring,
            pj_str,
        }
    }

    /// Get a copy of the internal `pj_str_t`.
    ///
    /// # Safety Contract
    ///
    /// The returned `pj_str_t` contains a raw pointer into this `PjString`'s
    /// backing `CString`. The caller **must** ensure this `PjString` outlives
    /// any use of the returned `pj_str_t`. Dropping the `PjString` while the
    /// `pj_str_t` is still in use results in a dangling pointer.
    pub fn as_pj_str(&self) -> ffi::pj_str_t {
        self.pj_str
    }

    /// Get a mutable pointer to the internal pj_str_t (needed by some PJSIP APIs).
    pub fn as_mut_pj_str(&mut self) -> *mut ffi::pj_str_t {
        &mut self.pj_str as *mut ffi::pj_str_t
    }
}

/// Convert a pj_str_t to a Rust String.
///
/// Returns an empty string if the pointer is null or the length is zero/negative.
/// Non-UTF8 data is replaced using lossy conversion.
pub fn pj_str_to_string(pj_str: &ffi::pj_str_t) -> String {
    if pj_str.ptr.is_null() || pj_str.slen <= 0 {
        return String::new();
    }
    let len = pj_str.slen as usize;
    let slice = unsafe { std::slice::from_raw_parts(pj_str.ptr as *const u8, len) };
    String::from_utf8_lossy(slice).to_string()
}

/// Format a `pj_status_t` error code into a human-readable string via `pj_strerror`.
///
/// Returns an empty string when `status == 0`. Uses a fixed `PJ_ERR_MSG_SIZE`
/// (256-byte) stack buffer per PJ documentation.
pub fn pj_strerror_to_string(status: ffi::pj_status_t) -> String {
    if status == 0 {
        return String::new();
    }
    const BUF_LEN: usize = 256;
    let mut buf = [0u8; BUF_LEN];
    let pj_str = unsafe {
        ffi::pj_strerror(
            status,
            buf.as_mut_ptr() as *mut std::os::raw::c_char,
            BUF_LEN,
        )
    };
    pj_str_to_string(&pj_str)
}

// The following extraction helpers are not currently used (the C trace module
// handles message extraction directly), but are retained for potential future use.
#[allow(dead_code)]
/// Extract the SDP body from a `pjsip_msg_body` by invoking its `print_body` callback.
///
/// Returns `None` if `body` is null, the callback is absent, or the printed length is zero.
pub unsafe fn extract_sdp_body(body: *mut ffi::pjsip_msg_body) -> Option<String> {
    if body.is_null() {
        return None;
    }
    let body_ref = unsafe { &*body };
    let print_fn = body_ref.print_body?;
    let mut buf = vec![0u8; 4096];
    let len = unsafe {
        print_fn(
            body as *mut ffi::pjsip_msg_body,
            buf.as_mut_ptr() as *mut std::os::raw::c_char,
            buf.len() as ffi::pj_size_t,
        )
    };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(String::from_utf8_lossy(&buf).to_string())
}

#[allow(dead_code)]
/// Extract a [`SipMessageInfo`] from a parsed `pjsip_msg`.
///
/// The returned `sip_call_id` is empty and `headers` is empty; the caller is
/// responsible for filling those in from the appropriate source (e.g. rdata
/// pre-parsed headers).
pub unsafe fn extract_sip_msg_info(
    msg: *const ffi::pjsip_msg,
    direction: TraceDirection,
) -> Option<SipMessageInfo> {
    if msg.is_null() {
        return None;
    }
    let msg_ref = unsafe { &*msg };

    let method_or_status = if msg_ref.type_ == ffi::pjsip_msg_type_e_PJSIP_REQUEST_MSG {
        // SAFETY: union variant is valid when type_ == REQUEST
        let req = unsafe { &msg_ref.line.req };
        pj_str_to_string(&req.method.name)
    } else {
        // SAFETY: union variant is valid when type_ == RESPONSE
        let status = unsafe { &msg_ref.line.status };
        let reason = pj_str_to_string(&status.reason);
        format!("{} {}", status.code, reason)
    };

    let sdp = unsafe { extract_sdp_body(msg_ref.body) };

    Some(SipMessageInfo {
        direction,
        method_or_status,
        sip_call_id: String::new(),
        sdp,
        headers: HashMap::new(),
        // Only the trace module sees the untouched message text.
        raw: None,
    })
}

#[allow(dead_code)]
/// Extract a [`SipMessageInfo`] from a received-data buffer (`pjsip_rx_data`).
///
/// Fills `sip_call_id` from the pre-parsed `cid` header and adds a `CSeq`
/// entry to `headers` from the pre-parsed `cseq` header.
pub unsafe fn extract_from_rx_data(rdata: *const ffi::pjsip_rx_data) -> Option<SipMessageInfo> {
    if rdata.is_null() {
        return None;
    }
    let rdata_ref = unsafe { &*rdata };

    let mut info =
        unsafe { extract_sip_msg_info(rdata_ref.msg_info.msg, TraceDirection::Received) }?;

    // Fill Call-ID from the pre-parsed cid header.
    if !rdata_ref.msg_info.cid.is_null() {
        let cid_ref = unsafe { &*rdata_ref.msg_info.cid };
        info.sip_call_id = pj_str_to_string(&cid_ref.id);
    }

    // Fill CSeq header from the pre-parsed cseq header.
    if !rdata_ref.msg_info.cseq.is_null() {
        let cseq_ref = unsafe { &*rdata_ref.msg_info.cseq };
        let method_name = pj_str_to_string(&cseq_ref.method.name);
        info.headers.insert(
            "CSeq".to_string(),
            format!("{} {}", cseq_ref.cseq, method_name),
        );
    }

    Some(info)
}

#[allow(dead_code)]
/// Extract a [`SipMessageInfo`] from a `pjsip_event`.
///
/// Only `PJSIP_EVENT_TSX_STATE` events carry a message; all other event types
/// return `None`.  Within a TSX state event the source union is consulted:
/// - `PJSIP_EVENT_RX_MSG` → delegates to [`extract_from_rx_data`]
/// - `PJSIP_EVENT_TX_MSG` → delegates to [`extract_sip_msg_info`] on the tdata message
pub unsafe fn extract_from_event(event: *const ffi::pjsip_event) -> Option<SipMessageInfo> {
    if event.is_null() {
        return None;
    }
    let event_ref = unsafe { &*event };

    if event_ref.type_ != ffi::pjsip_event_id_e_PJSIP_EVENT_TSX_STATE {
        return None;
    }

    // SAFETY: body.tsx_state is valid when type_ == PJSIP_EVENT_TSX_STATE
    let tsx_state = unsafe { &event_ref.body.tsx_state };

    match tsx_state.type_ {
        ffi::pjsip_event_id_e_PJSIP_EVENT_RX_MSG => {
            // SAFETY: src.rdata is valid when tsx_state.type_ == PJSIP_EVENT_RX_MSG
            let rdata = unsafe { tsx_state.src.rdata };
            unsafe { extract_from_rx_data(rdata) }
        }
        ffi::pjsip_event_id_e_PJSIP_EVENT_TX_MSG => {
            // SAFETY: src.tdata is valid when tsx_state.type_ == PJSIP_EVENT_TX_MSG
            let tdata = unsafe { tsx_state.src.tdata };
            if tdata.is_null() {
                return None;
            }
            let tdata_ref = unsafe { &*tdata };
            unsafe { extract_sip_msg_info(tdata_ref.msg, TraceDirection::Sent) }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let input = "sip:user@example.com";
        let pj = PjString::new(input);
        let pj_str = pj.as_pj_str();
        let output = pj_str_to_string(&pj_str);
        assert_eq!(input, output);
    }

    #[test]
    fn null_handling() {
        let pj_str = ffi::pj_str_t {
            ptr: std::ptr::null_mut(),
            slen: 0,
        };
        assert_eq!(pj_str_to_string(&pj_str), "");
    }

    #[test]
    fn empty_string() {
        let pj = PjString::new("");
        let pj_str = pj.as_pj_str();
        assert_eq!(pj_str.slen, 0);
        let output = pj_str_to_string(&pj_str);
        assert_eq!(output, "");
    }

    #[test]
    #[should_panic(expected = "interior null byte")]
    fn pjstring_interior_null_panics() {
        PjString::new("hello\0world");
    }

    #[test]
    fn negative_slen() {
        let pj_str = ffi::pj_str_t {
            ptr: std::ptr::null_mut(),
            slen: -1,
        };
        assert_eq!(pj_str_to_string(&pj_str), "");
    }
}
