use super::*;

#[test]
fn the_ring_keeps_everything_until_it_is_full() {
    let mut ring = Ring::new(16);
    ring.push(b"hello ");
    ring.push(b"world");
    assert_eq!(ring.snapshot(), b"hello world");
}

#[test]
fn a_full_ring_starts_at_the_next_whole_line() {
    let mut ring = Ring::new(12);
    ring.push(b"first line\r\n");
    ring.push(b"\x1b[31mred\r\n");
    // Only "\r\n" of the first line is left: the snapshot starts after it.
    assert_eq!(ring.snapshot(), b"\x1b[31mred\r\n");
    ring.push(b"next\r\n");
    assert_eq!(ring.snapshot(), b"next\r\n");
}

#[test]
fn a_full_ring_without_a_newline_gives_nothing_rather_than_half_a_sequence() {
    let mut ring = Ring::new(4);
    ring.push(b"\x1b[31mabcdef");
    assert!(ring.snapshot().is_empty());
}

#[test]
fn a_subscriber_gets_the_screen_and_then_every_new_read() {
    let mut tap = Tap::with_capacity(1024);
    tap.feed(Arc::new(b"before ".to_vec()));
    let (snapshot, receiver) = tap.subscribe();
    assert_eq!(snapshot, b"before ");
    tap.feed(Arc::new(b"after".to_vec()));
    assert_eq!(*receiver.try_recv().unwrap(), b"after".to_vec());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn a_stream_that_is_over_is_forgotten() {
    let mut tap = Tap::with_capacity(1024);
    let (_, receiver) = tap.subscribe();
    assert_eq!(tap.viewers(), 1);
    drop(receiver);
    tap.feed(Arc::new(b"x".to_vec()));
    assert_eq!(tap.viewers(), 0);
}

#[test]
fn the_stream_url_follows_the_service_address() {
    assert_eq!(
        worker_url("https://chimney.steep-man.ru/agents", "s-1").as_deref(),
        Some("wss://chimney.steep-man.ru/agents/api/v1/worker/stream/s-1")
    );
    assert_eq!(
        worker_url("http://127.0.0.1:8080/", "s-2").as_deref(),
        Some("ws://127.0.0.1:8080/api/v1/worker/stream/s-2")
    );
    assert_eq!(worker_url("ftp://example.com", "s-3"), None);
}

#[test]
fn control_messages_are_json() {
    let size: serde_json::Value = serde_json::from_str(&size_message(120, 40)).unwrap();
    assert_eq!(size["type"], "size");
    assert_eq!(size["cols"], 120);
    assert_eq!(size["rows"], 40);
    let ping: serde_json::Value = serde_json::from_str(&ping_message()).unwrap();
    assert_eq!(ping["type"], "ping");
}

#[test]
fn a_viewer_gives_its_size_in_a_text_frame() {
    assert_eq!(
        parse_viewer_frame(r#"{"type":"size","cols":132,"rows":41}"#),
        Some(ViewerFrame::Size {
            cols: 132,
            rows: 41
        })
    );
    assert_eq!(
        parse_viewer_frame(r#"{"type":"release"}"#),
        Some(ViewerFrame::Release)
    );
}

#[test]
fn text_frames_that_are_not_a_size_are_ignored() {
    for text in [
        "hello",
        "{}",
        r#"{"type":"ping"}"#,
        r#"{"type":"size"}"#,
        r#"{"type":"size","cols":120}"#,
        r#"{"type":"size","cols":0,"rows":40}"#,
        r#"{"type":"size","cols":-1,"rows":40}"#,
        r#"{"type":"size","cols":"120","rows":40}"#,
        r#"{"type":"size","cols":120.5,"rows":40}"#,
        r#"{"type":"size","cols":100000,"rows":40}"#,
        r#"{"type":"size","cols":120,"rows":100000}"#,
    ] {
        assert_eq!(parse_viewer_frame(text), None, "{text}");
    }
}

#[test]
fn the_tab_takes_the_size_of_the_last_viewer_and_gets_its_own_back() {
    let mut sizing = Sizing::default();
    assert_eq!(sizing.on_end(), None);
    assert_eq!(sizing.on_frame(ViewerFrame::Release), None);

    let first = ViewerFrame::Size {
        cols: 100,
        rows: 30,
    };
    let second = ViewerFrame::Size {
        cols: 160,
        rows: 48,
    };
    assert_eq!(
        sizing.on_frame(first),
        Some(Resize::To {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(
        sizing.on_frame(second),
        Some(Resize::To {
            cols: 160,
            rows: 48
        })
    );
    assert_eq!(sizing.given(), Some((160, 48)));
    // The first viewer sends its size again and takes the tab back.
    assert_eq!(
        sizing.on_frame(first),
        Some(Resize::To {
            cols: 100,
            rows: 30
        })
    );

    // No viewer asks for a size any more: the pane's own size, once.
    assert_eq!(sizing.on_frame(ViewerFrame::Release), Some(Resize::Back));
    assert_eq!(sizing.on_frame(ViewerFrame::Release), None);
    assert_eq!(sizing.on_end(), None);
    assert_eq!(Resize::Back.size(), None);
    assert_eq!(
        Resize::To {
            cols: 100,
            rows: 30
        }
        .size(),
        Some((100, 30))
    );
}

#[test]
fn the_end_of_the_stream_gives_the_tab_its_own_size_back() {
    let mut sizing = Sizing::default();
    sizing.on_frame(ViewerFrame::Size { cols: 90, rows: 25 });
    assert_eq!(sizing.on_end(), Some(Resize::Back));
    assert_eq!(sizing.given(), None);
    assert_eq!(sizing.on_end(), None);
}
