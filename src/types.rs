use serde::Deserialize;
use std::fmt;

use crate::ffi;

// ---------------------------------------------------------------------------
// Opaque IDs (newtypes for type safety)
// ---------------------------------------------------------------------------

/// A PJSUA account ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountId(pub i32);

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A PJSUA call ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallId(pub i32);

impl fmt::Display for CallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A PJSUA transport ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportId(pub i32);

impl fmt::Display for TransportId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A conference bridge port number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfPort(pub i32);

impl fmt::Display for ConfPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ConfPort {
    /// The master/sound device conference port (port 0).
    pub const MASTER: ConfPort = ConfPort(0);
}

/// A WAV player ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub i32);

impl fmt::Display for PlayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A WAV recorder ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecorderId(pub i32);

impl fmt::Display for RecorderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// SIP transport type.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    #[default]
    Udp,
    Tcp,
    Tls,
}

impl TransportType {
    /// Convert to the PJSIP C enum value.
    pub fn to_pjsip(self) -> u32 {
        match self {
            TransportType::Udp => ffi::pjsip_transport_type_e_PJSIP_TRANSPORT_UDP,
            TransportType::Tcp => ffi::pjsip_transport_type_e_PJSIP_TRANSPORT_TCP,
            TransportType::Tls => ffi::pjsip_transport_type_e_PJSIP_TRANSPORT_TLS,
        }
    }
}

impl fmt::Display for TransportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportType::Udp => write!(f, "udp"),
            TransportType::Tcp => write!(f, "tcp"),
            TransportType::Tls => write!(f, "tls"),
        }
    }
}

/// SRTP usage mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SrtpMode {
    #[default]
    Disabled,
    Optional,
    Mandatory,
}

impl SrtpMode {
    /// Convert to the PJSIP C enum value.
    pub fn to_pjsip(self) -> u32 {
        match self {
            SrtpMode::Disabled => ffi::pjmedia_srtp_use_PJMEDIA_SRTP_DISABLED,
            SrtpMode::Optional => ffi::pjmedia_srtp_use_PJMEDIA_SRTP_OPTIONAL,
            SrtpMode::Mandatory => ffi::pjmedia_srtp_use_PJMEDIA_SRTP_MANDATORY,
        }
    }
}

impl fmt::Display for SrtpMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SrtpMode::Disabled => write!(f, "disabled"),
            SrtpMode::Optional => write!(f, "optional"),
            SrtpMode::Mandatory => write!(f, "mandatory"),
        }
    }
}

/// Minimum signalling-security level required before SRTP is offered.
///
/// Maps to `pjsua_acc_config.srtp_secure_signaling` (0/1/2). PJSIP
/// computes a "security level" for the signalling path inside
/// `pjsua_call.c::get_secure_level()` and only offers/accepts SRTP when
/// the computed level is at least the configured minimum. Some
/// outbound-proxy + TLS combinations are not recognised as TLS by that
/// computation even when the wire is TLS-encrypted; with `TlsOrSips`
/// (PJSIP default) such accounts silently fall back to plain RTP/AVP.
/// Lower to `Any` to disable the gating check; signalling is still
/// TLS-encrypted because the transport itself is TLS.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SrtpSecureSignaling {
    /// Accept SRTP regardless of signalling transport.
    Any,
    /// Require TLS or `sips:` signalling (PJSIP default).
    #[default]
    TlsOrSips,
    /// Require end-to-end `sips:` signalling.
    EndToEndSips,
}

impl SrtpSecureSignaling {
    /// Numeric value passed to `pjsua_acc_config.srtp_secure_signaling`.
    pub fn to_pjsip(self) -> u32 {
        match self {
            SrtpSecureSignaling::Any => 0,
            SrtpSecureSignaling::TlsOrSips => 1,
            SrtpSecureSignaling::EndToEndSips => 2,
        }
    }
}

impl fmt::Display for SrtpSecureSignaling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SrtpSecureSignaling::Any => write!(f, "any"),
            SrtpSecureSignaling::TlsOrSips => write!(f, "tls_or_sips"),
            SrtpSecureSignaling::EndToEndSips => write!(f, "end_to_end_sips"),
        }
    }
}

/// Mirrors PJSIP's `pjsua_100rel_use` enum.
///
/// Controls the use of the 100rel / PRACK extension (RFC 3262) on this
/// account. Some SBCs (notably RingCentral) send a placeholder
/// unreliable `183 Session Progress` with `m=audio 0 a=inactive` SDP when
/// the call is in early-dialog routing — and PJSUA auto-cancels because
/// SDP negotiation produces zero active streams. Asking for
/// `Optional` advertises `Supported: 100rel` in the outbound INVITE,
/// which can prompt those SBCs to switch to reliable provisional
/// responses with proper SDP and avoid the auto-cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Use100Rel {
    /// PJSIP default. UAC still advertises `Supported: 100rel`; UAS
    /// will not actively use it.
    #[default]
    NotUsed,
    /// As UAC, require the peer to use 100rel. As UAS, only accept
    /// peers that advertise 100rel support.
    Mandatory,
    /// As UAC, advertise `Supported: 100rel` and allow the peer to
    /// opt in. As UAS, use 100rel for any 18x response.
    Optional,
}

impl Use100Rel {
    /// PJSIP C enum value.
    pub fn to_pjsip(self) -> u32 {
        match self {
            Use100Rel::NotUsed => 0,
            Use100Rel::Mandatory => 1,
            Use100Rel::Optional => 2,
        }
    }
}

/// Per-call options that don't belong on the account.
///
/// Passed to [`PjsuaApp::make_call_with_setting`](crate::PjsuaApp::make_call_with_setting).
/// Defaults are equivalent to plain
/// [`PjsuaApp::make_call`](crate::PjsuaApp::make_call).
#[derive(Debug, Clone, Copy, Default)]
pub struct CallSetting {
    /// Send the INVITE without an SDP body. The peer responds with a
    /// 200 OK that contains the SDP offer; we answer in ACK. Useful
    /// when the peer's early-dialog SDP behavior trips PJSUA's
    /// auto-cancel path (RingCentral's unreliable 183 with
    /// `m=audio 0 a=inactive`).
    pub late_offer: bool,
}

/// DTMF sending method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtmfMethod {
    Rfc2833,
    SipInfo,
}

impl fmt::Display for DtmfMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtmfMethod::Rfc2833 => write!(f, "RFC2833"),
            DtmfMethod::SipInfo => write!(f, "SIP-INFO"),
        }
    }
}

/// SIP call state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Null,
    Calling,
    Incoming,
    Early,
    Connecting,
    Confirmed,
    Disconnected,
    Unknown(u32),
}

impl CallState {
    /// Convert from the PJSIP C enum value.
    pub fn from_pjsip(state: u32) -> Self {
        match state {
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_NULL => CallState::Null,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_CALLING => CallState::Calling,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_INCOMING => CallState::Incoming,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_EARLY => CallState::Early,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_CONNECTING => CallState::Connecting,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_CONFIRMED => CallState::Confirmed,
            x if x == ffi::pjsip_inv_state_PJSIP_INV_STATE_DISCONNECTED => CallState::Disconnected,
            other => CallState::Unknown(other),
        }
    }
}

impl fmt::Display for CallState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallState::Null => write!(f, "NULL"),
            CallState::Calling => write!(f, "CALLING"),
            CallState::Incoming => write!(f, "INCOMING"),
            CallState::Early => write!(f, "EARLY"),
            CallState::Connecting => write!(f, "CONNECTING"),
            CallState::Confirmed => write!(f, "CONFIRMED"),
            CallState::Disconnected => write!(f, "DISCONNECTED"),
            CallState::Unknown(n) => write!(f, "UNKNOWN({n})"),
        }
    }
}

/// Media status for a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaStatus {
    None,
    Active,
    LocalHold,
    RemoteHold,
    Error,
    Unknown(u32),
}

impl MediaStatus {
    /// Convert from the PJSIP C enum value.
    pub fn from_pjsip(status: u32) -> Self {
        match status {
            x if x == ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_NONE => MediaStatus::None,
            x if x == ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_ACTIVE => MediaStatus::Active,
            x if x == ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_LOCAL_HOLD => {
                MediaStatus::LocalHold
            }
            x if x == ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_REMOTE_HOLD => {
                MediaStatus::RemoteHold
            }
            x if x == ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_ERROR => MediaStatus::Error,
            other => MediaStatus::Unknown(other),
        }
    }
}

impl fmt::Display for MediaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaStatus::None => write!(f, "NONE"),
            MediaStatus::Active => write!(f, "ACTIVE"),
            MediaStatus::LocalHold => write!(f, "LOCAL_HOLD"),
            MediaStatus::RemoteHold => write!(f, "REMOTE_HOLD"),
            MediaStatus::Error => write!(f, "ERROR"),
            MediaStatus::Unknown(n) => write!(f, "UNKNOWN({n})"),
        }
    }
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

/// High-level call information.
#[derive(Debug, Clone)]
pub struct CallInfo {
    pub call_id: CallId,
    pub account_id: AccountId,
    pub remote_uri: String,
    pub state: CallState,
    pub media_status: MediaStatus,
    pub conf_port: ConfPort,
    pub connect_duration_ms: u64,
    pub total_duration_ms: u64,
    pub last_status_code: u32,
    /// SIP `Call-ID` header value for this call. Stable for the lifetime of
    /// the dialog and unique per call. Useful for correlating
    /// [`SipEvent::SipMessageTrace`] events (which only carry the SIP
    /// `Call-ID`, not the PJSUA `call_id`) with this call.
    pub sip_call_id: String,
}

/// High-level account information.
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub account_id: AccountId,
    pub uri: String,
    pub is_registered: bool,
    pub status_code: u32,
    pub status_text: String,
}

/// Information about a conference bridge port.
#[derive(Debug, Clone)]
pub struct ConfPortInfo {
    pub port: ConfPort,
    pub name: String,
    pub clock_rate: u32,
    pub channel_count: u32,
    pub samples_per_frame: u32,
    pub bits_per_sample: u32,
    pub tx_level_adj: f32,
    pub rx_level_adj: f32,
    pub listeners: Vec<ConfPort>,
}

/// TLS transport configuration.
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    /// Path to CA certificate list file.
    pub ca_list_file: Option<String>,
    /// Path to a directory of CA certificates (e.g. `/etc/ssl/certs/`).
    pub ca_list_path: Option<String>,
    /// Path to the certificate file.
    pub cert_file: Option<String>,
    /// Path to the private key file.
    pub privkey_file: Option<String>,
    /// Password for the private key.
    pub password: Option<String>,
    /// Verify server certificate.
    pub verify_server: bool,
    /// Verify client certificate.
    pub verify_client: bool,
    /// Explicit OpenSSL security level (0-5) to force on the TLS transport via
    /// the cipher list `DEFAULT:@SECLEVEL=<n>`, or `None` to leave PJSIP's
    /// default ciphers and OpenSSL's built-in security level in place. Level 1
    /// accepts the weak (1024-bit) Diffie-Hellman parameters that level 2
    /// rejects with "dh key too small".
    pub security_level: Option<u8>,
}

/// Top-level PJSUA configuration.
#[derive(Debug, Clone)]
pub struct PjsuaConfig {
    /// Logging level (0-6).
    pub log_level: u32,
    /// Clock rate in Hz (e.g. 16000).
    pub clock_rate: u32,
    /// Use null audio device (no real audio hardware).
    pub null_audio: bool,
    /// Optional User-Agent string sent on outbound SIP requests and as the
    /// Server header on responses. When `None`, PJSIP's built-in default is
    /// used. Set at `Pjsua::new` time; cannot be changed after init.
    pub user_agent: Option<String>,
    /// DNS nameservers to populate into PJSIP's **async** resolver.
    ///
    /// When this list is non-empty, pjsua configures
    /// `pjsua_config::nameserver` / `nameserver_count` and PJSIP
    /// spins up its own `pj_dns_resolver` that performs async DNS
    /// over UDP using PJLIB's I/O queue. All subsequent pjsua calls
    /// that need DNS resolution (`pjsua_acc_add`,
    /// `pjsua_acc_set_registration`, outbound SIP calls) go through
    /// this resolver instead of calling `getaddrinfo` synchronously
    /// on the caller's thread. Blocking `getaddrinfo` on a bad
    /// network is the classic cause of multi-second hangs in pjsua
    /// account operations; the async resolver eliminates that class
    /// of hang entirely.
    ///
    /// When empty, pjsua falls back to synchronous `getaddrinfo`
    /// via the platform resolver (previous behavior).
    ///
    /// Maximum of 4 entries — pjsua's `nameserver[]` array is fixed
    /// at compile time. Extras are ignored with a warning.
    pub nameservers: Vec<String>,
}

/// Direction of a traced SIP message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceDirection {
    Sent,
    Received,
}

/// Extracted information from a SIP message for tracing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SipMessageInfo {
    pub direction: TraceDirection,
    pub method_or_status: String,
    pub sip_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdp: Option<String>,
    pub headers: std::collections::HashMap<String, String>,
    /// Verbatim message text. Only populated for REGISTER transactions — see
    /// `is_register_msg` in `trace_module.c`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl Default for PjsuaConfig {
    fn default() -> Self {
        PjsuaConfig {
            log_level: 3,
            clock_rate: 16000,
            null_audio: true,
            user_agent: None,
            nameservers: Vec::new(),
        }
    }
}

/// Configuration for a single SIP account.
#[derive(Debug, Clone)]
pub struct AccountConfig {
    /// Friendly name for this account.
    pub name: String,
    /// SIP username.
    pub username: String,
    /// SIP password.
    pub password: String,
    /// SIP server hostname or IP.
    pub server: String,
    /// SIP port.
    pub port: u16,
    /// Transport type (udp, tcp, tls).
    pub transport: TransportType,
    /// SRTP mode.
    pub srtp: SrtpMode,
    /// Optional access code (DTMF digits to send after call connects).
    pub access_code: Option<String>,
    /// Display name for the From header (e.g. "Alice").
    pub display_name: Option<String>,
    /// Authentication username (defaults to `username` if None).
    pub auth_username: Option<String>,
    /// Authentication realm (defaults to `"*"` if None).
    pub realm: Option<String>,
    /// Registration timeout in seconds.
    pub reg_timeout: Option<u32>,
    /// Use `sips:` URI scheme instead of `sip:`.
    pub use_sips: bool,
    /// Resolve the registrar via DNS SRV instead of a fixed `host:port`.
    ///
    /// When `true`, [`AccountConfig::registrar_uri`] omits the port so PJSIP does
    /// an RFC 3263 NAPTR/SRV lookup (with A-record fallback). No effect when a
    /// custom `registrar` is set or `server` already has an explicit port.
    pub dns_srv: bool,
    /// Override the auto-built registrar URI.
    pub registrar: Option<String>,
    /// Outbound proxy URI.
    pub proxy: Option<String>,
    /// Registration retry interval in seconds.
    pub reg_retry_interval: Option<u32>,
    /// RTP port start (set on `rtp_cfg.port`).
    pub rtp_port_start: Option<u16>,
    /// RTP port range (set on `rtp_cfg.port_range`).
    pub rtp_port_range: Option<u16>,
    /// Pin this account to a specific PJSUA transport ID.
    ///
    /// When `Some`, the value is written directly to `pjsua_acc_config.transport_id`,
    /// overriding PJSIP's auto-resolution logic.  This is required for TLS accounts
    /// because auto-resolution fails with `PJSIP_EUNSUPTRANSPORT` when the TLS
    /// transport has not yet been matched to the URI scheme.  Set to `None` to keep
    /// the default (`PJSUA_INVALID_ID = -1`) and let PJSIP auto-resolve.
    pub transport_id: Option<i32>,
    /// 100rel (PRACK) usage for this account. `None` keeps PJSIP's
    /// `pjsua_acc_config_default()` value (`PJSUA_100REL_NOT_USED`).
    /// Set to `Some(Use100Rel::Optional)` to advertise
    /// `Supported: 100rel` in outbound INVITEs — useful when the peer
    /// (e.g. RingCentral) sends an unreliable provisional 18x with
    /// inactive SDP and PJSUA's negotiator otherwise auto-cancels.
    pub require_100rel: Option<Use100Rel>,
    /// SRTP secure-signalling requirement (pass-through to
    /// `pjsua_acc_config.srtp_secure_signaling`).
    ///
    /// `None` keeps PJSIP's default (`TlsOrSips`). Set to
    /// `Some(SrtpSecureSignaling::Any)` for accounts where `use_srtp`
    /// is set but PJSIP's `get_secure_level` doesn't recognise the
    /// signalling path as secure (e.g. TLS transport plus an
    /// outbound-proxy URI of the form `sip:host:port;transport=tls`).
    /// Without it, PJSIP silently skips SRTP setup and the outbound
    /// INVITE goes out as plain `RTP/AVP`, which an SRTP-only peer
    /// rejects with `m=audio 0 a=inactive` in its 18x/200 OK SDP.
    pub srtp_secure_signaling: Option<SrtpSecureSignaling>,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            name: String::new(),
            username: String::new(),
            password: String::new(),
            server: String::new(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: None,
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            dns_srv: false,
            registrar: None,
            proxy: None,
            reg_retry_interval: None,
            rtp_port_start: None,
            rtp_port_range: None,
            transport_id: None,
            require_100rel: None,
            srtp_secure_signaling: None,
        }
    }
}

impl AccountConfig {
    /// SIP URI transport parameter suffix for non-UDP transports.
    ///
    /// PJSIP needs `;transport=tcp` or `;transport=tls` on SIP URIs to route
    /// signalling through the correct transport. UDP is the default and needs
    /// no parameter. The `sips:` scheme already implies TLS, so the parameter
    /// is only added for plain `sip:` URIs with TLS transport.
    fn transport_param(&self) -> &'static str {
        match self.transport {
            TransportType::Tcp => ";transport=tcp",
            TransportType::Tls if !self.use_sips => ";transport=tls",
            _ => "",
        }
    }

    /// Build the SIP URI for this account.
    ///
    /// Honors `display_name`, `use_sips`, and `transport`.
    pub fn sip_uri(&self) -> String {
        let scheme = if self.use_sips { "sips" } else { "sip" };
        let tp = self.transport_param();
        match &self.display_name {
            Some(name) => format!(
                "\"{}\" <{}:{}@{}{}>",
                name, scheme, self.username, self.server, tp
            ),
            None => format!("{}:{}@{}{}", scheme, self.username, self.server, tp),
        }
    }

    /// Build the registrar URI.
    ///
    /// Returns the custom `registrar` if set, otherwise `sip:server:port`.
    /// Appends `;transport=tcp` or `;transport=tls` for non-UDP transports.
    ///
    /// When `dns_srv` is set, the port is omitted so PJSIP resolves via SRV; the
    /// transport parameter is kept so the SRV service tag matches the transport.
    pub fn registrar_uri(&self) -> String {
        if let Some(ref reg) = self.registrar {
            return reg.clone();
        }
        let scheme = if self.use_sips { "sips" } else { "sip" };
        let tp = self.transport_param();
        if self.dns_srv {
            format!("{}:{}{}", scheme, self.server, tp)
        } else {
            format!("{}:{}:{}{}", scheme, self.server, self.port, tp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_port_master() {
        assert_eq!(ConfPort::MASTER.0, 0);
    }

    #[test]
    fn account_id_equality() {
        assert_eq!(AccountId(1), AccountId(1));
        assert_ne!(AccountId(1), AccountId(2));
    }

    #[test]
    fn call_id_equality() {
        assert_eq!(CallId(0), CallId(0));
        assert_ne!(CallId(0), CallId(1));
    }

    #[test]
    fn transport_type_to_pjsip() {
        // Just verify the conversions don't panic and produce distinct values
        let udp = TransportType::Udp.to_pjsip();
        let tcp = TransportType::Tcp.to_pjsip();
        let tls = TransportType::Tls.to_pjsip();
        assert_ne!(udp, tcp);
        assert_ne!(tcp, tls);
    }

    #[test]
    fn transport_type_display() {
        assert_eq!(format!("{}", TransportType::Udp), "udp");
        assert_eq!(format!("{}", TransportType::Tcp), "tcp");
        assert_eq!(format!("{}", TransportType::Tls), "tls");
    }

    #[test]
    fn transport_type_deserialize() {
        #[derive(Deserialize)]
        struct W {
            t: TransportType,
        }
        let w: W = toml::from_str("t = \"udp\"").unwrap();
        assert_eq!(w.t, TransportType::Udp);
    }

    #[test]
    fn srtp_mode_to_pjsip() {
        let disabled = SrtpMode::Disabled.to_pjsip();
        let optional = SrtpMode::Optional.to_pjsip();
        let mandatory = SrtpMode::Mandatory.to_pjsip();
        assert_ne!(disabled, optional);
        assert_ne!(optional, mandatory);
    }

    #[test]
    fn srtp_mode_deserialize() {
        #[derive(Deserialize)]
        struct W {
            s: SrtpMode,
        }
        let w: W = toml::from_str("s = \"disabled\"").unwrap();
        assert_eq!(w.s, SrtpMode::Disabled);
    }

    #[test]
    fn registrar_uri_a_record_includes_port() {
        // Default (dns_srv = false): explicit port is present, suppressing SRV.
        let cfg = AccountConfig {
            server: "sip.example.com".into(),
            port: 5060,
            ..Default::default()
        };
        assert_eq!(cfg.registrar_uri(), "sip:sip.example.com:5060");
    }

    #[test]
    fn registrar_uri_srv_udp_omits_port() {
        // UDP + SRV: bare host, no transport param, so PJSIP queries _sip._udp.
        let cfg = AccountConfig {
            server: "sip.example.com".into(),
            port: 5060,
            dns_srv: true,
            ..Default::default()
        };
        assert_eq!(cfg.registrar_uri(), "sip:sip.example.com");
    }

    #[test]
    fn registrar_uri_srv_tls_keeps_transport_omits_port() {
        // TLS + SRV: transport param retained so the SRV service tag matches,
        // but the explicit port is dropped so resolution goes via SRV.
        let cfg = AccountConfig {
            server: "sip.example.com".into(),
            port: 5061,
            transport: TransportType::Tls,
            dns_srv: true,
            ..Default::default()
        };
        assert_eq!(cfg.registrar_uri(), "sip:sip.example.com;transport=tls");
    }

    #[test]
    fn registrar_uri_custom_registrar_ignores_dns_srv() {
        // An explicit registrar override always wins, SRV flag notwithstanding.
        let cfg = AccountConfig {
            server: "sip.example.com".into(),
            registrar: Some("sip:reg.example.com:5070".into()),
            dns_srv: true,
            ..Default::default()
        };
        assert_eq!(cfg.registrar_uri(), "sip:reg.example.com:5070");
    }

    #[test]
    fn call_state_from_pjsip() {
        assert_eq!(
            CallState::from_pjsip(ffi::pjsip_inv_state_PJSIP_INV_STATE_CONFIRMED),
            CallState::Confirmed
        );
        assert_eq!(
            CallState::from_pjsip(ffi::pjsip_inv_state_PJSIP_INV_STATE_DISCONNECTED),
            CallState::Disconnected
        );
    }

    #[test]
    fn call_state_unknown() {
        assert_eq!(CallState::from_pjsip(99999), CallState::Unknown(99999));
    }

    #[test]
    fn call_state_display() {
        assert_eq!(format!("{}", CallState::Confirmed), "CONFIRMED");
        assert_eq!(format!("{}", CallState::Unknown(42)), "UNKNOWN(42)");
    }

    #[test]
    fn media_status_from_pjsip() {
        assert_eq!(
            MediaStatus::from_pjsip(ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_ACTIVE),
            MediaStatus::Active
        );
        assert_eq!(
            MediaStatus::from_pjsip(ffi::pjsua_call_media_status_PJSUA_CALL_MEDIA_NONE),
            MediaStatus::None
        );
    }

    #[test]
    fn media_status_unknown() {
        assert_eq!(MediaStatus::from_pjsip(99999), MediaStatus::Unknown(99999));
    }

    #[test]
    fn tls_config_default() {
        let cfg = TlsConfig::default();
        assert!(cfg.ca_list_file.is_none());
        assert!(cfg.ca_list_path.is_none());
        assert!(cfg.cert_file.is_none());
        assert!(cfg.privkey_file.is_none());
        assert!(cfg.password.is_none());
        assert!(!cfg.verify_server);
        assert!(!cfg.verify_client);
    }

    #[test]
    fn pjsua_config_default() {
        let cfg = PjsuaConfig::default();
        assert_eq!(cfg.log_level, 3);
        assert_eq!(cfg.clock_rate, 16000);
        assert!(cfg.null_audio);
    }

    #[test]
    fn account_config_sip_uri() {
        let cfg = AccountConfig {
            username: "6013".into(),
            server: "192.168.10.10".into(),
            ..Default::default()
        };
        assert_eq!(cfg.sip_uri(), "sip:6013@192.168.10.10");
        assert_eq!(cfg.registrar_uri(), "sip:192.168.10.10:5060");
    }

    #[test]
    fn account_config_sip_uri_with_display_name() {
        let cfg = AccountConfig {
            username: "6013".into(),
            server: "192.168.10.10".into(),
            display_name: Some("Alice".into()),
            ..Default::default()
        };
        assert_eq!(cfg.sip_uri(), "\"Alice\" <sip:6013@192.168.10.10>");
    }

    #[test]
    fn account_config_use_sips() {
        let cfg = AccountConfig {
            username: "6013".into(),
            server: "192.168.10.10".into(),
            port: 5061,
            transport: TransportType::Tls,
            use_sips: true,
            ..Default::default()
        };
        // sips: scheme implies TLS — no ;transport= needed
        assert_eq!(cfg.sip_uri(), "sips:6013@192.168.10.10");
        assert_eq!(cfg.registrar_uri(), "sips:192.168.10.10:5061");
    }

    #[test]
    fn account_config_tls_without_sips() {
        let cfg = AccountConfig {
            username: "user".into(),
            server: "sip.linphone.org".into(),
            port: 5061,
            transport: TransportType::Tls,
            use_sips: false,
            ..Default::default()
        };
        // TLS transport without sips: scheme needs ;transport=tls
        assert_eq!(cfg.sip_uri(), "sip:user@sip.linphone.org;transport=tls");
        assert_eq!(
            cfg.registrar_uri(),
            "sip:sip.linphone.org:5061;transport=tls"
        );
    }

    #[test]
    fn account_config_tcp_transport() {
        let cfg = AccountConfig {
            username: "user".into(),
            server: "pbx.example.com".into(),
            transport: TransportType::Tcp,
            ..Default::default()
        };
        assert_eq!(cfg.sip_uri(), "sip:user@pbx.example.com;transport=tcp");
        assert_eq!(
            cfg.registrar_uri(),
            "sip:pbx.example.com:5060;transport=tcp"
        );
    }

    #[test]
    fn account_config_custom_registrar() {
        let cfg = AccountConfig {
            username: "6013".into(),
            server: "192.168.10.10".into(),
            registrar: Some("sip:registrar.example.com".into()),
            ..Default::default()
        };
        assert_eq!(cfg.registrar_uri(), "sip:registrar.example.com");
    }

    #[test]
    fn media_status_display() {
        assert_eq!(format!("{}", MediaStatus::Active), "ACTIVE");
        assert_eq!(format!("{}", MediaStatus::None), "NONE");
        assert_eq!(format!("{}", MediaStatus::LocalHold), "LOCAL_HOLD");
        assert_eq!(format!("{}", MediaStatus::RemoteHold), "REMOTE_HOLD");
        assert_eq!(format!("{}", MediaStatus::Error), "ERROR");
        assert_eq!(format!("{}", MediaStatus::Unknown(42)), "UNKNOWN(42)");
    }

    #[test]
    fn srtp_mode_display() {
        assert_eq!(format!("{}", SrtpMode::Disabled), "disabled");
        assert_eq!(format!("{}", SrtpMode::Optional), "optional");
        assert_eq!(format!("{}", SrtpMode::Mandatory), "mandatory");
    }

    #[test]
    fn dtmf_method_display() {
        assert_eq!(format!("{}", DtmfMethod::Rfc2833), "RFC2833");
        assert_eq!(format!("{}", DtmfMethod::SipInfo), "SIP-INFO");
    }

    #[test]
    fn id_display() {
        assert_eq!(format!("{}", AccountId(5)), "5");
        assert_eq!(format!("{}", CallId(3)), "3");
        assert_eq!(format!("{}", TransportId(1)), "1");
        assert_eq!(format!("{}", ConfPort(0)), "0");
        assert_eq!(format!("{}", PlayerId(2)), "2");
        assert_eq!(format!("{}", RecorderId(7)), "7");
    }

    #[test]
    fn account_config_default() {
        let cfg = AccountConfig::default();
        assert_eq!(cfg.port, 5060);
        assert_eq!(cfg.transport, TransportType::Udp);
        assert_eq!(cfg.srtp, SrtpMode::Disabled);
        assert!(!cfg.use_sips);
        assert!(cfg.access_code.is_none());
        assert!(cfg.display_name.is_none());
        assert!(cfg.auth_username.is_none());
        assert!(cfg.realm.is_none());
        assert!(cfg.reg_timeout.is_none());
        assert!(cfg.registrar.is_none());
        assert!(cfg.proxy.is_none());
    }

    #[test]
    fn account_config_with_access_code() {
        let cfg = AccountConfig {
            access_code: Some("1234".into()),
            ..Default::default()
        };
        assert_eq!(cfg.access_code.as_deref(), Some("1234"));
    }
}
