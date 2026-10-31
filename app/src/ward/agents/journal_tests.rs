use std::io::Write as _;

use super::*;

#[test]
fn chunks_hold_whole_lines_with_growing_seq() {
    let mut journal = Journal::default();
    assert!(!journal.ingest(b"{\"a\":1"));
    assert!(journal.next_pending().is_none());

    assert!(journal.ingest(b"{\"a\":1}\n{\"b\":2}\n{\"c\""));
    let chunk = journal.next_pending().unwrap().clone();
    assert_eq!(chunk.seq, 1);
    assert_eq!(chunk.bytes, b"{\"a\":1}\n{\"b\":2}\n");
    assert_eq!(journal.offset, 16);

    // A failed send keeps the chunk; the next one queues behind it.
    assert!(journal.ingest(b"{\"c\":3}\n"));
    assert_eq!(journal.next_pending().unwrap().seq, 1);
    journal.mark_sent(2);
    assert_eq!(journal.next_pending().unwrap().seq, 1);
    journal.mark_sent(1);
    let chunk = journal.next_pending().unwrap();
    assert_eq!(chunk.seq, 2);
    assert_eq!(chunk.bytes, b"{\"c\":3}\n");
    journal.mark_sent(2);
    assert!(!journal.has_pending());
    assert_eq!(journal.next_seq, 3);
    assert_eq!(journal.tail(), "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n");
}

#[test]
fn long_runs_split_at_line_ends() {
    let mut journal = Journal::default();
    let line = format!("{}\n", "x".repeat(MAX_CHUNK_BYTES / 2 - 1));
    let huge = format!("{}\n", "y".repeat(MAX_CHUNK_BYTES + 10));
    let text = format!("{line}{line}{line}{huge}{line}");
    assert!(journal.ingest(text.as_bytes()));
    let mut sizes = Vec::new();
    let mut seqs = Vec::new();
    while let Some(chunk) = journal.next_pending().cloned() {
        assert!(chunk.bytes.ends_with(b"\n"));
        sizes.push(chunk.bytes.len());
        seqs.push(chunk.seq);
        journal.mark_sent(chunk.seq);
    }
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_eq!(sizes, [line.len() * 2, line.len(), huge.len(), line.len()]);
    assert_eq!(journal.offset, text.len() as u64);
    assert!(journal.tail().len() <= 64 * 1024);
}

#[test]
fn reads_appended_lines_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let mut journal = Journal::default();
    assert!(journal.read_from(&path).is_err());

    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"{\"n\":1}\n{\"n\":").unwrap();
    file.flush().unwrap();
    assert!(journal.read_from(&path).unwrap());
    assert!(!journal.read_from(&path).unwrap());
    file.write_all(b"2}\n").unwrap();
    file.flush().unwrap();
    assert!(journal.read_from(&path).unwrap());
    let first = journal.next_pending().unwrap().clone();
    journal.mark_sent(first.seq);
    let second = journal.next_pending().unwrap().clone();
    assert_eq!(first.bytes, b"{\"n\":1}\n");
    assert_eq!(second.bytes, b"{\"n\":2}\n");
    assert_eq!(second.seq, 2);
}

#[test]
fn a_new_part_keeps_the_numbering_and_starts_the_file_over() {
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("orchestrator.jsonl");
    let second_path = dir.path().join("executor.jsonl");
    std::fs::write(&first_path, "{\"n\":1}\n{\"n\":2}\n").unwrap();
    std::fs::write(&second_path, "{\"n\":3}\n").unwrap();

    let mut journal = Journal::default();
    journal.start_part("{\"role\":\"orchestrator\"}");
    journal.read_from(&first_path).unwrap();
    journal.start_part("{\"role\":\"executor\"}");
    journal.read_from(&second_path).unwrap();

    let mut chunks = Vec::new();
    while let Some(chunk) = journal.next_pending().cloned() {
        chunks.push((chunk.seq, String::from_utf8(chunk.bytes).unwrap()));
        journal.mark_sent(chunk.seq);
    }
    assert_eq!(
        chunks,
        [
            (1, "{\"role\":\"orchestrator\"}\n".to_owned()),
            (2, "{\"n\":1}\n{\"n\":2}\n".to_owned()),
            (3, "{\"role\":\"executor\"}\n".to_owned()),
            (4, "{\"n\":3}\n".to_owned()),
        ]
    );
    // The tail is the new part only: the end of the executor's transcript.
    assert_eq!(journal.tail(), "{\"n\":3}\n");
}
