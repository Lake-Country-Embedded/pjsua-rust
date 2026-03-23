//! TOML-based configuration for PJSUA accounts and general settings.

use crate::error::{PjError, Result};
use crate::types::*;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Default helpers
// ---------------------------------------------------------------------------

fn default_log_level() -> u32 {
    3
}
fn default_clock_rate() -> u32 {
    16000
}
fn default_true() -> bool {
    true
}
fn default_port() -> u16 {
    5060
}

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

/// Top-level TOML configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub accounts: Vec<TomlAccountConfig>,
}

/// General PJSUA settings from the `[general]` TOML section.
#[derive(Debug, Clone, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_log_level")]
    pub log_level: u32,
    #[serde(default = "default_clock_rate")]
    pub clock_rate: u32,
    #[serde(default = "default_true")]
    pub null_audio: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        GeneralConfig {
            log_level: default_log_level(),
            clock_rate: default_clock_rate(),
            null_audio: default_true(),
        }
    }
}

/// A single SIP account entry from `[[accounts]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct TomlAccountConfig {
    pub name: String,
    pub username: String,
    pub password: String,
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub transport: TransportType,
    #[serde(default)]
    pub srtp: SrtpMode,
    pub display_name: Option<String>,
    pub auth_username: Option<String>,
    pub realm: Option<String>,
    pub reg_timeout: Option<u32>,
    #[serde(default)]
    pub use_sips: bool,
    pub registrar: Option<String>,
    pub proxy: Option<String>,
    pub access_code: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversions and loading
// ---------------------------------------------------------------------------

impl TomlAccountConfig {
    /// Convert to the runtime [`AccountConfig`] used by [`PjsuaApp`](crate::PjsuaApp).
    pub fn to_account_config(&self) -> AccountConfig {
        AccountConfig {
            name: self.name.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            server: self.server.clone(),
            port: self.port,
            transport: self.transport,
            srtp: self.srtp,
            access_code: self.access_code.clone(),
            display_name: self.display_name.clone(),
            auth_username: self.auth_username.clone(),
            realm: self.realm.clone(),
            reg_timeout: self.reg_timeout,
            use_sips: self.use_sips,
            registrar: self.registrar.clone(),
            proxy: self.proxy.clone(),
            reg_retry_interval: None,
            rtp_port_start: None,
            rtp_port_range: None,
            transport_id: None,
        }
    }
}

impl std::str::FromStr for Config {
    type Err = PjError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| PjError::Config(format!("TOML parse error: {e}")))
    }
}

impl Config {
    /// Load configuration from a TOML file on disk.
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> Result<Config> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            PjError::Config(format!("failed to read {}: {}", path.as_ref().display(), e))
        })?;
        Self::from_str(&content)
    }

    /// Parse configuration from a TOML string.
    pub fn from_str(toml_str: &str) -> Result<Config> {
        <Self as std::str::FromStr>::from_str(toml_str)
    }

    /// Convert the `[general]` section into a [`PjsuaConfig`].
    pub fn to_pjsua_config(&self) -> PjsuaConfig {
        PjsuaConfig {
            log_level: self.general.log_level,
            clock_rate: self.general.clock_rate,
            null_audio: self.general.null_audio,
        }
    }

    /// Find an account by its `name` field.
    pub fn find_account(&self, name: &str) -> Option<&TomlAccountConfig> {
        self.accounts.iter().find(|a| a.name == name)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
[general]
log_level = 3
clock_rate = 16000
null_audio = true

[[accounts]]
name = "caller"
username = "6013"
password = "123456"
server = "192.168.10.10"
port = 5060
transport = "udp"
srtp = "disabled"

[[accounts]]
name = "receiver"
username = "6014"
password = "123456"
server = "192.168.10.10"
port = 5060
transport = "udp"
srtp = "disabled"
access_code = "1234"
"#;

    #[test]
    fn test_parse_full_config() {
        let config = Config::from_str(SAMPLE_TOML).unwrap();
        assert_eq!(config.general.log_level, 3);
        assert_eq!(config.general.clock_rate, 16000);
        assert!(config.general.null_audio);
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[0].name, "caller");
        assert_eq!(config.accounts[1].name, "receiver");
    }

    #[test]
    fn test_find_account() {
        let config = Config::from_str(SAMPLE_TOML).unwrap();
        let caller = config.find_account("caller");
        assert!(caller.is_some());
        assert_eq!(caller.unwrap().username, "6013");

        let missing = config.find_account("nonexistent");
        assert!(missing.is_none());
    }

    #[test]
    fn test_account_config_conversion() {
        let config = Config::from_str(SAMPLE_TOML).unwrap();
        let acct = config.accounts[0].to_account_config();
        assert_eq!(acct.name, "caller");
        assert_eq!(acct.username, "6013");
        assert_eq!(acct.password, "123456");
        assert_eq!(acct.server, "192.168.10.10");
        assert_eq!(acct.port, 5060);
        assert_eq!(acct.transport, TransportType::Udp);
        assert_eq!(acct.srtp, SrtpMode::Disabled);
        assert_eq!(acct.sip_uri(), "sip:6013@192.168.10.10");
    }

    #[test]
    fn test_access_code_parsed() {
        let config = Config::from_str(SAMPLE_TOML).unwrap();
        assert!(config.accounts[0].access_code.is_none());
        assert_eq!(config.accounts[1].access_code.as_deref(), Some("1234"));
    }

    #[test]
    fn test_defaults_applied() {
        let minimal = r#"
[[accounts]]
name = "minimal"
username = "user1"
password = "pass1"
server = "10.0.0.1"
"#;
        let config = Config::from_str(minimal).unwrap();
        // General defaults
        assert_eq!(config.general.log_level, 3);
        assert_eq!(config.general.clock_rate, 16000);
        assert!(config.general.null_audio);
        // Account defaults
        let acct = &config.accounts[0];
        assert_eq!(acct.port, 5060);
        assert_eq!(acct.transport, TransportType::Udp);
        assert_eq!(acct.srtp, SrtpMode::Disabled);
    }

    #[test]
    fn test_to_pjsua_config() {
        let config = Config::from_str(SAMPLE_TOML).unwrap();
        let pj_cfg = config.to_pjsua_config();
        assert_eq!(pj_cfg.log_level, 3);
        assert_eq!(pj_cfg.clock_rate, 16000);
        assert!(pj_cfg.null_audio);
    }

    #[test]
    fn test_invalid_toml() {
        let result = Config::from_str("this is not valid {{{ toml");
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_required_fields() {
        let bad = r#"
[[accounts]]
name = "bad"
"#;
        let result = Config::from_str(bad);
        assert!(result.is_err());
    }

    #[test]
    fn config_load_nonexistent_file() {
        let err = Config::load("/tmp/nonexistent-pjsua-config.toml").unwrap_err();
        match err {
            PjError::Config(msg) => assert!(
                msg.contains("No such file") || msg.contains("not found"),
                "msg: {msg}"
            ),
            other => panic!("Expected Config error, got: {:?}", other),
        }
    }

    #[test]
    fn config_from_str_trait() {
        let toml = r#"
        [general]
        log_level = 3
        [[accounts]]
        name = "test"
        username = "user"
        password = "pass"
        server = "127.0.0.1"
        "#;
        // .parse() requires FromStr impl
        let cfg: Config = toml.parse().unwrap();
        assert_eq!(cfg.accounts.len(), 1);
    }

    #[test]
    fn test_transport_types() {
        let toml_str = r#"
[[accounts]]
name = "tcp_test"
username = "user"
password = "pass"
server = "10.0.0.1"
transport = "tcp"
"#;
        let config = Config::from_str(toml_str).unwrap();
        assert_eq!(config.accounts[0].transport, TransportType::Tcp);
    }
}
