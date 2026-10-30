use super::*;

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

#[test]
fn parses_all_parameters() {
    let request = parse_new_tab_request(&url(
        "warposs://action/new_tab?path=%2Ftmp%2Fproj&command=claude%20%22do%20it%22%20%26%26%20exit\
         &title=%20Task%201%20&shell=pwsh&token=abc&ack=run-1_A",
    ))
    .unwrap();
    assert_eq!(request.path, PathBuf::from("/tmp/proj"));
    assert_eq!(request.command.as_deref(), Some("claude \"do it\" && exit"));
    assert_eq!(request.title.as_deref(), Some("Task 1"));
    assert_eq!(request.shell.as_deref(), Some("pwsh"));
    assert_eq!(request.token.as_deref(), Some("abc"));
    assert_eq!(request.ack, AckId::parse("run-1_A"));
    assert!(!request.invalid_ack);
}

#[test]
fn optional_parameters_may_be_missing_or_empty() {
    let request =
        parse_new_tab_request(&url("warposs://action/new_tab?path=~/x&command=&title=%20"))
            .unwrap();
    let home = dirs::home_dir().unwrap();
    assert_eq!(request.path, home.join("x"));
    assert_eq!(request.command, None);
    assert_eq!(request.title, None);
    assert_eq!(request.shell, None);
    assert_eq!(request.token, None);
    assert_eq!(request.ack, None);
    assert!(!request.invalid_ack);
}

#[test]
fn path_is_required() {
    assert!(parse_new_tab_request(&url("warposs://action/new_tab?command=ls")).is_err());
    assert!(parse_new_tab_request(&url("warposs://action/new_tab?path=&command=ls")).is_err());
}

#[test]
fn long_titles_are_cut() {
    let title = "a".repeat(500);
    let request = parse_new_tab_request(&url(&format!(
        "warposs://action/new_tab?path=/tmp&title={title}"
    )))
    .unwrap();
    assert_eq!(request.title.unwrap().chars().count(), 200);
}

#[test]
fn debug_output_hides_the_token_and_the_command() {
    let request = parse_new_tab_request(&url(
        "warposs://action/new_tab?path=/tmp&command=secret-command&token=secret-token",
    ))
    .unwrap();
    let debug = format!("{request:?}");
    assert!(!debug.contains("secret-command"), "{debug}");
    assert!(!debug.contains("secret-token"), "{debug}");
    assert!(debug.contains("command_len: Some(14)"), "{debug}");
}

#[test]
fn ward_parameters_are_detected() {
    assert!(!is_ward_new_tab(&url("warposs://action/new_tab?path=/tmp")));
    assert!(is_ward_new_tab(&url(
        "warposs://action/new_tab?path=/tmp&command=ls"
    )));
    assert!(is_ward_new_tab(&url(
        "warposs://action/new_tab?path=/tmp&title=x"
    )));
    assert!(is_ward_new_tab(&url(
        "warposs://action/new_tab?path=/tmp&ack=1"
    )));
}

#[test]
fn ack_ids_are_validated() {
    assert!(AckId::parse("a").is_some());
    assert!(AckId::parse("Run_2026-10-30_abc").is_some());
    assert!(AckId::parse(&"x".repeat(64)).is_some());
    assert!(AckId::parse(&"x".repeat(65)).is_none());
    assert!(AckId::parse("").is_none());
    assert!(AckId::parse("../evil").is_none());
    assert!(AckId::parse("a/b").is_none());
    assert!(AckId::parse("a.json").is_none());
    assert!(AckId::parse("a b").is_none());
    assert!(AckId::parse("\u{43f}\u{440}").is_none());

    let request =
        parse_new_tab_request(&url("warposs://action/new_tab?path=/tmp&ack=..%2Fx")).unwrap();
    assert_eq!(request.ack, None);
    assert!(request.invalid_ack);
}

#[test]
fn tokens_are_compared_exactly() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
    assert!(constant_time_eq(b"", b""));

    let expected = Some("00ff");
    assert_eq!(check_token(Some("00ff"), expected), TokenCheck::Valid);
    assert_eq!(check_token(Some("00fe"), expected), TokenCheck::Mismatch);
    assert_eq!(check_token(Some("00FF"), expected), TokenCheck::Mismatch);
    assert_eq!(check_token(None, expected), TokenCheck::Missing);
    assert_eq!(check_token(Some("00ff"), None), TokenCheck::Unavailable);
    assert_eq!(check_token(Some(""), Some("")), TokenCheck::Unavailable);
}

#[test]
fn the_token_file_is_created_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data").join(TOKEN_FILE_NAME);
    let token = ensure_token(&path).unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(read_token(&path).as_deref(), Some(token.as_str()));
    assert_eq!(ensure_token(&path).unwrap(), token);
    assert!(!path.with_extension("tmp").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // A file without a valid token is replaced.
    std::fs::write(&path, "garbage").unwrap();
    let replaced = ensure_token(&path).unwrap();
    assert_ne!(replaced, token);
    assert_eq!(read_token(&path).as_deref(), Some(replaced.as_str()));
}

#[test]
fn tokens_are_random() {
    assert_ne!(generate_token(), generate_token());
}

#[test]
fn ack_files_are_json() {
    let dir = tempfile::tempdir().unwrap();
    let id = AckId::parse("job-1").unwrap();
    let record = AckRecord {
        ok: true,
        reason: None,
        pane_uuid: Some("00ab".to_owned()),
        window_id: Some(3),
        ts: "2026-10-30T10:00:00+00:00".to_owned(),
    };
    write_ack(dir.path(), &id, &record).unwrap();
    let written = std::fs::read_to_string(dir.path().join("job-1.json")).unwrap();
    assert_eq!(
        written,
        r#"{"ok":true,"pane_uuid":"00ab","window_id":3,"ts":"2026-10-30T10:00:00+00:00"}"#
    );
    assert!(!dir.path().join("job-1.json.tmp").exists());

    let failure = AckRecord {
        ok: false,
        reason: Some("invalid token".to_owned()),
        pane_uuid: None,
        window_id: None,
        ts: "2026-10-30T10:00:00+00:00".to_owned(),
    };
    write_ack(dir.path(), &id, &failure).unwrap();
    let written = std::fs::read_to_string(dir.path().join("job-1.json")).unwrap();
    assert_eq!(
        written,
        r#"{"ok":false,"reason":"invalid token","ts":"2026-10-30T10:00:00+00:00"}"#
    );
}
