//! File contents as VMO pages.
//!
//! tmpfs keeps a regular file's bytes in whatever [`Pages`] it is given, and
//! the kernel gives it a VMO. That is the unification `docs/ARCHITECTURE.md`
//! §3 calls load-bearing: the object a `read` copies out of is the object a
//! future `mmap` of the same file maps, so a page is never held twice and a
//! write through one is seen through the other.
//!
//! # Sized for the largest file, paid for by the page
//!
//! Each file's VMO is created at [`MAX_FILE_SIZE`], which costs nothing: the
//! page list is sparse and a page with no frame is simply absent. The file's
//! real length is tmpfs's to keep, not the object's.
//!
//! # Who serialises what
//!
//! Nothing here takes a lock beyond the VMO's own. tmpfs holds the inode's
//! lock across every call, so a read cannot find a frame that a concurrent
//! truncate is in the middle of releasing. When `mmap` of a tmpfs file
//! arrives, a mapping will reach the same VMO without that lock, and the
//! release path will need the same care a copy-on-write fault already takes.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ops::Range;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_vfs::tmpfs::{Pages, Storage};
use ferrix_vfs::{Errno, Result};

use crate::mm;
use crate::user::vmo::{Vmo, VmoError};

/// The largest a tmpfs file may grow: one tebibyte.
///
/// A policy, not a limit of the structure. It keeps every file size and offset
/// comfortably inside what both `off_t` and a page index can say on all three
/// architectures.
pub(crate) const MAX_FILE_SIZE: u64 = 1 << 40;

/// Hands tmpfs a VMO per file.
#[derive(Debug)]
pub(crate) struct VmoStorage;

impl Storage for VmoStorage {
    fn allocate(&self) -> Result<Box<dyn Pages>> {
        Ok(Box::new(VmoPages {
            vmo: Vmo::new_anonymous(MAX_FILE_SIZE / PAGE_SIZE),
        }))
    }

    fn max_file_size(&self) -> u64 {
        MAX_FILE_SIZE
    }
}

/// One file's contents.
#[derive(Debug)]
pub(crate) struct VmoPages {
    vmo: Arc<Vmo>,
}

/// Visit each page-sized piece of `len` bytes at `offset`: its page index, the
/// offset within the page, and the range within the caller's buffer.
fn pieces(
    offset: u64,
    len: usize,
    mut visit: impl FnMut(u64, usize, Range<usize>) -> Result<()>,
) -> Result<()> {
    let page = usize::try_from(PAGE_SIZE).map_err(|_| Errno::EIO)?;
    let mut done = 0_usize;
    while done < len {
        let at = offset.checked_add(done as u64).ok_or(Errno::EFBIG)?;
        let within = usize::try_from(at % PAGE_SIZE).map_err(|_| Errno::EIO)?;
        let take = (page - within).min(len - done);
        visit(at / PAGE_SIZE, within, done..done + take)?;
        done += take;
    }
    Ok(())
}

/// The direct-map address of byte `within` of `frame`.
fn byte_of(frame: u64, within: usize) -> u64 {
    mm::direct_map(frame * PAGE_SIZE) + within as u64
}

impl Pages for VmoPages {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        pieces(offset, buf.len(), |index, within, range| {
            let out = buf.get_mut(range).ok_or(Errno::EIO)?;
            match self.vmo.page(index) {
                // SAFETY: the frame is committed in this object, tmpfs holds
                // the inode lock so nothing releases it meanwhile, and `out`
                // fits in the page because `pieces` stops at its end. `out`
                // is kernel memory the source cannot overlap.
                Some(frame) => unsafe {
                    core::ptr::copy_nonoverlapping(
                        byte_of(frame, within) as *const u8,
                        out.as_mut_ptr(),
                        out.len(),
                    );
                },
                None => out.fill(0),
            }
            Ok(())
        })
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<()> {
        pieces(offset, data.len(), |index, within, range| {
            let bytes = data.get(range).ok_or(Errno::EIO)?;
            let frame = self.vmo.commit(index).map_err(|error| match error {
                VmoError::OutOfRange { .. } => Errno::EFBIG,
                // What Linux's tmpfs says when memory runs out under it.
                VmoError::OutOfMemory => Errno::ENOSPC,
            })?;
            // SAFETY: as `read`, and the frame was zeroed when committed, so
            // the bytes around the ones written are zeros rather than an old
            // owner's data.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    byte_of(frame, within) as *mut u8,
                    bytes.len(),
                );
            }
            Ok(())
        })
    }

    fn discard_from(&self, offset: u64) {
        let _ = self.vmo.decommit_from(offset.div_ceil(PAGE_SIZE));
        let Ok(within) = usize::try_from(offset % PAGE_SIZE) else {
            return;
        };
        if within == 0 {
            return;
        }
        if let Some(frame) = self.vmo.page(offset / PAGE_SIZE) {
            let Ok(page) = usize::try_from(PAGE_SIZE) else {
                return;
            };
            // SAFETY: the frame is committed in this object and tmpfs holds
            // the inode lock; the range runs from `within` to the page's end.
            unsafe {
                core::ptr::write_bytes(byte_of(frame, within) as *mut u8, 0, page - within);
            }
        }
    }

    fn committed_bytes(&self) -> u64 {
        (self.vmo.committed() as u64).saturating_mul(PAGE_SIZE)
    }
}
