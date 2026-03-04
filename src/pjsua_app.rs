//! RAII singleton wrapper around the PJSUA library lifecycle.
//!
//! [`PjsuaApp`] manages pjsua_create / pjsua_init / pjsua_start / pjsua_destroy
//! and exposes safe wrappers for the most commonly used PJSUA operations.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::error::{check_status, PjError, Result};
use crate::event::{self, SipEvent};
use crate::ffi;
use crate::ffi_helpers::{pj_str_to_string, PjString};
use crate::types::*;

/// Guard against double initialisation.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// RAII wrapper around the PJSUA library.
///
/// Only one instance may exist at a time. Creating a second instance while
/// the first is alive returns [`PjError::AlreadyInitialized`].
///
/// Dropping the value calls `pjsua_destroy()`.
pub struct PjsuaApp {
    _private: (), // prevent external construction
}

impl PjsuaApp {
    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    /// Initialise PJSUA and return the app handle together with a receiver
    /// for SIP events.
    ///
    /// # Errors
    ///
    /// Returns `PjError::AlreadyInitialized` on double-init, or forwards
    /// any PJSIP error from the underlying C calls.
    pub fn new(config: PjsuaConfig) -> Result<(Self, mpsc::UnboundedReceiver<SipEvent>)> {
        // Atomically check-and-set.
        if INITIALIZED.swap(true, Ordering::SeqCst) {
            return Err(PjError::AlreadyInitialized);
        }

        // Set up the event channel.
        let (tx, rx) = mpsc::unbounded_channel();
        event::set_event_sender(tx);

        // --- pjsua_create ---
        let status = unsafe { ffi::pjsua_create() };
        if status != 0 {
            INITIALIZED.store(false, Ordering::SeqCst);
            return Err(check_status(status).unwrap_err());
        }
        debug!("pjsua_create OK");

        // --- config_default ---
        let mut ua_cfg: ffi::pjsua_config = Default::default();
        unsafe { ffi::pjsua_config_default(&mut ua_cfg) };

        // Install callbacks.
        ua_cfg.cb.on_reg_state = Some(event::on_reg_state);
        ua_cfg.cb.on_incoming_call = Some(event::on_incoming_call);
        ua_cfg.cb.on_call_state = Some(event::on_call_state);
        ua_cfg.cb.on_call_media_state = Some(event::on_call_media_state);
        ua_cfg.cb.on_dtmf_digit2 = Some(event::on_dtmf_digit2);

        // --- logging config ---
        let mut log_cfg: ffi::pjsua_logging_config = Default::default();
        unsafe { ffi::pjsua_logging_config_default(&mut log_cfg) };
        log_cfg.level = config.log_level;
        log_cfg.console_level = config.log_level;

        // --- media config ---
        let mut media_cfg: ffi::pjsua_media_config = Default::default();
        unsafe { ffi::pjsua_media_config_default(&mut media_cfg) };
        media_cfg.clock_rate = config.clock_rate;

        // --- pjsua_init ---
        let status = unsafe { ffi::pjsua_init(&ua_cfg, &log_cfg, &media_cfg) };
        if status != 0 {
            unsafe { ffi::pjsua_destroy() };
            INITIALIZED.store(false, Ordering::SeqCst);
            return Err(check_status(status).unwrap_err());
        }
        debug!("pjsua_init OK");

        // --- null audio ---
        if config.null_audio {
            let status = unsafe { ffi::pjsua_set_null_snd_dev() };
            if status != 0 {
                unsafe { ffi::pjsua_destroy() };
                INITIALIZED.store(false, Ordering::SeqCst);
                return Err(check_status(status).unwrap_err());
            }
            debug!("null sound device set");
        }

        // --- pjsua_start ---
        let status = unsafe { ffi::pjsua_start() };
        if status != 0 {
            unsafe { ffi::pjsua_destroy() };
            INITIALIZED.store(false, Ordering::SeqCst);
            return Err(check_status(status).unwrap_err());
        }
        info!("PJSUA started");

        Ok((PjsuaApp { _private: () }, rx))
    }

    // ------------------------------------------------------------------
    // Transport
    // ------------------------------------------------------------------

    /// Create a SIP transport.
    pub fn create_transport(
        &self,
        transport_type: TransportType,
        bind_addr: Option<&str>,
        port: u16,
    ) -> Result<TransportId> {
        let mut tp_cfg: ffi::pjsua_transport_config = Default::default();
        unsafe { ffi::pjsua_transport_config_default(&mut tp_cfg) };
        tp_cfg.port = port as u32;

        // Keep the PjString alive for the duration of the FFI call.
        let _bind_str;
        if let Some(addr) = bind_addr {
            _bind_str = PjString::new(addr);
            tp_cfg.bound_addr = _bind_str.as_pj_str();
        }

        let mut tp_id: ffi::pjsua_transport_id = -1;
        let status = unsafe {
            ffi::pjsua_transport_create(transport_type.to_pjsip(), &tp_cfg, &mut tp_id)
        };
        check_status(status)?;
        debug!("transport created: id={tp_id}");
        Ok(TransportId(tp_id))
    }

    // ------------------------------------------------------------------
    // Account management
    // ------------------------------------------------------------------

    /// Register a SIP account and return its ID.
    pub fn add_account(&self, config: &AccountConfig) -> Result<AccountId> {
        let mut acc_cfg: ffi::pjsua_acc_config = Default::default();
        unsafe { ffi::pjsua_acc_config_default(&mut acc_cfg) };

        // Build strings — all must stay alive until after pjsua_acc_add.
        let uri_str = PjString::new(&config.sip_uri());
        let reg_str = PjString::new(&config.registrar_uri());
        let realm_str = PjString::new("*");
        let scheme_str = PjString::new("digest");
        let username_str = PjString::new(&config.username);
        let password_str = PjString::new(&config.password);

        acc_cfg.id = uri_str.as_pj_str();
        acc_cfg.reg_uri = reg_str.as_pj_str();

        // Credentials
        acc_cfg.cred_count = 1;
        acc_cfg.cred_info[0].realm = realm_str.as_pj_str();
        acc_cfg.cred_info[0].scheme = scheme_str.as_pj_str();
        acc_cfg.cred_info[0].username = username_str.as_pj_str();
        acc_cfg.cred_info[0].data_type = 0; // PJSIP_CRED_DATA_PLAIN_PASSWD
        acc_cfg.cred_info[0].data = password_str.as_pj_str();

        // SRTP
        acc_cfg.use_srtp = config.srtp.to_pjsip();

        let mut acc_id: ffi::pjsua_acc_id = -1;
        let status = unsafe { ffi::pjsua_acc_add(&acc_cfg, 0, &mut acc_id) };
        check_status(status)?;
        info!("account added: id={acc_id} name={}", config.name);
        Ok(AccountId(acc_id))
    }

    /// Unregister and delete a SIP account.
    pub fn remove_account(&self, id: AccountId) -> Result<()> {
        // Unregister first (renew = PJ_FALSE = 0).
        let status = unsafe { ffi::pjsua_acc_set_registration(id.0, 0) };
        check_status(status)?;
        let status = unsafe { ffi::pjsua_acc_del(id.0) };
        check_status(status)?;
        info!("account removed: id={}", id.0);
        Ok(())
    }

    /// Retrieve information about an account.
    pub fn get_account_info(&self, id: AccountId) -> Result<AccountInfo> {
        let mut info: ffi::pjsua_acc_info = Default::default();
        let status = unsafe { ffi::pjsua_acc_get_info(id.0, &mut info) };
        check_status(status)?;

        let code = info.status as u32;
        Ok(AccountInfo {
            account_id: AccountId(info.id),
            uri: pj_str_to_string(&info.acc_uri),
            is_registered: code >= 200 && code < 300,
            status_code: code,
            status_text: pj_str_to_string(&info.status_text),
        })
    }

    // ------------------------------------------------------------------
    // Call control
    // ------------------------------------------------------------------

    /// Place an outgoing call and return the call ID.
    pub fn make_call(&self, account: AccountId, uri: &str) -> Result<CallId> {
        let dst = PjString::new(uri);
        let pj_dst = dst.as_pj_str();
        let mut call_id: ffi::pjsua_call_id = -1;
        let status = unsafe {
            ffi::pjsua_call_make_call(
                account.0,
                &pj_dst,
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                &mut call_id,
            )
        };
        check_status(status)?;
        debug!("call made: call_id={call_id} uri={uri}");
        Ok(CallId(call_id))
    }

    /// Answer an incoming call with the given SIP status code (e.g. 200).
    pub fn answer_call(&self, call: CallId, code: u32) -> Result<()> {
        let status = unsafe {
            ffi::pjsua_call_answer(call.0, code, ptr::null(), ptr::null())
        };
        check_status(status)
    }

    /// Hang up a call with the given SIP status code (e.g. 603 = Decline).
    pub fn hangup_call(&self, call: CallId, code: u32) -> Result<()> {
        let status = unsafe {
            ffi::pjsua_call_hangup(call.0, code, ptr::null(), ptr::null())
        };
        check_status(status)
    }

    /// Hang up all active calls.
    pub fn hangup_all(&self) -> Result<()> {
        unsafe { ffi::pjsua_call_hangup_all() };
        Ok(())
    }

    /// Retrieve information about a call.
    pub fn get_call_info(&self, call: CallId) -> Result<CallInfo> {
        let mut info: ffi::pjsua_call_info = Default::default();
        let status = unsafe { ffi::pjsua_call_get_info(call.0, &mut info) };
        check_status(status)?;

        Ok(CallInfo {
            call_id: CallId(info.id),
            account_id: AccountId(info.acc_id),
            remote_uri: pj_str_to_string(&info.remote_info),
            state: CallState::from_pjsip(info.state),
            media_status: MediaStatus::from_pjsip(info.media_status),
            conf_port: ConfPort(info.conf_slot),
            connect_duration_ms: (info.connect_duration.sec as u64) * 1000
                + (info.connect_duration.msec as u64),
            total_duration_ms: (info.total_duration.sec as u64) * 1000
                + (info.total_duration.msec as u64),
            last_status_code: info.last_status as u32,
        })
    }

    // ------------------------------------------------------------------
    // Media / Conference bridge
    // ------------------------------------------------------------------

    /// Set the null (no-op) sound device. Useful for headless operation.
    pub fn set_null_sound_device(&self) -> Result<()> {
        let status = unsafe { ffi::pjsua_set_null_snd_dev() };
        check_status(status)
    }

    /// Connect a conference bridge source port to a sink port.
    pub fn conf_connect(&self, source: ConfPort, sink: ConfPort) -> Result<()> {
        let status = unsafe { ffi::pjsua_conf_connect(source.0, sink.0) };
        check_status(status)
    }

    /// Disconnect a conference bridge source port from a sink port.
    pub fn conf_disconnect(&self, source: ConfPort, sink: ConfPort) -> Result<()> {
        let status = unsafe { ffi::pjsua_conf_disconnect(source.0, sink.0) };
        check_status(status)
    }

    /// Create a WAV file player.
    ///
    /// If `no_loop` is true, the file plays once and then the port goes
    /// silent.
    pub fn create_player(&self, path: &str, no_loop: bool) -> Result<PlayerId> {
        let filename = PjString::new(path);
        let pj_filename = filename.as_pj_str();
        // PJMEDIA_FILE_NO_LOOP = 0x01
        let options: u32 = if no_loop { 0x01 } else { 0 };
        let mut player_id: ffi::pjsua_player_id = -1;
        let status =
            unsafe { ffi::pjsua_player_create(&pj_filename, options, &mut player_id) };
        check_status(status)?;
        debug!("player created: id={player_id} path={path}");
        Ok(PlayerId(player_id))
    }

    /// Destroy a WAV file player.
    pub fn destroy_player(&self, id: PlayerId) -> Result<()> {
        let status = unsafe { ffi::pjsua_player_destroy(id.0) };
        check_status(status)
    }

    /// Get the conference bridge port number of a player.
    pub fn player_conf_port(&self, id: PlayerId) -> Result<ConfPort> {
        let port = unsafe { ffi::pjsua_player_get_conf_port(id.0) };
        if port < 0 {
            return Err(PjError::NotFound(format!("player {}", id.0)));
        }
        Ok(ConfPort(port))
    }

    /// Create a WAV file recorder.
    pub fn create_recorder(&self, path: &str) -> Result<RecorderId> {
        let filename = PjString::new(path);
        let pj_filename = filename.as_pj_str();
        let mut rec_id: ffi::pjsua_recorder_id = -1;
        let status = unsafe {
            ffi::pjsua_recorder_create(
                &pj_filename,
                0, // enc_type: default
                ptr::null_mut(),
                0, // max_size: unlimited
                0, // options
                &mut rec_id,
            )
        };
        check_status(status)?;
        debug!("recorder created: id={rec_id} path={path}");
        Ok(RecorderId(rec_id))
    }

    /// Destroy a WAV file recorder.
    pub fn destroy_recorder(&self, id: RecorderId) -> Result<()> {
        let status = unsafe { ffi::pjsua_recorder_destroy(id.0) };
        check_status(status)
    }

    /// Get the conference bridge port number of a recorder.
    pub fn recorder_conf_port(&self, id: RecorderId) -> Result<ConfPort> {
        let port = unsafe { ffi::pjsua_recorder_get_conf_port(id.0) };
        if port < 0 {
            return Err(PjError::NotFound(format!("recorder {}", id.0)));
        }
        Ok(ConfPort(port))
    }

    // ------------------------------------------------------------------
    // DTMF
    // ------------------------------------------------------------------

    /// Send DTMF digits on a call using RFC 2833.
    ///
    /// The `method` parameter is accepted for API compatibility but the
    /// underlying `pjsua_call_dial_dtmf` always uses RFC 2833.
    pub fn send_dtmf(&self, call: CallId, digits: &str, _method: DtmfMethod) -> Result<()> {
        let digits_str = PjString::new(digits);
        let pj_digits = digits_str.as_pj_str();
        let status = unsafe { ffi::pjsua_call_dial_dtmf(call.0, &pj_digits) };
        check_status(status)
    }

    // ------------------------------------------------------------------
    // Codec
    // ------------------------------------------------------------------

    /// Set the priority of a codec (0 = disabled, 255 = highest).
    pub fn set_codec_priority(&self, codec: &str, priority: u8) -> Result<()> {
        let codec_str = PjString::new(codec);
        let pj_codec = codec_str.as_pj_str();
        let status = unsafe { ffi::pjsua_codec_set_priority(&pj_codec, priority) };
        check_status(status)
    }
}

impl Drop for PjsuaApp {
    fn drop(&mut self) {
        info!("destroying PJSUA");
        unsafe { ffi::pjsua_destroy() };
        event::clear_event_sender();
        INITIALIZED.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pjsua_app_needs_drop() {
        // Verify PjsuaApp implements Drop (RAII cleanup).
        assert!(std::mem::needs_drop::<PjsuaApp>());
    }
}
