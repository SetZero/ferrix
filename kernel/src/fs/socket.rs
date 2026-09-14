//! Unix-domain sockets, as the inodes an open file reads and writes through.
//!
//! `libs/vfs`'s [`SocketBuffer`] is one direction of a socket as values: the
//! queue, the record boundaries, and a write that would wait saying so. This is
//! the half that waits, in the shape `fs::pipe` gives a pipe. Each direction is
//! a [`Channel`] -- that buffer behind a lock, with a wait queue for readers and
//! one for writers -- and a [`Socket`] reads from its own channel and writes
//! into its peer's.
//!
//! # What there is so far
//!
//! Sockets made by `socket(AF_UNIX, ...)`, and by `socketpair` connected to each
//! other: stream, sequenced-packet and datagram, read and written through
//! `read`, `write`, `send*` and `recv*`, shut down a direction at a time,
//! polled, and asked their queue lengths and options. Names -- `bind`,
//! `listen`, `connect`, `accept` -- come next, and descriptor passing after
//! them; until then a socket made by `socket` is connected to nothing.
//!
//! # Never waiting with a buffer locked
//!
//! As for a pipe: every operation binds what the buffer answered to a variable,
//! so the guard is gone before anything below can sleep, and a wait looks at
//! the buffer again under its lock after it has joined the queue.
//!
//! # A direction closes in more than one way
//!
//! A channel's buffer knows whether its writer has gone, which a reader sees as
//! end of file, and whether its reader has, which a writer sees as a broken
//! socket. `shutdown` needs a state between them that Linux has and the buffer
//! does not: a direction whose reader shut it down still holds what was queued
//! for reading, so its reader is not gone, yet a writer must be refused. That
//! is [`Channel::refused`], checked before every write.
//!
//! # `SIGPIPE`
//!
//! Only a stream socket raises it, and only when it has sent nothing, as
//! Linux's `unix_stream_sendmsg` does; `MSG_NOSIGNAL` keeps it back. A
//! sequenced-packet socket gets `EPIPE` alone, and a datagram socket whose peer
//! has gone `ECONNREFUSED`.
//!
//! # A datagram socket's peer leaving is not end of file
//!
//! A stream or sequenced-packet socket reads end of file once its peer has
//! gone and the queue is drained. A datagram socket does not: its reader waits
//! for the next datagram, which on Linux could come from anyone, and only its
//! own `shutdown(SHUT_RD)` ends its reads.

use alloc::format;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferrix_linux_abi::socket::{
    AF_UNIX, Linger, MSG_DONTWAIT, MSG_NOSIGNAL, MSG_PEEK, MSG_WAITALL, SHUT_RD, SHUT_RDWR,
    SHUT_WR, SIOCINQ, SIOCOUTQ, SO_ACCEPTCONN, SO_BROADCAST, SO_DEBUG, SO_DOMAIN, SO_DONTROUTE,
    SO_ERROR, SO_KEEPALIVE, SO_LINGER, SO_OOBINLINE, SO_PASSCRED, SO_PEERCRED, SO_PRIORITY,
    SO_PROTOCOL, SO_RCVBUF, SO_RCVBUFFORCE, SO_RCVLOWAT, SO_RCVTIMEO_NEW, SO_RCVTIMEO_OLD,
    SO_REUSEADDR, SO_SNDBUF, SO_SNDBUFFORCE, SO_SNDLOWAT, SO_SNDTIMEO_NEW, SO_SNDTIMEO_OLD,
    SO_TYPE, SOCK_DGRAM, SOCK_RAW, SOCK_SEQPACKET, SOCK_STREAM, SOCKET_BUFFER_DEFAULT,
    SOCKET_BUFFER_MAX, SOCKET_BUFFER_MIN, SOL_SOCKET, Ucred, Width,
};
use ferrix_sync::Once;
use ferrix_vfs::path::NAME_MAX;
use ferrix_vfs::socket::{Kind, ReadOutcome, SocketBuffer, WriteOutcome};
use ferrix_vfs::{
    Errno, FileSystem, FileType, Inode, Location, Metadata, OpenFile, OpenFlags, Readiness, StatFs,
    Timespec,
};

use crate::fs;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::syscall::process::{self, Process};
use crate::syscall::uaccess;

/// The deadline of a wait with no timeout: none.
const FOREVER: u64 = u64::MAX;

/// The block size `stat` reports for a socket: a page, as Linux reports.
const BLOCK_SIZE: u32 = 4096;

/// `SOCKFS_MAGIC`, what `fstatfs` on a socket reports.
const SOCKFS_MAGIC: u64 = 0x534F_434B;

/// The ids `SO_PEERCRED` reports for a socket that has no peer credentials:
/// Linux's `overflowuid` and `overflowgid`.
const OVERFLOW_ID: u32 = 65_534;

/// Nanoseconds in a second, for the timeouts a `timeval` carries.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Nanoseconds in a microsecond.
const NANOS_PER_MICRO: u64 = 1_000;

/// What travels beside the bytes of a message. Nothing yet: descriptors and
/// credentials come with descriptor passing.
type Ancillary = ();

/// The three kinds of `AF_UNIX` socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketType {
    /// `SOCK_STREAM`: a byte stream.
    Stream,
    /// `SOCK_SEQPACKET`: records, in order, connected.
    SeqPacket,
    /// `SOCK_DGRAM`: records.
    Datagram,
}

impl SocketType {
    /// The type a `socket` call's type names, if `AF_UNIX` has it. `SOCK_RAW`
    /// is a datagram socket, as `unix_create` makes it.
    pub(crate) fn from_linux(kind: u32) -> Option<SocketType> {
        match kind {
            SOCK_STREAM => Some(SocketType::Stream),
            SOCK_SEQPACKET => Some(SocketType::SeqPacket),
            SOCK_DGRAM | SOCK_RAW => Some(SocketType::Datagram),
            _ => None,
        }
    }

    /// The type as `SO_TYPE` reports it.
    pub(crate) fn linux(self) -> u32 {
        match self {
            SocketType::Stream => SOCK_STREAM,
            SocketType::SeqPacket => SOCK_SEQPACKET,
            SocketType::Datagram => SOCK_DGRAM,
        }
    }

    /// Whether its buffer keeps bytes or records.
    fn buffer_kind(self) -> Kind {
        match self {
            SocketType::Stream => Kind::Stream,
            SocketType::SeqPacket | SocketType::Datagram => Kind::Record,
        }
    }
}

/// One direction of a socket: the buffer, and who waits on it.
struct Channel {
    buffer: SpinLock<SocketBuffer<Ancillary>>,
    /// Woken when a read may no longer wait: data arrived, or the writer left
    /// or shut this direction down.
    readable: WaitQueue,
    /// Woken when a write may no longer wait: room was made, or the reader
    /// left or shut this direction down.
    writable: WaitQueue,
    /// Whether this direction takes no more writes: its reader shut it down,
    /// or went. See the module documentation for why the buffer's own state
    /// is not enough.
    refused: AtomicBool,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

impl Channel {
    /// An empty direction of a socket of `kind`, at the default buffer size.
    fn new(kind: SocketType) -> Arc<Channel> {
        Arc::new(Channel {
            buffer: SpinLock::new(SocketBuffer::new(kind.buffer_kind(), SOCKET_BUFFER_DEFAULT)),
            readable: WaitQueue::new(),
            writable: WaitQueue::new(),
            refused: AtomicBool::new(false),
        })
    }

    /// Wake both directions' waiters: an end closed or shut down, which can
    /// end a wait on either side.
    fn wake_both(&self) {
        self.readable.wake_all();
        self.writable.wake_all();
    }

    /// Whether a write into this direction is refused.
    fn is_refused(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }
}

/// What a socket keeps besides its directions.
#[derive(Debug, Clone, Copy)]
struct Options {
    /// `SO_RCVTIMEO`, in nanoseconds; zero waits forever.
    receive_timeout: u64,
    /// `SO_SNDTIMEO`, likewise.
    send_timeout: u64,
    /// `SO_SNDBUF` as set, which is reported back; the receive side's own
    /// buffer is what limits a sender here.
    send_buffer: usize,
    /// `SO_PASSCRED`, kept for when credentials travel.
    pass_credentials: bool,
    /// `shutdown(SHUT_WR)` was called: this socket sends no more.
    shut_write: bool,
}

/// An `AF_UNIX` socket, as the inode its open file reads and writes through.
pub(crate) struct Socket {
    kind: SocketType,
    /// What this socket reads from.
    receive: Arc<Channel>,
    /// What this socket writes into: its peer's receive direction, while it
    /// has a peer.
    send: SpinLock<Option<Arc<Channel>>>,
    options: SpinLock<Options>,
    /// What `SO_PEERCRED` reports: for a pair, whoever made it.
    peer_credentials: Option<Ucred>,
    /// What `stat` reports through it.
    metadata: Metadata,
}

impl fmt::Debug for Socket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Socket")
            .field("kind", &self.kind)
            .field("ino", &self.metadata.ino)
            .finish_non_exhaustive()
    }
}

/// What a receive took: the bytes copied, and for a record the whole record's
/// length, which `MSG_TRUNC` answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Received {
    /// Bytes copied into the caller's buffer.
    pub(crate) bytes: usize,
    /// For a record, its whole length; for a stream, `bytes`.
    pub(crate) full: usize,
}

/// The credentials `SO_PEERCRED` reports for a socket `process` made:
/// its pid and effective ids, as Linux's `init_peercred` takes them.
fn credentials_of(process: &Process) -> Ucred {
    let (uid, gid) = process
        .with_credentials(|credentials| (credentials.user.effective, credentials.group.effective));
    Ucred {
        pid: i32::try_from(process.pid()).unwrap_or(0),
        uid,
        gid,
    }
}

/// When a wait with `timeout` nanoseconds of patience must give up; zero
/// waits forever.
fn deadline(timeout: u64) -> u64 {
    if timeout == 0 {
        FOREVER
    } else {
        crate::timer::now_nanos().saturating_add(timeout)
    }
}

/// Sleep on `queue` until `ready`, the caller has a signal to take, or
/// `deadline` passes.
///
/// A signal is a restart code for a wait with no deadline and `EINTR` for one
/// with a timeout, which is Linux's `sock_intr_errno`; a timeout that runs out
/// is `EAGAIN`, as a socket answers.
fn wait(queue: &WaitQueue, mut ready: impl FnMut() -> bool, deadline: u64) -> Result<(), Errno> {
    let caller = process::current();
    let signalled = || {
        caller
            .as_ref()
            .is_some_and(|process| process.signal_pending())
    };
    let _ = queue.wait_until_deadline(|| ready() || signalled(), deadline);
    if signalled() {
        return Err(if deadline == FOREVER {
            Errno::ERESTARTSYS
        } else {
            Errno::EINTR
        });
    }
    if deadline != FOREVER && !ready() && crate::timer::now_nanos() >= deadline {
        return Err(Errno::EAGAIN);
    }
    Ok(())
}

impl Socket {
    /// A socket of `kind` reading from `receive` and writing into `send`,
    /// owned by `owner`: the creator's filesystem user and group ids.
    fn new(
        kind: SocketType,
        send: Option<Arc<Channel>>,
        receive: Arc<Channel>,
        peer_credentials: Option<Ucred>,
        (uid, gid): (u32, u32),
    ) -> Arc<Socket> {
        let ino = sockfs().next_ino.fetch_add(1, Ordering::Relaxed);
        let now = fs::clock().now();
        Arc::new(Socket {
            kind,
            receive,
            send: SpinLock::new(send),
            options: SpinLock::new(Options {
                receive_timeout: 0,
                send_timeout: 0,
                send_buffer: SOCKET_BUFFER_DEFAULT,
                pass_credentials: false,
                shut_write: false,
            }),
            peer_credentials,
            metadata: Metadata {
                ino,
                kind: FileType::Socket,
                // A socket's inode is `S_IFSOCK | 0777` on sockfs, as on Linux.
                permissions: 0o777,
                nlink: 1,
                uid,
                gid,
                size: 0,
                rdev: 0,
                blocks: 0,
                block_size: BLOCK_SIZE,
                atime: now,
                mtime: now,
                ctime: now,
            },
        })
    }

    /// Its type.
    pub(crate) fn kind(&self) -> SocketType {
        self.kind
    }

    /// Whether it has a peer to send to.
    pub(crate) fn is_connected(&self) -> bool {
        self.send.lock().is_some()
    }

    /// What a write into a direction nobody reads any more answers: Linux's
    /// signal for a stream, `EPIPE` for a sequenced-packet socket, and
    /// `ECONNREFUSED` for a datagram whose peer has gone.
    fn broken(&self, flags: u32) -> Errno {
        match self.kind {
            SocketType::Stream => {
                if flags & MSG_NOSIGNAL == 0 {
                    crate::syscall::kill::send_to_current(ferrix_linux_abi::types::SIGPIPE);
                }
                Errno::EPIPE
            }
            SocketType::SeqPacket => Errno::EPIPE,
            SocketType::Datagram => Errno::ECONNREFUSED,
        }
    }

    /// Send `data` to the peer: all of it for a stream, waiting for room as
    /// often as it takes unless non-blocking, and one whole record otherwise.
    ///
    /// # Errors
    ///
    /// `ENOTCONN` with no peer; `EPIPE` after `shutdown(SHUT_WR)` or with the
    /// peer gone (see [`Socket::broken`]); `EMSGSIZE` for a record larger than
    /// the peer's buffer; `EAGAIN` when it would wait and may not, or its
    /// timeout ran out; a restart code or `EINTR` for a signal. A stream that
    /// sent part of `data` first reports the count instead.
    pub(crate) fn send(&self, data: &[u8], flags: u32, nonblock: bool) -> Result<usize, Errno> {
        let peer = self.send.lock().clone().ok_or(Errno::ENOTCONN)?;
        let options = *self.options.lock();
        if options.shut_write {
            return Err(match self.kind {
                SocketType::Stream => self.broken(flags),
                SocketType::SeqPacket | SocketType::Datagram => Errno::EPIPE,
            });
        }
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let deadline = deadline(options.send_timeout);
        match self.kind {
            SocketType::Stream => self.send_stream(&peer, data, flags, nonblock, deadline),
            SocketType::SeqPacket | SocketType::Datagram => {
                self.send_record(&peer, data, flags, nonblock, deadline)
            }
        }
    }

    /// [`Socket::send`] for a stream.
    fn send_stream(
        &self,
        peer: &Channel,
        data: &[u8],
        flags: u32,
        nonblock: bool,
        deadline: u64,
    ) -> Result<usize, Errno> {
        // An empty write succeeds without looking for a reader, as on Linux.
        if data.is_empty() {
            return Ok(0);
        }
        let mut done = 0;
        while done < data.len() {
            let rest = data.get(done..).unwrap_or_default();
            let outcome = if peer.is_refused() {
                WriteOutcome::Broken
            } else {
                let mut nothing = None;
                // The guard is named and given back here, so that nothing
                // below this can wait while the buffer is locked.
                let mut buffer = peer.buffer.lock();
                let outcome = buffer.write(rest, &mut nothing);
                drop(buffer);
                outcome
            };
            let refusal = match outcome {
                WriteOutcome::Wrote(count) => {
                    done += count;
                    peer.readable.wake_all();
                    continue;
                }
                // Nothing sent: the refusal, with its signal. Something sent:
                // the count, and no signal, as Linux reports a short send.
                WriteOutcome::Broken if done == 0 => return Err(self.broken(flags)),
                WriteOutcome::Broken => return Ok(done),
                WriteOutcome::TooBig => Errno::EMSGSIZE,
                WriteOutcome::WouldBlock if nonblock => Errno::EAGAIN,
                WriteOutcome::WouldBlock => match wait(
                    &peer.writable,
                    || peer.is_refused() || peer.buffer.lock().can_write(1),
                    deadline,
                ) {
                    Ok(()) => continue,
                    Err(errno) => errno,
                },
            };
            return if done > 0 { Ok(done) } else { Err(refusal) };
        }
        Ok(done)
    }

    /// [`Socket::send`] for a sequenced-packet or datagram socket: one record,
    /// queued whole or not at all.
    fn send_record(
        &self,
        peer: &Channel,
        data: &[u8],
        flags: u32,
        nonblock: bool,
        deadline: u64,
    ) -> Result<usize, Errno> {
        loop {
            let outcome = if peer.is_refused() {
                // A datagram socket that shut its reading down refuses with
                // EPIPE, as `unix_dgram_sendmsg` does; one that has gone, with
                // ECONNREFUSED.
                if self.kind == SocketType::Datagram && !peer.buffer.lock().reader_closed() {
                    return Err(Errno::EPIPE);
                }
                WriteOutcome::Broken
            } else {
                let mut nothing = None;
                // The guard is named and given back here, so that nothing
                // below this can wait while the buffer is locked.
                let mut buffer = peer.buffer.lock();
                let outcome = buffer.write(data, &mut nothing);
                drop(buffer);
                outcome
            };
            match outcome {
                WriteOutcome::Wrote(count) => {
                    peer.readable.wake_all();
                    return Ok(count);
                }
                WriteOutcome::Broken => return Err(self.broken(flags)),
                WriteOutcome::TooBig => return Err(Errno::EMSGSIZE),
                WriteOutcome::WouldBlock if nonblock => return Err(Errno::EAGAIN),
                WriteOutcome::WouldBlock => wait(
                    &peer.writable,
                    || peer.is_refused() || peer.buffer.lock().can_write(data.len()),
                    deadline,
                )?,
            }
        }
    }

    /// Receive into `out`: for a stream, what is queued up to its length --
    /// and with `MSG_WAITALL` until it is full or the stream ends -- and for a
    /// record, one record, truncated to fit.
    ///
    /// # Errors
    ///
    /// `EINVAL` on a stream socket and `ENOTCONN` on a sequenced-packet socket
    /// that was never connected, as Linux answers each; `EAGAIN`
    /// when it would wait and may not, or its timeout ran out; a restart code
    /// or `EINTR` for a signal. A stream that received part first reports it.
    pub(crate) fn recv(
        &self,
        out: &mut [u8],
        flags: u32,
        nonblock: bool,
    ) -> Result<Received, Errno> {
        if self.kind != SocketType::Datagram && self.send.lock().is_none() {
            // `unix_stream_read_generic` answers EINVAL, `unix_seqpacket_recvmsg`
            // ENOTCONN.
            return Err(if self.kind == SocketType::Stream {
                Errno::EINVAL
            } else {
                Errno::ENOTCONN
            });
        }
        if out.is_empty() && self.kind == SocketType::Stream {
            return Ok(Received { bytes: 0, full: 0 });
        }
        let nonblock = nonblock || flags & MSG_DONTWAIT != 0;
        let peek = flags & MSG_PEEK != 0;
        let wait_all = flags & MSG_WAITALL != 0 && self.kind == SocketType::Stream && !peek;
        let deadline = deadline(self.options.lock().receive_timeout);
        let mut done = 0;
        loop {
            let rest = out.get_mut(done..).unwrap_or_default();
            // Bound first, so the guard is gone before anything below waits.
            let outcome = self.receive.buffer.lock().read(rest, peek);
            let refusal = match outcome {
                ReadOutcome::Read { bytes, full, .. } => {
                    if !peek {
                        self.receive.writable.wake_all();
                    }
                    if self.kind != SocketType::Stream {
                        return Ok(Received { bytes, full });
                    }
                    done += bytes;
                    if wait_all && done < out.len() {
                        continue;
                    }
                    return Ok(Received {
                        bytes: done,
                        full: done,
                    });
                }
                ReadOutcome::EndOfFile => {
                    return Ok(Received {
                        bytes: done,
                        full: done,
                    });
                }
                ReadOutcome::WouldBlock if nonblock => Errno::EAGAIN,
                ReadOutcome::WouldBlock => match wait(
                    &self.receive.readable,
                    || self.receive.buffer.lock().can_read(),
                    deadline,
                ) {
                    Ok(()) => continue,
                    Err(errno) => errno,
                },
            };
            return if done > 0 {
                Ok(Received {
                    bytes: done,
                    full: done,
                })
            } else {
                Err(refusal)
            };
        }
    }

    /// `shutdown`: stop receiving, sending, or both.
    ///
    /// Stopping receiving lets this socket read what is queued and then end of
    /// file, and refuses what its peer would still send; stopping sending
    /// refuses this socket's own writes and, for a stream or sequenced-packet
    /// socket, gives its peer end of file once drained -- `unix_shutdown`'s
    /// two halves.
    ///
    /// # Errors
    ///
    /// `EINVAL` for a `how` that is none of the three.
    pub(crate) fn shutdown(&self, how: u32) -> Result<(), Errno> {
        let (read, write) = match how {
            SHUT_RD => (true, false),
            SHUT_WR => (false, true),
            SHUT_RDWR => (true, true),
            _ => return Err(Errno::EINVAL),
        };
        if read {
            self.receive.buffer.lock().close_writer();
            self.receive.refused.store(true, Ordering::Release);
            self.receive.wake_both();
        }
        if write {
            self.options.lock().shut_write = true;
            let peer = self.send.lock().clone();
            if let Some(peer) = peer {
                if self.kind != SocketType::Datagram {
                    peer.buffer.lock().close_writer();
                }
                peer.wake_both();
            }
        }
        Ok(())
    }

    /// What `poll` reports: readable with data or end of file waiting,
    /// writable with room or a write that would fail at once, and hung up
    /// once nothing can come in and nothing can go out.
    fn readiness(&self) -> Readiness {
        let (readable, ended) = {
            let buffer = self.receive.buffer.lock();
            (buffer.can_read(), buffer.writer_closed())
        };
        let shut_write = self.options.lock().shut_write;
        let peer = self.send.lock().clone();
        let (room, peer_refuses) = match &peer {
            Some(peer) => {
                let refused = peer.is_refused();
                (refused || peer.buffer.lock().can_write(1), refused)
            }
            None => (true, false),
        };
        Readiness {
            readable,
            writable: room || shut_write,
            hangup: (peer.is_none() && self.kind != SocketType::Datagram)
                || (ended && (shut_write || peer_refuses)),
            error: false,
        }
    }

    /// The socket requests of `ioctl`: `SIOCINQ` (`FIONREAD`), what a read
    /// would find -- the whole queue for a stream or sequenced-packet socket,
    /// the next datagram for a datagram socket -- and `SIOCOUTQ`, what this
    /// socket sent that its peer has not read.
    ///
    /// # Errors
    ///
    /// `ENOTTY` for any other request; `EFAULT` for an unwritable `arg`.
    pub(crate) fn ioctl(&self, process: &Process, request: u32, arg: u64) -> Result<usize, Errno> {
        let count = match request {
            SIOCINQ => {
                let buffer = self.receive.buffer.lock();
                if self.kind == SocketType::Datagram {
                    buffer.next_record().unwrap_or(0)
                } else {
                    buffer.queued()
                }
            }
            SIOCOUTQ => {
                let peer = self.send.lock().clone();
                peer.map_or(0, |peer| peer.buffer.lock().queued())
            }
            _ => return Err(Errno::ENOTTY),
        };
        uaccess::put_u32(
            process.space(),
            arg,
            u32::try_from(count).unwrap_or(u32::MAX),
        )?;
        Ok(0)
    }

    /// The value `getsockopt(SOL_SOCKET, name)` answers, as the bytes a
    /// program reads, with time structures of `width`.
    ///
    /// The options a socket without a network has no use for -- keep-alive,
    /// broadcast, routing, priority, lingering -- read as their defaults.
    ///
    /// # Errors
    ///
    /// `EOPNOTSUPP` for a level other than `SOL_SOCKET`, and `ENOPROTOOPT` for
    /// an option this socket does not have.
    pub(crate) fn get_option(&self, level: i32, name: i32, width: Width) -> Result<Vec<u8>, Errno> {
        if level != SOL_SOCKET {
            return Err(Errno::EOPNOTSUPP);
        }
        let options = *self.options.lock();
        let int = |value: i32| value.to_le_bytes().to_vec();
        let size = |bytes: usize| int(i32::try_from(bytes).unwrap_or(i32::MAX));
        Ok(match name {
            SO_TYPE => int(i32::try_from(self.kind.linux()).unwrap_or(0)),
            SO_DOMAIN => int(i32::from(AF_UNIX)),
            SO_PROTOCOL | SO_ERROR | SO_ACCEPTCONN | SO_DEBUG | SO_REUSEADDR | SO_KEEPALIVE
            | SO_BROADCAST | SO_DONTROUTE | SO_OOBINLINE | SO_PRIORITY => int(0),
            SO_RCVLOWAT | SO_SNDLOWAT => int(1),
            SO_SNDBUF => size(options.send_buffer),
            SO_RCVBUF => size(self.receive.buffer.lock().capacity()),
            SO_PASSCRED => int(i32::from(options.pass_credentials)),
            SO_PEERCRED => self
                .peer_credentials
                .unwrap_or(Ucred {
                    pid: 0,
                    uid: OVERFLOW_ID,
                    gid: OVERFLOW_ID,
                })
                .to_bytes()
                .to_vec(),
            SO_LINGER => Linger {
                onoff: 0,
                linger: 0,
            }
            .to_bytes()
            .to_vec(),
            SO_RCVTIMEO_OLD => timeval(options.receive_timeout, width),
            SO_SNDTIMEO_OLD => timeval(options.send_timeout, width),
            SO_RCVTIMEO_NEW => timeval(options.receive_timeout, Width::Bits64),
            SO_SNDTIMEO_NEW => timeval(options.send_timeout, Width::Bits64),
            _ => return Err(Errno::ENOPROTOOPT),
        })
    }

    /// `setsockopt(SOL_SOCKET, name, value)`, with time structures of `width`.
    ///
    /// The options with no effect here are accepted and ignored, as a socket
    /// family that does not use them accepts them on Linux.
    ///
    /// # Errors
    ///
    /// `EOPNOTSUPP` for another level, `ENOPROTOOPT` for an unknown option,
    /// `EINVAL` for a value shorter than an `int` or than its option; `EDOM` for a timeout's microseconds out of range.
    pub(crate) fn set_option(
        &self,
        level: i32,
        name: i32,
        value: &[u8],
        width: Width,
    ) -> Result<(), Errno> {
        if level != SOL_SOCKET {
            return Err(Errno::EOPNOTSUPP);
        }
        // `sk_setsockopt` refuses a value shorter than an `int` before it looks
        // at the option.
        if value.len() < size_of::<i32>() {
            return Err(Errno::EINVAL);
        }
        let int = || {
            value
                .get(..4)
                .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                .map(i32::from_le_bytes)
                .ok_or(Errno::EINVAL)
        };
        match name {
            SO_SNDBUF | SO_SNDBUFFORCE => self.options.lock().send_buffer = buffer_size(int()?),
            SO_RCVBUF | SO_RCVBUFFORCE => {
                let bytes = buffer_size(int()?);
                self.receive.buffer.lock().set_capacity(bytes);
                self.receive.writable.wake_all();
            }
            SO_PASSCRED => self.options.lock().pass_credentials = int()? != 0,
            SO_RCVTIMEO_OLD => self.options.lock().receive_timeout = read_timeval(value, width)?,
            SO_SNDTIMEO_OLD => self.options.lock().send_timeout = read_timeval(value, width)?,
            SO_RCVTIMEO_NEW => {
                self.options.lock().receive_timeout = read_timeval(value, Width::Bits64)?;
            }
            SO_SNDTIMEO_NEW => {
                self.options.lock().send_timeout = read_timeval(value, Width::Bits64)?;
            }
            SO_DEBUG | SO_REUSEADDR | SO_KEEPALIVE | SO_BROADCAST | SO_DONTROUTE | SO_OOBINLINE
            | SO_PRIORITY | SO_RCVLOWAT | SO_LINGER => {
                let _ = int()?;
            }
            _ => return Err(Errno::ENOPROTOOPT),
        }
        Ok(())
    }
}

/// A buffer size as `SO_SNDBUF` and `SO_RCVBUF` store it: capped at the
/// maximum, doubled for bookkeeping, and no smaller than the minimum --
/// `__sock_set_rcvbuf`'s arithmetic, so a program reads back twice what it set.
fn buffer_size(requested: i32) -> usize {
    // A negative request is a huge unsigned one to Linux, capped like any other.
    let requested = usize::try_from(requested.cast_unsigned()).unwrap_or(SOCKET_BUFFER_MAX);
    requested
        .min(SOCKET_BUFFER_MAX)
        .saturating_mul(2)
        .max(SOCKET_BUFFER_MIN)
}

/// A timeout in nanoseconds as the `timeval` of `width` a program reads.
fn timeval(nanos: u64, width: Width) -> Vec<u8> {
    let mut bytes = vec![0_u8; width.bytes() * 2];
    let _ = width.put_word(&mut bytes, 0, nanos / NANOS_PER_SECOND);
    let _ = width.put_word(
        &mut bytes,
        width.bytes(),
        (nanos % NANOS_PER_SECOND) / NANOS_PER_MICRO,
    );
    bytes
}

/// A `timeval` of `width` as a timeout in nanoseconds: `sock_set_timeout`'s
/// rules. All zero waits forever; negative seconds give up at once.
fn read_timeval(value: &[u8], width: Width) -> Result<u64, Errno> {
    let seconds = width.word(value, 0).ok_or(Errno::EINVAL)?;
    let micros = width.word(value, width.bytes()).ok_or(Errno::EINVAL)?;
    // Signed fields, of the width's size.
    let (seconds, micros) = match width {
        Width::Bits64 => (seconds.cast_signed(), micros.cast_signed()),
        Width::Bits32 => (
            i64::from((seconds as u32).cast_signed()),
            i64::from((micros as u32).cast_signed()),
        ),
    };
    let micros = u64::try_from(micros)
        .ok()
        .filter(|&micros| micros < NANOS_PER_SECOND / NANOS_PER_MICRO)
        .ok_or(Errno::EDOM)?;
    let Ok(seconds) = u64::try_from(seconds) else {
        // The shortest wait there is: a timeout that has already passed.
        return Ok(1);
    };
    Ok(seconds
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(micros * NANOS_PER_MICRO))
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Nobody reads this direction again: what is queued goes, and a writer
        // sees a broken socket. Taken out under the lock, dropped after it.
        let dropped = self.receive.buffer.lock().close_reader();
        self.receive.refused.store(true, Ordering::Release);
        self.receive.wake_both();
        drop(dropped);
        // Nothing more is sent: the peer reads end of file once drained, unless
        // it is a datagram socket, whose reads end only with its own shutdown.
        let peer = self.send.lock().take();
        if let Some(peer) = peer {
            if self.kind != SocketType::Datagram {
                peer.buffer.lock().close_writer();
            }
            peer.wake_both();
        }
    }
}

impl Inode for Socket {
    fn metadata(&self) -> Metadata {
        self.metadata
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self) -> Readiness {
        self.readiness()
    }

    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        self.recv(buf, 0, nonblock).map(|received| received.bytes)
    }

    fn write_stream(&self, data: &[u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        self.send(data, 0, nonblock)
    }
}

// ---------------------------------------------------------------------------
// sockfs
// ---------------------------------------------------------------------------

/// The filesystem every socket is on: Linux's `sockfs`, which a program only
/// ever meets as `fstat`'s device and `fstatfs`'s magic number.
#[derive(Debug)]
struct SockFs {
    device: u64,
    root: Arc<dyn Inode>,
    /// The next socket's inode number; each socket has its own, as on Linux.
    next_ino: AtomicU64,
}

/// The one sockfs.
static SOCKFS: Once<Arc<SockFs>> = Once::new();

/// The one sockfs, made on first use.
fn sockfs() -> &'static Arc<SockFs> {
    SOCKFS.call_once(|| {
        Arc::new(SockFs {
            device: fs::anonymous_device(),
            root: Arc::new(SockRoot),
            next_ino: AtomicU64::new(2),
        })
    })
}

impl FileSystem for SockFs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::clone(&self.root)
    }

    fn name(&self) -> &'static str {
        "sockfs"
    }

    fn device(&self) -> u64 {
        self.device
    }

    fn statfs(&self) -> StatFs {
        StatFs {
            magic: SOCKFS_MAGIC,
            block_size: u64::from(BLOCK_SIZE),
            name_max: NAME_MAX as u64,
            ..StatFs::default()
        }
    }
}

/// sockfs's root directory, which is empty and which nothing can reach: every
/// socket's location is detached. It exists because a filesystem has a root.
#[derive(Debug)]
struct SockRoot;

impl Inode for SockRoot {
    fn metadata(&self) -> Metadata {
        Metadata {
            ino: 1,
            kind: FileType::Directory,
            permissions: 0o700,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: BLOCK_SIZE,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// An open file on `socket`, at a location of its own on sockfs, named as
/// `/proc/self/fd` shows a socket.
fn open(socket: Arc<Socket>, nonblock: bool) -> Result<Arc<OpenFile>, Errno> {
    let name = format!("socket:[{}]", socket.metadata.ino);
    let flags = OpenFlags {
        read: true,
        write: true,
        nonblock,
        ..OpenFlags::default()
    };
    let sockfs: Arc<SockFs> = Arc::clone(sockfs());
    // Where the description's sleeping lock waits; a stream never takes it.
    let parker = Arc::clone(fs::namespace().parker());
    OpenFile::new(
        Location::detached(sockfs, socket, name.as_bytes(), parker),
        &flags,
    )
}

/// A socket of `kind` connected to nothing, as the open file `socket`
/// installs, owned by `owner`.
///
/// # Errors
///
/// Whatever [`OpenFile::new`] refuses, which for a socket is nothing.
pub(crate) fn new_socket(
    kind: SocketType,
    nonblock: bool,
    owner: (u32, u32),
) -> Result<Arc<OpenFile>, Errno> {
    open(
        Socket::new(kind, None, Channel::new(kind), None, owner),
        nonblock,
    )
}

/// Two sockets of `kind` connected to each other, as the open files
/// `socketpair` installs; each reports `creator`'s credentials as its peer's.
///
/// # Errors
///
/// Whatever [`OpenFile::new`] refuses, which for a socket is nothing.
pub(crate) fn new_pair(
    kind: SocketType,
    nonblock: bool,
    creator: &Process,
) -> Result<(Arc<OpenFile>, Arc<OpenFile>), Errno> {
    let first = Channel::new(kind);
    let second = Channel::new(kind);
    let credentials = Some(credentials_of(creator));
    let owner = crate::syscall::path::creator_ids(creator);
    let one = Socket::new(
        kind,
        Some(Arc::clone(&second)),
        Arc::clone(&first),
        credentials,
        owner,
    );
    let other = Socket::new(kind, Some(first), second, credentials, owner);
    Ok((open(one, nonblock)?, open(other, nonblock)?))
}

/// The socket an open file reads and writes through, if it is one.
pub(crate) fn of(file: &OpenFile) -> Option<Arc<Socket>> {
    Arc::clone(file.io()).into_any().downcast::<Socket>().ok()
}
