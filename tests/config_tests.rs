use pjsua_rust::config::Config;

#[test]
fn test_load_example_config() {
    let config = Config::load("config/test-accounts.toml.example");
    assert!(config.is_ok(), "Failed to load example config: {:?}", config.err());
    let config = config.unwrap();
    assert!(!config.accounts.is_empty());
}
