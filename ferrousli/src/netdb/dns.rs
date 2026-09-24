//! DNS packets: `res_mkquery`, `dn_expand`, `dn_skipname`, the `ns_` parser
//! of `arpa/nameser.h`, and the answer walker the lookups use.
//!
//! Ported from musl 1.2.5's `res_mkquery.c`, `dn_expand.c`, `dn_skipname.c`,
//! `ns_parse.c` and `dns_parse.c` (MIT). The parsers work on byte slices and
//! offsets rather than pointers, so a length or a compression pointer that
//! leads out of the packet is refused where musl's pointer arithmetic would
//! also refuse it, and never read. musl's checks already bound every read and
//! every loop, and the results are its results; the C functions differ only
//! in refusing a `src` outside the packet, which musl would read.

use core::ffi::{c_char, c_int, c_uint, c_ulong};
use core::mem::{offset_of, size_of};
use core::ptr::null;

use super::{at, c_bytes_max};
use crate::errno;
use crate::time::{CLOCK_REALTIME, Timespec, clock_gettime};

/// The `A` record type.
pub const RR_A: c_int = 1;
/// The `CNAME` record type.
pub const RR_CNAME: c_int = 5;
/// The `PTR` record type.
pub const RR_PTR: c_int = 12;
/// The `AAAA` record type.
pub const RR_AAAA: c_int = 28;

/// The size of musl's query buffers: the longest query and some room.
pub const QUERY_MAX: usize = 280;

/// `NS_MAXDNAME`, from `include/arpa/nameser.h`.
pub const NS_MAXDNAME: usize = 1025;
/// `ns_s_max`, from `include/arpa/nameser.h`: how many sections a message has.
const NS_S_MAX: usize = 4;
/// `ns_s_qd`: the question section, whose entries have no data.
const NS_S_QD: c_int = 0;
/// The longest name `dn_expand` writes, its NUL included.
const EXPANDED_MAX: usize = 254;

/// Stores `value` at `index`, if it is inside `buf`.
fn set(buf: &mut [u8], index: usize, value: u8) {
    if let Some(slot) = buf.get_mut(index) {
        *slot = value;
    }
}

/// The big-endian 16-bit value at `index`, with NUL past the end.
fn get16(packet: &[u8], index: usize) -> usize {
    usize::from(at(packet, index)) << 8 | usize::from(at(packet, index + 1))
}

/// Builds a query for `dname` of `class` and `type`, as musl's
/// `__res_mkquery` does, with the id `id`. Returns the packet and its length,
/// or `None` for a name, class, type or opcode musl refuses: a name of more
/// than 253 bytes, an empty label or one of more than 63 bytes, two trailing
/// dots, and a class or type above 255.
///
/// `dname` is the name without its NUL, and at most 255 bytes of it are
/// looked at.
pub fn mkquery(
    op: c_int,
    dname: &[u8],
    class: c_int,
    kind: c_int,
    id: u16,
) -> Option<([u8; QUERY_MAX], usize)> {
    let mut l = dname.len().min(255);
    if l > 0 && at(dname, l - 1) == b'.' {
        l -= 1;
    }
    if l > 0 && at(dname, l - 1) == b'.' {
        return None;
    }
    let n = 17 + l + usize::from(l != 0);
    if l > 253
        || op.cast_unsigned() > 15
        || class.cast_unsigned() > 255
        || kind.cast_unsigned() > 255
    {
        return None;
    }

    let mut q = [0u8; QUERY_MAX];
    set(&mut q, 2, (op as u8) * 8 + 1);
    // The AD bit.
    set(&mut q, 3, 32);
    set(&mut q, 5, 1);
    for (index, &byte) in dname.iter().take(l).enumerate() {
        set(&mut q, 13 + index, byte);
    }
    let mut i = 13;
    while at(&q, i) != 0 {
        let mut j = i;
        while at(&q, j) != 0 && at(&q, j) != b'.' {
            j += 1;
        }
        // An empty label wraps to a huge length, as in musl.
        if (j - i).wrapping_sub(1) > 62 {
            return None;
        }
        set(&mut q, i - 1, (j - i) as u8);
        i = j + 1;
    }
    set(&mut q, i + 1, kind as u8);
    set(&mut q, i + 3, class as u8);
    let [high, low] = id.to_be_bytes();
    set(&mut q, 0, high);
    set(&mut q, 1, low);
    Some((q, n))
}

/// A reasonably unpredictable query id, from the clock as musl makes it.
pub fn query_id() -> u16 {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live local.
    let _ = unsafe { clock_gettime(CLOCK_REALTIME, &raw mut ts) };
    let nsec = ts.tv_nsec.unsigned_abs();
    (nsec.wrapping_add(nsec / 65536) & 0xffff) as u16
}

/// Builds a query for `dname` into the `buflen` bytes at `buf`, and returns
/// its length, or -1 if the query cannot be built or does not fit. `data` and
/// `newrr` are unused, as in musl.
///
/// # Safety
///
/// `dname` must be a NUL-terminated string and `buf` valid for writes of
/// `buflen` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn res_mkquery(
    op: c_int,
    dname: *const c_char,
    class: c_int,
    kind: c_int,
    data: *const u8,
    datalen: c_int,
    newrr: *const u8,
    buf: *mut u8,
    buflen: c_int,
) -> c_int {
    let _ = (data, datalen, newrr);
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { c_bytes_max(dname, 255) };
    let Some((q, n)) = mkquery(op, name, class, kind, query_id()) else {
        return -1;
    };
    if usize::try_from(buflen).is_ok_and(|room| room >= n) {
        for (index, &byte) in q.iter().take(n).enumerate() {
            // SAFETY: `n` is at most `buflen`, which the caller vouches for.
            unsafe { buf.wrapping_add(index).write(byte) };
        }
        n as c_int
    } else {
        -1
    }
}

/// Expands the compressed name at `src` in `packet` into `dest`, as musl's
/// `__dn_expand` does, and returns how many bytes of the packet the name took
/// where it began. `dest` is the room there is, which musl caps at 254 bytes.
///
/// `None` for a name that runs out of the packet or out of `dest`, a pointer
/// outside the packet, or more pointers than the packet has pairs of bytes,
/// which is how a loop of pointers ends.
pub fn expand(packet: &[u8], src: usize, dest: &mut [u8]) -> Option<usize> {
    let end = packet.len();
    if src >= end || dest.is_empty() {
        return None;
    }
    let dend = dest.len().min(EXPANDED_MAX);
    let mut p = src;
    let mut d = 0;
    let mut len = None;
    let mut i = 0;
    while i < end {
        let byte = *packet.get(p)?;
        if byte & 0xc0 != 0 {
            let low = *packet.get(p + 1)?;
            let j = usize::from(byte & 0x3f) << 8 | usize::from(low);
            if len.is_none() {
                len = Some(p + 2 - src);
            }
            if j >= end {
                return None;
            }
            p = j;
        } else if byte != 0 {
            if d != 0 {
                *dest.get_mut(d)? = b'.';
                d += 1;
            }
            let j = usize::from(byte);
            p += 1;
            if j >= end - p || j >= dend.checked_sub(d)? {
                return None;
            }
            let source = packet.get(p..p + j)?;
            let target = dest.get_mut(d..d + j)?;
            for (slot, &label_byte) in target.iter_mut().zip(source) {
                *slot = label_byte;
            }
            p += j;
            d += j;
        } else {
            *dest.get_mut(d)? = 0;
            return Some(len.unwrap_or_else(|| p + 1 - src));
        }
        i += 2;
    }
    None
}

/// The packet from `base` to `end`, and `src`'s offset in it, for the C
/// functions. `None` if `src` is not inside it.
///
/// # Safety
///
/// `base..end` must be readable.
unsafe fn packet<'a>(base: *const u8, end: *const u8, src: *const u8) -> Option<(&'a [u8], usize)> {
    let len = end.addr().checked_sub(base.addr())?;
    let from = src.addr().checked_sub(base.addr())?;
    if from >= len {
        return None;
    }
    // SAFETY: the caller vouches for `len` readable bytes at `base`.
    Some((unsafe { core::slice::from_raw_parts(base, len) }, from))
}

/// Expands the compressed name at `src`, in the message from `base` to `end`,
/// into the `space` bytes at `dest`, and returns how many bytes the name took
/// at `src`, or -1 for a malformed name or one that does not fit.
///
/// # Safety
///
/// `base..end` must be readable, and `dest` valid for writes of `space` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dn_expand(
    base: *const u8,
    end: *const u8,
    src: *const u8,
    dest: *mut c_char,
    space: c_int,
) -> c_int {
    let Ok(space) = usize::try_from(space) else {
        return -1;
    };
    // SAFETY: the caller vouches for the message.
    let Some((packet, from)) = (unsafe { packet(base, end, src) }) else {
        return -1;
    };
    if space == 0 {
        return -1;
    }
    let room = space.min(EXPANDED_MAX);
    // SAFETY: the caller passes `space` writable bytes, and `room` is no more.
    let dest = unsafe { core::slice::from_raw_parts_mut(dest.cast::<u8>(), room) };
    expand(packet, from, dest)
        .and_then(|len| c_int::try_from(len).ok())
        .unwrap_or(-1)
}

/// How many bytes the compressed name at `start` in `packet` takes, as musl's
/// `dn_skipname` counts them, or `None` if it runs out of the packet.
pub fn skipname(packet: &[u8], start: usize) -> Option<usize> {
    let end = packet.len();
    let mut p = start;
    while p < end {
        let byte = usize::from(at(packet, p));
        if byte == 0 {
            return Some(p - start + 1);
        } else if byte >= 192 {
            return (p + 1 < end).then_some(p - start + 2);
        } else if end - p < byte + 1 {
            return None;
        }
        p += byte + 1;
    }
    None
}

/// How many bytes the compressed name at `s` takes, before `end`, or -1.
///
/// # Safety
///
/// `s..end` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn dn_skipname(s: *const u8, end: *const u8) -> c_int {
    // SAFETY: the caller vouches for the bytes.
    let Some((packet, _)) = (unsafe { packet(s, end, s) }) else {
        return -1;
    };
    skipname(packet, 0)
        .and_then(|len| c_int::try_from(len).ok())
        .unwrap_or(-1)
}

/// Walks the answers of the response `r`, as musl's `__dns_parse` does,
/// calling `callback` with each record's type, the offset of its data and the
/// data's length. Returns 0, or -1 for a malformed packet or when `callback`
/// returns a negative value. A response with a nonzero code has no answers to
/// walk, and gives 0.
///
/// Like musl's, it skips names by skipping bytes from 1 to 127, and reads a
/// record's type from its low byte.
pub fn parse(r: &[u8], mut callback: impl FnMut(c_int, usize, usize) -> c_int) -> c_int {
    let rlen = r.len();
    if rlen < 12 {
        return -1;
    }
    if at(r, 3) & 15 != 0 {
        return 0;
    }
    let mut p = 12;
    let mut qdcount = get16(r, 4);
    let mut ancount = get16(r, 6);
    let label = |byte: u8| (1..=127).contains(&byte);
    while qdcount > 0 {
        qdcount -= 1;
        while p < rlen && label(at(r, p)) {
            p += 1;
        }
        if p > rlen - 6 {
            return -1;
        }
        p += 5 + usize::from(at(r, p) != 0);
    }
    while ancount > 0 {
        ancount -= 1;
        while p < rlen && label(at(r, p)) {
            p += 1;
        }
        if p > rlen - 12 {
            return -1;
        }
        p += 1 + usize::from(at(r, p) != 0);
        let len = get16(r, p + 8);
        if len + 10 > rlen - p {
            return -1;
        }
        if callback(c_int::from(at(r, p + 1)), p + 10, len) < 0 {
            return -1;
        }
        p += 10 + len;
    }
    0
}

/// C's `ns_msg`, from `include/arpa/nameser.h`: a parsed message. glibc's is
/// the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NsMsg {
    /// The message's first byte.
    pub _msg: *const u8,
    /// The byte after its last.
    pub _eom: *const u8,
    /// The id.
    pub _id: u16,
    /// The flags.
    pub _flags: u16,
    /// How many records each section has.
    pub _counts: [u16; NS_S_MAX],
    /// Where each section starts, or null for an empty one.
    pub _sections: [*const u8; NS_S_MAX],
    /// The section `ns_parserr` is in.
    pub _sect: c_int,
    /// The record `ns_parserr` reads next.
    pub _rrnum: c_int,
    /// Where that record starts.
    pub _msg_ptr: *const u8,
}

const _: () = assert!(size_of::<NsMsg>() == 80);
const _: () = assert!(offset_of!(NsMsg, _id) == 16);
const _: () = assert!(offset_of!(NsMsg, _counts) == 20);
const _: () = assert!(offset_of!(NsMsg, _sections) == 32);
const _: () = assert!(offset_of!(NsMsg, _sect) == 64);
const _: () = assert!(offset_of!(NsMsg, _rrnum) == 68);
const _: () = assert!(offset_of!(NsMsg, _msg_ptr) == 72);

/// C's `ns_rr`, from `include/arpa/nameser.h`: one parsed record. glibc's is
/// the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NsRr {
    /// The owner name.
    pub name: [c_char; NS_MAXDNAME],
    /// The type.
    pub r#type: u16,
    /// The class.
    pub rr_class: u16,
    /// The time to live.
    pub ttl: u32,
    /// The data's length.
    pub rdlength: u16,
    /// The data, inside the message, or null for a question.
    pub rdata: *const u8,
}

const _: () = assert!(size_of::<NsRr>() == 1048);
const _: () = assert!(offset_of!(NsRr, r#type) == 1026);
const _: () = assert!(offset_of!(NsRr, rr_class) == 1028);
const _: () = assert!(offset_of!(NsRr, ttl) == 1032);
const _: () = assert!(offset_of!(NsRr, rdlength) == 1036);
const _: () = assert!(offset_of!(NsRr, rdata) == 1040);

/// C's `struct _ns_flagdata`, from `include/arpa/nameser.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NsFlagdata {
    /// The flag's bits in the header's flags.
    pub mask: c_int,
    /// How far they are shifted.
    pub shift: c_int,
}

/// Where each `ns_flag` lies in a message's flags, for the header's
/// `ns_msg_getflag`.
#[allow(non_upper_case_globals, reason = "the C name")]
#[cfg_attr(not(test), unsafe(no_mangle))]
pub static _ns_flagdata: [NsFlagdata; 16] = {
    const fn flag(mask: c_int, shift: c_int) -> NsFlagdata {
        NsFlagdata { mask, shift }
    }
    [
        flag(0x8000, 15),
        flag(0x7800, 11),
        flag(0x0400, 10),
        flag(0x0200, 9),
        flag(0x0100, 8),
        flag(0x0080, 7),
        flag(0x0040, 6),
        flag(0x0020, 5),
        flag(0x0010, 4),
        flag(0x000f, 0),
        flag(0, 0),
        flag(0, 0),
        flag(0, 0),
        flag(0, 0),
        flag(0, 0),
        flag(0, 0),
    ]
};

/// The big-endian 16-bit value at `cp`.
///
/// # Safety
///
/// `cp` must be valid for reads of 2 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_get16(cp: *const u8) -> c_uint {
    // SAFETY: the caller passes 2 readable bytes.
    let bytes = unsafe { cp.cast::<[u8; 2]>().read() };
    c_uint::from(u16::from_be_bytes(bytes))
}

/// The big-endian 32-bit value at `cp`.
///
/// # Safety
///
/// `cp` must be valid for reads of 4 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_get32(cp: *const u8) -> c_ulong {
    // SAFETY: the caller passes 4 readable bytes.
    let bytes = unsafe { cp.cast::<[u8; 4]>().read() };
    c_ulong::from(u32::from_be_bytes(bytes))
}

/// Stores the low 16 bits of `s` at `cp`, big-endian.
///
/// # Safety
///
/// `cp` must be valid for writes of 2 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_put16(s: c_uint, cp: *mut u8) {
    // SAFETY: the caller passes 2 writable bytes.
    unsafe { cp.cast::<[u8; 2]>().write((s as u16).to_be_bytes()) };
}

/// Stores the low 32 bits of `l` at `cp`, big-endian.
///
/// # Safety
///
/// `cp` must be valid for writes of 4 bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_put32(l: c_ulong, cp: *mut u8) {
    // SAFETY: the caller passes 4 writable bytes.
    unsafe { cp.cast::<[u8; 4]>().write((l as u32).to_be_bytes()) };
}

/// How many bytes `count` records of `section` take from `start` in `packet`,
/// as musl's `ns_skiprr` counts them, or `None` if they run out of it.
pub fn skiprr(packet: &[u8], start: usize, section: c_int, count: c_int) -> Option<usize> {
    let eom = packet.len();
    let mut p = start;
    let mut left = count;
    while left != 0 {
        left = left.wrapping_sub(1);
        let r = skipname(packet, p)?;
        if r + 4 > eom - p {
            return None;
        }
        p += r + 4;
        if section != NS_S_QD {
            if 6 > eom - p {
                return None;
            }
            p += 4;
            let r = get16(packet, p);
            p += 2;
            if r > eom - p {
                return None;
            }
            p += r;
        }
    }
    Some(p - start)
}

/// How many bytes `count` records of `section` take at `ptr`, before `eom`,
/// or -1 with `EMSGSIZE`.
///
/// # Safety
///
/// `ptr..eom` must be readable.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_skiprr(
    ptr: *const u8,
    eom: *const u8,
    section: c_int,
    count: c_int,
) -> c_int {
    let skipped = eom.addr().checked_sub(ptr.addr()).and_then(|len| {
        // SAFETY: the caller vouches for `len` readable bytes.
        let packet = unsafe { core::slice::from_raw_parts(ptr, len) };
        skiprr(packet, 0, section, count)
    });
    match skipped.and_then(|len| c_int::try_from(len).ok()) {
        Some(len) => len,
        None => {
            errno::set(errno::EMSGSIZE);
            -1
        }
    }
}

/// Parses the header of the `msglen`-byte message at `msg` into `*handle`,
/// finding where each section starts. Returns 0, or -1 with `EMSGSIZE` if the
/// sections do not exactly fill the message.
///
/// # Safety
///
/// `msg` must be valid for reads of `msglen` bytes, and `handle` for writes of
/// an `ns_msg`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_initparse(msg: *const u8, msglen: c_int, handle: *mut NsMsg) -> c_int {
    let message = handle
        .wrapping_byte_add(offset_of!(NsMsg, _msg))
        .cast::<*const u8>();
    // SAFETY: the caller passes a writable handle.
    unsafe { message.write(msg) };
    let end = handle
        .wrapping_byte_add(offset_of!(NsMsg, _eom))
        .cast::<*const u8>();
    // SAFETY: as above.
    unsafe { end.write(msg.wrapping_offset(msglen as isize)) };
    let Ok(len) = usize::try_from(msglen) else {
        errno::set(errno::EMSGSIZE);
        return -1;
    };
    // SAFETY: the caller vouches for `msglen` readable bytes.
    let packet = unsafe { core::slice::from_raw_parts(msg, len) };
    match initparse(packet) {
        Some(parsed) => {
            let sections = parsed
                .sections
                .map(|section| section.map_or(null(), |offset| msg.wrapping_add(offset)));
            // SAFETY: as above.
            unsafe {
                handle.write(NsMsg {
                    _msg: msg,
                    _eom: msg.wrapping_add(len),
                    _id: parsed.id,
                    _flags: parsed.flags,
                    _counts: parsed.counts,
                    _sections: sections,
                    _sect: NS_S_MAX as c_int,
                    _rrnum: -1,
                    _msg_ptr: null(),
                });
            }
            0
        }
        None => {
            errno::set(errno::EMSGSIZE);
            -1
        }
    }
}

/// A message's header, and where its sections start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    /// The id.
    pub id: u16,
    /// The flags.
    pub flags: u16,
    /// How many records each section has.
    pub counts: [u16; NS_S_MAX],
    /// Where each non-empty section starts.
    pub sections: [Option<usize>; NS_S_MAX],
}

/// Parses `packet`'s header as `ns_initparse` does. `None` if it is shorter
/// than a header, or its sections do not exactly fill it.
pub fn initparse(packet: &[u8]) -> Option<Parsed> {
    if packet.len() < (2 + NS_S_MAX) * 2 {
        return None;
    }
    let mut parsed = Parsed {
        id: get16(packet, 0) as u16,
        flags: get16(packet, 2) as u16,
        counts: [0; NS_S_MAX],
        sections: [None; NS_S_MAX],
    };
    let mut offset = 12;
    for index in 0..NS_S_MAX {
        let count = get16(packet, 4 + 2 * index) as u16;
        *parsed.counts.get_mut(index)? = count;
        if count != 0 {
            *parsed.sections.get_mut(index)? = Some(offset);
            offset += skiprr(packet, offset, index as c_int, c_int::from(count))?;
        }
    }
    (offset == packet.len()).then_some(parsed)
}

/// A parsed record, as `ns_parserr` fills an `ns_rr` with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// How many bytes of the name `expand` wrote, its NUL included.
    pub name_len: usize,
    /// The type.
    pub kind: u16,
    /// The class.
    pub class: u16,
    /// The time to live.
    pub ttl: u32,
    /// The data's length.
    pub rdlength: u16,
    /// The data's offset, or `None` for a question.
    pub rdata: Option<usize>,
}

/// The state `ns_parserr` keeps in an `ns_msg`, as offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// The section it is in.
    pub sect: c_int,
    /// The record it reads next.
    pub rrnum: c_int,
    /// Where that record starts, or `None`.
    pub ptr: Option<usize>,
}

/// Reads record `rrnum` of `section`, or the next one if `rrnum` is -1, from
/// `parsed`'s `packet`, as musl's `ns_parserr` does, moving `cursor` past it
/// and expanding its name into `name`. `Err` holds the error number: `ENODEV`
/// for a section or record that does not exist, `EMSGSIZE` for one that runs
/// out of the packet.
pub fn parserr(
    packet: &[u8],
    parsed: &Parsed,
    cursor: &mut Cursor,
    section: c_int,
    rrnum: c_int,
    name: &mut [u8],
) -> Result<Record, c_int> {
    let Some(index) = usize::try_from(section)
        .ok()
        .filter(|&index| index < NS_S_MAX)
    else {
        return Err(errno::ENODEV);
    };
    let start = parsed.sections.get(index).copied().flatten();
    if section != cursor.sect {
        cursor.sect = section;
        cursor.rrnum = 0;
        cursor.ptr = start;
    }
    let rrnum = if rrnum == -1 { cursor.rrnum } else { rrnum };
    let count = c_int::from(parsed.counts.get(index).copied().unwrap_or(0));
    if rrnum < 0 || rrnum >= count {
        return Err(errno::ENODEV);
    }
    if rrnum < cursor.rrnum {
        cursor.rrnum = 0;
        cursor.ptr = start;
    }
    let eom = packet.len();
    let mut p = cursor.ptr.ok_or(errno::EMSGSIZE)?;
    if rrnum > cursor.rrnum {
        p += skiprr(packet, p, section, rrnum - cursor.rrnum).ok_or(errno::EMSGSIZE)?;
        cursor.ptr = Some(p);
        cursor.rrnum = rrnum;
    }
    let name_len = expand(packet, p, name).ok_or(errno::EMSGSIZE)?;
    let written = name
        .iter()
        .position(|&byte| byte == 0)
        .map_or(0, |nul| nul + 1);
    p += name_len;
    cursor.ptr = Some(p);
    if 4 > eom.saturating_sub(p) {
        return Err(errno::EMSGSIZE);
    }
    let kind = get16(packet, p) as u16;
    let class = get16(packet, p + 2) as u16;
    p += 4;
    cursor.ptr = Some(p);
    let mut record = Record {
        name_len: written,
        kind,
        class,
        ttl: 0,
        rdlength: 0,
        rdata: None,
    };
    if section != NS_S_QD {
        if 6 > eom - p {
            return Err(errno::EMSGSIZE);
        }
        record.ttl = (get16(packet, p) << 16 | get16(packet, p + 2)) as u32;
        record.rdlength = get16(packet, p + 4) as u16;
        p += 6;
        cursor.ptr = Some(p);
        if usize::from(record.rdlength) > eom - p {
            return Err(errno::EMSGSIZE);
        }
        record.rdata = Some(p);
        p += usize::from(record.rdlength);
        cursor.ptr = Some(p);
    }
    cursor.rrnum += 1;
    if cursor.rrnum > count {
        cursor.sect = section + 1;
        if cursor.sect == NS_S_MAX as c_int {
            cursor.rrnum = -1;
            cursor.ptr = None;
        } else {
            cursor.rrnum = 0;
        }
    }
    Ok(record)
}

/// Reads record `rrnum` of `section`, or the next one if `rrnum` is -1, of
/// the message `*handle` describes, into `*rr`. Returns 0, or -1 with `errno`
/// set: `ENODEV` for a section or record that does not exist, `EMSGSIZE` for
/// one that runs out of the message or a handle whose pointers are not inside
/// its message.
///
/// # Safety
///
/// `handle` must be an `ns_msg` `ns_initparse` filled, whose message is still
/// readable, and `rr` valid for writes of an `ns_rr`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_parserr(
    handle: *mut NsMsg,
    section: c_int,
    rrnum: c_int,
    rr: *mut NsRr,
) -> c_int {
    // SAFETY: the caller passes a handle `ns_initparse` filled.
    let h = unsafe { handle.read() };
    let msg = h._msg;
    let Some(len) = h._eom.addr().checked_sub(msg.addr()) else {
        errno::set(errno::EMSGSIZE);
        return -1;
    };
    let offset = |ptr: *const u8| {
        ptr.addr()
            .checked_sub(msg.addr())
            .filter(|&offset| !ptr.is_null() && offset <= len)
    };
    let parsed = Parsed {
        id: h._id,
        flags: h._flags,
        counts: h._counts,
        sections: h._sections.map(offset),
    };
    let mut cursor = Cursor {
        sect: h._sect,
        rrnum: h._rrnum,
        ptr: offset(h._msg_ptr),
    };
    // SAFETY: the message is readable, as the caller vouches.
    let packet = unsafe { core::slice::from_raw_parts(msg, len) };
    // SAFETY: the caller passes a writable `ns_rr`, whose name has
    // `NS_MAXDNAME` bytes.
    let name_ptr = rr.wrapping_byte_add(offset_of!(NsRr, name)).cast::<u8>();
    // SAFETY: as above.
    let name = unsafe { core::slice::from_raw_parts_mut(name_ptr, EXPANDED_MAX) };
    let result = parserr(packet, &parsed, &mut cursor, section, rrnum, name);
    let back = |offset: Option<usize>| offset.map_or(null(), |offset| msg.wrapping_add(offset));
    // SAFETY: the handle is writable, as the caller vouches.
    let handle_section = handle
        .wrapping_byte_add(offset_of!(NsMsg, _sect))
        .cast::<c_int>();
    // SAFETY: the handle is writable, as the caller vouches.
    unsafe { handle_section.write(cursor.sect) };
    // SAFETY: as above.
    let handle_record = handle
        .wrapping_byte_add(offset_of!(NsMsg, _rrnum))
        .cast::<c_int>();
    // SAFETY: as above.
    unsafe { handle_record.write(cursor.rrnum) };
    // SAFETY: as above.
    let handle_position = handle
        .wrapping_byte_add(offset_of!(NsMsg, _msg_ptr))
        .cast::<*const u8>();
    // SAFETY: as above.
    unsafe { handle_position.write(back(cursor.ptr)) };
    match result {
        Ok(record) => {
            // SAFETY: `rr` is writable, as the caller vouches.
            let record_type = rr.wrapping_byte_add(offset_of!(NsRr, r#type)).cast::<u16>();
            // SAFETY: as above.
            unsafe { record_type.write(record.kind) };
            // SAFETY: as above.
            let record_class = rr
                .wrapping_byte_add(offset_of!(NsRr, rr_class))
                .cast::<u16>();
            // SAFETY: as above.
            unsafe { record_class.write(record.class) };
            // SAFETY: as above.
            let record_ttl = rr.wrapping_byte_add(offset_of!(NsRr, ttl)).cast::<u32>();
            // SAFETY: as above.
            unsafe { record_ttl.write(record.ttl) };
            // SAFETY: as above.
            let record_length = rr
                .wrapping_byte_add(offset_of!(NsRr, rdlength))
                .cast::<u16>();
            // SAFETY: as above.
            unsafe { record_length.write(record.rdlength) };
            // SAFETY: as above.
            let record_data = rr
                .wrapping_byte_add(offset_of!(NsRr, rdata))
                .cast::<*const u8>();
            // SAFETY: as above.
            unsafe { record_data.write(back(record.rdata)) };
            0
        }
        Err(error) => {
            errno::set(error);
            -1
        }
    }
}

/// Expands the compressed name at `src`, in the message from `msg` to `eom`,
/// into the `dstsiz` bytes at `dst`, as `dn_expand` does, and returns how many
/// bytes it took, or -1 with `EMSGSIZE`.
///
/// # Safety
///
/// As `dn_expand`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn ns_name_uncompress(
    msg: *const u8,
    eom: *const u8,
    src: *const u8,
    dst: *mut c_char,
    dstsiz: usize,
) -> c_int {
    let space = c_int::try_from(dstsiz).unwrap_or(c_int::MAX);
    // SAFETY: the caller's contract is `dn_expand`'s.
    let r = unsafe { dn_expand(msg, eom, src, dst, space) };
    if r < 0 {
        errno::set(errno::EMSGSIZE);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netdb::testing::labels;

    fn expanded(packet: &[u8], src: usize, room: usize) -> Option<(usize, String)> {
        let mut dest = vec![0x58u8; room];
        let len = expand(packet, src, &mut dest)?;
        let nul = dest.iter().position(|&byte| byte == 0).unwrap_or(0);
        Some((
            len,
            String::from_utf8_lossy(dest.get(..nul).unwrap_or_default()).into_owned(),
        ))
    }

    #[test]
    fn a_query_is_musls_packet() {
        let (q, n) = mkquery(0, b"www.example.org.", 1, RR_AAAA, 0x1234).unwrap_or(([0; 280], 0));
        let mut expected = vec![0x12, 0x34, 1, 32, 0, 1, 0, 0, 0, 0, 0, 0];
        expected.extend_from_slice(&labels("www.example.org"));
        expected.extend_from_slice(&[0, 28, 0, 1]);
        assert_eq!(q.get(..n), Some(&expected[..]));
        assert_eq!(n, 17 + 15 + 1);

        let (q, n) = mkquery(0, b"", 1, RR_A, 0).unwrap_or(([9; 280], 0));
        assert_eq!(
            q.get(..n),
            Some(&[0, 0, 1, 32, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1][..])
        );
        let (_, n) = mkquery(0, b".", 1, RR_A, 0).unwrap_or(([9; 280], 0));
        assert_eq!(n, 17);

        let long_label = "a".repeat(64);
        let long_name = ["a"; 127].join(".") + "ab";
        for refused in [
            &b"a..b"[..],
            b".a",
            b"a..",
            b"..",
            long_label.as_bytes(),
            long_name.as_bytes(),
        ] {
            assert_eq!(mkquery(0, refused, 1, RR_A, 0), None, "{refused:?}");
        }
        assert!(mkquery(0, "a".repeat(63).as_bytes(), 1, RR_A, 0).is_some());
        assert_eq!(mkquery(16, b"a", 1, RR_A, 0), None);
        assert_eq!(mkquery(0, b"a", 256, RR_A, 0), None);
        assert_eq!(mkquery(0, b"a", 1, 256, 0), None);
        assert_eq!(mkquery(0, b"a", -1, RR_A, 0), None);
    }

    #[test]
    fn res_mkquery_refuses_a_buffer_too_small() {
        let mut buf = [0u8; 280];
        // SAFETY: the name is NUL-terminated and the buffer has 280 bytes.
        let n = unsafe {
            res_mkquery(
                0,
                c"a.b".as_ptr(),
                1,
                RR_A,
                null(),
                0,
                null(),
                buf.as_mut_ptr(),
                21,
            )
        };
        assert_eq!(n, 21);
        assert_eq!(buf.get(12..17), Some(&[1, b'a', 1, b'b', 0][..]));
        // SAFETY: as above.
        let n = unsafe {
            res_mkquery(
                0,
                c"a.b".as_ptr(),
                1,
                RR_A,
                null(),
                0,
                null(),
                buf.as_mut_ptr(),
                20,
            )
        };
        assert_eq!(n, -1);
    }

    #[test]
    fn names_expand_through_pointers() {
        // libc-test's dn_expand-empty and dn_expand-ptr-0.
        assert_eq!(expanded(&[0], 0, 1), Some((1, String::new())));
        let packet = [2, b'p', b'q', 0xc0, 5, 0];
        assert_eq!(expanded(&packet, 0, 3), Some((5, "pq".into())));
        assert_eq!(expanded(&[0xc0, 2, 0], 0, 1), Some((2, String::new())));

        let mut packet = labels("example.org");
        let second = packet.len();
        packet.extend_from_slice(&[3, b'w', b'w', b'w', 0xc0, 0]);
        assert_eq!(
            expanded(&packet, second, 254),
            Some((6, "www.example.org".into()))
        );
        // "www.example.org" and its NUL need 16 bytes.
        assert_eq!(expanded(&packet, second, 15), None);
        assert!(expanded(&packet, second, 16).is_some());
    }

    #[test]
    fn malformed_names_are_refused() {
        // A pointer to itself, and two pointing at each other.
        assert_eq!(expanded(&[0xc0, 0], 0, 254), None);
        assert_eq!(expanded(&[0xc0, 2, 0xc0, 0], 0, 254), None);
        // A label pointing back to its own start after a label.
        assert_eq!(expanded(&[1, b'a', 0xc0, 0], 0, 254), None);
        // A pointer past the end, or cut in half.
        assert_eq!(expanded(&[0xc0, 9, 0], 0, 254), None);
        assert_eq!(expanded(&[1, b'a', 0xc0], 0, 254), None);
        // A label longer than what is left, or without an end.
        assert_eq!(expanded(&[5, b'a', b'b', 0], 0, 254), None);
        assert_eq!(expanded(&[1, b'a'], 0, 254), None);
        // Nothing to start from, or nowhere to write.
        assert_eq!(expanded(&[0], 1, 254), None);
        assert_eq!(expanded(&[0], 0, 0), None);
        // A name longer than 253 bytes never fits.
        let mut long = Vec::new();
        for _ in 0..5 {
            long.extend_from_slice(&[63]);
            long.extend_from_slice(&[b'a'; 63]);
        }
        long.push(0);
        assert_eq!(expanded(&long, 0, 1000), None);
    }

    #[test]
    fn dn_expand_and_dn_skipname_check_their_pointers() {
        let packet = [2, b'p', b'q', 0xc0, 5, 0];
        let mut name = [0x58 as c_char; 8];
        let base = packet.as_ptr();
        let end = base.wrapping_add(packet.len());
        // SAFETY: the packet and the name are live locals of these sizes.
        let r = unsafe { dn_expand(base, end, base, name.as_mut_ptr(), 8) };
        assert_eq!(r, 5);
        // SAFETY: `dn_expand` ended the name.
        let expanded = unsafe { core::ffi::CStr::from_ptr(name.as_ptr()) };
        assert_eq!(expanded, c"pq");
        // SAFETY: as above; the source is at the end, then before the start.
        let result = unsafe { dn_expand(base, end, end, name.as_mut_ptr(), 8) };
        assert_eq!(result, -1);
        let before = base.wrapping_sub(1);
        // SAFETY: as above; nothing is read for a source before the start.
        let result = unsafe { dn_expand(base, end, before, name.as_mut_ptr(), 8) };
        assert_eq!(result, -1);
        // SAFETY: as above.
        let result = unsafe { dn_expand(base, end, base, name.as_mut_ptr(), 0) };
        assert_eq!(result, -1);

        assert_eq!(skipname(&packet, 0), Some(5));
        assert_eq!(skipname(&packet, 5), Some(1));
        assert_eq!(skipname(&[3, b'a', b'b'], 0), None);
        assert_eq!(skipname(&[0xc0], 0), None);
        // SAFETY: the packet is a live local.
        assert_eq!(unsafe { dn_skipname(base, end) }, 5);
    }

    /// A response to an `A` query for `example.org`: a CNAME and two
    /// addresses, then an authority record.
    fn response() -> Vec<u8> {
        let mut r = vec![0xab, 0xcd, 0x81, 0x80, 0, 1, 0, 3, 0, 1, 0, 0];
        r.extend_from_slice(&labels("example.org"));
        r.extend_from_slice(&[0, 1, 0, 1]);
        // CNAME www.example.org -> pointer to the question name.
        r.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 1, 0, 0, 2, 0xc0, 12]);
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 1]);
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 2]);
        r.extend_from_slice(&[0, 0, 2, 0, 1, 0, 0, 0, 5, 0, 1, 0]);
        r
    }

    #[test]
    fn the_answer_walker_finds_each_record() {
        let r = response();
        let mut seen = Vec::new();
        let ret = parse(&r, |rr, data, len| {
            seen.push((rr, r.get(data..data + len).map(<[u8]>::to_vec)));
            0
        });
        assert_eq!(ret, 0);
        assert_eq!(
            seen,
            [
                (RR_CNAME, Some(vec![0xc0, 12])),
                (RR_A, Some(vec![192, 0, 2, 1])),
                (RR_A, Some(vec![192, 0, 2, 2])),
            ]
        );
        // A callback that fails stops the walk.
        assert_eq!(parse(&r, |_, _, _| -1), -1);
        // A response code means no answers.
        let mut refused = r.clone();
        if let Some(flags) = refused.get_mut(3) {
            *flags = 0x83;
        }
        assert_eq!(parse(&refused, |_, _, _| -1), 0);
        // Every truncation of the packet is refused or walks less, and never
        // reads past the end.
        for cut in 0..r.len() {
            let short = r.get(..cut).unwrap_or_default();
            let ret = parse(short, |_, data, len| {
                assert!(data + len <= cut);
                0
            });
            assert!(ret == -1 || cut >= 12, "{cut}");
        }
        // A record length past the end.
        let mut long = r.clone();
        let len_at = long.len() - 12 - 16 + 10;
        if let Some(byte) = long.get_mut(len_at) {
            *byte = 0xff;
        }
        assert_eq!(parse(&long, |_, _, _| 0), -1);
    }

    #[test]
    fn ns_parse_walks_every_section() {
        let r = response();
        let parsed = initparse(&r).unwrap_or(Parsed {
            id: 0,
            flags: 0,
            counts: [0; 4],
            sections: [None; 4],
        });
        assert_eq!(parsed.id, 0xabcd);
        assert_eq!(parsed.counts, [1, 3, 1, 0]);
        assert_eq!(parsed.sections[0..1], [Some(12)]);
        let mut cursor = Cursor {
            sect: 4,
            rrnum: -1,
            ptr: None,
        };
        let mut name = [0u8; 254];
        let question = parserr(&r, &parsed, &mut cursor, 0, 0, &mut name);
        assert_eq!(
            question.map(|record| (record.kind, record.class, record.rdata)),
            Ok((1, 1, None))
        );
        assert_eq!(name.get(..12), Some(&b"example.org\0"[..]));
        let answers: Vec<_> = (0..3)
            .map(|_| parserr(&r, &parsed, &mut cursor, 1, -1, &mut name).map(|record| record.kind))
            .collect();
        assert_eq!(answers, [Ok(5), Ok(1), Ok(1)]);
        assert_eq!(
            parserr(&r, &parsed, &mut cursor, 1, -1, &mut name),
            Err(errno::ENODEV)
        );
        // Going back to an earlier record starts the section again.
        let first = parserr(&r, &parsed, &mut cursor, 1, 1, &mut name);
        assert_eq!(first.map(|record| record.rdlength), Ok(4));
        let authority = parserr(&r, &parsed, &mut cursor, 2, 0, &mut name);
        assert_eq!(
            authority.map(|record| (record.kind, record.ttl, record.name_len)),
            Ok((2, 5, 1))
        );
        assert_eq!(
            parserr(&r, &parsed, &mut cursor, 3, 0, &mut name),
            Err(errno::ENODEV)
        );
        assert_eq!(
            parserr(&r, &parsed, &mut cursor, 4, 0, &mut name),
            Err(errno::ENODEV)
        );
        assert_eq!(
            parserr(&r, &parsed, &mut cursor, -1, 0, &mut name),
            Err(errno::ENODEV)
        );
    }

    #[test]
    fn ns_initparse_refuses_what_does_not_fill_the_message() {
        let r = response();
        let mut extra = r.clone();
        extra.push(0);
        assert_eq!(initparse(&extra), None);
        for cut in 0..r.len() {
            assert_eq!(initparse(r.get(..cut).unwrap_or_default()), None, "{cut}");
        }
        // Counts far beyond the packet.
        let mut counts = r.clone();
        if let Some(byte) = counts.get_mut(6) {
            *byte = 0xff;
        }
        assert_eq!(initparse(&counts), None);
        // A compression loop in a name: skipping stops at the pointer, so the
        // header parses, and expanding the name is refused.
        let mut looped = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1];
        let parsed = initparse(&looped);
        assert!(parsed.is_some());
        if let Some(parsed) = parsed {
            let mut cursor = Cursor {
                sect: 4,
                rrnum: -1,
                ptr: None,
            };
            let mut name = [0u8; 254];
            assert_eq!(
                parserr(&looped, &parsed, &mut cursor, 0, 0, &mut name),
                Err(errno::EMSGSIZE)
            );
        }
        looped.truncate(15);
        assert_eq!(initparse(&looped), None);
    }

    #[test]
    fn the_c_ns_functions_keep_the_handle_in_step() {
        let r = response();
        // SAFETY: an `ns_msg` of zeros is a valid value to overwrite.
        let mut handle: NsMsg = unsafe { core::mem::zeroed() };
        let len = c_int::try_from(r.len()).unwrap_or(0);
        // SAFETY: the packet and the handle are live locals.
        assert_eq!(unsafe { ns_initparse(r.as_ptr(), len, &raw mut handle) }, 0);
        assert_eq!(handle._counts, [1, 3, 1, 0]);
        // SAFETY: an `ns_rr` of zeros is a valid value to overwrite.
        let mut rr: NsRr = unsafe { core::mem::zeroed() };
        let mut addresses = Vec::new();
        // SAFETY: the handle was filled by `ns_initparse`, and the packet and
        // the record are live locals.
        while unsafe { ns_parserr(&raw mut handle, 1, -1, &raw mut rr) } == 0 {
            if rr.r#type == 1 {
                // SAFETY: `rdata` points at `rdlength` bytes in the packet.
                addresses.push(unsafe { ns_get32(rr.rdata) });
            }
        }
        assert_eq!(crate::pwd::last_errno(), errno::ENODEV);
        assert_eq!(addresses, [0xc000_0201, 0xc000_0202]);
        // SAFETY: `rr.name` was ended by `ns_parserr`.
        let name = unsafe { core::ffi::CStr::from_ptr(rr.name.as_ptr()) };
        assert_eq!(name, c"example.org");
        // SAFETY: as above.
        assert_eq!(unsafe { ns_initparse(r.as_ptr(), 11, &raw mut handle) }, -1);
        assert_eq!(crate::pwd::last_errno(), errno::EMSGSIZE);
        let mut bytes = [0u8; 4];
        // SAFETY: the buffer has 4 bytes.
        unsafe { ns_put32(0x0102_0304, bytes.as_mut_ptr()) };
        assert_eq!(bytes, [1, 2, 3, 4]);
        // SAFETY: as above.
        unsafe { ns_put16(0xabcd, bytes.as_mut_ptr()) };
        // SAFETY: as above.
        assert_eq!(unsafe { ns_get16(bytes.as_ptr()) }, 0xabcd);
        assert_eq!(_ns_flagdata.get(1).map(|flag| flag.shift), Some(11));
    }
}
