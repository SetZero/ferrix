//! An AArch64 UEFI variable store that already says `Timeout` is 0.
//!
//! EDK2's AArch64 build waits at its boot menu for `PcdPlatformBootTimeOut`,
//! five seconds, before it starts a boot option, and it takes the wait from
//! the `Timeout` variable once one exists -- which on a store xtask made
//! fresh for the boot it never does until that boot has waited and written
//! it. 5.3 s of every AArch64 boot was that wait (ferrix-9a's survey of the
//! gates' time, 2026-09-27). QEMU's `-boot splash-time=` does not reach it:
//! the x86-64 build reads `etc/boot-menu-wait` from `fw_cfg`, the AArch64 build
//! does not, and a boot with it set to fifteen seconds still waited five.
//!
//! So the store xtask writes for a boot holds `Timeout` = 0 from the start.
//! The firmware path is the same one, every driver and the loader's own
//! reading of it unchanged; only the menu's wait goes. A store the firmware
//! did not accept would be one it formats again, as it formats an empty one,
//! and the cost of that is the five seconds back, never a boot that fails.
//!
//! Two layouts are written into. A distribution's template (`AAVMF_VARS.fd`,
//! as CI's Ubuntu has) is already formatted, and gets the variable in the
//! first free slot. Where there is no template -- QEMU's own share directory
//! ships none for AArch64 -- the store is made formatted and empty as EDK2
//! formats one ([`FORMATTED`], [`WORK_SPACE`]), then gets it the same way.

/// The first 0x64 bytes of the store EDK2's AArch64 `virt` build formats: the
/// firmware volume header (`gEfiSystemNvDataFvGuid`, 0xc0000 bytes) and the
/// authenticated variable store header (0x3ffb8 bytes, formatted, healthy).
/// Copied from the store edk2-stable202408 formatted on this host.
const FORMATTED: [u8; 0x64] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x8d, 0x2b, 0xf1, 0xff, 0x96, 0x76, 0x8b, 0x4c, 0xa9, 0x85, 0x27, 0x47, 0x07, 0x5b, 0x4f, 0x50,
    0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5f, 0x46, 0x56, 0x48, 0x36, 0x0e, 0x00, 0x00,
    0x48, 0x00, 0xf8, 0xf8, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0x2c, 0xf3, 0xaa, 0x7b, 0x94, 0x9a, 0x43,
    0xa1, 0x80, 0x2e, 0x14, 0x4e, 0xc3, 0x77, 0x92, 0xb8, 0xff, 0x03, 0x00, 0x5a, 0xfe, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// Where the fault-tolerant write's working block starts, and its header as
/// EDK2 formats it: the signature, its CRC, valid, and the queue's size.
const WORK_SPACE_AT: usize = 0x4_0000;
const WORK_SPACE: [u8; 32] = [
    0x2b, 0x29, 0x58, 0x9e, 0x68, 0x7c, 0x7d, 0x49, 0xa0, 0xce, 0x65, 0x00, 0xfd, 0x9f, 0x1b, 0x95,
    0x5b, 0xe7, 0xc6, 0x86, 0xfe, 0xff, 0xff, 0xff, 0xe0, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Where the volume ends: flash past it is left as zeros, as EDK2 leaves it.
const VOLUME_END: usize = 0xc_0000;

/// `gEfiAuthenticatedVariableGuid` and `gEfiVariableGuid`, the two store
/// formats, as they lie in memory.
const AUTHENTICATED: [u8; 16] = [
    0x78, 0x2c, 0xf3, 0xaa, 0x7b, 0x94, 0x9a, 0x43, 0xa1, 0x80, 0x2e, 0x14, 0x4e, 0xc3, 0x77, 0x92,
];
const PLAIN: [u8; 16] = [
    0x16, 0x36, 0xcf, 0xdd, 0x75, 0x32, 0x64, 0x41, 0x98, 0xb6, 0xfe, 0x85, 0x70, 0x7f, 0xfe, 0x7d,
];

/// `EFI_GLOBAL_VARIABLE`, whose `Timeout` is the boot menu's.
const GLOBAL: [u8; 16] = [
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
];

/// Non-volatile, boot service and runtime access, as EDK2 writes `Timeout`.
const ATTRIBUTES: u32 = 0x7;

/// A variable's first two bytes; `VAR_ADDED`, the state of one in use.
const START_ID: u16 = 0x55aa;
const ADDED: u8 = 0x3f;

/// The store EDK2 formats, empty, padded to `size` -- the code image's size,
/// which QEMU requires the two flash images of `virt` to share.
pub(crate) fn formatted(size: usize) -> Vec<u8> {
    let mut store = vec![0_u8; size.max(VOLUME_END)];
    if let Some(volume) = store.get_mut(..VOLUME_END) {
        volume.fill(0xff);
    }
    if let Some(header) = store.get_mut(..FORMATTED.len()) {
        header.copy_from_slice(&FORMATTED);
    }
    if let Some(work) = store.get_mut(WORK_SPACE_AT..WORK_SPACE_AT + WORK_SPACE.len()) {
        work.copy_from_slice(&WORK_SPACE);
    }
    store
}

/// Put `Timeout` = 0 in the first free slot of `store`, and say whether it
/// went in. A store in a layout this does not know, or one that already has
/// a `Timeout`, is left as it was.
pub(crate) fn without_boot_menu_wait(store: &mut [u8]) -> bool {
    let Some(fv_header) = read_u16(store, 48).map(usize::from) else {
        return false;
    };
    if store.get(40..44) != Some(b"_FVH") {
        return false;
    }
    let authenticated = match store.get(fv_header..fv_header + 16) {
        Some(guid) if guid == AUTHENTICATED => true,
        Some(guid) if guid == PLAIN => false,
        _ => return false,
    };
    let Some(size) = read_u32(store, fv_header + 16).map(|size| size as usize) else {
        return false;
    };
    let end = fv_header + size;
    // The variables start after the store's 28-byte header, each on a four
    // byte boundary.
    let header_len = if authenticated { 60 } else { 32 };
    let mut at = align4(fv_header + 28);
    loop {
        if at + header_len > end.min(store.len()) {
            return false;
        }
        if read_u16(store, at) != Some(START_ID) {
            break;
        }
        let (name_size, data_size, guid_at) = if authenticated {
            (read_u32(store, at + 36), read_u32(store, at + 40), at + 44)
        } else {
            (read_u32(store, at + 8), read_u32(store, at + 12), at + 16)
        };
        let (Some(name_size), Some(data_size)) = (name_size, data_size) else {
            return false;
        };
        let name_at = at + header_len;
        let is_timeout = store.get(guid_at..guid_at + 16) == Some(&GLOBAL[..])
            && store.get(name_at..name_at + name_size as usize) == Some(&timeout_name()[..]);
        if is_timeout && store.get(at + 2) == Some(&ADDED) {
            return false;
        }
        at = align4(name_at + name_size as usize + data_size as usize);
    }
    let variable = variable(authenticated);
    let Some(slot) = store.get_mut(at..at + variable.len()) else {
        return false;
    };
    if at + variable.len() > end || slot.iter().any(|&byte| byte != 0xff) {
        return false;
    }
    slot.copy_from_slice(&variable);
    true
}

/// `Timeout`, as UTF-16 with its terminator.
fn timeout_name() -> Vec<u8> {
    "Timeout\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The variable itself: `Timeout`, a `u16` of 0.
fn variable(authenticated: bool) -> Vec<u8> {
    let name = timeout_name();
    let data = 0_u16.to_le_bytes();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&START_ID.to_le_bytes());
    bytes.push(ADDED);
    bytes.push(0);
    bytes.extend_from_slice(&ATTRIBUTES.to_le_bytes());
    if authenticated {
        // Monotonic count, time stamp and public key index: none, for a
        // variable no signature covers.
        bytes.extend_from_slice(&[0; 8 + 16 + 4]);
    }
    bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&GLOBAL);
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&data);
    bytes
}

fn align4(at: usize) -> usize {
    at.next_multiple_of(4)
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::{FORMATTED, formatted, timeout_name, without_boot_menu_wait};

    /// The `Timeout` variables a store holds, as their data.
    fn timeouts(store: &[u8], header_len: usize) -> Vec<Vec<u8>> {
        let mut found = Vec::new();
        let mut at = 0x64;
        while u16::from_le_bytes([store[at], store[at + 1]]) == 0x55aa {
            let (name_size, data_size) = if header_len == 60 {
                (
                    u32::from_le_bytes(store[at + 36..at + 40].try_into().unwrap()) as usize,
                    u32::from_le_bytes(store[at + 40..at + 44].try_into().unwrap()) as usize,
                )
            } else {
                (
                    u32::from_le_bytes(store[at + 8..at + 12].try_into().unwrap()) as usize,
                    u32::from_le_bytes(store[at + 12..at + 16].try_into().unwrap()) as usize,
                )
            };
            let name = &store[at + header_len..at + header_len + name_size];
            if name == timeout_name().as_slice() {
                let data_at = at + header_len + name_size;
                found.push(store[data_at..data_at + data_size].to_vec());
            }
            at = (at + header_len + name_size + data_size).next_multiple_of(4);
        }
        found
    }

    #[test]
    fn an_empty_store_gets_a_timeout_of_zero_once() {
        let mut store = formatted(64 << 20);
        assert_eq!(store.len(), 64 << 20);
        assert!(without_boot_menu_wait(&mut store));
        assert_eq!(timeouts(&store, 60), vec![vec![0, 0]]);
        // A second time finds the first, and adds nothing.
        assert!(!without_boot_menu_wait(&mut store));
        assert_eq!(timeouts(&store, 60).len(), 1);
    }

    #[test]
    fn a_store_with_variables_gets_it_after_the_last() {
        let mut store = formatted(0xc_0000);
        assert!(without_boot_menu_wait(&mut store));
        // Pretend the first variable is another one: renamed, it no longer
        // counts as a Timeout, and the next goes after it.
        store[0x64 + 60] = b'X';
        assert!(without_boot_menu_wait(&mut store));
        assert_eq!(timeouts(&store, 60), vec![vec![0, 0]]);
    }

    #[test]
    fn a_plain_store_is_written_in_its_own_layout() {
        let mut store = formatted(0xc_0000);
        store[0x48..0x58].copy_from_slice(&super::PLAIN);
        assert!(without_boot_menu_wait(&mut store));
        assert_eq!(timeouts(&store, 32), vec![vec![0, 0]]);
    }

    #[test]
    fn a_store_it_does_not_know_is_left_alone() {
        let mut zeros = vec![0_u8; 0xc_0000];
        assert!(!without_boot_menu_wait(&mut zeros));
        assert!(zeros.iter().all(|&byte| byte == 0));
        let mut other = formatted(0xc_0000);
        other[0x48] ^= 0xff;
        let before = other.clone();
        assert!(!without_boot_menu_wait(&mut other));
        assert_eq!(other, before);
        assert_eq!(&formatted(0)[..FORMATTED.len()], &FORMATTED[..]);
    }
}
