//! Two-party VoIP test runner.
//!
//! Registers two SIP accounts (caller and receiver), places a call between
//! them through the PBX, sends DTMF digits, and reports the results.
//!
//! # Usage
//!
//! ```sh
//! cargo run --example voip_test
//! cargo run --example voip_test -- --config path/to/config.toml
//! ```

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{error, info, warn};

use pjsua_rust::config::Config;
use pjsua_rust::*;

const DEFAULT_CONFIG_PATH: &str = "config/test-accounts.toml";
const DTMF_DIGITS: &str = "12345";
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const REG_TIMEOUT: Duration = Duration::from_secs(10);
const DTMF_COLLECT_WAIT: Duration = Duration::from_secs(3);

fn parse_args() -> String {
    let args: Vec<String> = std::env::args().collect();
    for i in 1..args.len() {
        if args[i] == "--config" {
            if let Some(path) = args.get(i + 1) {
                return path.clone();
            }
        }
    }
    DEFAULT_CONFIG_PATH.to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load config
    let config_path = parse_args();
    info!("Loading config from: {}", config_path);
    let config =
        Config::load(&config_path).context(format!("Failed to load config from {config_path}"))?;

    let caller_toml = config
        .find_account("caller")
        .context("No 'caller' account in config")?
        .clone();
    let receiver_toml = config
        .find_account("receiver")
        .context("No 'receiver' account in config")?
        .clone();

    let caller_cfg = caller_toml.to_account_config();
    let receiver_cfg = receiver_toml.to_account_config();

    // Init PJSUA
    let pj_cfg = config.to_pjsua_config();
    info!(
        "Initializing PJSUA (log_level={}, clock_rate={}, null_audio={})",
        pj_cfg.log_level, pj_cfg.clock_rate, pj_cfg.null_audio
    );
    let (app, mut rx) = PjsuaApp::new(pj_cfg).context("Failed to initialize PJSUA")?;

    // Create UDP transport
    let _transport_id = app
        .create_transport(TransportType::Udp, None, 0, None)
        .context("Failed to create UDP transport")?;
    info!("UDP transport created");

    // Register both accounts
    let caller_id = app
        .add_account(&caller_cfg)
        .context("Failed to add caller account")?;
    info!("Caller account added: {:?}", caller_id);

    let _receiver_id = app
        .add_account(&receiver_cfg)
        .context("Failed to add receiver account")?;
    info!("Receiver account added: {:?}", _receiver_id);

    // Wait for both registrations
    info!("Waiting for SIP registrations...");
    let mut reg_count = 0u32;
    let reg_ok = tokio::time::timeout(REG_TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            match &event {
                SipEvent::RegistrationState {
                    account_id,
                    code,
                    is_registered,
                    reason,
                    ..
                } => {
                    info!(
                        "Registration event: account={:?} code={} registered={} reason={}",
                        account_id, code, is_registered, reason
                    );
                    if *is_registered {
                        reg_count += 1;
                        if reg_count >= 2 {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if !reg_ok {
        bail!(
            "Timed out waiting for registrations (got {}/2)",
            reg_count
        );
    }
    info!("Both accounts registered successfully");

    // Make call from caller to receiver
    let dest_uri = format!("sip:{}@{}", receiver_cfg.username, receiver_cfg.server);
    info!("Making call to {}", dest_uri);
    let call_id = app
        .make_call(caller_id, &dest_uri)
        .context("Failed to make call")?;
    info!("Call initiated: {:?}", call_id);

    // Event loop: auto-answer, bridge media, send DTMF, collect digits
    let mut call_connected = false;
    let mut dtmf_received: Vec<char> = Vec::new();
    let mut dtmf_sent = false;

    let result = tokio::time::timeout(CALL_TIMEOUT, async {
        while let Some(event) = rx.recv().await {
            match event {
                SipEvent::IncomingCall {
                    call_id,
                    account_id,
                    remote_uri,
                } => {
                    info!(
                        "Incoming call: call={:?} account={:?} from={}",
                        call_id, account_id, remote_uri
                    );
                    if let Err(e) = app.answer_call(call_id, 200) {
                        error!("Failed to answer call: {}", e);
                    } else {
                        info!("Auto-answered incoming call {:?}", call_id);
                    }
                }

                SipEvent::CallState {
                    call_id,
                    state,
                    last_code,
                    last_reason,
                    ..
                } => {
                    info!(
                        "Call state: call={:?} state={} code={} reason={}",
                        call_id, state, last_code, last_reason
                    );

                    if state == CallState::Confirmed && !dtmf_sent {
                        call_connected = true;
                        info!("Call confirmed! Sending DTMF: {}", DTMF_DIGITS);
                        if let Err(e) =
                            app.send_dtmf(call_id, DTMF_DIGITS, DtmfMethod::Rfc2833)
                        {
                            error!("Failed to send DTMF: {}", e);
                        }
                        dtmf_sent = true;

                        // Wait for DTMF digits to be received, then hangup
                        info!(
                            "Waiting {} seconds for DTMF collection...",
                            DTMF_COLLECT_WAIT.as_secs()
                        );
                        tokio::time::sleep(DTMF_COLLECT_WAIT).await;

                        info!("Hanging up all calls");
                        if let Err(e) = app.hangup_all() {
                            error!("Failed to hangup: {}", e);
                        }
                    }

                    if state == CallState::Disconnected && call_connected {
                        info!("Call disconnected after successful connection");
                        return;
                    }
                }

                SipEvent::CallMediaState {
                    call_id,
                    media_status,
                    conf_port,
                    ..
                } => {
                    info!(
                        "Media state: call={:?} status={:?} conf_port={:?}",
                        call_id, media_status, conf_port
                    );
                    if media_status == MediaStatus::Active {
                        if let Some(port) = conf_port {
                            info!("Connecting conference bridge port {:?}", port);
                            app.conf_connect(port, ConfPort::MASTER).ok();
                            app.conf_connect(ConfPort::MASTER, port).ok();
                        }
                    }
                }

                SipEvent::DtmfDigit { call_id, digit } => {
                    info!("DTMF digit received: call={:?} digit={}", call_id, digit);
                    dtmf_received.push(digit);
                }

                _ => {}
            }
        }
    })
    .await;

    if result.is_err() {
        warn!("Call event loop timed out");
    }

    // Print results
    println!();
    println!("========================================");
    println!("  VoIP Test Results");
    println!("========================================");
    println!(
        "  Call connected:    {}",
        if call_connected { "YES" } else { "NO" }
    );
    println!(
        "  DTMF sent:         {}",
        if dtmf_sent {
            DTMF_DIGITS
        } else {
            "(none)"
        }
    );
    println!(
        "  DTMF received:     {}",
        if dtmf_received.is_empty() {
            "(none)".to_string()
        } else {
            dtmf_received.iter().collect::<String>()
        }
    );
    println!("========================================");
    println!();

    // Clean shutdown (PjsuaApp Drop)
    info!("Shutting down PJSUA...");
    drop(app);
    info!("Done.");

    Ok(())
}
