use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};

use super::*;

const CHIMNEY: &str = "https://chimney.steep-man.ru";

fn presence() -> Presence {
    Presence {
        worker: "steep-pc".to_owned(),
        origin: CHIMNEY.to_owned(),
    }
}

fn request(method: &str, path: &str, origin: Option<&str>) -> Request {
    Request {
        method: method.to_owned(),
        path: path.to_owned(),
        origin: origin.map(str::to_owned),
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
        Some(presence())
    );
}

#[test]
fn requests_are_parsed_with_their_origin() {
    let head = "GET /ward/presence?t=1 HTTP/1.1\r\nHost: 127.0.0.1:47615\r\norigin: https://chimney.steep-man.ru\r\n\r\n";
    assert_eq!(parse(head), Some(request("GET", PATH, Some(CHIMNEY))));
    assert_eq!(parse("hello"), None);
    assert_eq!(parse("GET /ward/presence\r\n\r\n"), None);
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
    std::thread::spawn(move || run(listener, shared));

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
