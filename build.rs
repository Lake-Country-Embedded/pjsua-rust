use std::env;
use std::path::PathBuf;

fn main() {
    // Use pkg-config to find libpjproject and get include/link flags
    let lib = pkg_config::Config::new()
        .atleast_version("2.13")
        .statik(true)
        .probe("libpjproject")
        .expect("Failed to find libpjproject via pkg-config. Is PJSIP installed?");

    let version = &lib.version;
    println!("cargo:warning=Found PJSIP version: {version}");

    // Parse version for comparison
    let parts: Vec<u32> = version
        .split(['.', '-'])
        .filter_map(|s| s.parse().ok())
        .collect();
    let (major, minor) = (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
    );
    if major > 2 || (major == 2 && minor > 15) {
        println!("cargo:warning=PJSIP version {version} is newer than tested (2.15). Proceed with caution.");
    }

    // pkg-config with statik(true) already emits the correct cargo:rustc-link-lib=static=...
    // and cargo:rustc-link-search=... directives for all libraries, including the
    // arch-suffixed PJSIP libs and system dependencies (ssl, crypto, uuid, etc.).
    // No need to manually re-emit them.

    // Run bindgen on wrapper.h
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // Add include paths and defines from pkg-config
    for path in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }
    for (key, val) in &lib.defines {
        match val {
            Some(v) => builder = builder.clang_arg(format!("-D{key}={v}")),
            None => builder = builder.clang_arg(format!("-D{key}")),
        }
    }

    // Add GCC system include paths so libclang can find stddef.h etc.
    // Skip detection if BINDGEN_EXTRA_CLANG_ARGS is set (Yocto sets this
    // with the correct sysroot and system include paths).
    if env::var("BINDGEN_EXTRA_CLANG_ARGS").is_err() {
        let target = env::var("TARGET").unwrap_or_default();
        let gcc_triple = target.replace("-unknown-", "-").replace("-none-", "-");
        let gcc_base = PathBuf::from(format!("/usr/lib/gcc/{gcc_triple}"));
        let gcc_include = gcc_base
            .read_dir()
            .ok()
            .and_then(|mut entries| {
                entries.find_map(|e| {
                    let e = e.ok()?;
                    let p = e.path().join("include");
                    if p.is_dir() { Some(p) } else { None }
                })
            });
        if let Some(path) = gcc_include {
            builder = builder.clang_arg(format!("-isystem{}", path.display()));
        }
    }

    // Allowlist PJSUA functions
    builder = builder
        // Core lifecycle
        .allowlist_function("pjsua_create")
        .allowlist_function("pjsua_init")
        .allowlist_function("pjsua_start")
        .allowlist_function("pjsua_destroy")
        .allowlist_function("pjsua_destroy2")
        .allowlist_function("pjsua_get_state")
        .allowlist_function("pjsua_handle_events")
        .allowlist_function("pjsua_stop_worker_threads")
        .allowlist_function("pjsua_detect_nat_type")
        // Logging
        .allowlist_function("pjsua_logging_config_default")
        .allowlist_function("pjsua_config_default")
        .allowlist_function("pjsua_media_config_default")
        // Transport
        .allowlist_function("pjsua_transport_create")
        .allowlist_function("pjsua_transport_get_info")
        .allowlist_function("pjsua_transport_close")
        .allowlist_function("pjsua_transport_config_default")
        // Account
        .allowlist_function("pjsua_acc_config_default")
        .allowlist_function("pjsua_acc_add")
        .allowlist_function("pjsua_acc_del")
        .allowlist_function("pjsua_acc_modify")
        .allowlist_function("pjsua_acc_set_online_status")
        .allowlist_function("pjsua_acc_set_registration")
        .allowlist_function("pjsua_acc_get_info")
        .allowlist_function("pjsua_acc_get_count")
        .allowlist_function("pjsua_acc_is_valid")
        // Calls
        .allowlist_function("pjsua_call_make_call")
        .allowlist_function("pjsua_call_answer")
        .allowlist_function("pjsua_call_hangup")
        .allowlist_function("pjsua_call_hangup_all")
        .allowlist_function("pjsua_call_get_info")
        .allowlist_function("pjsua_call_get_count")
        .allowlist_function("pjsua_call_set_hold")
        .allowlist_function("pjsua_call_reinvite")
        .allowlist_function("pjsua_call_xfer")
        .allowlist_function("pjsua_call_dial_dtmf")
        .allowlist_function("pjsua_call_send_dtmf")
        .allowlist_function("pjsua_call_send_dtmf_param_default")
        // Media / Conference
        .allowlist_function("pjsua_conf_connect")
        .allowlist_function("pjsua_conf_disconnect")
        .allowlist_function("pjsua_conf_adjust_tx_level")
        .allowlist_function("pjsua_conf_adjust_rx_level")
        .allowlist_function("pjsua_conf_get_signal_level")
        .allowlist_function("pjsua_enum_conf_ports")
        .allowlist_function("pjsua_conf_get_port_info")
        .allowlist_function("pjsua_conf_add_port")
        .allowlist_function("pjsua_conf_remove_port")
        // Pool
        .allowlist_function("pjsua_pool_create")
        .allowlist_function("pj_pool_release")
        // Sound device
        .allowlist_function("pjsua_set_no_snd_dev")
        .allowlist_function("pjsua_set_snd_dev")
        .allowlist_function("pjsua_get_snd_dev")
        // Media port
        .allowlist_function("pjmedia_port_info_init")
        .allowlist_function("pjmedia_port_destroy")
        // Tone generator
        .allowlist_function("pjmedia_tonegen_create2")
        .allowlist_function("pjmedia_tonegen_play_digits")
        .allowlist_function("pjmedia_tonegen_play")
        .allowlist_function("pjmedia_tonegen_is_busy")
        .allowlist_function("pjmedia_tonegen_stop")
        // Player / Recorder
        .allowlist_function("pjsua_player_create")
        .allowlist_function("pjsua_player_get_conf_port")
        .allowlist_function("pjsua_player_destroy")
        .allowlist_function("pjsua_recorder_create")
        .allowlist_function("pjsua_recorder_get_conf_port")
        .allowlist_function("pjsua_recorder_destroy")
        // Sound device
        .allowlist_function("pjsua_snd_is_active")
        // Codec
        .allowlist_function("pjsua_codec_set_priority")
        // Buddy
        .allowlist_function("pjsua_verify_sip_url")
        .allowlist_function("pjsua_verify_url")
        // SRTP
        .allowlist_function("pjsua_set_null_snd_dev")
        // pj utility
        .allowlist_function("pj_strerror")
        .allowlist_function("pj_str")
        .allowlist_function("pj_cstr")
        // Types
        .allowlist_type("pjsua_config")
        .allowlist_type("pjsua_logging_config")
        .allowlist_type("pjsua_media_config")
        .allowlist_type("pjsua_transport_config")
        .allowlist_type("pjsua_acc_config")
        .allowlist_type("pjsua_acc_info")
        .allowlist_type("pjsua_call_info")
        .allowlist_type("pjsua_call_setting")
        .allowlist_type("pjsua_msg_data")
        .allowlist_type("pjsua_call_send_dtmf_param")
        .allowlist_type("pjsip_inv_state")
        .allowlist_type("pjsua_call_media_status")
        .allowlist_type("pjsip_transport_type_e")
        .allowlist_type("pjmedia_srtp_use")
        .allowlist_type("pjsua_state")
        .allowlist_type("pjsua_dtmf_method")
        .allowlist_type("pj_str_t")
        .allowlist_type("pj_status_t")
        .allowlist_type("pj_bool_t")
        .allowlist_type("pj_pool_t")
        .allowlist_type("pjsua_conf_port_info")
        .allowlist_type("pjmedia_port")
        .allowlist_type("pjmedia_tone_digit")
        .allowlist_type("pjmedia_tone_desc")
        .allowlist_type("pjmedia_frame")
        // Constants
        .allowlist_var("PJSUA_INVALID_ID")
        .allowlist_var("PJSIP_MAX_URL_SIZE")
        // Derive traits
        .derive_debug(true)
        .derive_default(true);

    let bindings = builder
        .generate()
        .expect("Failed to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("pjsip_bindings.rs"))
        .expect("Failed to write bindings");
}
