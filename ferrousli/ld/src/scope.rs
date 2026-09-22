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

/// The longest path the loader will build for a library.
const PATH_MAX: usize = 1024;

/// Where a library is looked for when its name has no slash in it.
///
/// `DT_RUNPATH` and `LD_LIBRARY_PATH` come first, and are handled by the
/// caller; these are the defaults every system has. There is no
/// `/etc/ld.so.cache`: a cache is a second source of truth about where a file
/// is, and the loader reads the directories instead.
const DEFAULT_PATHS: [&str; 4] = ["/lib", "/usr/lib", "/lib64", "/usr/lib64"];

/// Every object this process has loaded.
#[derive(Debug)]
pub struct Scope {
    /// The objects, in the order they were loaded, which is the order they
    /// are searched.
    objects: [Object; MAX_OBJECTS],
    /// The name each was loaded as, for diagnostics and for finding it again.
    names: [*const c_char; MAX_OBJECTS],
    /// How many there are.
    count: usize,
    /// `LD_LIBRARY_PATH`, as a colon-separated list, or null.
    library_path: *const c_char,
    /// Bytes below the initial thread pointer occupied by static TLS.
    tls_size: usize,
    /// The greatest alignment any static TLS image requires.
    tls_align: usize,
    /// The program's `PT_INTERP`: the name this loader was loaded as, or
    /// null.
    interpreter: *const c_char,
}

impl Scope {
    /// An empty scope.
    #[must_use]
    pub const fn new() -> Scope {
        Scope {
            objects: [Object::EMPTY; MAX_OBJECTS],
            names: [core::ptr::null(); MAX_OBJECTS],
            count: 0,
            library_path: core::ptr::null(),
            tls_size: 0,
            tls_align: 1,
            interpreter: core::ptr::null(),
        }
    }

    /// Set the colon-separated list `LD_LIBRARY_PATH` gave.
    ///
    /// Searched before the defaults and after an object's own `DT_RUNPATH`,
    /// which is the order every loader uses and the order a program that ships
    /// its own libraries beside itself depends on.
    pub fn set_library_path(&mut self, path: *const c_char) {
        self.library_path = path;
    }

    /// How many objects are loaded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Bytes the initial thread's static TLS reservation needs below its
    /// thread pointer. Valid after [`Self::layout_tls`].
    #[must_use]
    pub const fn tls_size(&self) -> usize {
        self.tls_size
    }

    /// The greatest alignment requested by a static TLS image. Valid after
    /// [`Self::layout_tls`].
    #[must_use]
    pub const fn tls_align(&self) -> usize {
        self.tls_align
    }

    /// The object at `index`, in search order.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Object> {
        self.objects.get(index)
    }

    /// Give every `PT_TLS` image its position below the initial thread
    /// pointer.
    ///
    /// x86-64's variant-II TLS packs images downwards from the thread pointer.
    /// Rounding after each image, rather than merely at the end, preserves
    /// every segment's required alignment.
    ///
    /// # Errors
    ///
    /// [`Error::MalformedObject`] if the layout would not fit in an address
    /// or a signed TLS offset.
    pub fn layout_tls(&mut self) -> Result<(), Error> {
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
        self.tls_size = size;
        self.tls_align = greatest_align;
        Ok(())
    }

    /// Add an object that is already mapped: the program, or the loader.
    ///
    /// # Errors
    ///
    /// [`Error::TooManyObjects`].
    pub fn push(&mut self, name: *const c_char, object: Object) -> Result<(), Error> {
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
        self.count += 1;
        Ok(())
    }

    /// Whether an object with this `DT_SONAME` or load name is already here.
    #[must_use]
    fn already_loaded(&self, name: *const c_char) -> bool {
        // The loader is in the process before anything is loaded, under the
        // name the program gave it; a `DT_NEEDED` names it by file name.
        if !self.interpreter.is_null() && same(file_name(self.interpreter), file_name(name)) {
            return true;
        }
        for index in 0..self.count {
            let Some(loaded) = self.names.get(index) else {
                continue;
            };
            if same(*loaded, name) {
                return true;
            }
            let Some(object) = self.objects.get(index) else {
                continue;
            };
            if let Some(soname) = object.soname()
                && same(soname, name)
            {
                return true;
            }
        }
        false
    }

    /// Load everything the objects already here need, and everything those
    /// need, until nothing is left to load.
    ///
    /// # Errors
    ///
    /// [`Error`], naming the library that could not be loaded.
    pub fn load_dependencies(&mut self, page_size: usize) -> Result<(), Error> {
        // Walked by index rather than iterated: the list grows as it is
        // walked, which is what makes this breadth-first without a queue of
        // its own.
        let mut at = 0;
        while at < self.count {
            let object = *self.objects.get(at).ok_or(Error::TooManyObjects)?;
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
                self.load_one(name, runpath, page_size)?;
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
        page_size: usize,
    ) -> Result<(), Error> {
        // A name with a slash is a path and is used as it is; one without is
        // looked for on the search path. That is the rule every loader has.
        if contains_slash(name) {
            return self.map_and_add(name, name, page_size);
        }
        // `DT_RUNPATH` of the object that asked, then `LD_LIBRARY_PATH`,
        // then the defaults.
        if let Some(mapped) = self.search(runpath, name, page_size) {
            return self.add_mapped(name, mapped);
        }
        if let Some(mapped) = self.search(self.library_path, name, page_size) {
            return self.add_mapped(name, mapped);
        }
        let mut buffer = [0_u8; PATH_MAX];
        for directory in DEFAULT_PATHS {
            let Some(path) = join(&mut buffer, directory.as_bytes(), name) else {
                continue;
            };
            if let Ok(mapped) = map::object(path, page_size) {
                return self.add_mapped(name, mapped);
            }
            // Any failure here means "not this directory": the next one is
            // tried, and the error reported if none works is the honest one,
            // that the library is nowhere on the path.
        }
        if PART_OF_LIBC.iter().any(|part| same(part.as_ptr(), name)) {
            if self.already_loaded(LIBC.as_ptr()) {
                return Ok(());
            }
            return self.load_one(LIBC.as_ptr(), runpath, page_size);
        }
        Err(Error::LibraryNotFound(name))
    }

    /// Look for `name` in each directory of a colon-separated list.
    fn search(
        &self,
        list: *const c_char,
        name: *const c_char,
        page_size: usize,
    ) -> Option<map::Mapped> {
        if list.is_null() {
            return None;
        }
        let mut buffer = [0_u8; PATH_MAX];
        let mut directory = [0_u8; PATH_MAX];
        let mut at = list;
        loop {
            let (len, next) = next_directory(at, &mut directory)?;
            if len > 0
                && let Some(text) = directory.get(..len)
                && let Some(path) = join(&mut buffer, text, name)
                && let Ok(mapped) = map::object(path, page_size)
            {
                return Some(mapped);
            }
            at = next?;
        }
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

    /// Read a mapped object's dynamic table and add it to the scope.
    fn add_mapped(&mut self, name: *const c_char, mapped: map::Mapped) -> Result<(), Error> {
        // SAFETY: `map::object` returns the object's own `PT_DYNAMIC` at its
        // run-time address, with the bias it was mapped at.
        let mut object =
            unsafe { Object::read(mapped.base, mapped.dynamic) }.ok_or(Error::TooManyObjects)?;
        object.relro = mapped.relro;
        object.tls = mapped.tls;
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
    pub unsafe fn lookup(
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
    pub fn set_interpreter(&mut self, path: *const c_char) {
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
    pub fn push_loader(&mut self, mut object: Object) -> Result<(), Error> {
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
/// maths included. So one of these names that is nowhere on the search path
/// is satisfied by the object that answers to `libc.so.6` -- and only when it
/// is nowhere, so that glibc's own files, where they are installed, are used.
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
        // SAFETY: the byte read was not the terminator.
        at = unsafe { at.add(1) };
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
        let (x, y) = unsafe { (a.read(), b.read()) };
        if x != y {
            return false;
        }
        if x == 0 {
            return true;
        }
        // SAFETY: neither byte was the terminator.
        (a, b) = unsafe { (a.add(1), b.add(1)) };
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
        // SAFETY: the byte read was not the terminator.
        at = unsafe { at.add(1) };
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
        // SAFETY: the byte read was not the terminator.
        from = unsafe { from.add(1) };
    }
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
            // SAFETY: the byte read was not the terminator.
            return Some((len, Some(unsafe { at.add(1) })));
        }
        *into.get_mut(len)? = byte;
        len += 1;
        // SAFETY: as above.
        at = unsafe { at.add(1) };
    }
}
