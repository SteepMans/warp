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
fn request_config_debug_hides_secrets() {
    for credentials in [
        Credentials::Token("secret-token-value".to_owned()),
        Credentials::Password {
            email: "bot@example.com".to_owned(),
            password: "secret-token-value".to_owned(),
        },
    ] {
        let config = RequestConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            credentials,
        };
        let printed = format!("{config:?}");
        assert!(!printed.contains("secret-token-value"));
        assert!(printed.contains("<redacted>"));
    }
}

#[test]
fn secrets_are_trimmed_but_passwords_keep_spaces() {
    assert_eq!(trim_secret(EMAIL_ENV_VAR, " bot@example.com\r\n"), "bot@example.com");
    assert_eq!(trim_secret(PASSWORD_ENV_VAR, " pass word \r\n"), " pass word ");
    assert_eq!(trim_secret(PASSWORD_KEY, "pw\r"), "pw");
    assert_eq!(trim_secret(TOKEN_ENV_VAR, " tok \n"), "tok");
}
