//! The `--everything` desktop as a session of the user `ferrix`
//! (`docs/AUTH.md` §6, customer 2026-10-03).
//!
//! `init`'s `hyprix.service` starts `/bin/sessiond --user ferrix --
//! /bin/hyprix ...` instead of the compositor itself. `sessiond` is root and
//! owns seat0's devices; the compositor, and every program it starts, run as
//! `ferrix` (`src/user/system/linux/compositor/sessiond`).
//!
//! What the desktop's configuration started as root and has to stay root
//! becomes a unit of `graphical.target` here instead of an `exec-once`:
//! `sshdt`, which listens on port 22 with a host key only root reads, and
//! `udhcpc`, which configures the interface. `data-home.service` gives
//! `/data/home` and `/data/steam` to the user, as Steam's desktop script did
//! as root.
//!
//! A desktop that runs as root points its clients' `XDG_RUNTIME_DIR` at
//! `/tmp` (`crate::chrome::DESKTOP_ENV`), since root's session has no runtime
//! directory of its own. A session has one, `/run/user/<uid>`, where the
//! compositor puts its sockets: the line is taken out, or every client would
//! look for the compositor in `/tmp` and `hyprctl` and waybar's workspaces
//! would find nothing.
//!
//! The host's own `hyprland.conf`, which `--everything` reads, is copied to
//! `~/.config/hypr` with the rest, and `/etc/hyprland.conf` sources that copy
//! where the host's text was (`source = ~/.config/hypr/hyprland.conf`), with
//! xtask's own lines before and after it as they were: an edit made to it in
//! Ferrix is what the next start reads.
//!
//! The carried dotfiles go to `/etc/skel` instead of root's home, and
//! `sessiond` copies each into `/home/ferrix` the first time, so an edit made
//! in Ferrix is kept; `--reset-flash` starts the home over and the next boot
//! copies them again. A carried file under `home/`, which a host's
//! configuration names by its host path, goes to [`HOME_COPY`] instead,
//! since the home disk's mount hides the archive's `/home`; `sessiond` puts
//! it back in place at every boot.

use crate::ports::{Content, File};

/// The account the session runs as.
pub(crate) const USER: &str = "ferrix";

/// What `sessiond` seeds the account's home from, once a file.
pub(crate) const SKEL: &str = "etc/skel";

/// What `sessiond` copies into `/home` at every boot.
pub(crate) const HOME_COPY: &str = "usr/share/ferrix/home";

/// The programs a desktop's configuration starts that must stay root, by the
/// path its `exec-once` names: the unit each becomes, and its type.
const ROOT_PROGRAMS: [(&str, &str, &str, &str); 2] = [
    ("/bin/sshdt", "sshd", "simple", "The ssh server"),
    (
        "/bin/udhcpc",
        "net",
        "oneshot",
        "The interface's address, by DHCP",
    ),
];

/// Where the host's `hyprland.conf` is carried, among the dotfiles, and what
/// `/etc/hyprland.conf` sources in its place.
const USER_CONFIG: &str = ".config/hypr/hyprland.conf";

/// The script `data-home.service` runs.
const DATA_HOME: &str = "#!/bin/sh\n\
# Give the volume's home and Steam's tree to the session's user, once: what\n\
# Steam's desktop script did as root before the session ran as the user.\n\
for dir in /data/home /data/steam; do\n\
\t[ -d \"$dir\" ] || continue\n\
\t[ \"$(stat -c %u \"$dir\")\" = 1000 ] && continue\n\
\tchown -R 1000:1000 \"$dir\" && echo \"data-home: $dir is ferrix's\"\n\
done\n\
exit 0\n";

/// Where [`DATA_HOME`] goes.
const DATA_HOME_PATH: &str = "etc/ferrix/data-home.sh";

/// `config` and `ports` made into a session's: the root programs' lines
/// taken out and made units, the units for them and for `/data`'s home
/// added, the dotfiles moved to [`SKEL`] and [`HOME_COPY`], and `host`, the
/// host's `hyprland.conf` as `config` carries it, sourced from the user's
/// copy instead when that copy is carried.
pub(crate) fn for_session(config: &str, ports: &mut Vec<File>, host: Option<&str>) -> String {
    let host = host
        .map(str::trim_end)
        .filter(|text| !text.is_empty() && config.contains(text))
        .filter(|_| ports.iter().any(|file| file.path == USER_CONFIG));
    let config = match host {
        Some(text) => config.replacen(
            text,
            &format!(
                "# The user's own configuration, seeded once from the host's and kept \
                 (tools/common/xtask/src/session.rs).\nsource = ~/{USER_CONFIG}"
            ),
            1,
        ),
        None => config.to_owned(),
    };
    let mut kept = String::new();
    for line in config.lines() {
        let setting = line.trim().strip_prefix("env").map(str::trim_start);
        if setting.is_some_and(|rest| {
            rest.trim_start_matches('=')
                .trim_start()
                .starts_with("XDG_RUNTIME_DIR,")
        }) {
            kept.push_str(&format!(
                "# {line} -- the session's own, /run/user/<uid>, is kept\n"
            ));
            continue;
        }
        let command = line
            .trim()
            .strip_prefix("exec-once")
            .map(|rest| rest.trim_start().trim_start_matches('=').trim());
        let root = command.and_then(|command| {
            ROOT_PROGRAMS
                .iter()
                .find(|(path, ..)| command.split_whitespace().next() == Some(path))
                .map(|program| (command, program))
        });
        match root {
            Some((command, (_, name, kind, description))) => {
                kept.push_str(&format!(
                    "# {line} -- root's, so init's {name}.service runs it\n"
                ));
                ports.extend(unit(name, kind, description, command));
            }
            None => {
                kept.push_str(line);
                kept.push('\n');
            }
        }
    }
    ports.push(File {
        path: DATA_HOME_PATH.to_owned(),
        mode: 0o755,
        content: Content::Bytes(DATA_HOME.as_bytes().to_vec()),
    });
    ports.extend(unit(
        "data-home",
        "oneshot",
        "The data volume's home, the session user's",
        &format!("/bin/busybox sh /{DATA_HOME_PATH}"),
    ));
    for file in ports.iter_mut() {
        if file.path.starts_with(".config/") || file.path == ".config" {
            file.path = format!("{SKEL}/{}", file.path);
        } else if let Some(rest) = file.path.strip_prefix("home/") {
            file.path = format!("{HOME_COPY}/{rest}");
        }
    }
    kept
}

/// A unit of `graphical.target` running `command` as root, before the
/// compositor's, and the link that makes the target want it.
fn unit(name: &str, kind: &str, description: &str, command: &str) -> [File; 2] {
    let text = format!(
        "# Root's part of the desktop, which its session runs as {USER} \
         (tools/common/xtask/src/session.rs).\n\
         [Unit]\n\
         Description={description}\n\
         Before=hyprix.service\n\
         \n\
         [Service]\n\
         Type={kind}\n\
         ExecStart={command}\n\
         StandardOutput=console\n\
         StandardError=console\n"
    );
    [
        File {
            path: format!("etc/ferrix/units/{name}.service"),
            mode: 0o644,
            content: Content::Bytes(text.into_bytes()),
        },
        File {
            path: format!("etc/ferrix/units/graphical.target.wants/{name}.service"),
            mode: 0o777,
            content: Content::Link(format!("/etc/ferrix/units/{name}.service")),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(file: &File) -> String {
        match &file.content {
            Content::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            _ => String::new(),
        }
    }

    #[test]
    fn the_hosts_config_is_sourced_from_the_users_copy() {
        let host = "$mainMod = SUPER\nbind = $mainMod, Q, exec, foot\n";
        let config = format!(
            "monitor = ,1920x1080,auto,1\n{}\nexec-once = /bin/vdagent\n",
            host.trim_end()
        );
        let mut ports = vec![File {
            path: USER_CONFIG.to_owned(),
            mode: 0o644,
            content: Content::Bytes(host.as_bytes().to_vec()),
        }];
        let kept = for_session(&config, &mut ports, Some(host));
        assert!(kept.contains("source = ~/.config/hypr/hyprland.conf\n"));
        assert!(!kept.contains("bind = $mainMod, Q"));
        assert!(kept.starts_with("monitor = ,1920x1080,auto,1\n"));
        assert!(kept.contains("exec-once = /bin/vdagent\n"));
        assert!(
            ports
                .iter()
                .any(|file| file.path == "etc/skel/.config/hypr/hyprland.conf")
        );
        // Not carried: the text stays as it was.
        let mut none = Vec::new();
        assert!(for_session(&config, &mut none, Some(host)).contains("bind = $mainMod, Q"));
    }

    #[test]
    fn root_programs_become_units_and_the_rest_stays() {
        let config = "env = XDG_RUNTIME_DIR,/tmp\n\
                      env = FONTCONFIG_FILE,/usr/share/ferrix/fonts/fonts.conf\n\
                      exec-once = /bin/term /bin/zinc\n\
                      exec-once = /bin/sshdt -b 0.0.0.0 -p 22\n\
                      exec-once = /bin/udhcpc -i eth0 -n -q\n\
                      exec-once = /bin/vdagent\n";
        let mut ports = vec![
            File {
                path: ".config/waybar/config.jsonc".to_owned(),
                mode: 0o644,
                content: Content::Bytes(b"{}".to_vec()),
            },
            File {
                path: "home/sebastian/.local/bin/hypr-launcher".to_owned(),
                mode: 0o755,
                content: Content::Bytes(b"#!/bin/sh\n".to_vec()),
            },
        ];
        let kept = for_session(config, &mut ports, None);
        assert!(kept.contains("exec-once = /bin/term /bin/zinc\n"));
        assert!(kept.contains("exec-once = /bin/vdagent\n"));
        assert!(
            !kept
                .lines()
                .any(|line| line.starts_with("env = XDG_RUNTIME_DIR"))
        );
        assert!(kept.contains("env = FONTCONFIG_FILE,"));
        assert!(
            !kept
                .lines()
                .any(|line| line.starts_with("exec-once = /bin/sshdt"))
        );
        assert!(
            !kept
                .lines()
                .any(|line| line.starts_with("exec-once = /bin/udhcpc"))
        );
        let sshd = ports
            .iter()
            .find(|file| file.path == "etc/ferrix/units/sshd.service")
            .expect("sshd.service");
        assert!(bytes(sshd).contains("ExecStart=/bin/sshdt -b 0.0.0.0 -p 22\n"));
        let net = ports
            .iter()
            .find(|file| file.path == "etc/ferrix/units/net.service")
            .expect("net.service");
        assert!(bytes(net).contains("Type=oneshot\n"));
        assert!(
            ports
                .iter()
                .any(|file| file.path == "etc/skel/.config/waybar/config.jsonc")
        );
        assert!(ports.iter().any(
            |file| file.path == "usr/share/ferrix/home/sebastian/.local/bin/hypr-launcher"
        ));
        assert!(
            ports.iter().any(
                |file| file.path == "etc/ferrix/units/graphical.target.wants/data-home.service"
            )
        );
    }
}
