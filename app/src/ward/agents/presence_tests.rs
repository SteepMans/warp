use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};

use super::*;

const CHIMNEY: &str = "https://chimney.steep-man.ru";
const SESSION: &str = "0b6f3c8e-1d2a-4c55-9a7b-1234567890ab";
const TICKET: &str = "Qm9yZWQ_b2YtdGlja2V0cy1hbmQtc29ja2V0cw";

fn presence() -> Presence {
    Presence {
        worker: "steep-pc".to_owned(),
        origin: CHIMNEY.to_owned(),
        base_url: "https://chimney.steep-man.ru/agents".to_owned(),
        can_attach: true,
    }
}

fn request(method: &str, path: &str, origin: Option<&str>) -> Request {
    Request {
        method: method.to_owned(),
        path: path.to_owned(),
        origin: origin.map(str::to_owned),
        ..Default::default()
    }
}

fn attach_body() -> String {
    serde_json::json!({
        "sessionId": SESSION,
        "ticket": TICKET,
        "title": "#4711 Задача",
        "worker": "laptop",
    })
    .to_string()
}

fn attach_request(origin: Option<&str>, body: &str) -> Request {
    Request {
        body: body.to_owned(),
        content_length: body.len(),
        ..request("POST", attach::PATH, origin)
    }
}

#[test]
fn the_origin_is_taken_from_the_service_address() {
    assert_eq!(
        origin_of("https://chimney.steep-man.ru/agents").as_deref(),
        Some(CHIMNEY)
    );
    assert_eq!(
        origin_of("http://localhost:8080/agents").as_deref(),
        Some("http://localhost:8080")
    );
    assert_eq!(origin_of("not a url"), None);
    assert_eq!(
        presence_of("  ", "https://chimney.steep-man.ru/agents"),
        None
    );
    assert_eq!(
        presence_of("steep-pc", "https://chimney.steep-man.ru/agents"),
        Some(Presence {
            can_attach: attach::SUPPORTED,
            ..presence()
        })
    );
}

#[test]
fn requests_are_parsed_with_their_origin() {
    let head = "GET /ward/presence?t=1 HTTP/1.1\r\nHost: 127.0.0.1:47615\r\norigin: https://chimney.steep-man.ru\r\n\r\n";
    assert_eq!(parse(head), Some(request("GET", PATH, Some(CHIMNEY))));
    assert_eq!(parse("hello"), None);
    assert_eq!(parse("GET /ward/presence\r\n\r\n"), None);

    let post = "POST /ward/attach HTTP/1.1\r\nOrigin: https://chimney.steep-man.ru\r\nContent-Type: application/json\r\ncontent-length: 42\r\n\r\n";
    let parsed = parse(post).unwrap();
    assert_eq!(parsed.path, attach::PATH);
    assert_eq!(parsed.content_length, 42);
    assert!(parsed.body.is_empty());
}

#[test]
fn the_chimney_page_gets_the_worker_name() {
    let response = respond(&request("GET", PATH, Some(CHIMNEY)), Some(&presence()));
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("Access-Control-Allow-Origin"),
        Some(CHIMNEY)
    );
    assert_eq!(response.header("Cache-Control"), Some("no-store"));
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["worker"], "steep-pc");
}

#[test]
fn the_preflight_allows_the_private_network() {
    let response = respond(&request("OPTIONS", PATH, Some(CHIMNEY)), Some(&presence()));
    assert_eq!(response.status, 204);
    assert_eq!(
        response.header("Access-Control-Allow-Origin"),
        Some(CHIMNEY)
    );
    assert_eq!(
        response.header("Access-Control-Allow-Private-Network"),
        Some("true")
    );
    assert_eq!(response.header("Access-Control-Allow-Methods"), Some("GET"));
}

#[test]
fn other_pages_and_paths_get_nothing() {
    let other = respond(
        &request("GET", PATH, Some("https://evil.example")),
        Some(&presence()),
    );
    assert_eq!(other.status, 403);
    assert_eq!(other.header("Access-Control-Allow-Origin"), None);
    assert!(other.body.is_empty());

    let no_origin = respond(&request("GET", PATH, None), Some(&presence()));
    assert_eq!(no_origin.status, 403);

    let path = respond(&request("GET", "/", Some(CHIMNEY)), Some(&presence()));
    assert_eq!(path.status, 404);

    let method = respond(&request("POST", PATH, Some(CHIMNEY)), Some(&presence()));
    assert_eq!(method.status, 405);
}

#[test]
fn a_worker_without_settings_does_not_answer() {
    let response = respond(&request("GET", PATH, Some(CHIMNEY)), None);
    assert_eq!(response.status, 404);
}

#[test]
fn the_server_answers_over_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let shared: Shared = Arc::new(RwLock::new(Some(presence())));
    let (attaches, _tabs) = async_channel::unbounded();
    std::thread::spawn(move || run(listener, shared, attaches));

    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            format!("GET {PATH} HTTP/1.1\r\nHost: {address}\r\nOrigin: {CHIMNEY}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(
        answer.contains(&format!("Access-Control-Allow-Origin: {CHIMNEY}\r\n")),
        "{answer}"
    );
    assert!(answer.ends_with(r#"{"worker":"steep-pc"}"#), "{answer}");
}

#[test]
fn the_chimney_page_asks_for_the_tab_of_a_remote_session() {
    let (response, tab) = handle(
        &attach_request(Some(CHIMNEY), &attach_body()),
        Some(&presence()),
    );
    assert_eq!(response.status, 202);
    assert_eq!(
        response.header("Access-Control-Allow-Origin"),
        Some(CHIMNEY)
    );
    let tab = tab.unwrap();
    assert_eq!(
        tab.url,
        format!("wss://chimney.steep-man.ru/agents/api/v1/stream/{SESSION}?ticket={TICKET}")
    );
    assert_eq!(tab.title, "laptop · #4711 Задача");
}

#[test]
fn the_preflight_of_a_request_to_connect_allows_post_with_json() {
    let response = respond(
        &request("OPTIONS", attach::PATH, Some(CHIMNEY)),
        Some(&presence()),
    );
    assert_eq!(response.status, 204);
    assert_eq!(
        response.header("Access-Control-Allow-Methods"),
        Some("POST")
    );
    assert_eq!(
        response.header("Access-Control-Allow-Headers"),
        Some("Content-Type")
    );
    assert_eq!(
        response.header("Access-Control-Allow-Private-Network"),
        Some("true")
    );
}

#[test]
fn only_the_chimney_page_can_open_a_tab() {
    let (other, tab) = handle(
        &attach_request(Some("https://evil.example"), &attach_body()),
        Some(&presence()),
    );
    assert_eq!(other.status, 403);
    assert_eq!(other.header("Access-Control-Allow-Origin"), None);
    assert!(tab.is_none());

    let (none, tab) = handle(&attach_request(None, &attach_body()), Some(&presence()));
    assert_eq!(none.status, 403);
    assert!(tab.is_none());

    let (get, tab) = handle(
        &request("GET", attach::PATH, Some(CHIMNEY)),
        Some(&presence()),
    );
    assert_eq!(get.status, 405);
    assert!(tab.is_none());

    let (unset, tab) = handle(&attach_request(Some(CHIMNEY), &attach_body()), None);
    assert_eq!(unset.status, 404);
    assert!(tab.is_none());
}

#[test]
fn a_broken_request_to_connect_is_turned_down_with_the_reason() {
    let (response, tab) = handle(
        &attach_request(Some(CHIMNEY), r#"{"sessionId":"x","ticket":"y"}"#),
        Some(&presence()),
    );
    assert_eq!(response.status, 400);
    assert!(tab.is_none());
    assert_eq!(
        response.header("Access-Control-Allow-Origin"),
        Some(CHIMNEY)
    );
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["error"], attach::Rejected::BadSession.message());
}

#[test]
fn a_ward_without_the_viewer_says_so() {
    let presence = Presence {
        can_attach: false,
        ..presence()
    };
    let (response, tab) = handle(
        &attach_request(Some(CHIMNEY), &attach_body()),
        Some(&presence),
    );
    assert_eq!(response.status, 501);
    assert!(tab.is_none());
}

#[test]
fn the_server_takes_a_request_to_connect_with_its_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let shared: Shared = Arc::new(RwLock::new(Some(presence())));
    let (attaches, tabs) = async_channel::unbounded();
    std::thread::spawn(move || run(listener, shared, attaches));

    let body = attach_body();
    let mut stream = TcpStream::connect(address).unwrap();
    let head = format!(
        "POST {} HTTP/1.1\r\nHost: {address}\r\nOrigin: {CHIMNEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        attach::PATH,
        body.len()
    );
    // The head and the body come apart, as they may over a socket.
    stream.write_all(head.as_bytes()).unwrap();
    stream.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    stream.write_all(body.as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 202 Accepted\r\n"), "{answer}");
    let tab = tabs.try_recv().unwrap();
    assert_eq!(tab.title, "laptop · #4711 Задача");
}

/// A socket the browser opened ahead of time and left empty does not hold the answer to the
/// card: it waits on its own thread, and the request after it is answered at once.
#[test]
fn an_empty_socket_does_not_hold_the_answer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let shared: Shared = Arc::new(RwLock::new(Some(presence())));
    let (attaches, _tabs) = async_channel::unbounded();
    std::thread::spawn(move || run(listener, shared, attaches));

    let _empty = TcpStream::connect(address).unwrap();
    let started = std::time::Instant::now();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            format!("GET {PATH} HTTP/1.1\r\nHost: {address}\r\nOrigin: {CHIMNEY}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(
        started.elapsed() < IO_TIMEOUT / 2,
        "answered in {:?}",
        started.elapsed()
    );
}
