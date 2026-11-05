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

/// The C# of the viewer, as `Add-Type` compiles it in the tab.
#[cfg(windows)]
fn viewer_source() -> &'static str {
    let start = SCRIPT.find("$source = @'").expect("the viewer has its C#");
    let body = &SCRIPT[start..];
    let body = &body[body.find('\n').expect("the C# starts on a line of its own") + 1..];
    let end = body.find("\n'@").expect("the C# ends");
    body[..end].trim_end_matches('\r')
}

/// The checks of the viewer's C# (`attach_tests.cs`), compiled after it: it has the usings.
#[cfg(windows)]
const VIEWER_CHECKS: &str = include_str!("attach_tests.cs");

/// Compiles the viewer with its checks under Windows PowerShell, as the tab compiles the
/// viewer, and runs them.
#[cfg(windows)]
const CHECKS_DRIVER: &str = r#"param([string]$Source, [string]$Out)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition ([IO.File]::ReadAllText($Source, [Text.Encoding]::UTF8)) -Language CSharp -IgnoreWarnings
exit ([WardAttachTests.Suite]::Run($Out))
"#;

/// The lines of the report of the viewer's checks: `ok`, `FAIL` with the reason, and whatever
/// else the checks write.
#[cfg(windows)]
fn run_viewer_checks() -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("viewer.cs");
    let driver = dir.path().join("run.ps1");
    let report = dir.path().join("report.txt");
    std::fs::write(&source, format!("{}\n{VIEWER_CHECKS}", viewer_source())).unwrap();
    std::fs::write(&driver, CHECKS_DRIVER).unwrap();
    let output = std::process::Command::new("powershell")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&driver)
        .arg("-Source")
        .arg(&source)
        .arg("-Out")
        .arg(&report)
        .output()
        .expect("Windows PowerShell runs");
    let lines = std::fs::read_to_string(&report).unwrap_or_default();
    assert!(
        output.status.success(),
        "the checks of the viewer failed:\n{lines}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    lines.lines().map(str::to_owned).collect()
}

/// The viewer itself, compiled as the tab compiles it: the filter of the output, the screen
/// and the frames of the service (see `attach_tests.cs`).
#[cfg(windows)]
#[test]
fn the_viewer_passes_its_checks() {
    let report = run_viewer_checks();
    let failed: Vec<&String> = report
        .iter()
        .filter(|line| line.starts_with("FAIL"))
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
    let passed = report
        .iter()
        .filter(|line| line.starts_with("ok\t"))
        .count();
    assert!(passed >= 7, "{report:#?}");
}

/// The output of the session goes through the filter before it is written here.
#[test]
fn the_viewer_writes_the_output_through_its_filter() {
    assert!(SCRIPT.contains("public sealed class Filter"));
    assert!(SCRIPT.contains("Write(filter.Feed("));
}
