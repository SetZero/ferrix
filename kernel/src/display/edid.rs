//! The EDID each connector carries: the file `drm.edid_firmware=` names for
//! it, read from `/lib/firmware` when the card is taken up.
//!
//! `docs/DISPLAY.md` §7 says why here and not in the driver. Linux's DRM
//! core does the same in `drm_edid_load.c`; the grammar and the checks are
//! `ferrix_displayctl::edid`'s, host-tested, and this is the reading and the
//! boot lines. A connector the command line names no file for, or whose file
//! cannot be read or is not an EDID, has none, which is what every connector
//! had before: the card never refuses a driver over it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use ferrix_bootinfo::option_in;
use ferrix_displayctl::edid::{self, FIRMWARE_DIR, PARAMETER};
use ferrix_sysfs::drm::connector_type_name;

use crate::console::println;
use crate::fs;

/// A connector's name, as Linux's `connector->name` is: the type's and its
/// number among the card's connectors of that type, `Virtual-1`.
pub(crate) fn connector_name(kind: u32, head: usize) -> String {
    format!("{}-{}", connector_type_name(kind), head.saturating_add(1))
}

/// Each head's EDID, for the `heads` connectors of type `kind` on card
/// `card`: what the command line's `drm.edid_firmware` names for each, read
/// and checked, with a boot line for each file named.
pub(crate) fn load(card: u32, kind: u32, heads: usize) -> Vec<Option<Vec<u8>>> {
    let setting = core::str::from_utf8(fs::procfs::command_line())
        .ok()
        .and_then(|line| option_in(line, PARAMETER));
    (0..heads)
        .map(|head| {
            let connector = connector_name(kind, head);
            let file = setting.and_then(|setting| edid::firmware_for(setting, &connector))?;
            let found = read(file);
            match &found {
                Ok((bytes, checked)) => {
                    let said = edid::identity(bytes).map(|monitor| {
                        let text = |part: Option<edid::Text>| {
                            part.map(|text| format!(" {}", text.as_str()))
                                .unwrap_or_default()
                        };
                        format!(
                            "{}{}{}",
                            monitor.manufacturer(),
                            text(monitor.name),
                            text(monitor.serial)
                        )
                    });
                    println!(
                        "  display  card{card} {connector}: EDID from \"{file}\": {}, {} \
                         extension{}{}{}",
                        said.as_deref().unwrap_or("?"),
                        checked.extensions,
                        if checked.extensions == 1 { "" } else { "s" },
                        if checked.dropped > 0 {
                            format!(", {} dropped as not valid", checked.dropped)
                        } else {
                            String::new()
                        },
                        if checked.repaired {
                            ", its header put right"
                        } else {
                            ""
                        }
                    );
                }
                Err(why) => println!(
                    "  display  card{card} {connector}: {PARAMETER} names \"{file}\", {why}; \
                     the connector has no EDID"
                ),
            }
            found.ok().map(|(bytes, _)| bytes)
        })
        .collect()
}

/// Read `/lib/firmware/<file>` and check it, or say why not.
fn read(file: &str) -> Result<(Vec<u8>, edid::Checked), String> {
    let path = format!("{FIRMWARE_DIR}{file}");
    let mut bytes = fs::read_file(&fs::namespace().context(), None, path.as_bytes())
        .map_err(|error| format!("which cannot be read (err=-{})", error.0))?;
    let checked = edid::check(&mut bytes).map_err(|why| format!("which is not an EDID: {why}"))?;
    bytes.truncate(checked.len);
    Ok((bytes, checked))
}
