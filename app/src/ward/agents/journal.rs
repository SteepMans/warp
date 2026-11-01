//! The transcript of a session, cut into chunks of whole new lines for
//! `POST /sessions/{id}/journal?seq=N`. `seq` starts at 1 and grows by one per chunk. Chunks
//! wait in order until the service took them, so a network failure only delays them; the same
//! `seq` is resent as is, which the service treats as a repeat.
//!
//! A journal outlives a restart of Ward through [`Journal::sent_position`] and
//! [`Journal::resume`]: what is saved is the position the service has already confirmed, not
//! the position read. Chunks the service had not taken yet are cut from the transcript again
//! after the restart and go out under the same numbers, so nothing of the transcript is lost
//! and no chunk is sent twice.

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
    /// Which part of the journal the chunk belongs to; a chunk of an earlier part says nothing
    /// about where the transcript being read now is taken up to.
    part: u64,
    /// Bytes of its part the transcript is read up to once this chunk is taken: the position a
    /// restart of Ward goes on from.
    end_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    /// Bytes of the transcript already cut into chunks.
    offset: u64,
    next_seq: u64,
    /// The part being read; [`Journal::start_part`] opens the next one.
    part: u64,
    /// Bytes of the part being read that the service has taken, and the `seq` of the last
    /// chunk it took.
    sent_offset: u64,
    sent_seq: u64,
    pending: VecDeque<Chunk>,
    tail: Vec<u8>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            offset: 0,
            next_seq: 1,
            part: 0,
            sent_offset: 0,
            sent_seq: 0,
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
            let bytes = complete[start..end].to_vec();
            let end_offset = self.offset + end as u64;
            self.push(bytes, end_offset);
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

    /// Queues one chunk under the next `seq`.
    fn push(&mut self, bytes: Vec<u8>, end_offset: u64) {
        self.pending.push_back(Chunk {
            seq: self.next_seq,
            bytes,
            part: self.part,
            end_offset,
        });
        self.next_seq += 1;
    }

    /// A line of Ward's own in the journal, at the position the transcript is read up to.
    pub fn mark(&mut self, line: &str) {
        let mut bytes = line.trim_end_matches('\n').as_bytes().to_vec();
        bytes.push(b'\n');
        self.push(bytes, self.offset);
    }

    /// Starts the part of the journal that belongs to another transcript: `marker` goes out as
    /// a line of its own and the next read starts at the beginning of the new file. The `seq`
    /// numbering goes on, so both sessions of one assignment share one journal, each part
    /// marked with its role.
    pub fn start_part(&mut self, marker: &str) {
        self.part += 1;
        self.offset = 0;
        self.sent_offset = 0;
        self.tail.clear();
        self.mark(marker);
    }

    /// Puts the journal back where the run before a restart of Ward left it: `offset` bytes of
    /// the transcript the service has taken and `next_seq` for the next chunk. What came after
    /// `offset` is read and sent again, because the service never confirmed it.
    pub fn resume(&mut self, offset: u64, next_seq: u64) {
        self.offset = offset;
        self.sent_offset = offset;
        self.sent_seq = next_seq.saturating_sub(1);
        self.next_seq = next_seq.max(1);
        self.pending.clear();
        self.tail.clear();
    }

    /// Where the service has taken the journal up to: bytes of the part being read and the
    /// `seq` the next chunk gets. Saved with the session, read back by [`Journal::resume`].
    pub fn sent_position(&self) -> (u64, u64) {
        (self.sent_offset, self.sent_seq + 1)
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

    /// Fills [`Journal::tail`] from the end of `path` without queueing anything and without
    /// moving the read position. A journal picked up after a restart of Ward sends only what
    /// the service has not taken, but what the session is read from — the agent's last turn, a
    /// subscription limit — is the end of its transcript, and that it has to know.
    pub fn prime_tail(&mut self, path: &Path) -> std::io::Result<()> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let from = len.saturating_sub(TAIL_BYTES as u64);
        file.seek(SeekFrom::Start(from))?;
        let mut bytes = Vec::new();
        file.take(TAIL_BYTES as u64).read_to_end(&mut bytes)?;
        // Whole lines only: the first line is cut off when the tail does not start the file,
        // and a line still being written at the end waits for the reader of the transcript.
        if from > 0 {
            let start = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |position| position + 1);
            bytes.drain(..start);
        }
        match bytes.iter().rposition(|byte| *byte == b'\n') {
            Some(last) => bytes.truncate(last + 1),
            None => bytes.clear(),
        }
        self.tail = bytes;
        Ok(())
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
        if self.pending.front().is_some_and(|chunk| chunk.seq == seq)
            && let Some(chunk) = self.pending.pop_front()
        {
            self.sent_seq = chunk.seq;
            // A chunk of a part already closed leaves the position of the current one alone.
            if chunk.part == self.part {
                self.sent_offset = chunk.end_offset;
            }
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
