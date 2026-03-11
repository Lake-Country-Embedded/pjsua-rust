//! RAII singleton wrapper around the PJSUA library lifecycle.
//!
//! [`PjsuaApp`] manages pjsua_create / pjsua_init / pjsua_start / pjsua_destroy
//! and exposes safe wrappers for the most commonly used PJSUA operations.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::{check_status, make_pj_error, PjError, Result};
use crate::event::{self, SipEvent};
use crate::ffi;
use crate::ffi_helpers::{pj_str_to_string, PjString};
use crate::types::*;

/// Guard against double initialisation.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Flag for `pjsua_call_reinvite` to take a call off hold.
const PJSUA_CALL_UNHOLD: u32 = 1;

/// RAII wrapper around the PJSUA library.
///
/// Only one instance may exist at a time. Creating a second instance while
/// the first is alive returns [`PjError::AlreadyInitialized`].
///
/// Dropping the value calls `pjsua_destroy()`.
///
/// # Thread Safety
///
/// `PjsuaApp` is `Send + Sync`. The PJSUA API (as opposed to lower-level
/// PJSIP/PJLIB APIs) is designed for multi-threaded use — all public
/// functions acquire PJSIP's internal mutex before accessing shared state.
/// This makes it safe to share a `PjsuaApp` across threads via `Arc`.
pub struct PjsuaApp {
    _private: (), // prevent external construction
}

// SAFETY: PJSUA functions (pjsua_*) acquire PJSIP's internal locking before
// accessing shared state. The PJSUA layer is explicitly designed for
// multi-threaded applications — see PJSIP documentation on thread safety.
// This allows `Arc<PjsuaApp>` to be shared across Tokio tasks in the daemon.
unsafe impl Sync for PjsuaApp {}

impl std::fmt::Debug for PjsuaApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PjsuaApp")
    }
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
            return Err(make_pj_error(status));
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
        ua_cfg.cb.on_call_transfer_status = Some(event::on_call_transfer_status);

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
            return Err(make_pj_error(status));
        }
        debug!("pjsua_init OK");

        // --- null audio ---
        if config.null_audio {
            let status = unsafe { ffi::pjsua_set_null_snd_dev() };
            if status != 0 {
                unsafe { ffi::pjsua_destroy() };
                INITIALIZED.store(false, Ordering::SeqCst);
                return Err(make_pj_error(status));
            }
            debug!("null sound device set");
        }

        // --- pjsua_start ---
        let status = unsafe { ffi::pjsua_start() };
        if status != 0 {
            unsafe { ffi::pjsua_destroy() };
            INITIALIZED.store(false, Ordering::SeqCst);
            return Err(make_pj_error(status));
        }
        info!("PJSUA started");

        Ok((PjsuaApp { _private: () }, rx))
    }

    // ------------------------------------------------------------------
    // Transport
    // ------------------------------------------------------------------

    /// Create a SIP transport.
    ///
    /// Pass `tls` to configure TLS settings for a TLS transport.
    pub fn create_transport(
        &self,
        transport_type: TransportType,
        bind_addr: Option<&str>,
        port: u16,
        tls: Option<&TlsConfig>,
        qos_dscp: Option<u8>,
    ) -> Result<TransportId> {
        let mut tp_cfg: ffi::pjsua_transport_config = Default::default();
        unsafe { ffi::pjsua_transport_config_default(&mut tp_cfg) };
        tp_cfg.port = port as u32;

        // Apply QoS DSCP if provided
        if let Some(dscp) = qos_dscp {
            tp_cfg.qos_params.flags = 1; // PJ_QOS_PARAM_HAS_DSCP
            tp_cfg.qos_params.dscp_val = dscp;
        }

        // Keep all PjStrings alive for the duration of the FFI call.
        let mut strings: Vec<PjString> = Vec::new();

        if let Some(addr) = bind_addr {
            let bind_str = PjString::new(addr);
            tp_cfg.bound_addr = bind_str.as_pj_str();
            strings.push(bind_str);
        }

        if let Some(tls_cfg) = tls {
            if let Some(ref ca) = tls_cfg.ca_list_file {
                let s = PjString::new(ca);
                tp_cfg.tls_setting.ca_list_file = s.as_pj_str();
                strings.push(s);
            }
            if let Some(ref ca_path) = tls_cfg.ca_list_path {
                let s = PjString::new(ca_path);
                tp_cfg.tls_setting.ca_list_path = s.as_pj_str();
                strings.push(s);
            }
            if let Some(ref cert) = tls_cfg.cert_file {
                let s = PjString::new(cert);
                tp_cfg.tls_setting.cert_file = s.as_pj_str();
                strings.push(s);
            }
            if let Some(ref key) = tls_cfg.privkey_file {
                let s = PjString::new(key);
                tp_cfg.tls_setting.privkey_file = s.as_pj_str();
                strings.push(s);
            }
            if let Some(ref pw) = tls_cfg.password {
                let s = PjString::new(pw);
                tp_cfg.tls_setting.password = s.as_pj_str();
                strings.push(s);
            }
            tp_cfg.tls_setting.verify_server = if tls_cfg.verify_server { 1 } else { 0 };
            tp_cfg.tls_setting.verify_client = if tls_cfg.verify_client { 1 } else { 0 };
        }

        let mut tp_id: ffi::pjsua_transport_id = -1;
        let status =
            unsafe { ffi::pjsua_transport_create(transport_type.to_pjsip(), &tp_cfg, &mut tp_id) };
        check_status(status)?;
        debug!("transport created: id={tp_id}");
        Ok(TransportId(tp_id))
    }

    /// Close a SIP transport.
    ///
    /// If `force` is false, the transport will be closed gracefully (waiting
    /// for pending transactions). If `force` is true, it is closed immediately.
    pub fn close_transport(&self, id: TransportId, force: bool) -> Result<()> {
        let status = unsafe { ffi::pjsua_transport_close(id.0, if force { 1 } else { 0 }) };
        check_status(status)?;
        debug!("transport closed: id={} force={force}", id.0);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Account management
    // ------------------------------------------------------------------

    /// Register a SIP account and return its ID.
    pub fn add_account(&self, config: &AccountConfig) -> Result<AccountId> {
        let mut acc_cfg: ffi::pjsua_acc_config = Default::default();
        unsafe { ffi::pjsua_acc_config_default(&mut acc_cfg) };
        let _strings = populate_acc_cfg(&mut acc_cfg, config);

        let mut acc_id: ffi::pjsua_acc_id = -1;
        let status = unsafe { ffi::pjsua_acc_add(&acc_cfg, 0, &mut acc_id) };
        check_status(status)?;
        info!("account added: id={acc_id} name={}", config.name);
        Ok(AccountId(acc_id))
    }

    /// Modify an existing SIP account's configuration.
    pub fn modify_account(&self, id: AccountId, config: &AccountConfig) -> Result<()> {
        let mut acc_cfg: ffi::pjsua_acc_config = Default::default();
        unsafe { ffi::pjsua_acc_config_default(&mut acc_cfg) };
        let _strings = populate_acc_cfg(&mut acc_cfg, config);

        let status = unsafe { ffi::pjsua_acc_modify(id.0, &acc_cfg) };
        check_status(status)?;
        info!("account modified: id={} name={}", id.0, config.name);
        Ok(())
    }

    /// Unregister and delete a SIP account.
    pub fn remove_account(&self, id: AccountId) -> Result<()> {
        // Best-effort unregister (fails for P2P accounts or mid-transaction).
        let status = unsafe { ffi::pjsua_acc_set_registration(id.0, 0) };
        if status != 0 {
            warn!(
                "could not unregister account {} (status={}), proceeding with removal",
                id.0, status
            );
        }
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

        let code = info.status;
        Ok(AccountInfo {
            account_id: AccountId(info.id),
            uri: pj_str_to_string(&info.acc_uri),
            is_registered: (200..300).contains(&code),
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
        let status = unsafe { ffi::pjsua_call_answer(call.0, code, ptr::null(), ptr::null()) };
        check_status(status)
    }

    /// Hang up a call with the given SIP status code (e.g. 603 = Decline).
    pub fn hangup_call(&self, call: CallId, code: u32) -> Result<()> {
        let status = unsafe { ffi::pjsua_call_hangup(call.0, code, ptr::null(), ptr::null()) };
        check_status(status)
    }

    /// Place a call on hold.
    pub fn hold_call(&self, call: CallId) -> Result<()> {
        let status = unsafe { ffi::pjsua_call_set_hold(call.0, ptr::null()) };
        check_status(status)
    }

    /// Take a call off hold (re-INVITE with unhold).
    pub fn unhold_call(&self, call: CallId) -> Result<()> {
        let status = unsafe { ffi::pjsua_call_reinvite(call.0, PJSUA_CALL_UNHOLD, ptr::null()) };
        check_status(status)
    }

    /// Hang up all active calls.
    pub fn hangup_all(&self) -> Result<()> {
        unsafe { ffi::pjsua_call_hangup_all() };
        Ok(())
    }

    /// Transfer (REFER) a call to the given destination URI.
    pub fn transfer_call(&self, call: CallId, destination: &str) -> Result<()> {
        let dest = PjString::new(destination);
        let pj_dest = dest.as_pj_str();
        let status = unsafe { ffi::pjsua_call_xfer(call.0, &pj_dest, ptr::null()) };
        check_status(status)?;
        debug!("call transferred: call_id={} dest={destination}", call.0);
        Ok(())
    }

    /// Get the number of active calls.
    #[must_use]
    pub fn get_call_count(&self) -> u32 {
        unsafe { ffi::pjsua_call_get_count() }
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
            last_status_code: info.last_status,
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

    /// Adjust the transmit (TX) level of a conference port.
    ///
    /// `level` is a linear factor: 1.0 = no change, 0.0 = mute, 2.0 = double.
    pub fn conf_adjust_tx_level(&self, port: ConfPort, level: f32) -> Result<()> {
        let status = unsafe { ffi::pjsua_conf_adjust_tx_level(port.0, level) };
        check_status(status)
    }

    /// Adjust the receive (RX) level of a conference port.
    ///
    /// `level` is a linear factor: 1.0 = no change, 0.0 = mute, 2.0 = double.
    pub fn conf_adjust_rx_level(&self, port: ConfPort, level: f32) -> Result<()> {
        let status = unsafe { ffi::pjsua_conf_adjust_rx_level(port.0, level) };
        check_status(status)
    }

    /// Get the signal level of a conference port.
    ///
    /// Returns `(tx_level, rx_level)` as unsigned integer values (0-255).
    pub fn conf_get_signal_level(&self, port: ConfPort) -> Result<(u32, u32)> {
        let mut tx: u32 = 0;
        let mut rx: u32 = 0;
        let status = unsafe { ffi::pjsua_conf_get_signal_level(port.0, &mut tx, &mut rx) };
        check_status(status)?;
        Ok((tx, rx))
    }

    // ------------------------------------------------------------------
    // Conference bridge port management
    // ------------------------------------------------------------------

    /// Add a media port to the conference bridge and return its port ID
    /// and the pool allocated for the port.
    ///
    /// The caller **must** release the returned pool (via `pj_pool_release`)
    /// after calling `conf_remove_port`. Failing to do so leaks memory.
    /// [`CustomPort`](crate::CustomPort) and [`ToneGenerator`](crate::ToneGenerator) handle this automatically.
    ///
    /// # Safety
    /// The caller must ensure the `pjmedia_port` pointer is valid and remains
    /// alive for as long as it is registered with the conference bridge.
    pub unsafe fn conf_add_port(
        &self,
        port: *mut ffi::pjmedia_port,
    ) -> Result<(ConfPort, *mut ffi::pj_pool_t)> {
        let pool = unsafe { ffi::pjsua_pool_create(c"conf-port".as_ptr(), 512, 512) };
        if pool.is_null() {
            return Err(PjError::Pjsip {
                status: -1,
                message: "Failed to create pool for conf_add_port".into(),
            });
        }
        let mut port_id: ffi::pjsua_conf_port_id = -1;
        let status = unsafe { ffi::pjsua_conf_add_port(pool, port, &mut port_id) };
        if status != 0 {
            unsafe { ffi::pj_pool_release(pool) };
            return Err(make_pj_error(status));
        }
        debug!("conference port added: id={port_id}");
        Ok((ConfPort(port_id), pool))
    }

    /// Remove a port from the conference bridge.
    pub fn conf_remove_port(&self, port: ConfPort) -> Result<()> {
        let status = unsafe { ffi::pjsua_conf_remove_port(port.0) };
        check_status(status)?;
        debug!("conference port removed: id={}", port.0);
        Ok(())
    }

    /// Get information about a conference bridge port.
    pub fn conf_get_port_info(&self, port: ConfPort) -> Result<ConfPortInfo> {
        let mut info: ffi::pjsua_conf_port_info = Default::default();
        let status = unsafe { ffi::pjsua_conf_get_port_info(port.0, &mut info) };
        check_status(status)?;

        let listeners = (0..info.listener_cnt as usize)
            .map(|i| ConfPort(info.listeners[i]))
            .collect();

        Ok(ConfPortInfo {
            port: ConfPort(info.slot_id),
            name: pj_str_to_string(&info.name),
            clock_rate: info.clock_rate,
            channel_count: info.channel_count,
            samples_per_frame: info.samples_per_frame,
            bits_per_sample: info.bits_per_sample,
            tx_level_adj: info.tx_level_adj,
            rx_level_adj: info.rx_level_adj,
            listeners,
        })
    }

    /// List all active conference bridge port IDs.
    pub fn conf_enum_ports(&self) -> Result<Vec<ConfPort>> {
        let mut ids = [0i32; 254];
        let mut count = ids.len() as u32;
        let status = unsafe { ffi::pjsua_enum_conf_ports(ids.as_mut_ptr(), &mut count) };
        check_status(status)?;
        Ok(ids[..count as usize]
            .iter()
            .map(|&id| ConfPort(id))
            .collect())
    }

    // ------------------------------------------------------------------
    // Sound device management
    // ------------------------------------------------------------------

    /// Disconnect the conference bridge from the hardware sound device.
    pub fn set_no_sound_device(&self) -> Result<()> {
        let port = unsafe { ffi::pjsua_set_no_snd_dev() };
        if port.is_null() {
            return Err(PjError::Pjsip {
                status: -1,
                message: "pjsua_set_no_snd_dev returned null".into(),
            });
        }
        debug!("sound device disconnected from conference bridge");
        Ok(())
    }

    /// Connect conference bridge to specific sound device IDs (-1 for default).
    pub fn set_sound_device(&self, capture_dev: i32, playback_dev: i32) -> Result<()> {
        let status = unsafe { ffi::pjsua_set_snd_dev(capture_dev, playback_dev) };
        check_status(status)
    }

    /// Get current sound device IDs (capture, playback).
    pub fn get_sound_device(&self) -> Result<(i32, i32)> {
        let mut capture: i32 = -1;
        let mut playback: i32 = -1;
        let status = unsafe { ffi::pjsua_get_snd_dev(&mut capture, &mut playback) };
        check_status(status)?;
        Ok((capture, playback))
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
        let status = unsafe { ffi::pjsua_player_create(&pj_filename, options, &mut player_id) };
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

    /// Get the underlying media port of a player.
    ///
    /// # Safety
    /// The returned pointer is only valid while the player exists.
    /// The caller must not destroy the player while holding this pointer.
    pub fn player_get_port(&self, id: PlayerId) -> Result<*mut ffi::pjmedia_port> {
        let mut port: *mut ffi::pjmedia_port = std::ptr::null_mut();
        let status = unsafe { ffi::pjsua_player_get_port(id.0, &mut port) };
        check_status(status)?;
        Ok(port)
    }

    /// Register an EOF callback for a WAV file player.
    ///
    /// When the player reaches end-of-file, it sends the `PlayerId` through
    /// the provided channel. Only one callback per player — subsequent calls
    /// replace the previous callback.
    ///
    /// The callback fires from PJSIP's media thread, so we use
    /// `std::sync::mpsc` (not tokio) for the channel to avoid needing a
    /// tokio runtime context in the callback.
    ///
    /// # Safety
    /// The `tx` sender is leaked (boxed) so it lives as long as the player.
    /// It is cleaned up when the player is destroyed or when a new callback
    /// replaces it (via PJSIP internally).
    pub fn register_player_eof_callback(
        &self,
        id: PlayerId,
        tx: std::sync::mpsc::Sender<PlayerId>,
    ) -> Result<()> {
        let port = self.player_get_port(id)?;

        // Box the sender + player ID so they survive the callback
        let user_data = Box::new((tx, id));
        let user_data_ptr = Box::into_raw(user_data) as *mut std::ffi::c_void;

        unsafe {
            ffi::pjmedia_wav_player_set_eof_cb2(
                port,
                user_data_ptr,
                Some(player_eof_trampoline),
            );
        }
        debug!("EOF callback registered for player {}", id.0);
        Ok(())
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

    /// Send DTMF digits on a call.
    ///
    /// Uses `pjsua_call_dial_dtmf` for RFC 2833 and
    /// `pjsua_call_send_dtmf_param` for SIP-INFO.
    pub fn send_dtmf(&self, call: CallId, digits: &str, method: DtmfMethod) -> Result<()> {
        let digits_str = PjString::new(digits);
        let pj_digits = digits_str.as_pj_str();

        match method {
            DtmfMethod::Rfc2833 => {
                let status = unsafe { ffi::pjsua_call_dial_dtmf(call.0, &pj_digits) };
                check_status(status)
            }
            DtmfMethod::SipInfo => {
                let mut param: ffi::pjsua_call_send_dtmf_param = Default::default();
                unsafe { ffi::pjsua_call_send_dtmf_param_default(&mut param) };
                param.method = ffi::pjsua_dtmf_method_PJSUA_DTMF_METHOD_SIP_INFO;
                param.digits = pj_digits;
                let status = unsafe { ffi::pjsua_call_send_dtmf(call.0, &param) };
                check_status(status)
            }
        }
    }

    // ------------------------------------------------------------------
    // Codec
    // ------------------------------------------------------------------

    /// Verify that a string is a valid SIP URL.
    ///
    /// Returns `Ok(())` if valid, `Err(PjError::InvalidArg)` otherwise.
    pub fn verify_sip_url(&self, url: &str) -> Result<()> {
        let c_url = std::ffi::CString::new(url)
            .map_err(|_| PjError::InvalidArg("URL contains null byte".into()))?;
        let status = unsafe { ffi::pjsua_verify_sip_url(c_url.as_ptr()) };
        if status != 0 {
            return Err(PjError::InvalidArg(format!("invalid SIP URL: {url}")));
        }
        Ok(())
    }

    /// Check if the sound device is currently active.
    #[must_use]
    pub fn is_sound_active(&self) -> bool {
        unsafe { ffi::pjsua_snd_is_active() != 0 }
    }

    /// Stop PJSUA worker threads.
    ///
    /// This stops background threads that deliver callbacks, useful for
    /// ensuring no callbacks fire during teardown. Called automatically
    /// during `Drop`.
    pub fn stop_worker_threads(&self) {
        unsafe { ffi::pjsua_stop_worker_threads() };
    }

    /// Set the priority of a codec (0 = disabled, 255 = highest).
    pub fn set_codec_priority(&self, codec: &str, priority: u8) -> Result<()> {
        let codec_str = PjString::new(codec);
        let pj_codec = codec_str.as_pj_str();
        let status = unsafe { ffi::pjsua_codec_set_priority(&pj_codec, priority) };
        check_status(status)
    }
}

/// C trampoline for WAV player EOF callback.
///
/// Called from PJSIP's media thread. Sends the PlayerId through the
/// std::sync::mpsc channel, then frees the user_data.
unsafe extern "C" fn player_eof_trampoline(
    _port: *mut ffi::pjmedia_port,
    usr_data: *mut std::ffi::c_void,
) {
    let data = Box::from_raw(usr_data as *mut (std::sync::mpsc::Sender<PlayerId>, PlayerId));
    let (tx, player_id) = *data;
    // Best-effort send — if receiver is dropped, that's fine
    let _ = tx.send(player_id);
}

/// Build a `pjsua_acc_config` from an `AccountConfig`.
///
/// Returns the config and a `Vec<PjString>` that must be kept alive for the
/// duration of any FFI call that uses the config.
///
/// # String Lifetimes
///
/// PJSUA's `pjsua_acc_add` and `pjsua_acc_modify` **copy** all string data
/// from the config into an internal pool, so the returned `PjString` values
/// only need to survive through the FFI call itself. After the call returns,
/// PJSUA holds its own copies and does not reference the original pointers.
/// Populate a `pjsua_acc_config` in place from an `AccountConfig`.
///
/// The caller must allocate the `acc_cfg` on the stack and call
/// `pjsua_acc_config_default` before this function. This avoids moving the
/// struct after initialisation, which would invalidate self-referential
/// pointers in embedded linked-list headers (`reg_hdr_list`, `sub_hdr_list`).
///
/// Returns a `Vec<PjString>` that must be kept alive for the duration of any
/// FFI call that uses the config.
fn populate_acc_cfg(acc_cfg: &mut ffi::pjsua_acc_config, config: &AccountConfig) -> Vec<PjString> {
    let mut strings = Vec::new();

    let uri_str = PjString::new(&config.sip_uri());
    acc_cfg.id = uri_str.as_pj_str();
    strings.push(uri_str);

    let reg_str = PjString::new(&config.registrar_uri());
    acc_cfg.reg_uri = reg_str.as_pj_str();
    strings.push(reg_str);

    // Credentials
    let realm_val = config.realm.as_deref().unwrap_or("*");
    let realm_str = PjString::new(realm_val);
    let scheme_str = PjString::new("digest");
    let cred_username = config.auth_username.as_deref().unwrap_or(&config.username);
    let username_str = PjString::new(cred_username);
    let password_str = PjString::new(&config.password);

    acc_cfg.cred_count = 1;
    acc_cfg.cred_info[0].realm = realm_str.as_pj_str();
    acc_cfg.cred_info[0].scheme = scheme_str.as_pj_str();
    acc_cfg.cred_info[0].username = username_str.as_pj_str();
    acc_cfg.cred_info[0].data_type = 0; // PJSIP_CRED_DATA_PLAIN_PASSWD
    acc_cfg.cred_info[0].data = password_str.as_pj_str();
    strings.push(realm_str);
    strings.push(scheme_str);
    strings.push(username_str);
    strings.push(password_str);

    // SRTP
    acc_cfg.use_srtp = config.srtp.to_pjsip();

    // Disable NAT contact/via rewriting — keep the local IP in Contact and Via
    // headers and let the registrar use rport/received to track the NAT mapping.
    // Without this, PJSIP rewrites headers with the STUN-resolved public IP after
    // the first registration response, which breaks incoming calls on many PBXes.
    acc_cfg.allow_contact_rewrite = 0;
    acc_cfg.allow_via_rewrite = 0;

    // Registration timeout
    if let Some(timeout) = config.reg_timeout {
        acc_cfg.reg_timeout = timeout;
    }

    // Outbound proxy
    if let Some(ref proxy) = config.proxy {
        let proxy_str = PjString::new(proxy);
        acc_cfg.proxy_cnt = 1;
        acc_cfg.proxy[0] = proxy_str.as_pj_str();
        strings.push(proxy_str);
    }

    // Registration retry interval
    if let Some(interval) = config.reg_retry_interval {
        acc_cfg.reg_retry_interval = interval;
    }

    // RTP port range (for media transport)
    if let Some(start) = config.rtp_port_start {
        acc_cfg.rtp_cfg.port = start as u32;
        if let Some(range) = config.rtp_port_range {
            acc_cfg.rtp_cfg.port_range = range as u32;
        }
    }

    strings
}

// ---------------------------------------------------------------------------
// Thread registration (process-level, not per-PjsuaApp)
// ---------------------------------------------------------------------------

/// Register the calling thread with PJLIB.
///
/// PJLIB requires every thread that calls pjlib/pjsua functions to be
/// registered. The main thread is registered automatically during
/// `PjsuaApp::new()`, but any additional threads (e.g. tokio tasks
/// scheduled on a different OS thread) must call this before using
/// PJSIP APIs.
///
/// It is safe to call this multiple times — if the thread is already
/// registered, this is a no-op.
///
/// # Safety Note
///
/// The `pj_thread_desc` storage is leaked intentionally — it must remain
/// valid for the thread's lifetime, and Rust threads don't have a
/// convenient destructor hook. The 512-byte allocation is negligible.
pub fn register_thread(name: &str) -> Result<()> {
    unsafe {
        if ffi::pj_thread_is_registered() != 0 {
            return Ok(());
        }
        // Allocate thread descriptor on the heap and leak it — it must
        // remain valid for the lifetime of the thread.
        let desc: Box<ffi::pj_thread_desc> = Box::new(std::mem::zeroed());
        let desc_ptr = Box::into_raw(desc);
        let mut thread_ptr: *mut ffi::pj_thread_t = ptr::null_mut();

        let name_cstr =
            std::ffi::CString::new(name).unwrap_or_else(|_| std::ffi::CString::new("rust").unwrap());
        let status = ffi::pj_thread_register(
            name_cstr.as_ptr(),
            (*desc_ptr).as_mut_ptr(),
            &mut thread_ptr,
        );
        check_status(status)
    }
}

impl Drop for PjsuaApp {
    fn drop(&mut self) {
        info!("destroying PJSUA");
        // Teardown order matters for safety:
        //
        // 1. Stop worker threads — prevents NEW callbacks from being dispatched.
        // 2. Clear event channel — any in-flight callback that races past step 1
        //    will see None in EVENT_TX and silently drop the event.
        // 3. Destroy PJSUA — tears down all PJSIP state (accounts, calls, etc.).
        // 4. Reset the singleton flag — allows a new PjsuaApp to be created.
        unsafe { ffi::pjsua_stop_worker_threads() };
        event::clear_event_sender();
        unsafe { ffi::pjsua_destroy() };
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
    fn pjsua_app_is_debug() {
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<PjsuaApp>();
    }

    #[test]
    fn pjsua_app_needs_drop() {
        // Verify PjsuaApp implements Drop (RAII cleanup).
        assert!(std::mem::needs_drop::<PjsuaApp>());
    }
}
