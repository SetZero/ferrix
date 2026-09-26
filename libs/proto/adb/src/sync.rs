//! The `sync:` service: `adb push` and `adb pull`, in the first version of
//! its protocol (`SYNC.TXT`), which a device that offers no `stat_v2`,
//! `ls_v2` or `sendrecv_v2` feature gets.
//!
//! Every packet is a four-letter id and a little-endian `u32`, and for most
//! ids that many bytes after it. A packet can span `WRTE`s, and one `WRTE`
//! can carry several packets, so [`Reader`] takes bytes as they come and
//! gives whole requests.

use alloc::vec::Vec;

/// A request's id, as four letters.
pub type Id = [u8; 4];

/// `STAT path`: the mode, size and modification time of a path.
pub const STAT: Id = *b"STAT";
/// `LIST path`: a directory's entries.
pub const LIST: Id = *b"LIST";
/// `SEND path,mode`: a file follows, as `DATA` packets and a `DONE`.
pub const SEND: Id = *b"SEND";
/// `RECV path`: send the file back, as `DATA` packets and a `DONE`.
pub const RECV: Id = *b"RECV";
/// Bytes of a file, either way.
pub const DATA: Id = *b"DATA";
/// The end of a file, with its modification time in the length's place; or
/// the end of a listing.
pub const DONE: Id = *b"DONE";
/// A directory entry, in a listing.
pub const DENT: Id = *b"DENT";
/// A `SEND` that worked.
pub const OKAY: Id = *b"OKAY";
/// Something that did not, with why.
pub const FAIL: Id = *b"FAIL";
/// The end of the session.
pub const QUIT: Id = *b"QUIT";

/// The most a `DATA` packet carries (`SYNC_DATA_MAX`).
pub const DATA_MAX: usize = 64 * 1024;

/// The longest path a request may name, as adbd's own limit.
pub const PATH_MAX: usize = 1024;

/// One request, whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Which: [`STAT`] and the rest.
    pub id: Id,
    /// The `u32` after the id: the payload's length for most requests, the
    /// modification time for a `DONE`.
    pub value: u32,
    /// The payload: a path, a `path,mode`, or a file's bytes.
    pub data: Vec<u8>,
}

/// What can be wrong with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// A payload longer than its id allows.
    TooLong(Id, u32),
}

/// Takes the stream's bytes as they come and gives whole requests.
#[derive(Debug, Default)]
pub struct Reader {
    pending: Vec<u8>,
}

impl Reader {
    /// An empty reader.
    #[must_use]
    pub fn new() -> Self {
        Reader::default()
    }

    /// Take bytes the stream carried.
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// The next whole request, if the bytes so far hold one.
    ///
    /// # Errors
    ///
    /// A request whose length is more than its id allows, after which the
    /// session is over.
    pub fn next_request(&mut self) -> Result<Option<Request>, RequestError> {
        let Some(head) = self.pending.get(..8) else {
            return Ok(None);
        };
        let mut id = [0; 4];
        id.copy_from_slice(head.get(..4).unwrap_or_default());
        let mut value = [0; 4];
        value.copy_from_slice(head.get(4..8).unwrap_or_default());
        let value = u32::from_le_bytes(value);
        // A DONE's and a QUIT's word is a value, not a length.
        let length = if id == DONE || id == QUIT {
            0
        } else {
            let limit = if id == DATA { DATA_MAX } else { PATH_MAX };
            let length = value as usize;
            if length > limit {
                return Err(RequestError::TooLong(id, value));
            }
            length
        };
        let Some(data) = self.pending.get(8..8 + length) else {
            return Ok(None);
        };
        let request = Request {
            id,
            value,
            data: data.to_vec(),
        };
        let _taken = self.pending.drain(..8 + length);
        Ok(Some(request))
    }
}

/// A packet: an id, a word, and bytes after it.
fn packet(id: Id, words: &[u32], data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + words.len() * 4 + data.len());
    bytes.extend_from_slice(&id);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(data);
    bytes
}

/// The answer to `STAT`: mode, size and modification time, all zero for a
/// path that does not exist.
#[must_use]
pub fn stat(mode: u32, size: u32, mtime: u32) -> Vec<u8> {
    packet(STAT, &[mode, size, mtime], &[])
}

/// One entry of a listing.
#[must_use]
pub fn dent(mode: u32, size: u32, mtime: u32, name: &[u8]) -> Vec<u8> {
    let length = u32::try_from(name.len()).unwrap_or(u32::MAX);
    packet(DENT, &[mode, size, mtime, length], name)
}

/// The end of a listing: a `DONE` shaped as an empty entry.
#[must_use]
pub fn list_done() -> Vec<u8> {
    packet(DONE, &[0, 0, 0, 0], &[])
}

/// Bytes of a file being received.
#[must_use]
pub fn data(bytes: &[u8]) -> Vec<u8> {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    packet(DATA, &[length], bytes)
}

/// The end of a file being received.
#[must_use]
pub fn recv_done() -> Vec<u8> {
    packet(DONE, &[0], &[])
}

/// A `SEND` that worked.
#[must_use]
pub fn okay() -> Vec<u8> {
    packet(OKAY, &[0], &[])
}

/// A request that did not, and why, which the host prints.
#[must_use]
pub fn fail(why: &str) -> Vec<u8> {
    let length = u32::try_from(why.len()).unwrap_or(u32::MAX);
    packet(FAIL, &[length], why.as_bytes())
}

/// A `SEND`'s payload, `path,mode`, split: the path, and the mode if the
/// part after the last comma is one.
#[must_use]
pub fn send_target(payload: &[u8]) -> (&[u8], Option<u32>) {
    let Some(comma) = payload.iter().rposition(|&byte| byte == b',') else {
        return (payload, None);
    };
    let (path, mode) = payload.split_at(comma);
    let mode = core::str::from_utf8(mode.get(1..).unwrap_or_default())
        .ok()
        .and_then(|text| text.parse().ok());
    (path, mode)
}
