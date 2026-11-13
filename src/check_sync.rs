//! Rust side of the check-sync module (`check_sync_module.c`).
//!
//! The C module decides: it answers an out-of-dialog `check-sync` NOTIFY
//! only when it comes from the server an account is registered with, over
//! the transport that registration uses. This file keeps its table of
//! registered servers up to date and turns each accepted request into a
//! [`SipEvent::CheckSync`].

use std::os::raw::{c_char, c_int};
use tracing::error;

use crate::event::{send_event, SipEvent};
use crate::ffi;
use crate::types::AccountId;

/// Register the check-sync module. Call after `pjsua_start()`.
pub(crate) fn register_module() {
    let status = unsafe { ffi::check_sync_module_register() };
    if status != 0 {
        error!("check_sync_module_register failed: {status}");
    }
}

/// Unregister the check-sync module and forget every registered server.
/// Call before `pjsua_destroy()`.
pub(crate) fn unregister_module() {
    unsafe { ffi::check_sync_module_unregister() };
}

/// Record or forget the server of `acc_id` from a registration outcome.
///
/// # Safety
///
/// `info` is the pointer PJSUA passed to `on_reg_state2`, or null.
pub(crate) unsafe fn on_reg_state(acc_id: ffi::pjsua_acc_id, info: *const ffi::pjsua_reg_info) {
    unsafe { ffi::check_sync_on_reg_state(acc_id, info) };
}

/// Forget the server of a deleted account.
pub(crate) fn forget_account(id: AccountId) {
    unsafe { ffi::check_sync_forget_account(id.0) };
}

/// FFI callback invoked from `check_sync_module.c` for each accepted request,
/// after the `200 OK` has been sent.
///
/// `source` and `reboot` are borrowed from the C caller for the duration of
/// this call only, so both are copied here. `has_reboot` tells an absent
/// `reboot` parameter (`None`) from one with an empty value (`Some("")`).
#[no_mangle]
pub extern "C" fn rust_check_sync_on_request(
    acc_id: c_int,
    source: *const c_char,
    source_len: c_int,
    reboot: *const c_char,
    reboot_len: c_int,
    has_reboot: c_int,
) {
    let source = unsafe { copy_c_bytes(source, source_len) };
    let reboot_param = if has_reboot != 0 {
        Some(unsafe { copy_c_bytes(reboot, reboot_len) })
    } else {
        None
    };

    send_event(SipEvent::CheckSync {
        account_id: AccountId(acc_id),
        reboot_param,
        source,
    });
}

/// Copy `len` bytes at `ptr` into an owned string. Null or a non-positive
/// length gives an empty string.
///
/// # Safety
///
/// When `ptr` is non-null and `len` positive, `ptr` must point to at least
/// `len` readable bytes.
unsafe fn copy_c_bytes(ptr: *const c_char, len: c_int) -> String {
    if ptr.is_null() || len <= 0 {
        return String::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(slice).into_owned()
}
