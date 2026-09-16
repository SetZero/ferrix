//! Filename generation (zsh's `glob.c`): globbing with qualifiers and
//! sorting, brace expansion, and the pattern matching behind `${x#pat}`,
//! `${x%pat}` and `${x/pat/repl}`.

use std::os::unix::fs::MetadataExt;

use crate::options::*;
use crate::pattern::{GF_MATCHREF, PAT_FILE, PAT_FILET, PAT_NOANCH, PAT_NOGLD, PAT_NOTEND};
use crate::pattern::{
    PAT_NOTSTART, PAT_PURES, PAT_SCAN, PAT_STATIC, Patprog, ZPC_BAR, ZPC_HASH, ZPC_INPAR,
};
use crate::pattern::{ZPC_KSH_AT, ZPC_TILDE};
use crate::shell::Shell;
use crate::tok::{
    self, BAR, COMMA, DASH, EQUALS, HAT, INBRACE, INBRACK, INPAR, META, OUTBRACE, OUTPAR, POUND,
};
use crate::tok::{QUEST, STAR, TILDE};
use crate::utils::{at, from, lossy, sub};

const GS_NAME: i32 = 1;
const GS_DEPTH: i32 = 2;
const GS_EXEC: i32 = 4;
const GS_SHIFT_BASE: i32 = 8;
const GS_SIZE: i32 = GS_SHIFT_BASE;
const GS_ATIME: i32 = GS_SHIFT_BASE << 1;
const GS_MTIME: i32 = GS_SHIFT_BASE << 2;
const GS_CTIME: i32 = GS_SHIFT_BASE << 3;
const GS_LINKS: i32 = GS_SHIFT_BASE << 4;
const GS_SHIFT: i32 = 5;
const GS__SIZE: i32 = GS_SIZE << GS_SHIFT;
const GS__ATIME: i32 = GS_ATIME << GS_SHIFT;
const GS__MTIME: i32 = GS_MTIME << GS_SHIFT;
const GS__CTIME: i32 = GS_CTIME << GS_SHIFT;
const GS__LINKS: i32 = GS_LINKS << GS_SHIFT;
const GS_DESC: i32 = GS_SHIFT_BASE << (2 * GS_SHIFT);
const GS_NONE: i32 = GS_SHIFT_BASE << (2 * GS_SHIFT + 1);
const GS_NORMAL: i32 = GS_SIZE | GS_ATIME | GS_MTIME | GS_CTIME | GS_LINKS;
const GS_LINKED: i32 = GS_NORMAL << GS_SHIFT;

const TT_DAYS: i32 = 0;
const TT_HOURS: i32 = 1;
const TT_MINS: i32 = 2;
const TT_WEEKS: i32 = 3;
const TT_MONTHS: i32 = 4;
const TT_SECONDS: i32 = 5;
const TT_BYTES: i32 = 0;
const TT_POSIX_BLOCKS: i32 = 1;
const TT_KILOBYTES: i32 = 2;
const TT_MEGABYTES: i32 = 3;
const TT_GIGABYTES: i32 = 4;
const TT_TERABYTES: i32 = 5;

pub(crate) const SUB_END: i32 = 0x0001;
pub(crate) const SUB_LONG: i32 = 0x0002;
pub(crate) const SUB_SUBSTR: i32 = 0x0004;
pub(crate) const SUB_MATCH: i32 = 0x0008;
pub(crate) const SUB_REST: i32 = 0x0010;
pub(crate) const SUB_BIND: i32 = 0x0020;
pub(crate) const SUB_EIND: i32 = 0x0040;
pub(crate) const SUB_LEN: i32 = 0x0080;
pub(crate) const SUB_ALL: i32 = 0x0100;
pub(crate) const SUB_GLOBAL: i32 = 0x0200;
pub(crate) const SUB_DOSUBST: i32 = 0x0400;
pub(crate) const SUB_RETFAIL: i32 = 0x0800;
pub(crate) const SUB_START: i32 = 0x1000;
pub(crate) const SUB_LIST: i32 = 0x2000;
pub(crate) const SUB_EGLOB: i32 = 0x4000;

/// What a qualifier tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualFunc {
    IsLnk,
    IsSock,
    IsFifo,
    IsDir,
    IsReg,
    IsBlk,
    IsChr,
    IsDev,
    IsCom,
    Flags,
    ModeFlags,
    Dev,
    Nlink,
    Uid,
    Gid,
    Size,
    Time,
    ShEval,
    NonEmptyDir,
}

#[derive(Debug, Clone)]
struct Qual {
    func: QualFunc,
    data: i64,
    sense: i32,
    amc: i32,
    range: i32,
    units: i32,
    sdata: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct Gmatch {
    name: Vec<u8>,
    uname: Vec<u8>,
    sortstrs: Vec<Vec<u8>>,
    size: i64,
    atime: (i64, i64),
    mtime: (i64, i64),
    ctime: (i64, i64),
    links: i64,
    _size: i64,
    _atime: (i64, i64),
    _mtime: (i64, i64),
    _ctime: (i64, i64),
    _links: i64,
}

#[derive(Debug, Clone, Default)]
struct GlobSort {
    tp: i32,
    exec: Option<Vec<u8>>,
}

/// A path component to match (zsh's `struct complist`).
#[derive(Debug)]
struct Complist {
    pat: Patprog,
    closure: i32,
    follow: bool,
    next: Option<Box<Complist>>,
}

/// The state of one glob (zsh's `struct globdata`).
#[derive(Debug, Default)]
struct Glob {
    pathbuf: Vec<u8>,
    matches: Vec<Gmatch>,
    colonmod: Option<Vec<u8>>,
    quals: Vec<Vec<Qual>>,
    qualct: i32,
    qualorct: i32,
    gf_nullglob: bool,
    gf_markdirs: bool,
    gf_noglobdots: bool,
    gf_listtypes: bool,
    gf_numsort: bool,
    gf_follow: bool,
    gf_sorts: i32,
    gf_sortlist: Vec<GlobSort>,
    gf_pre_words: Option<Vec<Vec<u8>>>,
    gf_post_words: Option<Vec<Vec<u8>>>,
    inserts: Option<Vec<Vec<u8>>>,
}

fn stat_path(path: &[u8], follow: bool) -> Option<std::fs::Metadata> {
    use std::os::unix::ffi::OsStrExt;
    let raw = tok::unmetafy(path);
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(&raw));
    if follow {
        std::fs::metadata(p).ok()
    } else {
        std::fs::symlink_metadata(p).ok()
    }
}

fn mode_is(m: u32, kind: u32) -> bool {
    m & libc::S_IFMT == kind
}

/// zsh's `mode_to_octal`: the mode's permission bits as written in octal.
pub(crate) fn mode_to_octal(mode: u32) -> i64 {
    i64::from(mode & 0o7777)
}

/// zsh's `file_type`: the `ls -F` mark for a mode.
pub(crate) fn file_type(mode: u32) -> u8 {
    if mode_is(mode, libc::S_IFBLK) {
        b'#'
    } else if mode_is(mode, libc::S_IFCHR) {
        b'%'
    } else if mode_is(mode, libc::S_IFDIR) {
        b'/'
    } else if mode_is(mode, libc::S_IFIFO) {
        b'|'
    } else if mode_is(mode, libc::S_IFLNK) {
        b'@'
    } else if mode_is(mode, libc::S_IFREG) {
        if mode & 0o111 != 0 { b'*' } else { b' ' }
    } else if mode_is(mode, libc::S_IFSOCK) {
        b'='
    } else {
        b'?'
    }
}

fn times(m: &std::fs::Metadata) -> ((i64, i64), (i64, i64), (i64, i64)) {
    (
        (m.atime(), m.atime_nsec()),
        (m.mtime(), m.mtime_nsec()),
        (m.ctime(), m.ctime_nsec()),
    )
}

impl Glob {
    /// zsh's `statfullpath`: stat `s` appended to the path so far.
    fn statfullpath(&self, s: &[u8], lstat: bool) -> Option<std::fs::Metadata> {
        let mut buf = self.pathbuf.clone();
        if s.is_empty() && !buf.is_empty() {
            buf.push(b'.');
            return stat_path(&buf, true);
        }
        buf.extend_from_slice(s);
        stat_path(&buf, !lstat)
    }

    /// zsh's `statfullpath` with no stat buffer: does it exist?
    fn exists(&self, s: &[u8], l: bool) -> bool {
        let mut buf = self.pathbuf.clone();
        if s.is_empty() && !buf.is_empty() {
            return stat_path(&buf, true).is_some_and(|m| m.is_dir());
        }
        buf.extend_from_slice(s);
        if stat_path(&buf, true).is_some() {
            return true;
        }
        l && stat_path(&buf, false).is_some()
    }

    fn addpath(&mut self, s: &[u8]) {
        self.pathbuf.extend_from_slice(s);
        self.pathbuf.push(b'/');
    }
}

impl Shell {
    fn qual_test(&mut self, g: &mut Glob, q: &Qual, name: &[u8], m: &std::fs::Metadata) -> bool {
        let mode = m.mode();
        match q.func {
            QualFunc::IsLnk => mode_is(mode, libc::S_IFLNK),
            QualFunc::IsSock => mode_is(mode, libc::S_IFSOCK),
            QualFunc::IsFifo => mode_is(mode, libc::S_IFIFO),
            QualFunc::IsDir => mode_is(mode, libc::S_IFDIR),
            QualFunc::IsReg => mode_is(mode, libc::S_IFREG),
            QualFunc::IsBlk => mode_is(mode, libc::S_IFBLK),
            QualFunc::IsChr => mode_is(mode, libc::S_IFCHR),
            QualFunc::IsDev => mode_is(mode, libc::S_IFBLK) || mode_is(mode, libc::S_IFCHR),
            QualFunc::IsCom => mode_is(mode, libc::S_IFREG) && mode & 0o111 != 0,
            QualFunc::Flags => mode_to_octal(mode) & q.data != 0,
            QualFunc::ModeFlags => {
                let v = mode_to_octal(mode);
                let y = q.data & 0o7777;
                let n = q.data >> 12;
                v & y == y && v & n == 0
            }
            QualFunc::Dev => i64::try_from(m.dev()).unwrap_or(-1) == q.data,
            QualFunc::Nlink => {
                let n = i64::try_from(m.nlink()).unwrap_or(0);
                cmp_range(q.range, n, q.data)
            }
            QualFunc::Uid => i64::from(m.uid()) == q.data,
            QualFunc::Gid => i64::from(m.gid()) == q.data,
            QualFunc::Size => {
                let mut scaled = i64::try_from(m.size()).unwrap_or(i64::MAX);
                scaled = match q.units {
                    TT_POSIX_BLOCKS => (scaled + 511) / 512,
                    TT_KILOBYTES => (scaled + 1023) / 1024,
                    TT_MEGABYTES => (scaled + 1_048_575) / 1_048_576,
                    TT_GIGABYTES => (scaled + 1_073_741_823) / 1_073_741_824,
                    TT_TERABYTES => (scaled + 1_099_511_627_775) / 1_099_511_627_776,
                    _ => scaled,
                };
                cmp_range(q.range, scaled, q.data)
            }
            QualFunc::Time => {
                let now = crate::params::now_tv().0;
                let t = match q.amc {
                    0 => m.atime(),
                    1 => m.mtime(),
                    _ => m.ctime(),
                };
                let mut diff = now - t;
                diff /= match q.units {
                    TT_DAYS => 86400,
                    TT_HOURS => 3600,
                    TT_MINS => 60,
                    TT_WEEKS => 604_800,
                    TT_MONTHS => 2_592_000,
                    _ => 1,
                };
                cmp_range(q.range, diff, q.data)
            }
            QualFunc::ShEval => {
                let code = q.sdata.clone().unwrap_or_default();
                let lv = self.lastval();
                let ef = self.errflag.get();
                let cshglob = self.badcshglob;
                self.unsetparam(b"reply");
                let _ = self.setsparam(b"REPLY", name.to_vec());
                self.badcshglob = 0;
                if !self.execstring_ctx(&code, "globqual") {
                    return false;
                }
                let ret = self.lastval();
                if ret != 0 {
                    self.badcshglob |= cshglob;
                }
                self.errflag
                    .set(ef | (self.errflag.get() & crate::shell::ERRFLAG_INT));
                self.set_lastval(lv);
                g.inserts = self
                    .getaparam(b"reply")
                    .or_else(|| self.gethparam(b"reply"))
                    .or_else(|| {
                        self.getsparam(b"reply")
                            .or_else(|| self.getsparam(b"REPLY"))
                            .map(|t| vec![t])
                    });
                ret == 0
            }
            QualFunc::NonEmptyDir => {
                if !mode_is(mode, libc::S_IFDIR) {
                    return false;
                }
                if m.nlink() > 2 {
                    return true;
                }
                use std::os::unix::ffi::OsStrExt;
                let raw = tok::unmetafy(name);
                std::fs::read_dir(std::ffi::OsStr::from_bytes(&raw))
                    .is_ok_and(|mut d| d.next().is_some())
            }
        }
    }

    /// zsh's `insert`: add `s` (in the directory so far) if it passes.
    fn glob_insert(&mut self, g: &mut Glob, s: &[u8], checked: bool) {
        g.inserts = None;
        let mut news = s.to_vec();
        let mut statted = 0;
        let mut buf: Option<std::fs::Metadata> = None;
        let mut buf2: Option<std::fs::Metadata> = None;
        let mut checked = checked;
        if g.gf_listtypes || g.gf_markdirs {
            let Some(m) = g.statfullpath(s, true) else {
                return;
            };
            checked = true;
            statted = 1;
            let mut mode = m.mode();
            buf = Some(m.clone());
            if g.gf_follow {
                let b2 = if mode_is(mode, libc::S_IFLNK) {
                    g.statfullpath(s, false).unwrap_or(m)
                } else {
                    m
                };
                mode = b2.mode();
                buf2 = Some(b2);
                statted |= 2;
            }
            if g.gf_listtypes || mode_is(mode, libc::S_IFDIR) {
                news.push(file_type(mode));
            }
        }
        if g.qualct != 0 || g.qualorct != 0 {
            if statted == 0 {
                match g.statfullpath(s, true) {
                    Some(m) => buf = Some(m),
                    None => return,
                }
            }
            let mut full = g.pathbuf.clone();
            full.extend_from_slice(&news);
            news = full;
            statted |= 1;
            let alts = g.quals.clone();
            let mut accepted = false;
            for alt in &alts {
                let mut ok = true;
                for q in alt {
                    if q.sense & 2 != 0 && statted & 2 == 0 {
                        let b = buf.clone();
                        buf2 = match &b {
                            Some(m) if mode_is(m.mode(), libc::S_IFLNK) => {
                                g.statfullpath(s, false).or(b)
                            }
                            _ => b,
                        };
                        statted |= 2;
                    }
                    let bp = if q.sense & 2 != 0 {
                        buf2.clone()
                    } else {
                        buf.clone()
                    };
                    let Some(bp) = bp else {
                        ok = false;
                        break;
                    };
                    let r = self.qual_test(g, q, &news, &bp);
                    if (i32::from(!r) ^ q.sense) & 1 != 0 {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    accepted = true;
                    break;
                }
            }
            if !accepted {
                return;
            }
        } else if !checked {
            if !g.exists(s, true) {
                return;
            }
            let mut full = g.pathbuf.clone();
            full.extend_from_slice(&news);
            news = full;
        } else {
            let mut full = g.pathbuf.clone();
            full.extend_from_slice(&news);
            news = full;
        }
        let inserts = g.inserts.take();
        let mut items: Vec<Vec<u8>> = match &inserts {
            Some(list) => list.clone(),
            None => vec![news],
        };
        for mut item in items.drain(..) {
            if let Some(cm) = g.colonmod.clone() {
                let mut mi = 0;
                self.modify(&mut item, &cm, &mut mi, true);
            }
            if statted == 0 && g.gf_sorts & GS_NORMAL != 0 {
                buf = g.statfullpath(s, true);
                statted = 1;
            }
            if statted & 2 == 0 && g.gf_sorts & GS_LINKED != 0 {
                if statted != 0 {
                    buf2 = match &buf {
                        Some(m) if mode_is(m.mode(), libc::S_IFLNK) => {
                            g.statfullpath(s, false).or_else(|| buf.clone())
                        }
                        other => other.clone(),
                    };
                } else {
                    buf2 = g.statfullpath(s, false).or_else(|| g.statfullpath(s, true));
                }
                statted |= 2;
            }
            let (size, atime, mtime, ctime, links) = match (&buf, statted & 1 != 0) {
                (Some(m), true) => {
                    let (a, mm, c) = times(m);
                    (
                        i64::try_from(m.size()).unwrap_or(0),
                        a,
                        mm,
                        c,
                        i64::try_from(m.nlink()).unwrap_or(0),
                    )
                }
                _ => (0, (0, 0), (0, 0), (0, 0), 0),
            };
            let (_size, _atime, _mtime, _ctime, _links) = match (&buf2, statted & 2 != 0) {
                (Some(m), true) => {
                    let (a, mm, c) = times(m);
                    (
                        i64::try_from(m.size()).unwrap_or(0),
                        a,
                        mm,
                        c,
                        i64::try_from(m.nlink()).unwrap_or(0),
                    )
                }
                _ => (0, (0, 0), (0, 0), (0, 0), 0),
            };
            g.matches.push(Gmatch {
                name: item,
                uname: Vec::new(),
                sortstrs: Vec::new(),
                size,
                atime,
                mtime,
                ctime,
                links,
                _size,
                _atime,
                _mtime,
                _ctime,
                _links,
            });
        }
    }

    /// zsh's `scanner`.
    fn scanner(&mut self, g: &mut Glob, q: &mut Complist, shortcircuit: usize) {
        if self.errflag() {
            return;
        }
        let errssofar = self.errsfound.get();
        let closure = q.closure;
        if closure != 0 {
            if q.closure == 2 {
                q.closure = 1;
            } else if let Some(next) = q.next.as_deref_mut() {
                self.scanner(g, next, shortcircuit);
                if shortcircuit != 0 && shortcircuit == g.matches.len() {
                    return;
                }
            }
        }
        if q.pat.flags & PAT_PURES != 0 {
            let str_ = q.pat.pure_string().unwrap_or(&[]).to_vec();
            if q.next.is_some() {
                let oppos = g.pathbuf.len();
                if !self.errflag() {
                    let mut add = true;
                    if q.closure != 0 && !g.pathbuf.is_empty() {
                        if str_ == b"." {
                            add = false;
                        } else if str_ == b".." {
                            let root = stat_path(b"/", true);
                            let here = stat_path(&g.pathbuf, true);
                            add = match (root, here) {
                                (Some(r), Some(h)) => r.ino() != h.ino() || r.dev() != h.dev(),
                                _ => true,
                            };
                        }
                    }
                    if add {
                        g.addpath(&str_);
                        if closure == 0 || g.exists(b"", true) {
                            if q.closure != 0 {
                                self.scanner(g, q, shortcircuit);
                            } else if let Some(next) = q.next.as_deref_mut() {
                                self.scanner(g, next, shortcircuit);
                            }
                            if shortcircuit != 0 && shortcircuit == g.matches.len() {
                                return;
                            }
                        }
                        g.pathbuf.truncate(oppos);
                    }
                }
            } else {
                self.glob_insert(g, &str_, false);
            }
            return;
        }
        let dir: Vec<u8> = if g.pathbuf.is_empty() {
            b".".to_vec()
        } else {
            tok::unmetafy(&g.pathbuf)
        };
        let dirs = q.next.is_some();
        use std::os::unix::ffi::OsStrExt;
        let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes(&dir)) else {
            return;
        };
        let mut subdirs: Vec<(Vec<u8>, i32)> = Vec::new();
        let prefix = if g.pathbuf.is_empty() {
            None
        } else {
            Some(g.pathbuf.clone())
        };
        for e in rd.flatten() {
            if self.errflag() {
                break;
            }
            let fname = tok::metafy(e.file_name().as_bytes());
            if !dirs
                && g.colonmod.is_none()
                && (self
                    .glob_pre
                    .as_ref()
                    .is_some_and(|p| !fname.starts_with(p))
                    || self.glob_suf.as_ref().is_some_and(|p| !fname.ends_with(p)))
            {
                continue;
            }
            self.errsfound.set(errssofar);
            if self
                .pattryrefs(&q.pat, &fname, prefix.as_deref(), 0, false)
                .is_none()
            {
                continue;
            }
            if dirs {
                if self.errsfound.get() > errssofar {
                    self.forceerrs.set(self.errsfound.get() - 1);
                    while self.forceerrs.get() >= errssofar {
                        self.errsfound.set(errssofar);
                        if self
                            .pattryrefs(&q.pat, &fname, prefix.as_deref(), 0, false)
                            .is_none()
                        {
                            break;
                        }
                        self.forceerrs.set(self.errsfound.get() - 1);
                    }
                    self.errsfound.set(self.forceerrs.get() + 1);
                    self.forceerrs.set(-1);
                }
                if closure != 0 {
                    match g.statfullpath(&fname, !q.follow) {
                        None => continue,
                        Some(m) if !m.is_dir() => continue,
                        Some(_) => {}
                    }
                }
                subdirs.push((fname, self.errsfound.get()));
            } else {
                self.glob_insert(g, &fname, true);
                if shortcircuit != 0 && shortcircuit == g.matches.len() {
                    return;
                }
            }
        }
        if !subdirs.is_empty() {
            let oppos = g.pathbuf.len();
            for (fname, errs) in subdirs {
                g.addpath(&fname);
                self.errsfound.set(errs);
                if q.closure != 0 {
                    self.scanner(g, q, shortcircuit);
                } else if let Some(next) = q.next.as_deref_mut() {
                    self.scanner(g, next, shortcircuit);
                }
                if shortcircuit != 0 && shortcircuit == g.matches.len() {
                    return;
                }
                g.pathbuf.truncate(oppos);
            }
        }
    }

    /// zsh's `parsecomplist`.
    fn parsecomplist(&mut self, g: &Glob, instr: &[u8]) -> Option<Box<Complist>> {
        let compflags = if g.gf_noglobdots {
            PAT_FILE | PAT_NOGLD
        } else {
            PAT_FILE
        };
        if at(instr, 0) == STAR && at(instr, 1) == STAR {
            let shortglob = !(at(instr, 2) == b'/'
                || (at(instr, 2) == STAR && at(instr, 3) == b'/'))
                && self.isset(GLOBSTARSHORT);
            if at(instr, 2) == b'/' || (at(instr, 2) == STAR && at(instr, 3) == b'/') || shortglob {
                let follow = at(instr, 2) == STAR;
                let skip = (if shortglob { 1 } else { 3 }) + usize::from(follow);
                let next = self.parsecomplist(g, from(instr, skip));
                let Some(next) = next else {
                    self.errflag_set_error();
                    return None;
                };
                let mut pat = Patprog::any();
                pat.flags |= compflags;
                return Some(Box::new(Complist {
                    pat,
                    closure: 1,
                    follow,
                    next: Some(next),
                }));
            }
        }
        let special_inpar = self.pat_file_special[ZPC_INPAR];
        let special_hash = self.pat_file_special[ZPC_HASH];
        if at(instr, 0) == special_inpar {
            let mut s = 0usize;
            if skipparens(INPAR, OUTPAR, instr, &mut s) == 0
                && at(instr, s) == special_hash
                && s >= 2
                && at(instr, s - 2) == b'/'
            {
                let mut end = 0usize;
                let p1 = self.patcompile(from(instr, 1), compflags, Some(&mut end))?;
                let rest = from(instr, 1 + end);
                if at(rest, 0) == b'/' && at(rest, 1) == OUTPAR && at(rest, 2) == POUND {
                    let mut k = 3;
                    let mut pdflag = 0;
                    if at(rest, k) == POUND {
                        pdflag = 1;
                        k += 1;
                    }
                    let nonempty = !(p1.flags & PAT_PURES != 0
                        && p1.pure_string().is_some_and(<[u8]>::is_empty));
                    let closure = if nonempty { 1 + pdflag } else { 0 };
                    let next = self.parsecomplist(g, from(rest, k));
                    return Some(Box::new(Complist {
                        pat: p1,
                        closure,
                        follow: false,
                        next,
                    }));
                }
                self.errflag_set_error();
                return None;
            }
        }
        let mut end = 0usize;
        let p1 = self.patcompile(instr, compflags | PAT_FILET, Some(&mut end))?;
        let rest = from(instr, end);
        if at(rest, 0) == b'/' || rest.is_empty() {
            let ef = at(rest, 0) == b'/';
            let next = if ef {
                self.parsecomplist(g, from(rest, 1))
            } else {
                None
            };
            if ef && next.is_none() {
                return None;
            }
            return Some(Box::new(Complist {
                pat: p1,
                closure: 0,
                follow: false,
                next,
            }));
        }
        self.errflag_set_error();
        None
    }

    /// zsh's `parsepat`.
    fn parsepat(&mut self, g: &mut Glob, s: &[u8]) -> Option<Box<Complist>> {
        self.patcompstart();
        let mut s = s;
        let sp = self.pat_file_special;
        if (at(s, 0) == sp[ZPC_INPAR] && at(s, 1) == sp[ZPC_HASH])
            || (at(s, 0) == sp[ZPC_KSH_AT] && at(s, 1) == INPAR && at(s, 2) == sp[ZPC_HASH])
        {
            let skip = if at(s, 0) == INPAR { 2 } else { 3 };
            let (ok, rest) = self.glob_flags_prefix(from(s, skip));
            if !ok {
                return None;
            }
            s = from(s, s.len() - rest.len());
        }
        if at(s, 0) == b'/' {
            g.pathbuf = b"/".to_vec();
            s = from(s, 1);
        } else {
            g.pathbuf.clear();
        }
        let s = s.to_vec();
        self.parsecomplist(g, &s)
    }

    /// Parse `(#X)` flags at the start of a glob, updating the file-pattern
    /// flags; returns the rest of the string.
    fn glob_flags_prefix<'a>(&mut self, s: &'a [u8]) -> (bool, &'a [u8]) {
        let mut probe = s.to_vec();
        probe.insert(0, POUND);
        probe.insert(0, INPAR);
        // Compile just the flags as a file pattern prefix: the compiler keeps
        // the flags in pat_file_globflags.
        let close = s.iter().position(|&c| c == OUTPAR);
        let Some(close) = close else {
            return (false, s);
        };
        let flags_part = sub(s, 0, close + 1).to_vec();
        let mut text = vec![INPAR, POUND];
        text.extend(flags_part);
        let mut end = 0;
        let _ = self.patcompile(&text, PAT_FILE, Some(&mut end));
        (true, from(s, close + 1))
    }

    /// zsh's `checkglobqual`: 1 for bare qualifiers, 2 for `(#q...)`, with
    /// the index of the opening parenthesis.
    pub(crate) fn checkglobqual(&self, s: &[u8], nobareglob: bool) -> (i32, usize) {
        let sl = s.len();
        if sl == 0 || at(s, sl - 1) != OUTPAR {
            return (0, 0);
        }
        let mut nobareglob = nobareglob;
        let mut paren = 0;
        let mut i = sl as isize - 2;
        while i >= 0 {
            let c = at(s, usize::try_from(i).unwrap_or(0));
            if c == INPAR && paren == 0 {
                break;
            }
            match c {
                OUTPAR => {
                    paren += 1;
                    if !self.zpc_disables[ZPC_BAR] {
                        nobareglob = true;
                    }
                }
                BAR => {
                    if !self.zpc_disables[ZPC_BAR] {
                        nobareglob = true;
                    }
                }
                TILDE => {
                    if self.isset(EXTENDEDGLOB) && !self.zpc_disables[ZPC_TILDE] {
                        nobareglob = true;
                    }
                }
                INPAR => paren -= 1,
                _ => {}
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        let pos = usize::try_from(i.max(0)).unwrap_or(0);
        if i < 0 || at(s, pos) != INPAR {
            return (0, 0);
        }
        let mut ret = 1;
        if self.isset(EXTENDEDGLOB) && !self.zpc_disables[ZPC_HASH] && at(s, pos + 1) == POUND {
            if at(s, pos + 2) != b'q' {
                return (0, 0);
            }
            ret = 2;
        } else if nobareglob {
            return (0, 0);
        }
        (ret, pos)
    }

    /// zsh's `zglob`: glob the word at `idx` of `list`, replacing it with the
    /// matches.
    #[expect(clippy::too_many_lines, reason = "zsh's zglob")]
    pub(crate) fn zglob(&mut self, list: &mut Vec<Vec<u8>>, idx: usize, nountok: bool) -> usize {
        let Some(ostr) = list.get(idx).cloned() else {
            return idx + 1;
        };
        let mut probe = ostr.clone();
        if !self.isset(GLOBOPT) || !self.haswilds(&mut probe) || !self.isset(EXECOPT) {
            if !nountok && let Some(w) = list.get_mut(idx) {
                tok::untokenize(w);
            }
            return idx + 1;
        }
        let mut s = probe.clone();
        let _ = list.remove(idx);
        let mut g = Glob {
            gf_nullglob: self.isset(NULLGLOB),
            gf_markdirs: self.isset(MARKDIRS),
            gf_noglobdots: !self.isset(GLOBDOTS),
            gf_numsort: self.isset(NUMERICGLOBSORT),
            ..Glob::default()
        };
        let mut nobareglob = !self.isset(BAREGLOBQUAL);
        let mut shortcircuit = 0usize;
        let mut first: i64 = 0;
        let mut end: i64 = -1;
        loop {
            if !(!nobareglob || (self.isset(EXTENDEDGLOB) && !self.zpc_disables[ZPC_HASH])) {
                break;
            }
            let (qualsfound, open) = self.checkglobqual(&s, nobareglob);
            if qualsfound == 0 {
                break;
            }
            nobareglob = true;
            let mut sense = 0;
            let mut data: i64 = 0;
            let mut sdata: Option<Vec<u8>> = None;
            let mut newcolonmod = false;
            let mut qs: Vec<u8> = sub(&s, open + 1, s.len() - 1).to_vec();
            s.truncate(open);
            if qualsfound == 2 {
                qs = from(&qs, 2).to_vec();
            }
            for c in &mut qs {
                if *c == DASH {
                    *c = b'-';
                }
            }
            let mut newquals: Vec<Vec<Qual>> = Vec::new();
            let mut cur: Vec<Qual> = Vec::new();
            let mut have_alt = false;
            let (mut g_range, mut g_amc, mut g_units) = (0, 0, 0);
            let mut i = 0usize;
            while i < qs.len() && !newcolonmod {
                let mut func: Option<QualFunc> = None;
                let c = at(&qs, i);
                if c == b',' {
                    i += 1;
                    sense = 0;
                    if g.qualct != 0 {
                        newquals.push(std::mem::take(&mut cur));
                        have_alt = true;
                        g.qualorct += 1;
                        g.qualct = 0;
                    }
                    continue;
                }
                i += 1;
                match c {
                    b':' => {
                        let mut cm = from(&qs, i - 1).to_vec();
                        tok::untokenize(&mut cm);
                        g.colonmod = Some(match g.colonmod.take() {
                            Some(old) => {
                                let mut n = cm;
                                n.extend(old);
                                n
                            }
                            None => cm,
                        });
                        newcolonmod = true;
                    }
                    HAT | b'^' => sense ^= 1,
                    b'-' | DASH => sense ^= 2,
                    b'@' => func = Some(QualFunc::IsLnk),
                    EQUALS | b'=' => func = Some(QualFunc::IsSock),
                    b'p' => func = Some(QualFunc::IsFifo),
                    b'/' => func = Some(QualFunc::IsDir),
                    b'.' => func = Some(QualFunc::IsReg),
                    b'%' => {
                        if at(&qs, i) == b'b' {
                            i += 1;
                            func = Some(QualFunc::IsBlk);
                        } else if at(&qs, i) == b'c' {
                            i += 1;
                            func = Some(QualFunc::IsChr);
                        } else {
                            func = Some(QualFunc::IsDev);
                        }
                    }
                    STAR => func = Some(QualFunc::IsCom),
                    b'R' | b'W' | b'X' | b'A' | b'I' | b'E' | b'r' | b'w' | b'x' | b's' | b'S'
                    | b't' => {
                        func = Some(QualFunc::Flags);
                        data = match c {
                            b'R' => 0o004,
                            b'W' => 0o002,
                            b'X' => 0o001,
                            b'A' => 0o040,
                            b'I' => 0o020,
                            b'E' => 0o010,
                            b'r' => 0o400,
                            b'w' => 0o200,
                            b'x' => 0o100,
                            b's' => 0o4000,
                            b'S' => 0o2000,
                            _ => 0o1000,
                        };
                    }
                    b'd' => {
                        func = Some(QualFunc::Dev);
                        data = self.qgetnum(&qs, &mut i);
                    }
                    b'l' => {
                        func = Some(QualFunc::Nlink);
                        g_amc = -1;
                        self.getrange(&qs, &mut i, g_amc, &mut g_units, &mut g_range, &mut data);
                    }
                    b'U' => {
                        func = Some(QualFunc::Uid);
                        // SAFETY: geteuid has no preconditions.
                        data = i64::from(unsafe { libc::geteuid() });
                    }
                    b'G' => {
                        func = Some(QualFunc::Gid);
                        // SAFETY: getegid has no preconditions.
                        data = i64::from(unsafe { libc::getegid() });
                    }
                    b'u' | b'g' => {
                        func = Some(if c == b'u' {
                            QualFunc::Uid
                        } else {
                            QualFunc::Gid
                        });
                        if at(&qs, i).is_ascii_digit() {
                            data = self.qgetnum(&qs, &mut i);
                        } else {
                            let (tt, arglen) = self.get_strarg(&qs, i);
                            if tt >= qs.len() {
                                self.zerr(&format!(
                                    "missing delimiter for '{}' glob qualifier",
                                    char::from(c)
                                ));
                                data = 0;
                            } else {
                                let nm = sub(&qs, i + arglen, tt).to_vec();
                                let id = if c == b'u' {
                                    self.user_id(&nm)
                                } else {
                                    self.group_id(&nm)
                                };
                                match id {
                                    Some(v) => data = i64::from(v),
                                    None => {
                                        if c == b'u' {
                                            self.zerr(&format!(
                                                "unknown username '{}'",
                                                lossy(&nm)
                                            ));
                                        } else {
                                            self.zerr("unknown group");
                                        }
                                        data = 0;
                                    }
                                }
                                i = tt + arglen;
                            }
                        }
                    }
                    b'f' => {
                        func = Some(QualFunc::ModeFlags);
                        data = self.qgetmodespec(&qs, &mut i);
                    }
                    b'F' => func = Some(QualFunc::NonEmptyDir),
                    b'M' => {
                        g.gf_markdirs = sense & 1 == 0;
                        if g.gf_markdirs {
                            g.gf_follow = sense & 2 != 0;
                        }
                    }
                    b'T' => {
                        g.gf_listtypes = sense & 1 == 0;
                        if g.gf_listtypes {
                            g.gf_follow = sense & 2 != 0;
                        }
                    }
                    b'N' => g.gf_nullglob = sense & 1 == 0,
                    b'D' => g.gf_noglobdots = sense & 1 != 0,
                    b'n' => g.gf_numsort = sense & 1 == 0,
                    b'Y' => {
                        shortcircuit = usize::from(sense & 1 == 0);
                        if shortcircuit != 0 {
                            data = self.qgetnum(&qs, &mut i);
                            shortcircuit = usize::try_from(data).unwrap_or(0);
                        }
                    }
                    b'a' | b'm' | b'c' => {
                        g_amc = match c {
                            b'a' => 0,
                            b'm' => 1,
                            _ => 2,
                        };
                        func = Some(QualFunc::Time);
                        self.getrange(&qs, &mut i, g_amc, &mut g_units, &mut g_range, &mut data);
                    }
                    b'L' => {
                        func = Some(QualFunc::Size);
                        g_amc = -1;
                        g_units = TT_BYTES;
                        match at(&qs, i) {
                            b'p' | b'P' => {
                                g_units = TT_POSIX_BLOCKS;
                                i += 1;
                            }
                            b'k' | b'K' => {
                                g_units = TT_KILOBYTES;
                                i += 1;
                            }
                            b'm' | b'M' => {
                                g_units = TT_MEGABYTES;
                                i += 1;
                            }
                            b'g' | b'G' => {
                                g_units = TT_GIGABYTES;
                                i += 1;
                            }
                            b't' | b'T' => {
                                g_units = TT_TERABYTES;
                                i += 1;
                            }
                            _ => {}
                        }
                        self.getrange(&qs, &mut i, g_amc, &mut g_units, &mut g_range, &mut data);
                    }
                    b'o' | b'O' => {
                        if g.gf_sortlist.len() == 12 {
                            self.zerr("too many glob sort specifiers");
                            return self.restore_word(list, idx, ostr, nountok, false);
                        }
                        let mut send = i + 1;
                        let mut exec = None;
                        let t = match at(&qs, i) {
                            b'n' => GS_NAME,
                            b'L' => GS_SIZE,
                            b'l' => GS_LINKS,
                            b'a' => GS_ATIME,
                            b'm' => GS_MTIME,
                            b'c' => GS_CTIME,
                            b'd' => GS_DEPTH,
                            b'N' => GS_NONE,
                            b'e' | b'+' => {
                                match self.glob_exec_string(&qs, &mut send) {
                                    Some(x) => exec = Some(x),
                                    None => {
                                        return self.restore_word(list, idx, ostr, nountok, false);
                                    }
                                }
                                GS_EXEC
                            }
                            _ => {
                                self.zerr("unknown sort specifier");
                                return self.restore_word(list, idx, ostr, nountok, false);
                            }
                        };
                        let t = if sense & 2 != 0
                            && t & (GS_SIZE | GS_ATIME | GS_MTIME | GS_CTIME | GS_LINKS) != 0
                        {
                            t << GS_SHIFT
                        } else {
                            t
                        };
                        if t != GS_EXEC && g.gf_sorts & t != 0 {
                            self.zerr("doubled sort specifier");
                            return self.restore_word(list, idx, ostr, nountok, false);
                        }
                        g.gf_sorts |= t;
                        let desc = ((sense & 1) != 0) ^ (c == b'O');
                        g.gf_sortlist.push(GlobSort {
                            tp: t | if desc { GS_DESC } else { 0 },
                            exec,
                        });
                        i = send;
                    }
                    b'+' | b'e' => match self.glob_exec_string(&qs, &mut i) {
                        None => data = 0,
                        Some(tt) => {
                            func = Some(QualFunc::ShEval);
                            sdata = Some(tt);
                        }
                    },
                    b'[' | INBRACK => {
                        let os = i - 1;
                        let mut v = crate::params::Value::new(crate::params::PmRef::Argv);
                        v.isarr = crate::params::SCANPM_WANTVALS;
                        v.end = -1;
                        let mut qbuf = qs.clone();
                        let mut p = os;
                        if self.getindex(&mut qbuf, &mut p, &mut v, 0) != 0 || p == os {
                            self.zerr("invalid subscript");
                            return self.restore_word(list, idx, ostr, nountok, false);
                        }
                        first = v.start;
                        end = v.end;
                        i = p;
                    }
                    b'P' => {
                        if let Some(tt) = self.glob_exec_string(&qs, &mut i) {
                            let words = if sense & 1 != 0 {
                                &mut g.gf_post_words
                            } else {
                                &mut g.gf_pre_words
                            };
                            words.get_or_insert_with(Vec::new).push(tt);
                        }
                    }
                    _ => {
                        let mut shown = vec![c];
                        tok::untokenize(&mut shown);
                        self.zerr(&format!(
                            "unknown file attribute: {}",
                            char::from(at(&shown, 0))
                        ));
                        return self.restore_word(list, idx, ostr, nountok, false);
                    }
                }
                if let Some(f) = func {
                    cur.push(Qual {
                        func: f,
                        data,
                        sense,
                        amc: g_amc,
                        range: g_range,
                        units: g_units,
                        sdata: sdata.take(),
                    });
                    g.qualct += 1;
                }
                if self.errflag() {
                    return self.restore_word(list, idx, ostr, nountok, false);
                }
            }
            if !cur.is_empty() || have_alt {
                newquals.push(cur);
            }
            // zsh only builds a list when a test was added.
            if newquals.iter().any(|a| !a.is_empty()) || have_alt {
                if g.quals.is_empty() {
                    g.quals = newquals;
                } else {
                    let mut merged = Vec::new();
                    for n in &newquals {
                        for o in &g.quals {
                            let mut combo = n.clone();
                            combo.extend(o.iter().cloned());
                            merged.push(combo);
                        }
                    }
                    g.quals = merged;
                }
            }
        }
        let q = self.parsepat(&mut g, &s);
        let Some(mut q) = q.filter(|_| !self.errflag()) else {
            if !self.isset(BADPATTERN) {
                let mut w = ostr.clone();
                if !nountok {
                    tok::untokenize(&mut w);
                }
                list.insert(idx, w);
                return idx + 1;
            }
            self.errflag_clear_error();
            let mut shown = ostr.clone();
            tok::untokenize(&mut shown);
            self.zerr(&format!("bad pattern: {}", lossy(&shown)));
            return idx;
        };
        if g.gf_sortlist.is_empty() {
            let t = if shortcircuit != 0 { GS_NONE } else { GS_NAME };
            g.gf_sortlist.push(GlobSort { tp: t, exec: None });
            g.gf_sorts = t;
        }
        self.errsfound.set(0);
        self.forceerrs.set(-1);
        let saved_pathbuf = std::mem::take(&mut g.pathbuf);
        g.pathbuf = saved_pathbuf;
        self.scanner(&mut g, &mut q, shortcircuit);
        if !g.matches.is_empty() {
            self.badcshglob |= 2;
        } else if !g.gf_nullglob {
            if self.isset(CSHNULLGLOB) {
                self.badcshglob |= 1;
            } else if self.isset(NOMATCH) {
                let mut shown = ostr.clone();
                tok::untokenize(&mut shown);
                self.zerr(&format!("no matches found: {}", lossy(&shown)));
                return idx;
            } else {
                let mut w = ostr.clone();
                tok::untokenize(&mut w);
                g.matches.push(Gmatch {
                    name: w,
                    uname: Vec::new(),
                    sortstrs: Vec::new(),
                    size: 0,
                    atime: (0, 0),
                    mtime: (0, 0),
                    ctime: (0, 0),
                    links: 0,
                    _size: 0,
                    _atime: (0, 0),
                    _mtime: (0, 0),
                    _ctime: (0, 0),
                    _links: 0,
                });
            }
        }
        let first_sort_none = g.gf_sortlist.first().is_some_and(|s| s.tp & GS_NONE != 0);
        if !first_sort_none {
            let execs: Vec<Vec<u8>> = g
                .gf_sortlist
                .iter()
                .filter(|s| s.tp & GS_EXEC != 0)
                .map(|s| s.exec.clone().unwrap_or_default())
                .collect();
            for code in &execs {
                let ef = self.errflag.get();
                let lv = self.lastval();
                for mi in 0..g.matches.len() {
                    let name = g
                        .matches
                        .get(mi)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    let _ = self.setsparam(b"REPLY", name.clone());
                    let ok = self.execstring_ctx(code, "globsort");
                    let val = if ok && !self.errflag() {
                        self.getsparam(b"REPLY").unwrap_or_default()
                    } else {
                        name
                    };
                    if let Some(m) = g.matches.get_mut(mi) {
                        m.sortstrs.push(val);
                    }
                }
                self.errflag
                    .set(ef | (self.errflag.get() & crate::shell::ERRFLAG_INT));
                self.set_lastval(lv);
            }
            for m in &mut g.matches {
                m.uname = tok::unmetafy(&m.name);
            }
            let sortlist = g.gf_sortlist.clone();
            let numsort = g.gf_numsort;
            let mut matches = std::mem::take(&mut g.matches);
            matches.sort_by(|a, b| self.gmatchcmp(a, b, &sortlist, numsort).cmp(&0));
            g.matches = matches;
        }
        let matchct = i64::try_from(g.matches.len()).unwrap_or(0);
        if first < 0 {
            first += matchct;
            if first < 0 {
                first = 0;
            }
        }
        if end < 0 {
            end += matchct + 1;
        } else if end > matchct {
            end = matchct;
        }
        let mut out: Vec<Vec<u8>> = Vec::new();
        let count = end - first;
        if count > 0 {
            let f = usize::try_from(first).unwrap_or(0);
            let n = usize::try_from(count).unwrap_or(0);
            // zsh sorts in reverse and inserts after the node, so the list
            // comes out in forward order; GS_NONE is never reversed.
            let chosen: Vec<&Gmatch> = if first_sort_none {
                g.matches
                    .iter()
                    .rev()
                    .skip(f)
                    .take(n)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            } else {
                g.matches.iter().skip(f).take(n).collect()
            };
            for m in chosen {
                self.insert_glob_match(&g, &mut out, m.name.clone());
            }
        } else if self.badcshglob == 0
            && !self.isset(NOMATCH)
            && matchct == 1
            && let Some(m) = g.matches.last()
        {
            self.insert_glob_match(&g, &mut out, m.name.clone());
        }
        let n = out.len();
        for (k, w) in out.into_iter().enumerate() {
            list.insert(idx + k, w);
        }
        idx + n
    }

    fn restore_word(
        &mut self,
        list: &mut Vec<Vec<u8>>,
        idx: usize,
        ostr: Vec<u8>,
        _nountok: bool,
        _keep: bool,
    ) -> usize {
        let _ = (list, ostr);
        idx
    }

    fn insert_glob_match(&self, g: &Glob, out: &mut Vec<Vec<u8>>, data: Vec<u8>) {
        if let Some(pre) = &g.gf_pre_words {
            out.extend(pre.iter().cloned());
        }
        out.push(data);
        if let Some(post) = &g.gf_post_words {
            out.extend(post.iter().cloned());
        }
    }

    fn gmatchcmp(&self, a: &Gmatch, b: &Gmatch, sortlist: &[GlobSort], numsort: bool) -> i32 {
        let flags = if numsort {
            crate::sort::SORTIT_NUMERICALLY
        } else {
            0
        };
        let mut exec_i = 0usize;
        for s in sortlist {
            let r: i64 = match s.tp & !GS_DESC {
                GS_NAME => i64::from(self.zstrcmp(&b.uname, &a.uname, flags)),
                GS_DEPTH => {
                    let an = &a.name;
                    let bn = &b.name;
                    let mut k = 0;
                    while k < an.len() && at(an, k) == at(bn, k) {
                        k += 1;
                    }
                    if (k >= an.len() || k >= bn.len()) && k > 0 && at(an, k - 1) == b'/' {
                        k -= 1;
                    }
                    let slash = |s: &[u8]| -> i64 {
                        let mut j = k;
                        if j < s.len() {
                            while j + 1 < s.len() {
                                if at(s, j) == b'/' {
                                    return 1;
                                }
                                j += 1;
                            }
                        }
                        0
                    };
                    slash(an) - slash(bn)
                }
                GS_EXEC => {
                    let ra = a.sortstrs.get(exec_i).cloned().unwrap_or_default();
                    let rb = b.sortstrs.get(exec_i).cloned().unwrap_or_default();
                    exec_i += 1;
                    i64::from(self.zstrcmp(&rb, &ra, flags))
                }
                GS_SIZE => b.size - a.size,
                GS_ATIME => time_cmp(a.atime, b.atime),
                GS_MTIME => time_cmp(a.mtime, b.mtime),
                GS_CTIME => time_cmp(a.ctime, b.ctime),
                GS_LINKS => b.links - a.links,
                GS__SIZE => b._size - a._size,
                GS__ATIME => time_cmp(a._atime, b._atime),
                GS__MTIME => time_cmp(a._mtime, b._mtime),
                GS__CTIME => time_cmp(a._ctime, b._ctime),
                GS__LINKS => b._links - a._links,
                _ => 0,
            };
            if r != 0 {
                return if s.tp & GS_DESC != 0 {
                    if r < 0 { 1 } else { -1 }
                } else if r > 0 {
                    1
                } else {
                    -1
                };
            }
        }
        0
    }

    fn qgetnum(&mut self, s: &[u8], i: &mut usize) -> i64 {
        if !at(s, *i).is_ascii_digit() {
            self.zerr("number expected");
            return 0;
        }
        let mut v: i64 = 0;
        while at(s, *i).is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add(i64::from(at(s, *i) - b'0'));
            *i += 1;
        }
        v
    }

    fn getrange(
        &mut self,
        s: &[u8],
        i: &mut usize,
        amc: i32,
        units: &mut i32,
        range: &mut i32,
        data: &mut i64,
    ) {
        if amc >= 0 {
            *units = TT_DAYS;
            match at(s, *i) {
                b'h' => {
                    *units = TT_HOURS;
                    *i += 1;
                }
                b'm' => {
                    *units = TT_MINS;
                    *i += 1;
                }
                b'w' => {
                    *units = TT_WEEKS;
                    *i += 1;
                }
                b'M' => {
                    *units = TT_MONTHS;
                    *i += 1;
                }
                b's' => {
                    *units = TT_SECONDS;
                    *i += 1;
                }
                b'd' => *i += 1,
                _ => {}
            }
        }
        *range = match at(s, *i) {
            b'+' => 1,
            b'-' | DASH => -1,
            _ => 0,
        };
        if *range != 0 {
            *i += 1;
        }
        *data = self.qgetnum(s, i);
    }

    /// zsh's `qgetmodespec`.
    fn qgetmodespec(&mut self, s: &[u8], i: &mut usize) -> i64 {
        let (mut yes, mut no): (i64, i64) = (0, 0);
        let mut p = *i;
        let c0 = at(s, p);
        let end;
        if matches!(c0, b'=' | EQUALS | b'+' | b'-' | b'?' | QUEST) || (b'0'..=b'7').contains(&c0) {
            end = 0u8;
        } else {
            end = match c0 {
                b'<' => b'>',
                b'[' => b']',
                b'{' => b'}',
                tok::INANG => tok::OUTANG,
                INBRACK => tok::OUTBRACK,
                INBRACE => OUTBRACE,
                other => other,
            };
            p += 1;
        }
        loop {
            let mut mask: i64 = 0;
            let mut c = at(s, p);
            while matches!(c, b'u' | b'g' | b'o' | b'a') && end != 0 {
                mask |= match c {
                    b'o' => 0o1007,
                    b'g' => 0o2070,
                    b'u' => 0o4700,
                    _ => 0o7777,
                };
                p += 1;
                c = at(s, p);
            }
            let how = if c == b'+' || c == b'-' { c } else { b'=' };
            if matches!(c, b'+' | b'-' | b'=' | EQUALS) {
                p += 1;
            }
            let mut val: i64 = 0;
            if mask != 0 {
                loop {
                    c = at(s, p);
                    p += 1;
                    if c == b',' || c == end {
                        break;
                    }
                    match c {
                        b'x' => val |= 0o111,
                        b'w' => val |= 0o222,
                        b'r' => val |= 0o444,
                        b's' => val |= 0o6000,
                        b't' => val |= 0o1000,
                        b'0'..=b'7' => {
                            let t = i64::from(c - b'0');
                            val |= t | (t << 3) | (t << 6);
                        }
                        _ => {
                            self.zerr("invalid mode specification");
                            return 0;
                        }
                    }
                }
                if how == b'=' || how == b'+' {
                    yes |= val & mask;
                    val = !val;
                }
                if how == b'=' || how == b'-' {
                    no |= val & mask;
                }
            } else if !(end != 0 && c == end) && c != b',' && c != 0 {
                let mut t: i64 = 0o7777;
                loop {
                    c = at(s, p);
                    if c == b'?' || c == QUEST {
                        t = (t << 3) | 7;
                        val <<= 3;
                    } else if (b'0'..=b'7').contains(&c) {
                        t <<= 3;
                        val = (val << 3) | i64::from(c - b'0');
                    } else {
                        break;
                    }
                    p += 1;
                }
                if end != 0 && c != end && c != b',' {
                    self.zerr("invalid mode specification");
                    return 0;
                }
                if how == b'=' {
                    yes = (yes & !t) | val;
                    no = (no & !t) | (!val & !t);
                } else if how == b'+' {
                    yes |= val;
                } else {
                    no |= val;
                }
            } else {
                self.zerr("invalid mode specification");
                return 0;
            }
            if !(end != 0 && c != end) {
                break;
            }
        }
        *i = p;
        (yes & 0o7777) | ((no & 0o7777) << 12)
    }

    /// zsh's `glob_exec_string`: the code after `e`, `P` or `+` at `*sp`.
    fn glob_exec_string(&mut self, s: &[u8], sp: &mut usize) -> Option<Vec<u8>> {
        let start = *sp;
        let plus_q = start > 0 && at(s, start - 1) == b'+';
        let (tt, plus) = if plus_q {
            let tt = self.itype_end(s, start, crate::utils::IIDENT, false);
            if tt == start {
                self.zerr("missing identifier after `+'");
                return None;
            }
            (tt, 0)
        } else {
            let (tt, arglen) = self.get_strarg(s, start);
            if tt >= s.len() {
                self.zerr("missing end of string");
                return None;
            }
            (tt, arglen)
        };
        let mut sdata = sub(s, start + plus, tt).to_vec();
        tok::untokenize(&mut sdata);
        *sp = if tt < s.len() { tt + plus } else { tt };
        Some(sdata)
    }

    fn user_id(&self, name: &[u8]) -> Option<u32> {
        let c = std::ffi::CString::new(tok::unmetafy(name)).ok()?;
        // SAFETY: c is NUL-terminated; getpwnam returns a pointer to static
        // storage or NULL.
        let pw = unsafe { libc::getpwnam(c.as_ptr()) };
        if pw.is_null() {
            return None;
        }
        // SAFETY: pw is non-null and points to a passwd record.
        Some(unsafe { (*pw).pw_uid })
    }

    fn group_id(&self, name: &[u8]) -> Option<u32> {
        let c = std::ffi::CString::new(tok::unmetafy(name)).ok()?;
        // SAFETY: c is NUL-terminated; getgrnam returns static storage or NULL.
        let gr = unsafe { libc::getgrnam(c.as_ptr()) };
        if gr.is_null() {
            return None;
        }
        // SAFETY: gr is non-null and points to a group record.
        Some(unsafe { (*gr).gr_gid })
    }

    /// zsh's `hasbraces`. May rewrite tokens it decides are literal.
    pub(crate) fn hasbraces(&self, s: &mut [u8]) -> bool {
        if self.isset(BRACECCL) {
            let mut bc = 0;
            let mut i = 0;
            while i < s.len() {
                let c = at(s, i);
                if c == INBRACE {
                    if bc == 0 && at(s, i + 1) == OUTBRACE {
                        if let Some(x) = s.get_mut(i) {
                            *x = b'{';
                        }
                        i += 1;
                        if let Some(x) = s.get_mut(i) {
                            *x = b'}';
                        }
                    } else {
                        bc += 1;
                    }
                } else if c == OUTBRACE {
                    if bc == 0 {
                        if let Some(x) = s.get_mut(i) {
                            *x = b'}';
                        }
                    } else {
                        bc -= 1;
                        if bc == 0 {
                            return true;
                        }
                    }
                }
                i += 1;
            }
            return false;
        }
        let (mut lbr, mut mbr, mut comma): (Option<usize>, Option<usize>, Option<usize>) =
            (None, None, None);
        let mut i = 0usize;
        loop {
            let c = if i < s.len() { at(s, i) } else { 0 };
            i += 1;
            match c {
                INBRACE => {
                    if lbr.is_none() {
                        if bracechardots(self, s, i - 1).is_some() {
                            return true;
                        }
                        lbr = Some(i - 1);
                        let mut j = i;
                        if matches!(at(s, j), b'-' | DASH) {
                            j += 1;
                        }
                        while at(s, j).is_ascii_digit() {
                            j += 1;
                        }
                        if at(s, j) == b'.' && at(s, j + 1) == b'.' {
                            j += 2;
                            if matches!(at(s, j), b'-' | DASH) {
                                j += 1;
                            }
                            while at(s, j).is_ascii_digit() {
                                j += 1;
                            }
                            let l = i - 1;
                            if at(s, j) == OUTBRACE
                                && (at(s, l + 1).is_ascii_digit()
                                    || (j > 0 && at(s, j - 1).is_ascii_digit()))
                            {
                                return true;
                            } else if at(s, j) == b'.' && at(s, j + 1) == b'.' {
                                j += 2;
                                if matches!(at(s, j), b'-' | DASH) {
                                    j += 1;
                                }
                                while at(s, j).is_ascii_digit() {
                                    j += 1;
                                }
                                if at(s, j) == OUTBRACE
                                    && (at(s, l + 1).is_ascii_digit()
                                        || (j > 0 && at(s, j - 1).is_ascii_digit()))
                                {
                                    return true;
                                }
                            }
                        }
                        i = j;
                    } else {
                        let so = i - 1;
                        let mut j = so;
                        if skipparens(INBRACE, OUTBRACE, s, &mut j) != 0 {
                            if let Some(l) = lbr
                                && let Some(x) = s.get_mut(l)
                            {
                                *x = b'{';
                            }
                            if let Some(x) = s.get_mut(so) {
                                *x = b'{';
                            }
                            i = j;
                            if let Some(cm) = comma {
                                i = cm;
                            }
                            if let Some(m) = mbr
                                && m < i
                            {
                                i = m;
                            }
                            lbr = None;
                            mbr = None;
                            comma = None;
                        } else {
                            if mbr.is_none() {
                                mbr = Some(so);
                            }
                            i = j;
                        }
                    }
                }
                OUTBRACE => {
                    if lbr.is_none() {
                        if let Some(x) = s.get_mut(i - 1) {
                            *x = b'}';
                        }
                    } else if comma.is_some() {
                        return true;
                    } else {
                        if let Some(l) = lbr
                            && let Some(x) = s.get_mut(l)
                        {
                            *x = b'{';
                        }
                        if let Some(x) = s.get_mut(i - 1) {
                            *x = b'}';
                        }
                        if let Some(m) = mbr {
                            i = m;
                        }
                        mbr = None;
                        lbr = None;
                    }
                }
                COMMA => {
                    if lbr.is_none() {
                        if let Some(x) = s.get_mut(i - 1) {
                            *x = b',';
                        }
                    } else if comma.is_none() {
                        comma = Some(i - 1);
                    }
                }
                0 if i > s.len() => {
                    if let Some(l) = lbr
                        && let Some(x) = s.get_mut(l)
                    {
                        *x = b'{';
                    }
                    if mbr.is_none() && comma.is_none() {
                        return false;
                    }
                    let mut k = usize::MAX;
                    if let Some(cm) = comma {
                        k = cm;
                    }
                    if let Some(m) = mbr
                        && m < k
                    {
                        k = m;
                    }
                    i = k;
                    lbr = None;
                    mbr = None;
                    comma = None;
                }
                _ => {}
            }
        }
    }

    /// zsh's `xpandbraces`: expand the braces of `list[idx]` in place,
    /// returning the index of the first word produced.
    #[expect(clippy::too_many_lines, reason = "zsh's xpandbraces")]
    pub(crate) fn xpandbraces(&mut self, list: &mut Vec<Vec<u8>>, idx: usize) -> usize {
        let Some(str3) = list.get(idx).cloned() else {
            return idx;
        };
        let Some(lb) = str3.iter().position(|&c| c == INBRACE) else {
            return idx;
        };
        let mut bc = 0;
        let (mut comma, mut dotdot) = (0, 0);
        let mut str2 = lb;
        while str2 < str3.len() {
            let c = at(&str3, str2);
            if c == INBRACE {
                bc += 1;
            } else if c == OUTBRACE {
                bc -= 1;
                if bc == 0 {
                    break;
                }
            } else if bc == 1 {
                if c == COMMA {
                    comma += 1;
                } else if c == b'.' && at(&str3, str2 + 1) == b'.' {
                    dotdot += 1;
                    str2 += 1;
                }
            }
            str2 += 1;
        }
        let prefix = sub(&str3, 0, lb).to_vec();
        let suffix = from(&str3, str2 + 1).to_vec();
        if comma == 0 && dotdot != 0 {
            if let Some((mut cstart, mut cend)) = bracechardots(self, &str3, lb) {
                let mut rev = false;
                if cend < cstart {
                    std::mem::swap(&mut cstart, &mut cend);
                    rev = true;
                }
                let _ = list.remove(idx);
                let mut words = Vec::new();
                let mut c = cend;
                loop {
                    let (nc, _) = self.wcs_nicechar(c);
                    let mut w = prefix.clone();
                    w.extend(nc);
                    w.extend_from_slice(&suffix);
                    words.push(w);
                    if c <= cstart {
                        break;
                    }
                    c -= 1;
                }
                if !rev {
                    words.reverse();
                }
                let n = words.len();
                for (k, w) in words.into_iter().enumerate() {
                    list.insert(idx + k, w);
                }
                let _ = n;
                return idx;
            }
            let body = sub(&str3, lb + 1, str2).to_vec();
            let (rstart, used1) = crate::utils::zstrtol(&body, 10);
            let dots = used1;
            let mut err = dots == 0 || at(&body, dots) != b'.' || at(&body, dots + 1) != b'.';
            let mut rend = 0i64;
            let mut rincr = 1i64;
            let wid1 = dots;
            let mut wid2 = body.len().saturating_sub(dots + 2);
            let mut wid3 = 0usize;
            let mut dots2: Option<usize> = None;
            if !err {
                let (v, used2) = crate::utils::zstrtol(from(&body, dots + 2), 10);
                rend = v;
                let p = dots + 2 + used2;
                if used2 == 0 {
                    err = true;
                }
                if p != body.len() {
                    wid2 = p - dots - 2;
                    dots2 = Some(p);
                    if dotdot == 2 && at(&body, p) == b'.' && at(&body, p + 1) == b'.' {
                        let (inc, used3) = crate::utils::zstrtol(from(&body, p + 2), 10);
                        rincr = inc;
                        let p3 = p + 2 + used3;
                        wid3 = p3 - p - 2;
                        if p3 != body.len() || rincr == 0 {
                            err = true;
                        }
                    } else {
                        err = true;
                    }
                }
            }
            if !err {
                let is_dash = |c: u8| c == b'-' || c == DASH;
                let minw =
                    if at(&body, 0) == b'0' || (is_dash(at(&body, 0)) && at(&body, 1) == b'0') {
                        wid1
                    } else if at(&body, dots + 2) == b'0'
                        || (is_dash(at(&body, dots + 2)) && at(&body, dots + 3) == b'0')
                    {
                        wid2
                    } else if dots2.is_some_and(|d| {
                        at(&body, d + 2) == b'0'
                            || (is_dash(at(&body, d + 2)) && at(&body, d + 3) == b'0')
                    }) {
                        wid3
                    } else {
                        0
                    };
                let mut rev = false;
                let mut rstart = rstart;
                if rincr < 0 {
                    rincr = -rincr;
                    rev = !rev;
                }
                if rstart > rend {
                    std::mem::swap(&mut rstart, &mut rend);
                    rev = !rev;
                } else if rincr > 1 {
                    rend -= (rend - rstart) % rincr;
                }
                let _ = list.remove(idx);
                let mut words = Vec::new();
                let mut v = rend;
                while v >= rstart {
                    let num = if v < 0 {
                        format!("-{:0>w$}", v.unsigned_abs(), w = minw.saturating_sub(1))
                    } else {
                        format!("{v:0>minw$}")
                    };
                    let mut w = prefix.clone();
                    w.extend(num.bytes());
                    w.extend_from_slice(&suffix);
                    words.push(w);
                    v -= rincr;
                }
                if !rev {
                    words.reverse();
                }
                for (k, w) in words.into_iter().enumerate() {
                    list.insert(idx + k, w);
                }
                return idx;
            }
        }
        if comma == 0 && self.isset(BRACECCL) {
            let _ = list.remove(idx);
            let mut ccl = [false; 256];
            let mut p = lb + 1;
            let mut lastch: i32 = -1;
            while p < str2 {
                let mut c1 = at(&str3, p);
                p += 1;
                if tok::is_tok(c1) {
                    c1 = tok::detok(c1);
                }
                if c1 == META {
                    c1 = at(&str3, p) ^ 32;
                    p += 1;
                }
                let mut c2 = at(&str3, p);
                if tok::is_tok(c2) {
                    c2 = tok::detok(c2);
                }
                if c2 == META {
                    c2 = at(&str3, p + 1) ^ 32;
                }
                if c1 == b'-' && lastch >= 0 && p < str2 && lastch <= i32::from(c2) {
                    while lastch < i32::from(c2) {
                        if let Some(x) = ccl.get_mut(usize::try_from(lastch).unwrap_or(0)) {
                            *x = true;
                        }
                        lastch += 1;
                    }
                    lastch = -1;
                } else {
                    lastch = i32::from(c1);
                    if let Some(slot) = ccl.get_mut(usize::from(c1)) {
                        *slot = true;
                    }
                }
            }
            let mut k = 0;
            for c in 0..=255u8 {
                if ccl.get(usize::from(c)).copied().unwrap_or(false) {
                    let mut w = prefix.clone();
                    if tok::is_meta(c) {
                        w.push(META);
                        w.push(c ^ 32);
                    } else {
                        w.push(c);
                    }
                    w.extend_from_slice(&suffix);
                    list.insert(idx + k, w);
                    k += 1;
                }
            }
            return idx;
        }
        let _ = list.remove(idx);
        let mut words = Vec::new();
        let mut s = lb + 1;
        loop {
            let s4 = s;
            let mut cnt = 0;
            while cnt != 0 || (at(&str3, s) != COMMA && at(&str3, s) != OUTBRACE) {
                if s >= str3.len() {
                    break;
                }
                if at(&str3, s) == INBRACE {
                    cnt += 1;
                } else if at(&str3, s) == OUTBRACE {
                    cnt -= 1;
                }
                s += 1;
            }
            let mut w = prefix.clone();
            w.extend_from_slice(sub(&str3, s4, s));
            w.extend_from_slice(&suffix);
            words.push(w);
            if at(&str3, s) != OUTBRACE && s < str3.len() {
                s += 1;
            } else {
                break;
            }
        }
        for (k, w) in words.into_iter().enumerate() {
            list.insert(idx + k, w);
        }
        idx
    }

    /// zsh's `matchpat`.
    pub(crate) fn matchpat(&mut self, a: &[u8], b: &[u8]) -> bool {
        match self.patcompile(b, PAT_STATIC, None) {
            None => {
                self.zerr(&format!("bad pattern: {}", lossy(b)));
                false
            }
            Some(p) => self.pattry(&p, a),
        }
    }

    // --------------------------------------------------------------------
    // ${x#pat} and friends.
    // --------------------------------------------------------------------

    /// zsh's `compgetmatch`.
    fn compgetmatch(
        &mut self,
        pat: &[u8],
        flp: &mut i32,
        replstr: &mut Option<Vec<u8>>,
    ) -> Option<Patprog> {
        let mut patflags = PAT_SCAN | PAT_NOANCH | if replstr.is_some() { 0 } else { PAT_STATIC };
        if *flp & SUB_ALL != 0 || (*flp & SUB_END != 0 && *flp & SUB_SUBSTR == 0) {
            patflags &= !PAT_NOANCH;
        }
        let Some(p) = self.patcompile(pat, patflags, None) else {
            self.zerr(&format!("bad pattern: {}", lossy(pat)));
            return None;
        };
        if let Some(r) = replstr.as_mut() {
            if p.patnpar != 0 || p.globend & GF_MATCHREF != 0 {
                *flp |= SUB_DOSUBST;
            } else {
                *r = self.singsub(r);
                tok::untokenize(r);
            }
        }
        Some(p)
    }

    /// zsh's `getmatch`: `false` means the pattern was bad.
    pub(crate) fn getmatch(
        &mut self,
        sp: &mut Vec<u8>,
        pat: &[u8],
        fl: i32,
        n: i32,
        replstr: Option<Vec<u8>>,
    ) -> bool {
        let mut fl = fl;
        let mut replstr = replstr;
        let Some(mut p) = self.compgetmatch(pat, &mut fl, &mut replstr) else {
            return false;
        };
        let _ = self.igetmatch(sp, &mut p, fl, n, replstr.as_deref(), None);
        true
    }

    /// zsh's `getmatcharr`.
    pub(crate) fn getmatcharr(
        &mut self,
        ap: &mut Vec<Vec<u8>>,
        pat: &[u8],
        fl: i32,
        n: i32,
        replstr: Option<Vec<u8>>,
    ) {
        let mut fl = fl;
        let mut replstr = replstr;
        let Some(mut p) = self.compgetmatch(pat, &mut fl, &mut replstr) else {
            return;
        };
        let old = std::mem::take(ap);
        for mut e in old {
            if self.igetmatch(&mut e, &mut p, fl, n, replstr.as_deref(), None) {
                ap.push(e);
            }
        }
    }

    /// zsh's `getmatchlist`: the (start, end) byte ranges of every match.
    pub(crate) fn getmatchlist(&mut self, s: &[u8], p: &mut Patprog) -> Vec<(usize, usize)> {
        let mut sp = s.to_vec();
        let mut out = Vec::new();
        let _ = self.igetmatch(
            &mut sp,
            p,
            SUB_LONG | SUB_GLOBAL | SUB_SUBSTR | SUB_LIST,
            0,
            None,
            Some(&mut out),
        );
        out.into_iter().map(|(b, e, _)| (b, e)).collect()
    }

    /// Try `p` on `u[t..t+len]` (unmetafied) with `offset` characters before
    /// it: zsh's `pattrylen` with a `patstralloc`. Returns the unmetafied
    /// length of the match.
    fn pattrylen(
        &mut self,
        p: &Patprog,
        u: &[u8],
        t: usize,
        len: usize,
        offset: i64,
    ) -> Option<usize> {
        let piece = tok::metafy(sub(u, t, t + len));
        let info = self.pattryrefs(p, &piece, None, offset, false)?;
        let matched_meta = info.len.min(piece.len());
        Some(tok::unmetafy(sub(&piece, 0, matched_meta)).len())
    }

    /// zsh's `get_match_ret`: `b` and `e` are unmetafied offsets.
    #[expect(clippy::too_many_arguments, reason = "zsh's imatchdata, unpacked")]
    fn get_match_ret(
        &mut self,
        mstr: &[u8],
        ustr: &[u8],
        fl: i32,
        replstr: Option<&[u8]>,
        repllist: Option<&mut Vec<(usize, usize, Vec<u8>)>>,
        b: usize,
        e: usize,
    ) -> Option<Vec<u8>> {
        let add_b = ustr.iter().take(b).filter(|&&c| tok::is_meta(c)).count();
        let add_e = ustr.iter().take(e).filter(|&&c| tok::is_meta(c)).count();
        let b = b + add_b;
        let e = e + add_e;
        let mut replstr = replstr.map(<[u8]>::to_vec);
        let mut ll = 0usize;
        if replstr.is_some() || fl & SUB_LIST != 0 {
            if fl & SUB_DOSUBST != 0
                && let Some(r) = replstr.as_mut()
            {
                *r = self.singsub(r);
                tok::untokenize(r);
            }
            if fl & (SUB_GLOBAL | SUB_LIST) != 0
                && let Some(list) = repllist
            {
                list.push((b, e, replstr.unwrap_or_default()));
                return Some(mstr.to_vec());
            }
            if let Some(r) = &replstr {
                ll += r.len();
            }
        }
        let mlen = mstr.len();
        if fl & SUB_MATCH != 0 {
            ll += 1 + (e - b);
        }
        if fl & SUB_REST != 0 {
            ll += 1 + (mlen - (e - b));
        }
        let mut buf = String::new();
        if fl & SUB_BIND != 0 {
            buf.push_str(&format!(
                "{} ",
                crate::utils::mb_metastrlen0(self, sub(mstr, 0, b)) + 1
            ));
        }
        if fl & SUB_EIND != 0 {
            buf.push_str(&format!(
                "{} ",
                crate::utils::mb_metastrlen0(self, sub(mstr, 0, e)) + 1
            ));
        }
        if fl & SUB_LEN != 0 {
            buf.push_str(&format!(
                "{} ",
                crate::utils::mb_metastrlen0(self, sub(mstr, b, e))
            ));
        }
        ll += buf.len();
        if buf.ends_with(' ') {
            let _ = buf.pop();
        }
        if ll == 0 {
            return None;
        }
        let mut r = Vec::new();
        let mut t = false;
        if fl & SUB_MATCH != 0 {
            r.extend_from_slice(sub(mstr, b, e));
            t = true;
        }
        if fl & SUB_REST != 0 {
            if t {
                r.push(b' ');
            }
            r.extend_from_slice(sub(mstr, 0, b));
            if let Some(rp) = &replstr {
                r.extend_from_slice(rp);
            }
            r.extend_from_slice(from(mstr, e));
            t = true;
        }
        if !buf.is_empty() {
            if t {
                r.push(b' ');
            }
            r.extend(buf.bytes());
        }
        Some(r)
    }

    /// zsh's `igetmatch` (the multibyte version).
    #[expect(clippy::too_many_lines, reason = "zsh's igetmatch")]
    fn igetmatch(
        &mut self,
        sp: &mut Vec<u8>,
        p: &mut Patprog,
        fl: i32,
        n: i32,
        replstr: Option<&[u8]>,
        repllistp: Option<&mut Vec<(usize, usize, Vec<u8>)>>,
    ) -> bool {
        let mstr = sp.clone();
        let l = mstr.len();
        let u = tok::unmetafy(&mstr);
        let umltot = u.len();
        let send = umltot;
        let mut n = n;
        let mut matched = true;
        if let Some(must) = p.must_string() {
            matched = must.len() <= umltot
                && (must.is_empty() || u.windows(must.len()).any(|w| w == must));
        }
        p.flags &= !(PAT_NOTSTART | PAT_NOTEND);
        let mut local_list: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        let use_list = fl & SUB_GLOBAL != 0 && fl & (SUB_SUBSTR) != 0;
        macro_rules! ret {
            ($b:expr, $e:expr) => {{
                let list = if use_list {
                    Some(&mut local_list)
                } else {
                    None
                };
                self.get_match_ret(&mstr, &u, fl, replstr, list, $b, $e)
            }};
        }
        let charlen = |sh: &Shell, pos: usize| -> usize {
            if !sh.isset(MULTIBYTE) {
                return 1;
            }
            crate::utils::utf8_char(from(&u, pos)).0.max(1)
        };
        if fl & SUB_ALL != 0 {
            let i = matched
                && self
                    .pattrylen(p, &u, 0, umltot, 0)
                    .is_some_and(|m| m == umltot);
            let r = if i {
                self.get_match_ret(&mstr, &u, fl, replstr, None, 0, umltot)
            } else {
                self.get_match_ret(&mstr, &u, fl, None, None, 0, 0)
            };
            *sp = r.unwrap_or_default();
            return !(sp.is_empty() && ((fl & SUB_MATCH != 0 && !i) || (fl & SUB_REST != 0 && i)));
        }
        if matched {
            match fl & (SUB_END | SUB_LONG | SUB_SUBSTR) {
                0 | SUB_LONG => {
                    if let Some(mut mlen) = self.pattrylen(p, &u, 0, umltot, 0) {
                        if fl & SUB_LONG == 0 && p.flags & PAT_PURES == 0 {
                            let sendm = mlen;
                            let mut t = 0usize;
                            let mut umlen = 0usize;
                            while t < sendm {
                                set_pat_end(p, true);
                                if let Some(m) = self.pattrylen(p, &u, 0, umlen, 0) {
                                    mlen = m;
                                    break;
                                }
                                let c = charlen(self, t);
                                t += c;
                                umlen += c;
                            }
                        }
                        *sp = ret!(0, mlen).unwrap_or_default();
                        return true;
                    }
                }
                SUB_END => {
                    let mut tmatch: Option<usize> = None;
                    set_pat_start(p, l);
                    if self
                        .pattrylen(p, &u, send, 0, i64::try_from(umltot).unwrap_or(0))
                        .is_some()
                    {
                        n -= 1;
                        if n == 0 {
                            *sp = ret!(umltot, umltot).unwrap_or_default();
                            return true;
                        }
                    }
                    let mut ioff = 0i64;
                    let mut t = 0usize;
                    let mut umlen = umltot;
                    while t < send {
                        set_pat_start(p, t);
                        if self
                            .pattrylen(p, &u, t, umlen, ioff)
                            .is_some_and(|m| m == umlen)
                        {
                            tmatch = Some(t);
                        }
                        if fl & SUB_START != 0 {
                            break;
                        }
                        let c = charlen(self, t);
                        umlen -= c;
                        t += c;
                        ioff += 1;
                    }
                    if let Some(tm) = tmatch {
                        *sp = ret!(tm, umltot).unwrap_or_default();
                        return true;
                    }
                    if fl & SUB_START == 0 && self.pattrylen(p, &u, umltot, 0, ioff).is_some() {
                        *sp = ret!(umltot, umltot).unwrap_or_default();
                        return true;
                    }
                }
                x if x == SUB_END | SUB_LONG => {
                    let mut ioff = 0i64;
                    let mut t = 0usize;
                    let mut umlen = umltot;
                    loop {
                        if t > send {
                            break;
                        }
                        set_pat_start(p, t);
                        if self
                            .pattrylen(p, &u, t, umlen, ioff)
                            .is_some_and(|m| m == umlen)
                        {
                            *sp = ret!(t, umltot).unwrap_or_default();
                            return true;
                        }
                        if fl & SUB_START != 0 || t == send {
                            break;
                        }
                        let c = charlen(self, t);
                        umlen -= c;
                        t += c;
                        ioff += 1;
                    }
                    if fl & SUB_START == 0 && self.pattrylen(p, &u, send, 0, ioff).is_some() {
                        *sp = ret!(umltot, umltot).unwrap_or_default();
                        return true;
                    }
                }
                SUB_SUBSTR | 0x0006 => {
                    let substr_short = fl & (SUB_END | SUB_LONG | SUB_SUBSTR) == SUB_SUBSTR;
                    if substr_short {
                        set_pat_start(p, l);
                        if fl & SUB_GLOBAL == 0 && self.pattrylen(p, &u, send, 0, 0).is_some() {
                            n -= 1;
                            if n == 0 {
                                *sp = ret!(0, 0).unwrap_or_default();
                                return true;
                            }
                        }
                    }
                    let mut t = 0usize;
                    let mut ioff = 0i64;
                    let mut umlen = umltot;
                    let mut got_list = false;
                    loop {
                        let mut matched2 = false;
                        while t <= send {
                            set_pat_start(p, t);
                            if let Some(mlen) = self.pattrylen(p, &u, t, umlen, ioff) {
                                let mut mpos = t + mlen;
                                if fl & SUB_LONG == 0 && p.flags & PAT_PURES == 0 {
                                    let mut ptr = t;
                                    let mut umlen2 = 0usize;
                                    while ptr < mpos {
                                        set_pat_end(p, true);
                                        if let Some(m2) = self.pattrylen(p, &u, t, umlen2, ioff) {
                                            mpos = t + m2;
                                            break;
                                        }
                                        let c = charlen(self, ptr);
                                        ptr += c;
                                        umlen2 += c;
                                    }
                                }
                                n -= 1;
                                if n == 0 || (n <= 0 && fl & SUB_GLOBAL != 0) {
                                    let r = {
                                        let list = if fl & SUB_GLOBAL != 0 {
                                            Some(&mut local_list)
                                        } else {
                                            None
                                        };
                                        self.get_match_ret(&mstr, &u, fl, replstr, list, t, mpos)
                                    };
                                    if fl & SUB_GLOBAL != 0 {
                                        got_list = true;
                                    } else {
                                        *sp = r.unwrap_or_default();
                                    }
                                    if mpos == t && mpos < send {
                                        mpos += charlen(self, mpos);
                                    }
                                }
                                if fl & SUB_GLOBAL == 0 {
                                    if n != 0 {
                                        let c = charlen(self, t);
                                        umlen = umlen.saturating_sub(c);
                                        t += c;
                                        ioff += 1;
                                        continue;
                                    }
                                    return true;
                                }
                                matched2 = true;
                                if t == send {
                                    break;
                                }
                                while t < mpos {
                                    ioff += 1;
                                    let c = charlen(self, t);
                                    umlen = umlen.saturating_sub(c);
                                    t += c;
                                }
                                break;
                            }
                            if t == send {
                                break;
                            }
                            let c = charlen(self, t);
                            umlen = umlen.saturating_sub(c);
                            t += c;
                            ioff += 1;
                        }
                        if !(matched2 && t < send) {
                            break;
                        }
                    }
                    let _ = got_list;
                    set_pat_start(p, l);
                    if fl & (SUB_LONG | SUB_GLOBAL) == SUB_LONG
                        && self.pattrylen(p, &u, send, 0, 0).is_some()
                    {
                        n -= 1;
                        if n == 0 {
                            *sp = ret!(0, 0).unwrap_or_default();
                            return true;
                        }
                    }
                }
                x if x == SUB_END | SUB_SUBSTR || x == SUB_END | SUB_LONG | SUB_SUBSTR => {
                    set_pat_start(p, l);
                    if self
                        .pattrylen(p, &u, send, 0, i64::try_from(umltot).unwrap_or(0))
                        .is_some()
                    {
                        n -= 1;
                        if n == 0 {
                            *sp = ret!(umltot, umltot).unwrap_or_default();
                            return true;
                        }
                    }
                    let mut nmatches = 0;
                    let mut tmatch: Option<(usize, usize, i64)> = None;
                    let mut ioff = 0i64;
                    let mut t = 0usize;
                    let mut umlen = umltot;
                    while t < send {
                        set_pat_start(p, t);
                        if let Some(m) = self.pattrylen(p, &u, t, umlen, ioff) {
                            nmatches += 1;
                            tmatch = Some((t, m, ioff));
                        }
                        let c = charlen(self, t);
                        umlen -= c;
                        t += c;
                        ioff += 1;
                    }
                    if nmatches > 0 {
                        if n > 1 {
                            let mut k = nmatches - n;
                            let mut ioff = 0i64;
                            let mut t = 0usize;
                            let mut umlen = umltot;
                            while t < send {
                                set_pat_start(p, t);
                                if let Some(m) = self.pattrylen(p, &u, t, umlen, ioff) {
                                    if k == 0 {
                                        tmatch = Some((t, m, ioff));
                                        break;
                                    }
                                    k -= 1;
                                }
                                let c = charlen(self, t);
                                umlen -= c;
                                t += c;
                                ioff += 1;
                            }
                        }
                        if let Some((tm, mlen, toff)) = tmatch {
                            let mut mpos = tm + mlen;
                            if fl & SUB_LONG == 0 && p.flags & PAT_PURES == 0 {
                                let mut tt = tm;
                                let mut umlen2 = 0usize;
                                while tt < mpos {
                                    set_pat_end(p, true);
                                    if let Some(m2) = self.pattrylen(p, &u, tm, umlen2, toff) {
                                        mpos = tm + m2;
                                        break;
                                    }
                                    let c = charlen(self, tt);
                                    tt += c;
                                    umlen2 += c;
                                }
                            }
                            *sp = ret!(tm, mpos).unwrap_or_default();
                            return true;
                        }
                    }
                    set_pat_start(p, l);
                    if fl & SUB_LONG != 0
                        && self
                            .pattrylen(p, &u, send, 0, i64::try_from(umltot).unwrap_or(0))
                            .is_some()
                    {
                        n -= 1;
                        if n == 0 {
                            *sp = ret!(umltot, umltot).unwrap_or_default();
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        if !local_list.is_empty() {
            if fl & SUB_LIST != 0 {
                if let Some(out) = repllistp {
                    *out = local_list;
                }
                return true;
            }
            let mut start = Vec::new();
            let mut i = 0usize;
            for (b, e, r) in &local_list {
                start.extend_from_slice(sub(&mstr, i, *b));
                start.extend_from_slice(r);
                i = *e;
            }
            start.extend_from_slice(from(&mstr, i));
            *sp = start;
            return true;
        }
        if fl & SUB_LIST != 0 {
            return false;
        }
        *sp = self
            .get_match_ret(&mstr, &u, fl, None, None, 0, 0)
            .unwrap_or_default();
        fl & SUB_RETFAIL == 0
    }
}

/// zsh's `set_pat_start`.
fn set_pat_start(p: &mut Patprog, offs: usize) {
    if offs != 0 {
        p.flags |= PAT_NOTSTART;
    } else {
        p.flags &= !PAT_NOTSTART;
    }
}

/// zsh's `set_pat_end`: the string is being cut short of its real end.
fn set_pat_end(p: &mut Patprog, null_me: bool) {
    if null_me {
        p.flags |= PAT_NOTEND;
    } else {
        p.flags &= !PAT_NOTEND;
    }
}

fn cmp_range(range: i32, v: i64, data: i64) -> bool {
    match range {
        r if r < 0 => v < data,
        r if r > 0 => v > data,
        _ => v == data,
    }
}

fn time_cmp(a: (i64, i64), b: (i64, i64)) -> i64 {
    let r = a.0 - b.0;
    if r != 0 { r } else { a.1 - b.1 }
}

/// zsh's `skipparens`: skip from the `inpar` at `*i` to after its matching
/// `outpar`; 0 on success, the unclosed depth otherwise.
pub(crate) fn skipparens(inpar: u8, outpar: u8, s: &[u8], i: &mut usize) -> i32 {
    if at(s, *i) != inpar {
        return -1;
    }
    let mut level: i32 = 0;
    loop {
        if *i >= s.len() {
            break;
        }
        let c = at(s, *i);
        *i += 1;
        if c == inpar {
            level += 1;
        } else if c == outpar {
            level -= 1;
        }
        if level == 0 {
            break;
        }
    }
    level
}

/// zsh's `bracechardots`: `{a..z}` at `i`, giving the two characters.
fn bracechardots(sh: &Shell, s: &[u8], i: usize) -> Option<(u32, u32)> {
    let mut p = i + 1;
    let decode = |p: usize| -> Option<(u32, usize)> {
        let c = at(s, p);
        if tok::is_tok(c) {
            if c == INBRACE {
                return None;
            }
            return Some((u32::from(tok::detok(c)), 1));
        }
        if p >= s.len() {
            return None;
        }
        let (len, wc) = crate::utils::mb_metacharlenconv(sh, from(s, p));
        wc.map(|w| (w, len))
    };
    let (cstart, l1) = decode(p)?;
    p += l1;
    if at(s, p) != b'.' || at(s, p + 1) != b'.' {
        return None;
    }
    p += 2;
    if p >= s.len() {
        return None;
    }
    let (cend, l2) = decode(p)?;
    p += l2;
    if at(s, p) != OUTBRACE {
        return None;
    }
    Some((cstart, cend))
}
