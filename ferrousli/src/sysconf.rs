//! `unistd.h`'s `sysconf` and `getpagesize`: the system's limits, as the
//! running kernel reports them.

use core::ffi::{c_int, c_long, c_ulong};
use core::mem::{offset_of, size_of};
use core::ptr::null;

use crate::resource::{self, RLIM_INFINITY, RLIMIT_NOFILE, RLIMIT_NPROC, Rlimit};
use crate::syscall::{self, nr};
use crate::{auxv, errno};

/// `_SC_ARG_MAX`, from musl's `unistd.h`.
const SC_ARG_MAX: c_int = 0;
/// `_SC_CHILD_MAX`.
const SC_CHILD_MAX: c_int = 1;
/// `_SC_CLK_TCK`.
const SC_CLK_TCK: c_int = 2;
/// `_SC_OPEN_MAX`.
const SC_OPEN_MAX: c_int = 4;
/// `_SC_PAGESIZE`, and `_SC_PAGE_SIZE`, which is the same number.
const SC_PAGESIZE: c_int = 30;
/// `_SC_LOGIN_NAME_MAX`.
const SC_LOGIN_NAME_MAX: c_int = 71;
/// `_SC_NPROCESSORS_CONF`.
const SC_NPROCESSORS_CONF: c_int = 83;
/// `_SC_NPROCESSORS_ONLN`.
const SC_NPROCESSORS_ONLN: c_int = 84;
/// `_SC_PHYS_PAGES`.
const SC_PHYS_PAGES: c_int = 85;
/// `_SC_HOST_NAME_MAX`.
const SC_HOST_NAME_MAX: c_int = 180;

/// `ARG_MAX`, from musl's `limits.h`, which is `linux/limits.h`'s.
const ARG_MAX: c_long = 131_072;
/// `HOST_NAME_MAX`, from musl's `limits.h`.
const HOST_NAME_MAX: c_long = 255;
/// `LOGIN_NAME_MAX`, from musl's `limits.h`.
const LOGIN_NAME_MAX: c_long = 256;
/// `AT_CLKTCK`, from `linux/auxvec.h`: the kernel's clock ticks per second.
const AT_CLKTCK: usize = 17;
/// Clock ticks per second if the kernel does not say: `USER_HZ`, which is 100
/// on every architecture Linux user space sees.
const DEFAULT_CLK_TCK: c_long = 100;
/// The page size if the kernel does not say, as in unit tests: x86-64's.
const DEFAULT_PAGE_SIZE: usize = 4096;
/// The bytes of CPU mask `sched_getaffinity` is given: room for 8192 CPUs, and
/// a multiple of a long, which the kernel requires.
const CPU_MASK_BYTES: usize = 1024;

/// The kernel's `struct sysinfo`, from `linux/sysinfo.h`, on a 64-bit
/// architecture. Its `_f` padding is empty there.
#[repr(C)]
#[derive(Debug, Default)]
struct Sysinfo {
    uptime: c_long,
    loads: [c_ulong; 3],
    totalram: c_ulong,
    freeram: c_ulong,
    sharedram: c_ulong,
    bufferram: c_ulong,
    totalswap: c_ulong,
    freeswap: c_ulong,
    procs: u16,
    pad: u16,
    totalhigh: c_ulong,
    freehigh: c_ulong,
    mem_unit: u32,
}

const _: () = assert!(offset_of!(Sysinfo, totalram) == 32);
const _: () = assert!(offset_of!(Sysinfo, procs) == 80);
const _: () = assert!(offset_of!(Sysinfo, totalhigh) == 88);
const _: () = assert!(offset_of!(Sysinfo, mem_unit) == 104);
const _: () = assert!(size_of::<Sysinfo>() == 112);

/// The page size, from the auxiliary vector.
pub fn page_size() -> usize {
    auxv::get(auxv::AT_PAGESZ).unwrap_or(DEFAULT_PAGE_SIZE)
}

/// The size of a page of memory.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getpagesize() -> c_int {
    c_int::try_from(page_size()).unwrap_or(c_int::MAX)
}

/// The soft limit of `resource`, -1 if there is none, or saturated at the
/// largest `long`.
fn soft_limit(resource: c_int) -> c_long {
    let mut limit = Rlimit::default();
    // SAFETY: `limit` is a live local, and nothing is set.
    if unsafe { resource::prlimit(0, resource, null(), &raw mut limit) } != 0 {
        return -1;
    }
    if limit.rlim_cur == RLIM_INFINITY {
        return -1;
    }
    c_long::try_from(limit.rlim_cur).unwrap_or(c_long::MAX)
}

/// How many CPUs a mask names.
fn count_cpus(mask: &[u8]) -> c_long {
    mask.iter()
        .map(|byte| c_long::from(byte.count_ones() as u8))
        .sum()
}

/// How many CPUs the calling thread may run on. POSIX asks for the CPUs
/// configured and online; like musl, both are answered with the ones this
/// thread can use, which is what a program sizing a thread pool wants.
fn processors() -> c_long {
    let mut mask = [0_u8; CPU_MASK_BYTES];
    // SAFETY: the kernel writes at most `CPU_MASK_BYTES` into the live local.
    let ret = unsafe {
        syscall::syscall3(
            nr::SCHED_GETAFFINITY,
            0,
            CPU_MASK_BYTES,
            mask.as_mut_ptr().addr(),
        )
    };
    match errno::decode(ret) {
        Ok(written) => count_cpus(mask.get(..written).unwrap_or(&mask)).max(1),
        // There is at least the CPU this is running on.
        Err(_) => 1,
    }
}

/// Pages of physical memory from a `sysinfo` result.
fn phys_pages(info: &Sysinfo, page: usize) -> c_long {
    let unit = u128::from(info.mem_unit.max(1));
    let bytes = u128::from(info.totalram) * unit;
    let pages = bytes / page.max(1) as u128;
    c_long::try_from(pages).unwrap_or(c_long::MAX)
}

/// The value of system limit or option `name`.
///
/// It answers the page size, clock ticks, the open file and child process
/// limits, the CPU count, physical memory, and `ARG_MAX`, `HOST_NAME_MAX` and
/// `LOGIN_NAME_MAX`. Any other name fails with `EINVAL`.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        SC_ARG_MAX => ARG_MAX,
        SC_CHILD_MAX => soft_limit(RLIMIT_NPROC),
        SC_CLK_TCK => auxv::get(AT_CLKTCK)
            .and_then(|ticks| c_long::try_from(ticks).ok())
            .unwrap_or(DEFAULT_CLK_TCK),
        SC_OPEN_MAX => soft_limit(RLIMIT_NOFILE),
        SC_PAGESIZE => c_long::try_from(page_size()).unwrap_or(c_long::MAX),
        SC_LOGIN_NAME_MAX => LOGIN_NAME_MAX,
        SC_NPROCESSORS_CONF | SC_NPROCESSORS_ONLN => processors(),
        SC_PHYS_PAGES => {
            let mut info = Sysinfo::default();
            // SAFETY: the kernel writes the live local.
            let ret = unsafe { syscall::syscall2(nr::SYSINFO, (&raw mut info).addr(), 0) };
            match errno::decode(ret) {
                Ok(_) => phys_pages(&info, page_size()),
                Err(error) => {
                    errno::set(error);
                    -1
                }
            }
        }
        SC_HOST_NAME_MAX => HOST_NAME_MAX,
        _ => {
            errno::set(errno::EINVAL);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpus_are_the_bits_in_the_mask() {
        assert_eq!(count_cpus(&[0b1011, 0, 0x80]), 4);
        assert_eq!(count_cpus(&[]), 0);
    }

    #[test]
    fn physical_pages_scale_by_the_memory_unit() {
        let info = Sysinfo {
            totalram: 4,
            mem_unit: 4096,
            ..Sysinfo::default()
        };
        assert_eq!(phys_pages(&info, 4096), 4);
        let unitless = Sysinfo {
            totalram: 8192,
            ..Sysinfo::default()
        };
        assert_eq!(phys_pages(&unitless, 4096), 2);
    }

    #[test]
    fn the_answers_are_plausible_and_an_unknown_name_is_refused() {
        assert!(sysconf(SC_NPROCESSORS_ONLN) >= 1);
        assert!(sysconf(SC_PHYS_PAGES) > 0);
        assert_eq!(sysconf(SC_HOST_NAME_MAX), 255);
        assert_eq!(sysconf(-1), -1);
        // SAFETY: the pointer is this thread's errno.
        assert_eq!(unsafe { errno::__errno_location().read() }, errno::EINVAL);
    }
}
