# pjsua-rust

Safe Rust bindings for [PJSIP](https://www.pjsip.org/)'s PJSUA library — a high-level SIP user agent API for VoIP applications.

## Overview

pjsua-rust wraps the PJSUA level-1 C API in safe Rust types with RAII lifecycle management, typed IDs, and async event delivery via Tokio channels. It is designed as a reusable crate for building SIP applications such as VoIP phones, intercom systems, and automated test tools.

### Key features

- **RAII singleton** — `PjsuaApp` manages the full `pjsua_create`/`init`/`start`/`destroy` lifecycle. Dropping the handle cleans up all PJSIP resources.
- **Typed IDs** — `AccountId`, `CallId`, `ConfPort`, etc. prevent mixing up integer handles.
- **Async events** — SIP callbacks are bridged to a `tokio::sync::mpsc` channel producing typed `SipEvent` variants (registration, incoming call, call state, media state, DTMF).
- **TOML configuration** — Load account and general settings from TOML files with sensible defaults.
- **Custom audio ports** — Implement the `MediaPort` trait to inject or process audio frames on the conference bridge.
- **Tone generator** — Play DTMF digits and custom tones via `ToneGenerator`.
- **Conference bridge** — Connect, disconnect, and enumerate ports on the PJSUA conference bridge.
- **WAV player/recorder** — Create file players and recorders attached to the conference bridge.
- **Codec control** — Set codec priorities to enable, disable, or reorder codecs.

## Prerequisites

- **PJSIP 2.16** installed with development headers
- **pkg-config** — the build system uses `pkg-config` to locate `libpjproject`
- **Rust 1.70+** with the stable toolchain
- **libclang** — required by `bindgen` for generating FFI bindings

### Installing PJSIP (from source)

```bash
git clone https://github.com/pjsip/pjproject.git
cd pjproject
./configure --enable-shared=no
make dep && make
sudo make install
sudo ldconfig
```

Verify the installation:

```bash
pkg-config --modversion libpjproject
# Should print something like: 2.15-dev
```

## Quick start

Add to your `Cargo.toml`:

```toml
[dependencies]
pjsua-rust = { path = "../pjsua-rust" }
tokio = { version = "1", features = ["sync", "rt-multi-thread", "macros"] }
```

### Minimal example

```rust
use pjsua_rust::*;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize PJSUA with null audio (no sound hardware needed)
    let config = PjsuaConfig {
        log_level: 3,
        clock_rate: 16000,
        null_audio: true,
    };
    let (app, mut rx) = PjsuaApp::new(config)?;

    // Create a UDP transport
    app.create_transport(TransportType::Udp, None, 0)?;

    // Register a SIP account
    let account = app.add_account(&AccountConfig {
        name: "my-phone".into(),
        username: "1000".into(),
        password: "secret".into(),
        server: "192.168.1.1".into(),
        port: 5060,
        transport: TransportType::Udp,
        srtp: SrtpMode::Disabled,
        access_code: None,
    })?;

    // Process SIP events
    while let Some(event) = rx.recv().await {
        match event {
            SipEvent::RegistrationState { is_registered, .. } => {
                println!("Registered: {is_registered}");
            }
            SipEvent::IncomingCall { call_id, remote_uri, .. } => {
                println!("Incoming call from {remote_uri}");
                app.answer_call(call_id, 200)?;
            }
            SipEvent::CallMediaState { conf_port, .. } => {
                if let Some(port) = conf_port {
                    app.conf_connect(port, ConfPort::MASTER)?;
                    app.conf_connect(ConfPort::MASTER, port)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Loading configuration from TOML

```rust
use pjsua_rust::config::Config;

let config = Config::load("config/accounts.toml")?;
let pj_cfg = config.to_pjsua_config();
let (app, rx) = PjsuaApp::new(pj_cfg)?;

for account in &config.accounts {
    app.add_account(&account.to_account_config())?;
}
```

See `config/test-accounts.toml.example` for the configuration format.

## API overview

### Core types

| Type | Description |
|------|-------------|
| `PjsuaApp` | RAII singleton managing the PJSUA lifecycle |
| `SipEvent` | Enum of async SIP events received via channel |
| `Config` | TOML configuration parser (implements `FromStr`) |
| `PjError` | Error type with PJSIP status codes and messages |

### ID types

| Type | Description |
|------|-------------|
| `AccountId` | SIP account handle |
| `CallId` | Active call handle |
| `TransportId` | SIP transport handle |
| `ConfPort` | Conference bridge port number |
| `PlayerId` | WAV file player handle |
| `RecorderId` | WAV file recorder handle |

### Media types

| Type | Description |
|------|-------------|
| `ToneGenerator` | DTMF and custom tone playback |
| `CustomPort` | User-defined audio port (implement `MediaPort` trait) |
| `AudioFrame` | PCM audio frame exchanged with custom ports |

### SIP events

| Variant | Delivered when |
|---------|---------------|
| `RegistrationState` | Account registration status changes |
| `IncomingCall` | A new inbound call arrives |
| `CallState` | Call state transitions (calling, confirmed, disconnected, etc.) |
| `CallMediaState` | Media becomes active/held/error |
| `DtmfDigit` | A DTMF digit is received on a call |

## Configuration

The TOML configuration supports the following structure:

```toml
[general]
log_level = 3        # PJSIP log verbosity (0-6)
clock_rate = 16000   # Audio clock rate in Hz
null_audio = true    # Use null audio device (no hardware)

[[accounts]]
name = "my-phone"
username = "1000"
password = "secret"
server = "pbx.example.com"
port = 5060                  # default: 5060
transport = "udp"            # udp, tcp, or tls (default: udp)
srtp = "disabled"            # disabled, optional, or mandatory (default: disabled)
access_code = "1234"         # optional DTMF access code
```

## Running tests

### Unit tests

Unit tests run without PJSIP initialization and cover types, config parsing, error handling, and trait implementations:

```bash
cargo test
```

### Integration tests

Integration tests initialize PJSUA with null audio and exercise the conference bridge, tone generator, custom ports, and codec APIs. They require PJSIP to be installed and are gated behind the `integration-tests` feature:

```bash
cargo test --features integration-tests -- --test-threads=1
```

The `--test-threads=1` flag is required because PJSUA is a global singleton — only one instance can exist at a time.

### Memory leak testing

Leak tests run under Valgrind to verify that the RAII cleanup paths properly release all PJSIP resources:

```bash
./scripts/valgrind-test.sh
```

This builds the leak tests, runs them under Valgrind with PJSIP-specific suppressions, and reports any definitely or possibly lost memory. Requires `valgrind` to be installed.

To run a specific leak test:

```bash
./scripts/valgrind-test.sh test_repeated_init_destroy
```

## Project structure

```
pjsua-rust/
├── build.rs                  # pkg-config + bindgen build script
├── wrapper.h                 # C headers included for FFI generation
├── src/
│   ├── lib.rs                # Public re-exports
│   ├── ffi.rs                # Generated bindgen bindings (via include!)
│   ├── ffi_helpers.rs        # PjString, pj_str_t conversion helpers
│   ├── error.rs              # PjError enum, check_status, make_pj_error
│   ├── types.rs              # ID newtypes, enums, config structs
│   ├── config.rs             # TOML config parsing
│   ├── event.rs              # SipEvent enum, C callback bridge
│   ├── pjsua_app.rs          # PjsuaApp RAII singleton
│   ├── media_port.rs         # MediaPort trait, CustomPort
│   ├── tonegen.rs            # ToneGenerator
│   └── version.rs            # PJSIP version parsing utilities
├── config/
│   └── test-accounts.toml.example
├── examples/
│   └── voip_test.rs          # Two-party VoIP test with DTMF
├── tests/
│   ├── config_tests.rs       # Config file loading tests
│   ├── type_tests.rs         # Type equality, hashing, event variants
│   ├── integration.rs        # Live SIP integration tests (feature-gated)
│   ├── media_tests.rs        # Conference bridge, tonegen, custom port tests
│   └── leak_tests.rs         # Memory leak stress tests (for Valgrind)
├── scripts/
│   └── valgrind-test.sh      # Automated Valgrind leak checker
└── valgrind.supp             # PJSIP-specific Valgrind suppressions
```

## Supported PJSIP versions

| Version | Status |
|---------|--------|
| 2.16.x | Actively tested and confirmed |
| 2.13–2.15 | Minimum build requirement met, not actively tested |

The build script checks the PJSIP version via pkg-config and will error if it is below 2.13.

## License

MIT
