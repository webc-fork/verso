use versoview::config::Config;
use versoview_messages::ConfigFromController;

#[test]
fn controller_config_fills_in_defaults() {
    let config = Config::from_controller_config(ConfigFromController::default());

    assert_eq!(config.url.as_str(), "https://example.com/");
    assert!(config.custom_protocols.is_empty());
    assert!(!config.user_agent.is_empty());
}

#[test]
fn cli_config_falls_back_to_default_url() {
    let config = Config::from_cli_args(Default::default());

    assert_eq!(config.url.as_str(), "https://example.com/");
    assert!(config.devtools_port.is_none());
    assert!(config.webdriver_port.is_none());
}
