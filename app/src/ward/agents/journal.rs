//! The transcript of a session, cut into chunks of whole new lines for
//! `POST /sessions/{id}/journal?seq=N`. `seq` starts at 1 and grows by one per chunk. Chunks
//! wait in order until the service took them, so a network failure only delays them; the same
//! `seq` is resent as is, which the service treats as a repeat.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// Largest chunk; a longer run of new lines is split at line ends.
pub const MAX_CHUNK_BYTES: usize = 1024 * 1024;

/// Most bytes read from the transcript per tick.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// How much of the newest text is kept for [`Journal::tail`].
const TAIL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub seq: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    /// Bytes of the transcript already cut into chunks.
    offset: u64,
    next_seq: u64,
    pending: VecDeque<Chunk>,
    tail: Vec<u8>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            offset: 0,
            next_seq: 1,
            pending: VecDeque::new(),
            tail: Vec::new(),
        }
    }
}

impl Journal {
    /// Takes the complete lines of `bytes`, the transcript from where the last call stopped. A
    /// line still being written (no `\n` yet) stays for the next call. Returns whether new
    /// chunks were added.
    pub fn ingest(&mut self, bytes: &[u8]) -> bool {
        let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            return false;
        };
        let complete = &bytes[..=last_newline];
        let mut start = 0;
        while start < complete.len() {
            let mut end = (start + MAX_CHUNK_BYTES).min(complete.len());
            if end < complete.len() {
                // Cut after the last line end inside the limit; one huge line goes whole.
                end = match complete[start..end].iter().rposition(|byte| *byte == b'\n') {
                    Some(position) => start + position + 1,
                    None => {
                        start
                            + complete[start..]
                                .iter()
                                .position(|byte| *byte == b'\n')
                                .map_or(complete.len() - start, |position| position + 1)
                    }
                };
            }
            self.pending.push_back(Chunk {
                seq: self.next_seq,
                bytes: complete[start..end].to_vec(),
            });
            self.next_seq += 1;
            start = end;
        }
        self.offset += complete.len() as u64;
        self.tail.extend_from_slice(complete);
        if self.tail.len() > TAIL_BYTES {
            let cut = self.tail.len() - TAIL_BYTES;
            let cut = self.tail[cut..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(cut, |position| cut + position + 1);
            self.tail.drain(..cut);
        }
        true
    }

    /// Starts the part of the journal that belongs to another transcript: `marker` goes out as
    /// a line of its own and the next read starts at the beginning of the new file. The `seq`
    /// numbering goes on, so both sessions of one assignment share one journal, each part
    /// marked with its role.
    pub fn start_part(&mut self, marker: &str) {
        let mut bytes = marker.trim_end_matches('\n').as_bytes().to_vec();
        bytes.push(b'\n');
        self.pending.push_back(Chunk {
            seq: self.next_seq,
            bytes,
        });
        self.next_seq += 1;
        self.offset = 0;
        self.tail.clear();
    }

    /// Reads what was appended to `path` since the last call. Returns whether new chunks were
    /// added.
    pub fn read_from(&mut self, path: &Path) -> std::io::Result<bool> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len <= self.offset {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.take(MAX_READ_BYTES).read_to_end(&mut bytes)?;
        Ok(self.ingest(&bytes))
    }

    /// The oldest chunk the service has not taken yet.
    pub fn next_pending(&self) -> Option<&Chunk> {
        self.pending.front()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether the transcript of the part being read has given a whole line yet: the one sign
    /// that the agent of this launch is working at all.
    pub fn has_lines(&self) -> bool {
        self.offset > 0
    }

    /// The service took chunk `seq`.
    pub fn mark_sent(&mut self, seq: u64) {
        if self.pending.front().is_some_and(|chunk| chunk.seq == seq) {
            self.pending.pop_front();
        }
    }

    /// The newest whole lines, for telling how the session ended.
    pub fn tail(&self) -> String {
        String::from_utf8_lossy(&self.tail).into_owned()
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
