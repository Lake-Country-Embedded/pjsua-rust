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
}

impl Default for PjsuaConfig {
    fn default() -> Self {
        PjsuaConfig {
            log_level: 3,
            clock_rate: 16000,
            null_audio: true,
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
            registrar: None,
            proxy: None,
            reg_retry_interval: None,
            rtp_port_start: None,
            rtp_port_range: None,
        }
    }
}

impl AccountConfig {
    /// Build the SIP URI for this account.
    ///
    /// Honors `display_name` and `use_sips`.
    pub fn sip_uri(&self) -> String {
        let scheme = if self.use_sips { "sips" } else { "sip" };
        match &self.display_name {
            Some(name) => format!(
                "\"{}\" <{}:{}@{}>",
                name, scheme, self.username, self.server
            ),
            None => format!("{}:{}@{}", scheme, self.username, self.server),
        }
    }

    /// Build the registrar URI.
    ///
    /// Returns the custom `registrar` if set, otherwise `sip:server:port`.
    pub fn registrar_uri(&self) -> String {
        if let Some(ref reg) = self.registrar {
            return reg.clone();
        }
        let scheme = if self.use_sips { "sips" } else { "sip" };
        format!("{}:{}:{}", scheme, self.server, self.port)
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
            name: "test".into(),
            username: "6013".into(),
            password: "secret".into(),
            server: "192.168.10.10".into(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: None,
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            registrar: None,
            proxy: None,
        };
        assert_eq!(cfg.sip_uri(), "sip:6013@192.168.10.10");
        assert_eq!(cfg.registrar_uri(), "sip:192.168.10.10:5060");
    }

    #[test]
    fn account_config_sip_uri_with_display_name() {
        let cfg = AccountConfig {
            name: "test".into(),
            username: "6013".into(),
            password: "secret".into(),
            server: "192.168.10.10".into(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: None,
            display_name: Some("Alice".into()),
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            registrar: None,
            proxy: None,
        };
        assert_eq!(cfg.sip_uri(), "\"Alice\" <sip:6013@192.168.10.10>");
    }

    #[test]
    fn account_config_use_sips() {
        let cfg = AccountConfig {
            name: "test".into(),
            username: "6013".into(),
            password: "secret".into(),
            server: "192.168.10.10".into(),
            port: 5061,
            transport: TransportType::Tls,
            srtp: SrtpMode::Disabled,
            access_code: None,
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: true,
            registrar: None,
            proxy: None,
        };
        assert_eq!(cfg.sip_uri(), "sips:6013@192.168.10.10");
        assert_eq!(cfg.registrar_uri(), "sips:192.168.10.10:5061");
    }

    #[test]
    fn account_config_custom_registrar() {
        let cfg = AccountConfig {
            name: "test".into(),
            username: "6013".into(),
            password: "secret".into(),
            server: "192.168.10.10".into(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: None,
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            registrar: Some("sip:registrar.example.com".into()),
            proxy: None,
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
            name: "receiver".into(),
            username: "6014".into(),
            password: "secret".into(),
            server: "192.168.10.10".into(),
            port: 5060,
            transport: TransportType::Udp,
            srtp: SrtpMode::Disabled,
            access_code: Some("1234".into()),
            display_name: None,
            auth_username: None,
            realm: None,
            reg_timeout: None,
            use_sips: false,
            registrar: None,
            proxy: None,
        };
        assert_eq!(cfg.access_code.as_deref(), Some("1234"));
    }
}
