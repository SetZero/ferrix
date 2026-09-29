//! The set of loaded objects, and the order a symbol is searched for in.
//!
//! # The order is the program, then breadth-first
//!
//! A symbol is looked for in the program first, then in its `DT_NEEDED`
//! libraries in the order it names them, then in theirs, and so on. The first
//! definition found wins, and that is not an optimisation but the rule
//! programs depend on: it is what lets a program define `malloc` and have
//! every library in the process use that one.
//!
//! Breadth-first, not depth-first, for the same reason: a library named
//! directly by the program takes precedence over one pulled in beneath
//! another.
//!
//! # Loading is breadth-first too, and that matters
//!
//! Each object is loaded once, found by the name it was asked for. A diamond
//! — two libraries both needing a third — loads the third once and both see
//! the same copy, which is the whole point of a shared object having one set
//! of writable data per process.

use core::ffi::c_char;

use crate::map;
use crate::object::{MAX_OBJECTS, Object};
use crate::report::Error;
use crate::sym::{self, Found};
use crate::sys::{self, nr};

/// The longest path the loader will build for a library.
const PATH_MAX: usize = 1024;

/// The longest `LD_LIBRARY_PATH` the loader keeps a copy of.
const LIBRARY_PATH_MAX: usize = 4096;

/// Bytes for the paths libraries were found at by searching, which are
/// built in a buffer on the stack and so must be copied to be kept. When it
/// is full, a library is reported by the name it was asked for instead.
const PATH_POOL: usize = 32 * 1024;

/// Words of a set with one bit per object, as `lookup_from` marks the
/// objects it has queued.
const QUEUED_WORDS: usize = MAX_OBJECTS.div_ceil(64);

/// Where a library is looked for when its name has no slash in it.
///
/// `DT_RUNPATH` and `LD_LIBRARY_PATH` come first, and are handled by the
/// caller; these are the defaults every system has. There is no
/// `/etc/ld.so.cache`: a cache is a second source of truth about where a file
/// is, and the loader reads the directories instead.
const DEFAULT_PATHS: [&str; 4] = ["/lib", "/usr/lib", "/lib64", "/usr/lib64"];

/// Bytes of static TLS kept past the start-up modules' blocks for the
/// libraries `dlopen` brings: glibc's default surplus. Chrome's GPU process
/// opens four with TLS -- ANGLE's EGL and GLES, the Vulkan loader and
/// SwiftShader -- which take 136 bytes of it.
pub(crate) const TLS_SURPLUS: usize = 1664;

/// Every object this process has loaded.
#[derive(Debug)]
pub(crate) struct Scope {
    /// The objects, in the order they were loaded, which is the order they
    /// are searched.
    objects: [Object; MAX_OBJECTS],
    /// The name each was loaded as, for diagnostics and for finding it again.
    names: [*const c_char; MAX_OBJECTS],
    /// The path each was found at by searching, or null when it was not
    /// searched for: glibc reports this path, not the name, to `dladdr`,
    /// `dl_iterate_phdr` and `link_map`. NSS loads its soft token from the
    /// directory `dladdr` names for `libnss3.so`, and did not find it when
    /// that was the bare name.
    paths: [*const c_char; MAX_OBJECTS],
    /// The bytes [`Self::paths`] point into, and how many are used.
    path_pool: [u8; PATH_POOL],
    path_pool_used: usize,
    /// How many there are.
    count: usize,
    /// `LD_LIBRARY_PATH`, as a colon-separated list, or null.
    library_path: *const c_char,
    /// The copy of it [`Self::library_path`] points at.
    library_path_copy: [u8; LIBRARY_PATH_MAX],
    /// Whether the program runs with privileges it was not started with,
    /// which `$ORIGIN` is then not expanded for.
    secure: bool,
    /// Bytes below the initial thread pointer occupied by static TLS, and
    /// [`TLS_SURPLUS`] kept for libraries `dlopen` brings later.
    tls_size: usize,
    /// How much of `tls_size` is given out: the start-up modules' blocks,
    /// and those `dlopen` has placed in the surplus since.
    tls_used: usize,
    /// The greatest alignment any static TLS image requires.
    tls_align: usize,
    /// The program's `PT_INTERP`: the name this loader was loaded as, or
    /// null.
    interpreter: *const c_char,
}

impl Scope {
    /// An empty scope.
    #[must_use]
    pub(crate) const fn new() -> Scope {
        Scope {
            objects: [Object::EMPTY; MAX_OBJECTS],
            names: [core::ptr::null(); MAX_OBJECTS],
            paths: [core::ptr::null(); MAX_OBJECTS],
            path_pool: [0; PATH_POOL],
            path_pool_used: 0,
            count: 0,
            library_path: core::ptr::null(),
            library_path_copy: [0; LIBRARY_PATH_MAX],
            secure: false,
            tls_size: 0,
            tls_used: 0,
            tls_align: 1,
            interpreter: core::ptr::null(),
        }
    }

    /// Set the colon-separated list `LD_LIBRARY_PATH` gave.
    ///
    /// Searched before the defaults and after an object's own `DT_RUNPATH`,
    /// which is the order every loader uses and the order a program that ships
    /// its own libraries beside itself depends on.
    ///
    /// Copied, as glibc copies it, because `dlopen` searches it long after
    /// the program started, and the environment block it came from is the
    /// program's to overwrite: Chrome writes its process title over it, and
    /// NSS's `dlopen` of its soft token then searched only the defaults. A
    /// list longer than the copy holds is used where it is.
    pub(crate) fn set_library_path(&mut self, path: *const c_char) {
        let mut len = 0;
        // SAFETY: `path` is NUL-terminated, and this stops at the NUL.
        while unsafe { path.wrapping_add(len).read() } != 0 {
            len += 1;
        }
        self.library_path = path;
        if let Some(copy) = self.library_path_copy.get_mut(..=len) {
            // SAFETY: `len + 1` bytes of `path` are its value and its NUL.
            let value = unsafe { core::slice::from_raw_parts(path.cast::<u8>(), len + 1) };
            copy.copy_from_slice(value);
            self.library_path = copy.as_ptr().cast();
        }
    }

    /// Say that the program runs with privileges it was not started with
    /// (`AT_SECURE`).
    pub(crate) fn set_secure(&mut self, secure: bool) {
        self.secure = secure;
    }

    /// How many objects are loaded.
    #[must_use]
    pub(crate) const fn len(&self) -> usize {
        self.count
    }

    /// Bytes the initial thread's static TLS reservation needs below its
    /// thread pointer on x86-64, or past the thread control block above it
    /// on AArch64 and ARMv7-A. Valid after [`Self::layout_tls`].
    #[must_use]
    pub(crate) const fn tls_size(&self) -> usize {
        self.tls_size
    }

    /// The greatest alignment requested by a static TLS image. Valid after
    /// [`Self::layout_tls`].
    #[must_use]
    pub(crate) const fn tls_align(&self) -> usize {
        self.tls_align
    }

    /// The object at `index`, in search order.
    #[must_use]
    pub(crate) fn get(&self, index: usize) -> Option<&Object> {
        self.objects.get(index)
    }

    /// Give every `PT_TLS` image its position relative to the thread
    /// pointer.
    ///
    /// x86-64's variant-II TLS packs images downwards from the thread pointer.
    /// Rounding after each image, rather than merely at the end, preserves
    /// every segment's required alignment.
    ///
    /// AArch64's and ARMv7-A's variant I packs them upwards, after the
    /// [`crate::tls::TCB_SIZE`] bytes of control block at the thread pointer:
    /// the first at that size rounded up to its own alignment, which is
    /// where the static linker put the program's local-exec variables, and
    /// each next one at the end of the last, rounded up to its own.
    ///
    /// # Errors
    ///
    /// [`Error::MalformedObject`] if the layout would not fit in an address
    /// or a signed TLS offset.
    pub(crate) fn layout_tls(&mut self) -> Result<(), Error> {
        #[cfg(not(target_arch = "x86_64"))]
        return self.layout_tls_upwards();
        #[cfg(target_arch = "x86_64")]
        self.layout_tls_downwards()
    }

    /// [`Self::layout_tls`] for variant II.
    #[cfg(target_arch = "x86_64")]
    fn layout_tls_downwards(&mut self) -> Result<(), Error> {
        let mut size = 0_usize;
        let mut greatest_align = 1_usize;
        for object in self.objects.iter_mut().take(self.count) {
            let Some(tls) = object.tls.as_mut() else {
                continue;
            };
            let unaligned = size
                .checked_add(tls.memsz)
                .ok_or(Error::MalformedObject("TLS layout is too large"))?;
            let rounded = unaligned
                .checked_add(tls.align - 1)
                .map(|value| value & !(tls.align - 1))
                .ok_or(Error::MalformedObject("TLS layout is too large"))?;
            let offset = isize::try_from(rounded)
                .map_err(|_| Error::MalformedObject("TLS layout is too large"))?;
            tls.offset = -offset;
            size = rounded;
            greatest_align = greatest_align.max(tls.align);
        }
        self.tls_used = size;
        self.tls_size = size
            .checked_add(TLS_SURPLUS)
            .ok_or(Error::MalformedObject("TLS layout is too large"))?;
        self.tls_align = greatest_align;
        Ok(())
    }

    /// [`Self::layout_tls`] for variant I.
    #[cfg(not(target_arch = "x86_64"))]
    fn layout_tls_upwards(&mut self) -> Result<(), Error> {
        const TOO_LARGE: Error = Error::MalformedObject("TLS layout is too large");
        let mut end = crate::tls::TCB_SIZE;
        let mut greatest_align = 1_usize;
        for object in self.objects.iter_mut().take(self.count) {
            let Some(tls) = object.tls.as_mut() else {
                continue;
            };
            let start = end
                .checked_add(tls.align - 1)
                .map(|value| value & !(tls.align - 1))
                .ok_or(TOO_LARGE)?;
            tls.offset = isize::try_from(start).map_err(|_| TOO_LARGE)?;
            end = start.checked_add(tls.memsz).ok_or(TOO_LARGE)?;
            greatest_align = greatest_align.max(tls.align);
        }
        // What the blocks take past the control block. A C library that
        // reserves this much after the control block rounded up to
        // `tls_align` covers them all, since every block starts at or after
        // the end of the control block.
        self.tls_used = end - crate::tls::TCB_SIZE;
        self.tls_size = self.tls_used.checked_add(TLS_SURPLUS).ok_or(TOO_LARGE)?;
        self.tls_align = greatest_align;
        Ok(())
    }

    /// Give object `index`, which `dlopen` just loaded, a block in the
    /// static TLS surplus, if it has `PT_TLS`: the same offset from every
    /// thread's pointer, as the start-up modules have.
    ///
    /// # Errors
    ///
    /// [`Error::MalformedObject`] when the surplus has no room left, or the
    /// block asks for more alignment than every thread pointer has.
    pub(crate) fn place_in_surplus(&mut self, index: usize) -> Result<(), Error> {
        const FULL: Error =
            Error::MalformedObject("no room left in static TLS for a dlopened library");
        let used = self.tls_used;
        let size = self.tls_size;
        let Some(tls) = self
            .objects
            .get_mut(index)
            .and_then(|object| object.tls.as_mut())
        else {
            return Ok(());
        };
        if tls.align > crate::tls::TP_ALIGN {
            return Err(Error::MalformedObject(
                "a dlopened library's TLS asks for more alignment than a thread pointer has",
            ));
        }
        #[cfg(target_arch = "x86_64")]
        let (offset, used) = {
            let end = used
                .checked_add(tls.memsz)
                .and_then(|value| value.checked_add(tls.align - 1))
                .map(|value| value & !(tls.align - 1))
                .ok_or(FULL)?;
            (-(isize::try_from(end).map_err(|_| FULL)?), end)
        };
        #[cfg(not(target_arch = "x86_64"))]
        let (offset, used) = {
            let start = (crate::tls::TCB_SIZE + used)
                .checked_add(tls.align - 1)
                .map(|value| value & !(tls.align - 1))
                .ok_or(FULL)?;
            let end = start.checked_add(tls.memsz).ok_or(FULL)? - crate::tls::TCB_SIZE;
            (isize::try_from(start).map_err(|_| FULL)?, end)
        };
        if used > size {
            return Err(FULL);
        }
        tls.offset = offset;
        self.tls_used = used;
        Ok(())
    }

    /// Add an object that is already mapped: the program, or the loader.
    ///
    /// # Errors
    ///
    /// [`Error::TooManyObjects`].
    pub(crate) fn push(&mut self, name: *const c_char, object: Object) -> Result<(), Error> {
        let object = Object {
            index: self.count,
            ..object
        };
        let slot = self
            .objects
            .get_mut(self.count)
            .ok_or(Error::TooManyObjects)?;
        *slot = object;
        let slot = self
            .names
            .get_mut(self.count)
            .ok_or(Error::TooManyObjects)?;
        *slot = name;
        // A slot a failed `dlopen` gave back may hold an earlier object's.
        if let Some(path) = self.paths.get_mut(self.count) {
            *path = core::ptr::null();
        }
        self.count += 1;
        Ok(())
    }

    /// The index of the object that answers the `DT_NEEDED` `name`: the one
    /// loaded as it, or `libc.so.6` for a name that is part of it.
    #[must_use]
    pub(crate) fn dependency(&self, name: *const c_char) -> Option<usize> {
        self.find(name).or_else(|| {
            PART_OF_LIBC
                .iter()
                .any(|part| same(part.as_ptr(), name))
                .then(|| self.find(LIBC.as_ptr()))
                .flatten()
        })
    }

    /// Whether an object with this `DT_SONAME` or load name is already here.
    #[must_use]
    fn already_loaded(&self, name: *const c_char) -> bool {
        // The loader is in the process before anything is loaded, under the
        // name the program gave it; a `DT_NEEDED` names it by file name.
        if !self.interpreter.is_null() && same(file_name(self.interpreter), file_name(name)) {
            return true;
        }
        self.find(name).is_some()
    }

    /// The index of the object loaded as `name`, or whose `DT_SONAME` it is;
    /// the loader's for the name the program gave its interpreter.
    #[must_use]
    pub(crate) fn find(&self, name: *const c_char) -> Option<usize> {
        for index in 0..self.count {
            let object = self.objects.get(index)?;
            if object.is_loader
                && !self.interpreter.is_null()
                && same(file_name(self.interpreter), file_name(name))
            {
                return Some(index);
            }
            if self
                .names
                .get(index)
                .is_some_and(|loaded| same(*loaded, name))
            {
                return Some(index);
            }
            if self
                .paths
                .get(index)
                .is_some_and(|path| !path.is_null() && same(*path, name))
            {
                return Some(index);
            }
            if let Some(soname) = object.soname()
                && same(soname, name)
            {
                return Some(index);
            }
        }
        None
    }

    /// The path object `index` was found at, or the name it was loaded as
    /// when it was not searched for.
    #[must_use]
    pub(crate) fn name_at(&self, index: usize) -> *const c_char {
        match self.paths.get(index) {
            Some(path) if !path.is_null() => *path,
            _ => self.names.get(index).copied().unwrap_or(core::ptr::null()),
        }
    }

    /// Load `name` and everything it needs, as `dlopen` asks, and answer its
    /// index. An object already here is answered as it is.
    ///
    /// Whatever this adds is at [`Self::len`] as it was on entry and after;
    /// on an error some of it may be there, and [`Self::truncate`] forgets it.
    ///
    /// # Errors
    ///
    /// [`Error`], naming what could not be loaded.
    pub(crate) fn open(&mut self, name: *const c_char, page_size: usize) -> Result<usize, Error> {
        if let Some(index) = self.find(name) {
            return Ok(index);
        }
        self.load_one(name, core::ptr::null(), &[], page_size)?;
        // One of glibc's split-off names is answered by `libc.so.6`, which
        // then is what was opened.
        let index = self
            .find(name)
            .or_else(|| self.find(LIBC.as_ptr()))
            .ok_or(Error::LibraryNotFound(name))?;
        self.load_dependencies(page_size)?;
        Ok(index)
    }

    /// Forget every object from `count` on: a `dlopen` that failed part of
    /// the way. Their mappings stay, unused; nothing refers to them.
    pub(crate) fn truncate(&mut self, count: usize) {
        self.count = self.count.min(count);
    }

    /// The handle `dlopen` gives for object `index`: the address of its slot,
    /// which never moves, since the scope is a fixed array in a `static`.
    #[must_use]
    pub(crate) fn handle(&self, index: usize) -> *mut core::ffi::c_void {
        self.objects
            .get(index)
            .map_or(core::ptr::null_mut(), |object| {
                core::ptr::from_ref(object).cast_mut().cast()
            })
    }

    /// The index a handle from [`Self::handle`] stands for, and `None` for
    /// anything else.
    #[must_use]
    pub(crate) fn index_of_handle(&self, handle: *const core::ffi::c_void) -> Option<usize> {
        let first = self.objects.as_ptr().addr();
        let offset = handle.addr().checked_sub(first)?;
        let size = size_of::<Object>();
        let index = offset / size;
        (offset % size == 0 && index < self.count).then_some(index)
    }

    /// Find `name`, at `version` when one is given, in object `root` and then
    /// in what it needs, breadth-first as the loader loaded them: what
    /// `dlsym` and `dlvsym` on a handle search.
    ///
    /// # Safety
    ///
    /// Every object in the scope must be mapped.
    #[must_use]
    pub(crate) unsafe fn lookup_from(
        &self,
        root: usize,
        name: *const c_char,
        version: Option<*const c_char>,
    ) -> Option<Found> {
        let hash = sym::hash(name);
        let mut queued = [0_u64; QUEUED_WORDS];
        let mark = |queued: &mut [u64; QUEUED_WORDS], index: usize| -> Option<bool> {
            let word = queued.get_mut(index / 64)?;
            let bit = 1 << (index % 64);
            let fresh = *word & bit == 0;
            *word |= bit;
            Some(fresh)
        };
        let _ = mark(&mut queued, root)?;
        let mut queue = [0_usize; MAX_OBJECTS];
        *queue.first_mut()? = root;
        let (mut head, mut tail) = (0, 1);
        while head < tail {
            let index = *queue.get(head)?;
            head += 1;
            let object = self.objects.get(index)?;
            if let Some(mut found) = sym::lookup(object, name, hash, version) {
                found.tls_offset = object.tls.map(|tls| tls.offset);
                found.module = index;
                return Some(found);
            }
            for slot in 0..object.needed_count {
                let needed = object
                    .needed
                    .get(slot)
                    .and_then(|offset| object.name(*offset))
                    .and_then(|needed| self.find(needed));
                let Some(next) = needed else {
                    continue;
                };
                if mark(&mut queued, next)? {
                    *queue.get_mut(tail)? = next;
                    tail += 1;
                }
            }
        }
        None
    }

    /// Load everything the objects already here need, and everything those
    /// need, until nothing is left to load.
    ///
    /// # Errors
    ///
    /// [`Error`], naming the library that could not be loaded.
    pub(crate) fn load_dependencies(&mut self, page_size: usize) -> Result<(), Error> {
        // Walked by index rather than iterated: the list grows as it is
        // walked, which is what makes this breadth-first without a queue of
        // its own.
        let mut at = 0;
        let mut origin = [0_u8; PATH_MAX];
        while at < self.count {
            let object = *self.objects.get(at).ok_or(Error::TooManyObjects)?;
            // Where the object's file is, for `$ORIGIN` in its run path;
            // empty when that is not known, or not to be trusted.
            let origin_len = if object.runpath == 0 || self.secure {
                0
            } else {
                self.origin_of(at, &mut origin)
            };
            for slot in 0..object.needed_count {
                let offset = *object.needed.get(slot).ok_or(Error::TooManyObjects)?;
                let name = object.name(offset).ok_or(Error::MalformedObject(
                    "a DT_NEEDED outside the string table",
                ))?;
                if self.already_loaded(name) {
                    continue;
                }
                let runpath = if object.runpath == 0 {
                    core::ptr::null()
                } else {
                    object.name(object.runpath).unwrap_or(core::ptr::null())
                };
                self.load_one(
                    name,
                    runpath,
                    origin.get(..origin_len).unwrap_or_default(),
                    page_size,
                )?;
            }
            at += 1;
        }
        Ok(())
    }

    /// Find one library by name, map it, and add it.
    fn load_one(
        &mut self,
        name: *const c_char,
        runpath: *const c_char,
        origin: &[u8],
        page_size: usize,
    ) -> Result<(), Error> {
        // A name with a slash is a path and is used as it is; one without is
        // looked for on the search path. That is the rule every loader has.
        if contains_slash(name) {
            return self.map_and_add(name, name, page_size);
        }
        // Before the search path, which may hold glibc's own file of the name.
        if PART_OF_LIBC.iter().any(|part| same(part.as_ptr(), name)) {
            if self.already_loaded(LIBC.as_ptr()) {
                return Ok(());
            }
            return self.load_one(LIBC.as_ptr(), runpath, origin, page_size);
        }
        // `DT_RUNPATH` of the object that asked, then `LD_LIBRARY_PATH`,
        // then the defaults.
        let mut buffer = [0_u8; PATH_MAX];
        if let Some(mapped) = self.search(runpath, name, &mut buffer, origin, page_size) {
            return self.add_found(name, &buffer, mapped);
        }
        if let Some(mapped) = self.search(self.library_path, name, &mut buffer, &[], page_size) {
            return self.add_found(name, &buffer, mapped);
        }
        for directory in DEFAULT_PATHS {
            let Some(path) = join(&mut buffer, directory.as_bytes(), name) else {
                continue;
            };
            if let Ok(mapped) = map::object(path, page_size) {
                return self.add_found(name, &buffer, mapped);
            }
            // Any failure here means "not this directory": the next one is
            // tried, and the error reported if none works is the honest one,
            // that the library is nowhere on the path.
        }
        Err(Error::LibraryNotFound(name))
    }

    /// Look for `name` in each directory of a colon-separated list, leaving
    /// the path it was found at in `buffer`. A directory that starts with
    /// `$ORIGIN` or `${ORIGIN}` starts with `origin` instead, and is skipped
    /// when `origin` is empty.
    fn search(
        &self,
        list: *const c_char,
        name: *const c_char,
        buffer: &mut [u8; PATH_MAX],
        origin: &[u8],
        page_size: usize,
    ) -> Option<map::Mapped> {
        if list.is_null() {
            return None;
        }
        let mut directory = [0_u8; PATH_MAX];
        let mut expanded = [0_u8; PATH_MAX];
        let mut at = list;
        loop {
            let (len, next) = next_directory(at, &mut directory)?;
            if len > 0
                && let Some(text) = directory.get(..len)
                && let Some(text) = with_origin(text, origin, &mut expanded)
                && let Some(path) = join(buffer, text, name)
                && let Ok(mapped) = map::object(path, page_size)
            {
                return Some(mapped);
            }
            at = next?;
        }
    }

    /// Copy the directory of object `index`'s file into `into`, and answer
    /// its length, or 0 when it is not known.
    ///
    /// The program's is where `/proc/self/exe` leads, links followed, as
    /// glibc finds it: `rustc` is run through `/bin/rustc`, a link to where
    /// its libraries are beside it. A library's is the path it was found at.
    fn origin_of(&self, index: usize, into: &mut [u8; PATH_MAX]) -> usize {
        let len = if index == 0 {
            const AT_FDCWD: usize = -100_isize as usize;
            // SAFETY: the path is NUL-terminated, and `into` is writable for
            // its length.
            let read = unsafe {
                sys::syscall4(
                    nr::READLINKAT,
                    AT_FDCWD,
                    c"/proc/self/exe".as_ptr() as usize,
                    into.as_mut_ptr() as usize,
                    into.len(),
                )
            };
            match usize::try_from(read) {
                // A link as long as the buffer may have been cut short.
                Ok(read) if read < into.len() => read,
                _ => return 0,
            }
        } else {
            let path = self.name_at(index);
            let mut len = 0;
            // SAFETY: the name is NUL-terminated, and this stops at the NUL.
            while !path.is_null() && unsafe { path.wrapping_add(len).read() } != 0 {
                let Some(slot) = into.get_mut(len) else {
                    return 0;
                };
                // SAFETY: as above.
                *slot = unsafe { path.wrapping_add(len).read() } as u8;
                len += 1;
            }
            len
        };
        into.get(..len)
            .and_then(|path| path.iter().rposition(|&byte| byte == b'/'))
            .map_or(0, |slash| slash.max(1))
    }

    /// Map `path` and add it under `name`.
    fn map_and_add(
        &mut self,
        name: *const c_char,
        path: *const c_char,
        page_size: usize,
    ) -> Result<(), Error> {
        let mapped = map::object(path, page_size)?;
        self.add_mapped(name, mapped)
    }

    /// [`Self::add_mapped`] for a library found by searching, keeping the
    /// NUL-terminated path in `found` as the one it is reported by.
    fn add_found(
        &mut self,
        name: *const c_char,
        found: &[u8; PATH_MAX],
        mapped: map::Mapped,
    ) -> Result<(), Error> {
        let index = self.count;
        self.add_mapped(name, mapped)?;
        let kept = self.keep_path(found);
        if let Some(slot) = self.paths.get_mut(index) {
            *slot = kept;
        }
        Ok(())
    }

    /// Copy a NUL-terminated path into [`Self::path_pool`], or null when it
    /// does not fit.
    fn keep_path(&mut self, path: &[u8; PATH_MAX]) -> *const c_char {
        let Some(len) = path.iter().position(|&byte| byte == 0) else {
            return core::ptr::null();
        };
        let start = self.path_pool_used;
        let Some(room) = self.path_pool.get_mut(start..=start + len) else {
            return core::ptr::null();
        };
        let Some(bytes) = path.get(..=len) else {
            return core::ptr::null();
        };
        room.copy_from_slice(bytes);
        self.path_pool_used = start + len + 1;
        room.as_ptr().cast()
    }

    /// Read a mapped object's dynamic table and add it to the scope.
    fn add_mapped(&mut self, name: *const c_char, mapped: map::Mapped) -> Result<(), Error> {
        // SAFETY: `map::object` returns the object's own `PT_DYNAMIC` at its
        // run-time address, with the bias it was mapped at.
        let mut object =
            unsafe { Object::read(mapped.base, mapped.dynamic) }.ok_or(Error::TooManyObjects)?;
        object.relro = mapped.relro;
        object.tls = mapped.tls;
        object.phdr = mapped.phdr;
        object.phnum = mapped.phnum;
        self.push(name, object)
    }

    /// Find `name` in the first object from `first` on whose definition
    /// answers `version`.
    ///
    /// `first` is zero for every relocation but `COPY`, which must find the
    /// library's definition and not the room the program made for it -- the
    /// program's own symbol has the same name and would otherwise be found
    /// first, and copied onto itself.
    ///
    /// # Safety
    ///
    /// Every object in the scope must be mapped.
    #[must_use]
    pub(crate) unsafe fn lookup(
        &self,
        name: *const c_char,
        version: Option<*const c_char>,
        first: usize,
    ) -> Option<Found> {
        let hash = sym::hash(name);
        for index in first..self.count {
            let object = self.objects.get(index)?;
            if let Some(mut found) = sym::lookup(object, name, hash, version) {
                found.tls_offset = object.tls.map(|tls| tls.offset);
                found.module = index;
                return Some(found);
            }
        }
        None
    }

    /// Record the path the program named as its interpreter: this loader's
    /// own name, which a `DT_NEEDED` may ask for too.
    ///
    /// glibc's programs on AArch64 and ARM name `ld-linux-aarch64.so.1` or
    /// `ld-linux-armhf.so.3` as a library as well as an interpreter, because
    /// glibc's `libc.so.6` reaches into its loader. Here the loader is already
    /// in the process, and loading a second copy of it from the same file
    /// would map a program nothing calls.
    pub(crate) fn set_interpreter(&mut self, path: *const c_char) {
        self.interpreter = path;
    }

    /// Add the loader itself, last, so that its symbols can be found and its
    /// name recognised. Its relocations were applied by its own start, and it
    /// has no initialisers, so the loops that walk the scope skip it by
    /// [`Object::is_loader`].
    ///
    /// # Errors
    ///
    /// [`Error::TooManyObjects`].
    pub(crate) fn push_loader(&mut self, mut object: Object) -> Result<(), Error> {
        object.is_loader = true;
        let name = if self.interpreter.is_null() {
            c"ld-ferrousli".as_ptr()
        } else {
            self.interpreter
        };
        self.push(name, object)
    }
}

/// The names glibc gives the libraries it splits its C library into.
///
/// Since glibc 2.34 all but `libm.so.6` are empty but for compatibility
/// symbols, their contents moved into `libc.so.6`; ferrousli is one library,
/// maths included. So each of these names is answered by the object that
/// answers to `libc.so.6`, even where glibc's own file of the name is on the
/// search path: glibc's `libm.so.6`, `librt.so.1` and `libresolv.so.2` import
/// `GLIBC_PRIVATE` names -- `_rtld_global_ro`, which `libm`'s ifunc resolvers
/// read the processor's features from, `__libc_fatal`, the resolver's context
/// -- that only glibc's own loader and C library define. Loaded beside
/// ferrousli, `libm`'s first resolver read through a null pointer; a Debian
/// volume, such as Chrome's, carries all of them.
const PART_OF_LIBC: [&core::ffi::CStr; 7] = [
    c"libm.so.6",
    c"libpthread.so.0",
    c"libdl.so.2",
    c"libresolv.so.2",
    c"librt.so.1",
    c"libutil.so.1",
    c"libanl.so.1",
];

/// The name every [`PART_OF_LIBC`] falls back to.
const LIBC: &core::ffi::CStr = c"libc.so.6";

/// The last component of a path: `ld-linux-x86-64.so.2` of
/// `/lib64/ld-linux-x86-64.so.2`, and a name with no slash itself.
fn file_name(path: *const c_char) -> *const c_char {
    let mut at = path;
    let mut last = path;
    loop {
        // SAFETY: the path is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { at.read() } as u8;
        if byte == 0 {
            return last;
        }
        // The byte read was not the terminator.
        at = at.wrapping_add(1);
        if byte == b'/' {
            last = at;
        }
    }
}

/// Whether two NUL-terminated names are the same.
fn same(a: *const c_char, b: *const c_char) -> bool {
    if a.is_null() || b.is_null() {
        return false;
    }
    let mut a = a;
    let mut b = b;
    loop {
        // SAFETY: both are NUL-terminated, and this stops at the first NUL.
        let x = unsafe { a.read() };
        // SAFETY: as above.
        let y = unsafe { b.read() };
        if x != y {
            return false;
        }
        if x == 0 {
            return true;
        }
        // Neither byte was the terminator.
        (a, b) = (a.wrapping_add(1), b.wrapping_add(1));
    }
}

/// Whether a name holds a slash, which makes it a path rather than a name to
/// search for.
fn contains_slash(name: *const c_char) -> bool {
    let mut at = name;
    loop {
        // SAFETY: the name is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { at.read() } as u8;
        if byte == 0 {
            return false;
        }
        if byte == b'/' {
            return true;
        }
        // The byte read was not the terminator.
        at = at.wrapping_add(1);
    }
}

/// Write `directory/name` into `buffer` and return it as a path.
///
/// `None` when it would not fit, which is a name too long to be looked for
/// there rather than an error: the next directory may be shorter.
fn join(
    buffer: &mut [u8; PATH_MAX],
    directory: &[u8],
    name: *const c_char,
) -> Option<*const c_char> {
    let mut at = 0;
    for byte in directory {
        *buffer.get_mut(at)? = *byte;
        at += 1;
    }
    *buffer.get_mut(at)? = b'/';
    at += 1;
    let mut from = name;
    loop {
        // SAFETY: the name is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { from.read() } as u8;
        *buffer.get_mut(at)? = byte;
        if byte == 0 {
            return Some(buffer.as_ptr().cast::<c_char>());
        }
        at += 1;
        // The byte read was not the terminator.
        from = from.wrapping_add(1);
    }
}

/// `directory` with a leading `$ORIGIN` or `${ORIGIN}` replaced by `origin`,
/// built in `into`; `directory` itself when it has neither; and `None` when
/// it has one and `origin` is empty or the result does not fit.
fn with_origin<'a>(
    directory: &'a [u8],
    origin: &[u8],
    into: &'a mut [u8; PATH_MAX],
) -> Option<&'a [u8]> {
    let rest = [b"$ORIGIN".as_slice(), b"${ORIGIN}".as_slice()]
        .iter()
        .find_map(|token| {
            directory
                .strip_prefix(*token)
                .filter(|rest| rest.is_empty() || rest.first() == Some(&b'/'))
        });
    let Some(rest) = rest else {
        return Some(directory);
    };
    if origin.is_empty() {
        return None;
    }
    let len = origin.len().checked_add(rest.len())?;
    into.get_mut(..origin.len())?.copy_from_slice(origin);
    into.get_mut(origin.len()..len)?.copy_from_slice(rest);
    into.get(..len)
}

/// Copy the next colon-separated directory out of `list` into `into`.
///
/// Returns how many bytes it is, and where the next one starts — `None` for
/// the last. An empty element means the working directory, as it does for
/// `PATH`, and is skipped here rather than searched: a loader that searched
/// `.` because a variable ended in a colon would load a library from wherever
/// the program happened to be started.
fn next_directory(
    list: *const c_char,
    into: &mut [u8; PATH_MAX],
) -> Option<(usize, Option<*const c_char>)> {
    let mut at = list;
    let mut len = 0;
    loop {
        // SAFETY: the list is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { at.read() } as u8;
        if byte == 0 {
            return Some((len, None));
        }
        if byte == b':' {
            // The byte read was not the terminator.
            return Some((len, Some(at.wrapping_add(1))));
        }
        *into.get_mut(len)? = byte;
        len += 1;
        // As above.
        at = at.wrapping_add(1);
    }
}
