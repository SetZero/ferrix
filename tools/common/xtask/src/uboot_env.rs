//! A U-Boot environment for ARMv7-A's `virt` with `bootdelay=0`.
//!
//! U-Boot counts down two seconds before it boots -- "Hit any key to stop
//! autoboot" -- on every ARMv7-A boot, because the environment it loads from
//! flash is not there ("bad CRC, using default environment") and the
//! compiled-in default says `bootdelay=2`. 2.0 s of every ARMv7-A boot was
//! that count (ferrix-9a's survey of the gates' time, 2026-09-27).
//!
//! So xtask gives U-Boot an environment in the flash bank it reads one from:
//! its own default environment, read out of the `u-boot.bin` the boot runs so
//! that it is that build's and nothing else, with `bootdelay` set to 0. An
//! environment in flash replaces the default whole rather than being merged
//! into it, which is why all of it is carried. Nothing else changes: the
//! same boot command scans the same devices and starts the same loader.
//!
//! The environment's place is the U-Boot build's own choice, and `qemu_arm`
//! keeps it at the start of the second flash bank (`CONFIG_ENV_ADDR`
//! 0x4000000), 256 KiB of it (`CONFIG_ENV_SIZE`): a CRC-32 of the rest, then
//! `name=value` strings each ended by a NUL, and a NUL after the last. A
//! U-Boot built otherwise finds a bad CRC or no environment there, uses its
//! default as before, and counts its two seconds; nothing fails.

/// `CONFIG_ENV_SIZE` for `qemu_arm`.
const ENV_SIZE: usize = 0x4_0000;

/// A flash bank of `virt`: QEMU wants a pflash image to be exactly this.
pub(crate) const BANK: usize = 64 << 20;

/// The second flash bank for a boot of `uboot`, holding its default
/// environment with `bootdelay=0`; `None` when its default cannot be found in
/// it.
pub(crate) fn bank(uboot: &[u8]) -> Option<Vec<u8>> {
    let mut env = default_environment(uboot)?;
    let delay = env
        .iter()
        .position(|entry| entry.starts_with(b"bootdelay="))?;
    if let Some(entry) = env.get_mut(delay) {
        *entry = b"bootdelay=0".to_vec();
    }
    let mut data = Vec::with_capacity(ENV_SIZE - 4);
    for entry in &env {
        data.extend_from_slice(entry);
        data.push(0);
    }
    data.push(0);
    if data.len() > ENV_SIZE - 4 {
        return None;
    }
    data.resize(ENV_SIZE - 4, 0);
    let mut bank = Vec::with_capacity(BANK);
    bank.extend_from_slice(&crc32(&data).to_le_bytes());
    bank.extend_from_slice(&data);
    // Erased flash past the environment.
    bank.resize(BANK, 0xff);
    Some(bank)
}

/// The default environment compiled into `uboot`: the run of NUL-ended
/// `name=value` strings that holds `bootdelay=`, ending at the empty string.
fn default_environment(uboot: &[u8]) -> Option<Vec<Vec<u8>>> {
    let at = find(uboot, b"\0bootdelay=")? + 1;
    // Back to the first entry: each earlier string ends with the NUL before
    // the next, and is a printable `name=value`.
    let mut start = at;
    while let Some(end) = start.checked_sub(1) {
        let begin = uboot
            .get(..end)?
            .iter()
            .rposition(|&byte| byte == 0)
            .map_or(0, |nul| nul + 1);
        if !is_entry(uboot.get(begin..end)?) {
            break;
        }
        start = begin;
    }
    let mut entries = Vec::new();
    let mut at = start;
    loop {
        let len = uboot.get(at..)?.iter().position(|&byte| byte == 0)?;
        if len == 0 {
            break;
        }
        let entry = uboot.get(at..at + len)?;
        if !is_entry(entry) {
            return None;
        }
        entries.push(entry.to_vec());
        at += len + 1;
    }
    Some(entries)
}

/// A `name=value` of printable ASCII.
fn is_entry(bytes: &[u8]) -> bool {
    let Some(equals) = bytes.iter().position(|&byte| byte == b'=') else {
        return false;
    };
    equals > 0 && bytes.iter().all(|&byte| (0x20..0x7f).contains(&byte))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The CRC-32 U-Boot checks an environment with: IEEE 802.3's, reflected.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{BANK, ENV_SIZE, bank, crc32, default_environment};

    /// A binary with an environment in it, between other bytes.
    fn binary() -> Vec<u8> {
        let mut bytes = b"\x7fcode\0\x01\x02not=an entry\x01".to_vec();
        bytes.push(0);
        for entry in ["bootcmd=bootflow scan -lb", "bootdelay=2", "arch=arm"] {
            bytes.extend_from_slice(entry.as_bytes());
            bytes.push(0);
        }
        bytes.extend_from_slice(b"\0more code");
        bytes
    }

    #[test]
    fn the_crc_is_the_ieee_one() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn the_default_environment_is_read_whole() {
        let env = default_environment(&binary()).unwrap();
        assert_eq!(
            env,
            [
                b"bootcmd=bootflow scan -lb".to_vec(),
                b"bootdelay=2".to_vec(),
                b"arch=arm".to_vec()
            ]
        );
    }

    #[test]
    fn the_bank_carries_it_with_no_delay() {
        let bank = bank(&binary()).unwrap();
        assert_eq!(bank.len(), BANK);
        let data = &bank[4..ENV_SIZE];
        assert_eq!(
            u32::from_le_bytes(bank[..4].try_into().unwrap()),
            crc32(data)
        );
        assert!(data.starts_with(b"bootcmd=bootflow scan -lb\0bootdelay=0\0arch=arm\0\0"));
        assert!(bank[ENV_SIZE..].iter().all(|&byte| byte == 0xff));
    }

    #[test]
    fn a_binary_without_one_gets_none() {
        assert!(bank(b"no environment here").is_none());
        assert!(bank(b"\0bootcmd=x\0\0").is_none());
    }
}
