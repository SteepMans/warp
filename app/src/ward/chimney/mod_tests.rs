use super::*;

#[test]
fn base_url_is_normalized() {
    assert_eq!(
        normalize_base_url(" https://chimney.steep-man.ru/ "),
        Some("https://chimney.steep-man.ru".to_owned())
    );
    assert_eq!(
        normalize_base_url("http://localhost:8080/chimney//"),
        Some("http://localhost:8080/chimney".to_owned())
    );
    assert_eq!(normalize_base_url("chimney.steep-man.ru"), None);
    assert_eq!(normalize_base_url("https://"), None);
    assert_eq!(normalize_base_url("https://a b"), None);
}

#[test]
fn request_config_debug_hides_the_token() {
    let config = RequestConfig {
        base_url: DEFAULT_BASE_URL.to_owned(),
        token: "secret-token-value".to_owned(),
    };
    let printed = format!("{config:?}");
    assert!(!printed.contains("secret-token-value"));
    assert!(printed.contains("<redacted>"));
}
