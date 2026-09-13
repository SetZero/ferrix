//! `grp.h`: the group database in `/etc/group`, with `getgrouplist` and
//! `initgroups`.
//!
//! The parsing is musl's `passwd/getgrent_a.c`: a name, a password, a decimal
//! group id, and the members separated by commas, an empty list meaning none.
//! As in [`crate::pwd`], there is no nscd, only a trailing newline is removed,
//! and the reentrant lookups need room for the line rather than for
//! `getline`'s whole buffer. `getgrent`, `getgrnam` and `getgrgid` return
//! static storage the next call to any of them overwrites.
//!
//! `initgroups` asks `getgrouplist` for at most `NGROUPS_MAX`, 32, groups, as
//! musl does, so a user in more fails.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int, c_uint};
use core::mem::{align_of, offset_of, size_of};
use core::ptr::null_mut;

use crate::errno;
use crate::malloc::{free, realloc};
use crate::process::setgroups;
use crate::pwd::{Line, Shared, colon, copy, cut, digits, last_errno, open_database};
use crate::stdio::file::File;
use crate::stdio::open::fclose;
use crate::string::strcmp;

/// `NGROUPS_MAX`, from `include/limits.h`.
const NGROUPS_MAX: usize = 32;

/// C's `struct group`, from `include/grp.h`. glibc's is the same.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Group {
    /// The group name.
    pub gr_name: *mut c_char,
    /// The password field.
    pub gr_passwd: *mut c_char,
    /// The group id.
    pub gr_gid: c_uint,
    /// The members' names, ending in a null pointer.
    pub gr_mem: *mut *mut c_char,
}

const _: () = assert!(size_of::<Group>() == 32);
const _: () = assert!(offset_of!(Group, gr_mem) == 24);

impl Group {
    /// An entry with every pointer null.
    const EMPTY: Self = Self {
        gr_name: null_mut(),
        gr_passwd: null_mut(),
        gr_gid: 0,
        gr_mem: null_mut(),
    };
}

/// The array of member pointers a group's entry points to, kept between reads.
#[derive(Debug)]
struct Members {
    /// The array, from `malloc`, or null.
    ptr: *mut *mut c_char,
    /// How many pointers it holds.
    slots: usize,
}

impl Members {
    /// No array yet.
    const EMPTY: Self = Self {
        ptr: null_mut(),
        slots: 0,
    };

    /// Makes room for `slots` pointers. False, with `errno` set, if there is
    /// no memory.
    fn reserve(&mut self, slots: usize) -> bool {
        if self.slots >= slots {
            return true;
        }
        let Some(bytes) = slots.checked_mul(size_of::<*mut c_char>()) else {
            errno::set(errno::ENOMEM);
            return false;
        };
        // SAFETY: the array is null or from `malloc`.
        let grown = unsafe { realloc(self.ptr.cast(), bytes) };
        if grown.is_null() {
            return false;
        }
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "malloc aligns to 16, more than a pointer needs"
        )]
        let array = grown.cast::<*mut c_char>();
        self.ptr = array;
        self.slots = slots;
        true
    }

    /// Stores `member` at `index`.
    fn set(&mut self, index: usize, member: *mut c_char) {
        if index < self.slots {
            // SAFETY: the index is inside the array.
            unsafe { self.ptr.wrapping_add(index).write(member) };
        }
    }

    /// The member at `index`.
    fn get(&self, index: usize) -> *mut c_char {
        if index < self.slots {
            // SAFETY: the index is inside the array, which `next` filled.
            unsafe { self.ptr.wrapping_add(index).read() }
        } else {
            null_mut()
        }
    }

    /// Frees the array.
    fn release(&mut self) {
        // SAFETY: the array is null or from `malloc`.
        unsafe { free(self.ptr.cast()) };
        *self = Self::EMPTY;
    }
}

/// Where a group line's fields are.
#[derive(Debug, PartialEq, Eq)]
struct Fields {
    /// Where the password field starts.
    passwd: usize,
    /// The group id.
    gid: c_uint,
    /// Where the member list starts.
    members: usize,
}

/// Splits `line`, which ends in a NUL, into its fields with NULs, as musl's
/// `__getgrent_a` does, or `None` if one is missing.
fn parse(line: &mut [u8]) -> Option<Fields> {
    let end_name = colon(line, 1)?;
    cut(line, end_name);
    let passwd = end_name + 1;
    let end_passwd = colon(line, passwd)?;
    cut(line, end_passwd);
    let (gid, end_gid) = digits(line, end_passwd + 1);
    if line.get(end_gid) != Some(&b':') {
        return None;
    }
    cut(line, end_gid);
    Some(Fields {
        passwd,
        gid,
        members: end_gid + 1,
    })
}

/// How many members the list starting at `from` has: none if it is empty,
/// otherwise one more than it has commas, so an empty member counts too.
fn member_count(line: &[u8], from: usize) -> usize {
    let Some(rest) = line.get(from..) else {
        return 0;
    };
    match rest.first() {
        Some(&byte) if byte != 0 => {
            1 + rest
                .iter()
                .take_while(|&&byte| byte != 0)
                .filter(|&&byte| byte == b',')
                .count()
        }
        _ => 0,
    }
}

/// Splits the member list starting at `from` at its commas, with NULs, and
/// calls `each` with each member's index and where it starts.
fn split_members(line: &mut [u8], from: usize, mut each: impl FnMut(usize, usize)) {
    if !line.get(from).is_some_and(|&byte| byte != 0) {
        return;
    }
    each(0, from);
    let mut index = 1;
    let mut at = from;
    while let Some(byte) = line.get_mut(at) {
        match *byte {
            0 => break,
            b',' => {
                *byte = 0;
                each(index, at + 1);
                index += 1;
            }
            _ => {}
        }
        at += 1;
    }
}

/// Whether `user` is among the first `n` members.
///
/// # Safety
///
/// `user` must be a NUL-terminated string, and `members` hold `n` members
/// `next` stored.
unsafe fn lists(members: &Members, n: usize, user: *const c_char) -> bool {
    (0..n).any(|index| {
        // SAFETY: both are NUL-terminated strings: the caller's, and a member
        // `next` stored.
        unsafe { strcmp(user, members.get(index)) == 0 }
    })
}

/// Stores `gid` as group number `count`, counting from 1, if that is within
/// `limit`.
///
/// # Safety
///
/// `groups` must be valid for writes of `limit` ids.
unsafe fn store(groups: *mut c_uint, limit: usize, count: usize, gid: c_uint) {
    if count >= 1 && count <= limit {
        // SAFETY: `count - 1` is below `limit`, which the caller vouches for.
        unsafe { groups.wrapping_add(count - 1).write(gid) };
    }
}

/// Reads the next group of `stream` into `line` and `members`, skipping lines
/// that do not parse, and returns it with the line's length and its member
/// count.
///
/// # Safety
///
/// `stream` must be a live stream.
unsafe fn next(
    stream: *mut File,
    line: &mut Line,
    members: &mut Members,
) -> Result<Option<(Group, usize, usize)>, c_int> {
    loop {
        // SAFETY: the caller passes a live stream.
        let Some(len) = (unsafe { line.read(stream) })? else {
            return Ok(None);
        };
        let base = line.ptr;
        // SAFETY: the line was just read: `len` bytes and a NUL.
        let bytes = unsafe { line.bytes(len) };
        let Some(fields) = parse(bytes) else {
            continue;
        };
        let count = member_count(bytes, fields.members);
        if !members.reserve(count + 1) {
            return Err(last_errno());
        }
        split_members(bytes, fields.members, |index, start| {
            members.set(index, base.wrapping_add(start));
        });
        members.set(count, null_mut());
        let group = Group {
            gr_name: base,
            gr_passwd: base.wrapping_add(fields.passwd),
            gr_gid: fields.gid,
            gr_mem: members.ptr,
        };
        return Ok(Some((group, len, count)));
    }
}

/// Finds the group named `name`, or with id `gid` if `name` is null.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string.
unsafe fn find(
    name: *const c_char,
    gid: c_uint,
    line: &mut Line,
    members: &mut Members,
) -> Result<Option<(Group, usize, usize)>, c_int> {
    let Some(stream) = open_database(c"/etc/group")? else {
        return Ok(None);
    };
    let found = loop {
        // SAFETY: `stream` is the file just opened.
        match unsafe { next(stream, line, members) } {
            Ok(Some(entry)) => {
                let hit = if name.is_null() {
                    entry.0.gr_gid == gid
                } else {
                    // SAFETY: both are NUL-terminated strings.
                    unsafe { strcmp(name, entry.0.gr_name) == 0 }
                };
                if hit {
                    break Ok(Some(entry));
                }
            }
            other => break other,
        }
    };
    // SAFETY: `stream` is live, and not used again.
    let _ = unsafe { fclose(stream) };
    found
}

/// What `getgrent`, `getgrnam` and `getgrgid` keep.
#[derive(Debug)]
struct State {
    /// The file `getgrent` reads, or null.
    stream: *mut File,
    /// The line the last entry's strings are in.
    line: Line,
    /// The last entry's member array.
    members: Members,
    /// The entry returned.
    entry: Group,
}

/// The state of `getgrent`, `getgrnam` and `getgrgid`.
static STATE: Shared<State> = Shared(UnsafeCell::new(State {
    stream: null_mut(),
    line: Line::EMPTY,
    members: Members::EMPTY,
    entry: Group::EMPTY,
}));

/// The state, for the length of one call.
fn state() -> &'static mut State {
    // SAFETY: see `Shared`.
    unsafe { &mut *STATE.0.get() }
}

/// Stores `found` as the entry to return, or reports what went wrong.
fn answer(state: &mut State, found: Result<Option<(Group, usize, usize)>, c_int>) -> *mut Group {
    match found {
        Ok(Some((entry, _, _))) => {
            state.entry = entry;
            &raw mut state.entry
        }
        Ok(None) => null_mut(),
        Err(error) => {
            errno::set(error);
            null_mut()
        }
    }
}

/// The next entry of the group database, in static storage, or null at the
/// end or with `errno` set.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getgrent() -> *mut Group {
    let state = state();
    if state.stream.is_null() {
        match open_database(c"/etc/group") {
            Ok(Some(stream)) => state.stream = stream,
            Ok(None) => return null_mut(),
            Err(error) => {
                errno::set(error);
                return null_mut();
            }
        }
    }
    // SAFETY: the stream is the file opened above or by an earlier call.
    let found = unsafe { next(state.stream, &mut state.line, &mut state.members) };
    answer(state, found)
}

/// Starts `getgrent` again from the first entry.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setgrent() {
    let state = state();
    if !state.stream.is_null() {
        // SAFETY: the stream is live, and forgotten here.
        let _ = unsafe { fclose(state.stream) };
        state.stream = null_mut();
    }
}

/// Closes the file `getgrent` reads. It is `setgrent`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endgrent() {
    setgrent();
}

/// The entry for group `name`, in static storage, or null if there is none.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getgrnam(name: *const c_char) -> *mut Group {
    let state = state();
    // SAFETY: the caller passes a NUL-terminated name.
    let found = unsafe { find(name, 0, &mut state.line, &mut state.members) };
    answer(state, found)
}

/// The entry for group id `gid`, in static storage, or null if there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getgrgid(gid: c_uint) -> *mut Group {
    let state = state();
    // SAFETY: there is no name.
    let found = unsafe { find(null_mut(), gid, &mut state.line, &mut state.members) };
    answer(state, found)
}

/// The reentrant lookup: the entry goes in `*gr`, the member array and the
/// strings in the `size` bytes at `buf`, and `*result` is `gr`, or null if
/// there is none. Returns 0 or the error, `ERANGE` if they do not fit.
///
/// # Safety
///
/// `name` must be null or a NUL-terminated string, `gr` and `result` valid
/// for writes, and `buf` for writes of `size` bytes.
unsafe fn lookup_r(
    name: *const c_char,
    gid: c_uint,
    gr: *mut Group,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Group,
) -> c_int {
    // SAFETY: the caller passes a writable result.
    unsafe { result.write(null_mut()) };
    let mut line = Line::EMPTY;
    let mut members = Members::EMPTY;
    // SAFETY: the caller passes a valid name.
    let error = match unsafe { find(name, gid, &mut line, &mut members) } {
        Ok(Some((found, len, count))) => {
            let word = size_of::<*mut c_char>();
            let pad = buf.addr().wrapping_neg() % align_of::<*mut c_char>();
            let array = (count + 1) * word;
            if size < pad + array + len + 1 {
                errno::ERANGE
            } else {
                let slots = buf.wrapping_add(pad);
                let text = slots.wrapping_add(array);
                // SAFETY: the line holds `len` bytes and a NUL, and `buf` room
                // for them after the array, in other memory.
                unsafe { copy(line.ptr, text, len + 1) };
                let moved = |field: *mut c_char| {
                    text.wrapping_add(field.addr().wrapping_sub(line.ptr.addr()))
                };
                #[allow(
                    clippy::cast_ptr_alignment,
                    reason = "`pad` aligned the array for pointers"
                )]
                let slots = slots.cast::<*mut c_char>();
                for index in 0..=count {
                    let member = members.get(index);
                    let member = if member.is_null() {
                        member
                    } else {
                        moved(member)
                    };
                    // SAFETY: the array has `count + 1` slots inside `buf`.
                    unsafe { slots.wrapping_add(index).write(member) };
                }
                let entry = Group {
                    gr_name: moved(found.gr_name),
                    gr_passwd: moved(found.gr_passwd),
                    gr_gid: found.gr_gid,
                    gr_mem: slots,
                };
                // SAFETY: the caller passes a writable entry.
                unsafe { gr.write(entry) };
                // SAFETY: the caller passes a writable result.
                unsafe { result.write(gr) };
                0
            }
        }
        Ok(None) => 0,
        Err(error) => error,
    };
    line.release();
    members.release();
    if error != 0 {
        errno::set(error);
    }
    error
}

/// The entry for group `name`, reentrantly: see `lookup_r`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string, `gr` and `result` valid for
/// writes, and `buf` for writes of `size` bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getgrnam_r(
    name: *const c_char,
    gr: *mut Group,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Group,
) -> c_int {
    // SAFETY: the caller's promises are `lookup_r`'s.
    unsafe { lookup_r(name, 0, gr, buf, size, result) }
}

/// The entry for group id `gid`, reentrantly: see `lookup_r`.
///
/// # Safety
///
/// `gr` and `result` must be valid for writes, and `buf` for writes of `size`
/// bytes.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getgrgid_r(
    gid: c_uint,
    gr: *mut Group,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Group,
) -> c_int {
    // SAFETY: the caller's promises are `lookup_r`'s.
    unsafe { lookup_r(null_mut(), gid, gr, buf, size, result) }
}

/// Stores in `groups` the group id `gid`, then the id of every group listing
/// `user` as a member, up to `*ngroups` of them, and sets `*ngroups` to how
/// many there are. Returns that count, or -1 if they did not all fit, or -1
/// with `errno` set if `/etc/group` could not be read.
///
/// # Safety
///
/// `user` must be a NUL-terminated string, `ngroups` valid for a read and a
/// write, and `groups` for writes of `*ngroups` ids.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getgrouplist(
    user: *const c_char,
    gid: c_uint,
    groups: *mut c_uint,
    ngroups: *mut c_int,
) -> c_int {
    // SAFETY: the caller passes a readable count.
    let limit = usize::try_from(unsafe { ngroups.read() }).unwrap_or(0);
    let mut count = 1_usize;
    if limit >= 1 {
        // SAFETY: the caller passes room for `limit` ids.
        unsafe { groups.write(gid) };
    }
    let stream = match open_database(c"/etc/group") {
        Ok(stream) => stream,
        Err(error) => {
            errno::set(error);
            return -1;
        }
    };
    let mut line = Line::EMPTY;
    let mut members = Members::EMPTY;
    let mut failure = None;
    if let Some(stream) = stream {
        loop {
            // SAFETY: `stream` is the file just opened.
            match unsafe { next(stream, &mut line, &mut members) } {
                Ok(Some((group, _, n))) => {
                    // SAFETY: `user` is the caller's string, and `next` stored
                    // `n` members.
                    if unsafe { lists(&members, n, user) } {
                        count += 1;
                        // SAFETY: the caller passes room for `limit` ids.
                        unsafe { store(groups, limit, count, group.gr_gid) };
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        // SAFETY: `stream` is live, and not used again.
        let _ = unsafe { fclose(stream) };
    }
    line.release();
    members.release();
    if let Some(error) = failure {
        errno::set(error);
        return -1;
    }
    let total = c_int::try_from(count).unwrap_or(c_int::MAX);
    // SAFETY: the caller passes a writable count.
    unsafe { ngroups.write(total) };
    if count > limit { -1 } else { total }
}

/// Sets the calling process's supplementary groups to `gid` and every group
/// that lists `user`.
///
/// # Safety
///
/// `user` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn initgroups(user: *const c_char, gid: c_uint) -> c_int {
    let mut list: [c_uint; NGROUPS_MAX] = [0; NGROUPS_MAX];
    let mut count = NGROUPS_MAX as c_int;
    // SAFETY: the list holds `count` ids, and `user` is the caller's string.
    if unsafe { getgrouplist(user, gid, list.as_mut_ptr(), &raw mut count) } < 0 {
        return -1;
    }
    // SAFETY: `getgrouplist` filled `count` ids.
    unsafe { setgroups(count as usize, list.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members_of(text: &str) -> (Option<Fields>, Vec<Vec<u8>>) {
        let mut line: Vec<u8> = text.bytes().chain([0]).collect();
        let Some(fields) = parse(&mut line) else {
            return (None, Vec::new());
        };
        let count = member_count(&line, fields.members);
        let mut starts = Vec::new();
        split_members(&mut line, fields.members, |index, start| {
            assert_eq!(index, starts.len());
            starts.push(start);
        });
        assert_eq!(count, starts.len());
        let names = starts
            .into_iter()
            .map(|start| {
                line.get(start..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|&&byte| byte != 0)
                    .copied()
                    .collect()
            })
            .collect();
        (Some(fields), names)
    }

    #[test]
    fn members_split_at_commas_and_an_empty_list_has_none() {
        let (fields, names) = members_of("wheel:x:10:root,alice,,bob");
        assert_eq!(fields.map(|fields| fields.gid), Some(10));
        assert_eq!(names, [&b"root"[..], b"alice", b"", b"bob"]);
        let (fields, names) = members_of("nogroup:x:65534:");
        assert!(fields.is_some() && names.is_empty());
        let (_, names) = members_of("g:x:1:a,");
        assert_eq!(names, [&b"a"[..], b""]);
        assert_eq!(members_of("g:x:1").0, None);
        assert_eq!(members_of("g:x:z:a").0, None);
    }
}
