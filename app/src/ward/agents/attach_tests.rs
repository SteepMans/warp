use super::*;

const BASE: &str = "https://chimney.steep-man.ru/agents";
const SESSION: &str = "0b6f3c8e-1d2a-4c55-9a7b-1234567890ab";
const TICKET: &str = "Qm9yZWQ_b2YtdGlja2V0cy1hbmQtc29ja2V0cw";

fn body(session: &str, ticket: &str) -> String {
    serde_json::json!({
        "sessionId": session,
        "ticket": ticket,
        "title": "#4711 Подключиться к сессии",
        "worker": "steep-pc",
    })
    .to_string()
}

#[test]
fn the_viewer_socket_follows_the_service_address() {
    assert_eq!(
        viewer_url(BASE, SESSION, TICKET).as_deref(),
        Some(
            format!("wss://chimney.steep-man.ru/agents/api/v1/stream/{SESSION}?ticket={TICKET}")
                .as_str()
        )
    );
    assert_eq!(
        viewer_url("http://127.0.0.1:8080/", SESSION, "abc").as_deref(),
        Some(format!("ws://127.0.0.1:8080/api/v1/stream/{SESSION}?ticket=abc").as_str())
    );
    assert_eq!(viewer_url("ftp://example.com", SESSION, TICKET), None);
    assert_eq!(viewer_url("not a url", SESSION, TICKET), None);
}

#[test]
fn a_request_of_the_card_becomes_a_tab() {
    let attach = parse(&body(SESSION, TICKET), BASE).unwrap();
    assert_eq!(
        attach.url,
        format!("wss://chimney.steep-man.ru/agents/api/v1/stream/{SESSION}?ticket={TICKET}")
    );
    assert_eq!(attach.title, "steep-pc · #4711 Подключиться к сессии");
    assert_eq!(attach.task, "#4711 Подключиться к сессии");
    assert_eq!(attach.worker, "steep-pc");
}

#[test]
fn the_host_comes_from_the_worker_and_not_from_the_page() {
    let body = serde_json::json!({
        "sessionId": SESSION,
        "ticket": TICKET,
        "url": "wss://evil.example/steal",
        "host": "evil.example",
    })
    .to_string();
    let attach = parse(&body, BASE).unwrap();
    assert!(
        attach
            .url
            .starts_with("wss://chimney.steep-man.ru/agents/api/v1/stream/")
    );
}

#[test]
fn broken_requests_are_turned_down() {
    assert_eq!(parse("hello", BASE), Err(Rejected::NotJson));
    assert_eq!(parse("{}", BASE), Err(Rejected::NotJson));
    assert_eq!(
        parse(&body("../../worker/stream/x", TICKET), BASE),
        Err(Rejected::BadSession)
    );
    assert_eq!(
        parse(&body("0b6f3c8e11d2a14c5519a7b11234567890ab", TICKET), BASE),
        Err(Rejected::BadSession)
    );
    assert_eq!(
        parse(&body(SESSION, "short"), BASE),
        Err(Rejected::BadTicket)
    );
    assert_eq!(
        parse(&body(SESSION, "ticket&url=wss://evil.example"), BASE),
        Err(Rejected::BadTicket)
    );
    assert_eq!(
        parse(&body(SESSION, TICKET), "ftp://example.com"),
        Err(Rejected::NoAddress)
    );
}

#[test]
fn the_ticket_is_not_in_debug_output() {
    let attach = parse(&body(SESSION, TICKET), BASE).unwrap();
    let printed = format!("{attach:?}");
    assert!(!printed.contains(TICKET), "{printed}");
    assert!(printed.contains("steep-pc"), "{printed}");
}

#[test]
fn the_title_is_one_short_line_with_the_machine_first() {
    assert_eq!(tab_title(None, None), "Удалённо · Сессия агента");
    assert_eq!(
        tab_title(Some("  #1\nзадача\u{1b}[31m  "), Some("steep-pc")),
        "steep-pc · #1 задача [31m"
    );
    assert_eq!(
        tab_title(Some("#2 задача"), Some(" steep\npc ")),
        "steep pc · #2 задача"
    );
    let long = "я".repeat(500);
    let title = tab_title(Some(long.as_str()), None);
    assert_eq!(
        title.chars().count(),
        MAX_TITLE_CHARS + "Удалённо · ".chars().count()
    );
    assert_eq!(task_title(Some("  ")), "Сессия агента");
}

#[test]
fn a_request_without_a_machine_still_opens_a_tab() {
    let body = serde_json::json!({ "sessionId": SESSION, "ticket": TICKET }).to_string();
    let attach = parse(&body, BASE).unwrap();
    assert_eq!(attach.worker, "");
    assert_eq!(attach.task, "Сессия агента");
    assert_eq!(attach.title, "Удалённо · Сессия агента");
}

#[test]
fn the_script_is_written_with_a_byte_order_mark_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("ward-attach.ps1");
    write_script(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert_eq!(&bytes[3..], SCRIPT.as_bytes());
    // The same viewer again leaves the file alone.
    write_script(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn the_viewer_reads_its_address_from_the_environment() {
    assert!(SCRIPT.contains(&format!("$env:{URL_ENV_VAR}")));
    assert!(SCRIPT.contains(&format!("$env:{TITLE_ENV_VAR}")));
    assert!(SCRIPT.contains(&format!("$env:{WORKER_ENV_VAR}")));
    let command = tab_command(Path::new(r"C:\Users\me\AppData\Local\warp\ward-attach.ps1"));
    assert_eq!(
        command,
        r#"powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File "C:\Users\me\AppData\Local\warp\ward-attach.ps1""#
    );
}

/// The size frame the viewer builds is the one the stream reads on the machine of the session.
#[test]
fn the_viewer_sends_its_size_as_the_stream_reads_it() {
    let built = r#""{\"type\":\"size\",\"cols\":" + cols + ",\"rows\":" + rows + "}""#;
    assert!(
        SCRIPT.contains(built),
        "the viewer builds its size frame differently"
    );
    let sent = format!(r#"{{"type":"size","cols":{},"rows":{}}}"#, 132, 41);
    assert_eq!(
        super::super::stream::parse_viewer_frame(&sent),
        Some(super::super::stream::ViewerFrame::Size {
            cols: 132,
            rows: 41
        })
    );
}

/// The recent output is not drawn at a foreign size: the viewer holds it until the sizes meet
/// and draws it as it is only when a machine never takes the size.
#[test]
fn the_viewer_holds_the_output_until_the_sizes_meet() {
    for part in [
        "public sealed class Screen",
        "HoldFor = TimeSpan.FromSeconds(3)",
        "screen.Size(",
        "screen.Output(",
        "screen.Tick(",
        "screen.Reset(",
    ] {
        assert!(SCRIPT.contains(part), "{part}");
    }
}
