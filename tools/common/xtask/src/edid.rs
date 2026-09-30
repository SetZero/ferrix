//! A monitor of this machine's, for the guest's screen: its EDID, read from
//! the host's sysfs when the image is built, carried to `/lib/firmware/edid/`
//! and named to the kernel by `drm.edid_firmware=`, as a Linux user gives a
//! screen the EDID of a monitor it cannot read (`docs/DISPLAY.md` §7).
//!
//! What it is for: a `hyprland.conf` names its monitors by description --
//! `monitor = desc:Lenovo Group Limited R27qe Gen2 UTP03KBB, …` -- and so
//! does a bar's `"output"`. Under QEMU the screen has no EDID and so no
//! description, and neither finds it. With the monitor's own EDID it has the
//! monitor's description, and the configuration works unchanged.
//!
//! The monitor is found by its description, never by its connector: which
//! connector a monitor is on (`card2-DP-1`) moves between boots of the host.
//! Every `/sys/class/drm/card*-*/edid` is read and described the way the
//! compositor describes one, with `hwdata`'s PNP registry where the host has
//! it, and the first whose description starts with the one asked for is
//! taken. Neither the EDID nor the registry is ever committed: both are
//! read from the machine the image is built on.
//!
//! A machine without the monitor says so in one line and carries nothing:
//! the screen then has no EDID, as it had before, and its description is
//! only its connector's name, `Virtual-1`.

use std::path::Path;

use ferrix_displayctl::edid;

use crate::{Error, Result};

/// The monitor `run-compositor` looks for when `--edid` is not given: the
/// customer's middle monitor, the one their waybar's bar is on (their
/// choice, 2026-09-26).
pub(crate) const DEFAULT_MONITOR: &str = "Lenovo Group Limited R27qe Gen2";

/// Where the host's connectors are.
pub(crate) const SYSFS_DRM: &str = "/sys/class/drm";

/// `hwdata`'s copy of the PNP registry, which turns `LEN` into `Lenovo Group
/// Limited`: on the host, and at the same place in the guest, where the
/// compositor looks for it (`src/user/system/linux/compositor/drm`'s `registered`).
pub(crate) const REGISTRY: &str = "/usr/share/hwdata/pnp.ids";

/// A monitor found on this machine.
#[derive(Debug, Clone)]
pub(crate) struct Monitor {
    /// The host connector it was read from, `card2-DP-1`: for the log only.
    pub(crate) connector: String,
    /// Its EDID, checked as the kernel will check it.
    pub(crate) bytes: Vec<u8>,
    /// What it calls itself, as the compositor will describe it given the
    /// same registry.
    pub(crate) description: String,
}

/// What an image carries for a monitor: the files, and the word for the
/// kernel's command line.
#[derive(Debug, Clone)]
pub(crate) struct Carried {
    /// `lib/firmware/edid/<name>.bin`, and the registry when the host has one.
    pub(crate) files: Vec<crate::ports::File>,
    /// `drm.edid_firmware=edid/<name>.bin`.
    pub(crate) argument: String,
    /// The monitor's description, as the compositor will give it.
    pub(crate) description: String,
}

/// The EDID `run-compositor` carries, as `--edid` asks: the default monitor
/// or the one named, or none for `none`, saying what it found either way.
pub(crate) fn for_run(asked: Option<&str>) -> Result<Option<Carried>> {
    let wanted = match asked {
        Some("none") => return Ok(None),
        Some(wanted) => wanted,
        None => DEFAULT_MONITOR,
    };
    let registry = std::fs::read_to_string(REGISTRY).ok();
    match find(Path::new(SYSFS_DRM), wanted, registry.as_deref())? {
        Some(monitor) => {
            println!(
                "  edid: the screen is {} (this machine's {}), by drm.edid_firmware{}",
                monitor.description,
                monitor.connector,
                if registry.is_some() {
                    ""
                } else {
                    "; no /usr/share/hwdata/pnp.ids here, so the make is its three-letter code"
                }
            );
            Ok(Some(carry(&monitor, registry)))
        }
        None => {
            println!(
                "  edid: no monitor of this machine describes itself as \"{wanted}\"; the screen \
                 has no EDID and is only Virtual-1 (--edid <description> names another)"
            );
            Ok(None)
        }
    }
}

/// The first monitor under `root` -- `/sys/class/drm` -- whose description
/// starts with `wanted`, described with `registry` where there is one and by
/// its three-letter code too, so a name written either way finds it.
pub(crate) fn find(root: &Path, wanted: &str, registry: Option<&str>) -> Result<Option<Monitor>> {
    let wanted = wanted.trim();
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::new(format!("{}: {error}", root.display()))),
    };
    let mut connectors: Vec<String> = listing
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("card") && name.contains('-'))
        .collect();
    // In order, so which of two identical monitors is taken does not depend
    // on the order the directory lists them in.
    connectors.sort();
    for connector in connectors {
        let Ok(mut bytes) = std::fs::read(root.join(&connector).join("edid")) else {
            continue;
        };
        let Ok(checked) = edid::check(&mut bytes) else {
            continue;
        };
        bytes.truncate(checked.len);
        let (Some(named), Some(coded)) = (describe(&bytes, registry), describe(&bytes, None))
        else {
            continue;
        };
        if !wanted.is_empty() && (named.starts_with(wanted) || coded.starts_with(wanted)) {
            return Ok(Some(Monitor {
                connector,
                bytes,
                description: named,
            }));
        }
    }
    Ok(None)
}

/// What a monitor's EDID says it is, as Hyprland's short description and
/// `src/user/system/linux/compositor/drm`'s `Edid::describe` put it: the make, the model and the
/// serial, with single spaces and no commas. The make is the registry's
/// name for the PNP id where `registry` has it, and the id where not.
pub(crate) fn describe(bytes: &[u8], registry: Option<&str>) -> Option<String> {
    let monitor = edid::identity(bytes)?;
    let code = monitor.manufacturer();
    let make = registry
        .and_then(|text| registered(text, code))
        .unwrap_or_else(|| code.to_owned());
    let model = monitor.name.map_or_else(
        || format!("{:04X}", monitor.product),
        |name| name.as_str().to_owned(),
    );
    let serial = monitor.serial.map_or_else(
        || {
            if monitor.serial_number == 0 {
                String::new()
            } else {
                format!("{:08X}", monitor.serial_number)
            }
        },
        |serial| serial.as_str().to_owned(),
    );
    let described = [make.as_str(), model.as_str(), serial.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(described.replace(',', ""))
}

/// The company `registry` -- `pnp.ids`'s text, a code, a tab and a name a
/// line -- gives for `code`.
fn registered(registry: &str, code: &str) -> Option<String> {
    registry.lines().find_map(|line| {
        let (id, name) = line.split_once('\t')?;
        (id.trim() == code).then(|| name.trim().to_owned())
    })
}

/// What an image carries for `monitor`: its EDID under `/lib/firmware/edid/`,
/// named for the monitor, the registry when there is one, and the kernel's
/// argument naming the file.
pub(crate) fn carry(monitor: &Monitor, registry: Option<String>) -> Carried {
    let name = file_name(&monitor.bytes);
    let mut files = vec![crate::ports::File {
        path: format!("lib/firmware/{name}"),
        mode: 0o644,
        content: crate::ports::Content::Bytes(monitor.bytes.clone()),
    }];
    if let Some(registry) = registry {
        files.push(crate::ports::File {
            path: REGISTRY.trim_start_matches('/').to_owned(),
            mode: 0o644,
            content: crate::ports::Content::Bytes(registry.into_bytes()),
        });
    }
    Carried {
        files,
        argument: format!("{}={name}", edid::PARAMETER),
        description: monitor.description.clone(),
    }
}

/// The file an EDID is kept as, relative to `/lib/firmware`: `edid/`, the
/// PNP id and the model, `edid/LEN-R27qe-Gen2.bin`, with anything but a
/// letter, a digit, a dot or a dash made a dash.
fn file_name(bytes: &[u8]) -> String {
    let stem = describe(bytes, None)
        .and_then(|description| {
            let monitor = edid::identity(bytes)?;
            let serial = monitor.serial.map(|serial| serial.as_str().to_owned());
            Some(match serial {
                Some(serial) => description
                    .strip_suffix(serial.as_str())
                    .unwrap_or(&description)
                    .trim_end()
                    .to_owned(),
                None => description,
            })
        })
        .unwrap_or_else(|| "monitor".to_owned());
    let stem: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("edid/{stem}.bin")
}

/// A monitor no machine has, for a gate on a machine without the one asked
/// for: `FRX`, "Ferrix Test", serial `EDID0001`, one base block with a
/// 1024x768 detailed timing. `FRX` is not in the PNP registry, so its make
/// is the code wherever the gate runs.
pub(crate) fn stand_in() -> Monitor {
    let mut block = [0u8; edid::BLOCK_BYTES];
    block[..8].copy_from_slice(&edid::HEADER);
    // F = 6, R = 18, X = 24, five bits each.
    block[8..10].copy_from_slice(&((6u16 << 10) | (18 << 5) | 24).to_be_bytes());
    block[10..12].copy_from_slice(&0x0001u16.to_le_bytes());
    block[18] = 1;
    block[19] = 4;
    // 1024x768 at 60 Hz, 65 MHz: VESA DMT's timing.
    block[54..72].copy_from_slice(&[
        0x64, 0x19, 0x00, 0x40, 0x41, 0x00, 0x26, 0x30, 0x18, 0x88, 0x36, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x18,
    ]);
    // Each descriptor's text is shorter than its thirteen bytes, so a
    // newline ends it and spaces pad it.
    let text = |tag: u8, text: &[u8]| {
        let mut descriptor = [0x20u8; 18];
        descriptor[..5].copy_from_slice(&[0, 0, 0, tag, 0]);
        for (slot, &byte) in descriptor
            .iter_mut()
            .skip(5)
            .zip(text.iter().chain(&[0x0A]))
        {
            *slot = byte;
        }
        descriptor
    };
    block[72..90].copy_from_slice(&text(0xFC, b"Ferrix Test"));
    block[90..108].copy_from_slice(&text(0xFF, b"EDID0001"));
    block[108..126].copy_from_slice(&text(0xFE, b"stand-in"));
    let sum = block[..127]
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    block[127] = 0u8.wrapping_sub(sum);
    Monitor {
        connector: "none".to_owned(),
        description: describe(&block, None).unwrap_or_default(),
        bytes: block.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY_LINES: &str = "FRX\tNot really\nLEN\tLenovo Group Limited\nDEL\tDell Inc.\n";

    #[test]
    fn a_monitor_is_described_as_the_compositor_describes_it() {
        let monitor = stand_in();
        assert_eq!(monitor.description, "FRX Ferrix Test EDID0001");
        assert_eq!(
            describe(&monitor.bytes, Some(REGISTRY_LINES)).as_deref(),
            Some("Not really Ferrix Test EDID0001")
        );
        assert_eq!(describe(&[0u8; 128], None), None);
        let mut checked = monitor.bytes;
        assert!(edid::check(&mut checked).is_ok(), "the stand-in is an EDID");
    }

    #[test]
    fn a_monitor_is_found_by_the_start_of_its_description_on_any_connector() {
        let root = std::env::temp_dir().join(format!("edid-override-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let monitor = stand_in();
        for (connector, bytes) in [
            ("card1-HDMI-A-1", Vec::new()),
            ("card2-DP-1", vec![1u8; 128]),
            ("card2-DP-3", monitor.bytes.clone()),
            ("card2", monitor.bytes),
        ] {
            std::fs::create_dir_all(root.join(connector)).unwrap();
            std::fs::write(root.join(connector).join("edid"), bytes).unwrap();
        }
        let found = find(&root, "Not really Ferrix", Some(REGISTRY_LINES))
            .unwrap()
            .unwrap();
        assert_eq!(found.connector, "card2-DP-3");
        assert_eq!(found.description, "Not really Ferrix Test EDID0001");
        // The code finds it too, as a host without the registry names it.
        assert!(
            find(&root, "FRX Ferrix", Some(REGISTRY_LINES))
                .unwrap()
                .is_some()
        );
        assert!(
            find(&root, "Lenovo", Some(REGISTRY_LINES))
                .unwrap()
                .is_none()
        );
        assert!(find(&root, "", None).unwrap().is_none());
        assert!(find(&root.join("absent"), "FRX", None).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_an_image_carries_is_the_file_the_argument_names() {
        let carried = carry(&stand_in(), Some(REGISTRY_LINES.to_owned()));
        assert_eq!(
            carried.argument,
            "drm.edid_firmware=edid/FRX-Ferrix-Test.bin"
        );
        let paths: Vec<&str> = carried
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "lib/firmware/edid/FRX-Ferrix-Test.bin",
                "usr/share/hwdata/pnp.ids"
            ]
        );
        assert_eq!(carry(&stand_in(), None).files.len(), 1);
    }
}
