//! `--ssh <port>`: a watched boot you can log in to.
//!
//! `run-compositor --ssh 2222` forwards the host's `127.0.0.1:2222` to the
//! guest's port 22 and starts `sshdt` there from the compositor's own
//! `exec-once`, so `ssh -p 2222 root@127.0.0.1` reaches the guest as soon as
//! it is up. The server is the one `cargo xtask ports` builds
//! (`ferrousli/tools/ports/sshdt`); without it the forward leads nowhere and
//! the boot says so.
//!
//! # Who may log in
//!
//! The keys that may already log in to the machine running QEMU: its user's
//! `~/.ssh/authorized_keys`, and the public halves beside it in `~/.ssh`. The
//! forward only listens on that machine's loopback, so anybody who reaches it
//! is on that machine already; the keys make them prove they are its user.
//! It is also what lets `remote-desktop` work without a key of its own: the
//! key this PC logs in to the remote with is in the remote's
//! `authorized_keys`, and so in the guest's.
//!
//! With no key found, `sshdt` is not started at all. Given no key and no
//! password it accepts anyone, and a server that lets anyone in is not the
//! quiet failure a missing key should be.
//!
//! # The host key
//!
//! Kept on the machine running QEMU, at
//! `~/.local/share/ferrix/ssh/ssh_host_ed25519_key`, made with `ssh-keygen`
//! the first time. The initramfs is rebuilt for every boot, so a key the
//! guest made itself would be a new one each time, and `ssh` would refuse the
//! second boot for having changed its key.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::args::Args;
use crate::ports::{Content, File};
use crate::{Error, Result};

/// The port `sshdt` listens on in the guest.
pub(crate) const GUEST_PORT: u16 = 22;

/// Where the server is in the image, as `ports` installs it.
const SERVER_PATH: &str = "bin/sshdt";

/// Where the keys allowed to log in go in the image.
const KEYS_PATH: &str = "etc/ferrix/authorized_keys";

/// Where the host key goes in the image.
const HOST_KEY_PATH: &str = "etc/ferrix/ssh_host_ed25519_key";

/// `args` without `--ssh` when its port is already taken on this machine.
///
/// A `--forward` that cannot listen stops the run, because it was the point
/// of the run. `--ssh` on a desktop is a convenience beside the screen, and
/// `remote-desktop` asks for it on every boot, so a port somebody else holds
/// -- on the machine this was written on, a container publishing 2222 --
/// costs the SSH and says so, and the desktop comes up anyway.
pub(crate) fn checked(args: &Args) -> Args {
    let Some(port) = args.ssh else {
        return args.clone();
    };
    if std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok() {
        return args.clone();
    }
    println!("  ssh: 127.0.0.1:{port} is taken on this machine, so this boot has no SSH");
    println!("    --ssh <another port>, or [ssh] port for remote-desktop");
    Args {
        ssh: None,
        forwards: args
            .forwards
            .iter()
            .copied()
            .filter(|forward| (forward.host, forward.guest) != (port, GUEST_PORT))
            .collect(),
        ..args.clone()
    }
}

/// `config` with `sshdt` started, and what it needs added to `carried`, when
/// `--ssh` asked for it.
///
/// The server binds every address, because what it is reached through is the
/// gateway, which opens its connections to `10.0.2.15` and not to the
/// guest's loopback. It is started before the lease is taken, and that is
/// fine: a socket bound to `0.0.0.0` answers on an address that arrives
/// after it.
///
/// # Errors
///
/// A key file that exists and cannot be read, or a host key that could not
/// be made.
pub(crate) fn with_server(config: String, args: &Args, carried: &mut Vec<File>) -> Result<String> {
    let Some(port) = args.ssh else {
        return Ok(config);
    };
    if !carried.iter().any(|file| file.path == SERVER_PATH) {
        println!("  ssh: no sshdt in the image, so 127.0.0.1:{port} leads nowhere");
        println!("    `cargo xtask ports` builds it");
        return Ok(config);
    }
    let home = std::env::home_dir()
        .ok_or_else(|| Error::new("--ssh: no home directory to find the keys in"))?;
    let (keys, count) = authorized_keys(&home.join(".ssh"))?;
    if count == 0 {
        println!(
            "  ssh: no public keys in {}, so sshdt is not started: it would let anyone in",
            home.join(".ssh").display()
        );
        return Ok(config);
    }
    let host_key = host_key(&home)?;
    carried.push(File {
        path: KEYS_PATH.to_owned(),
        mode: 0o644,
        content: Content::Bytes(keys),
    });
    carried.push(File {
        path: HOST_KEY_PATH.to_owned(),
        mode: 0o600,
        content: Content::Bytes(host_key),
    });
    let mut config = config;
    if !config.ends_with('\n') {
        config.push('\n');
    }
    config.push_str("# Appended by `cargo xtask run-compositor --ssh`:\n");
    config.push_str(&format!(
        "exec-once = /{SERVER_PATH} -b 0.0.0.0 -p {GUEST_PORT} -h /{HOST_KEY_PATH} \
         --authorized-keys /{KEYS_PATH}\n"
    ));
    println!(
        "  ssh: sshdt on the guest's port {GUEST_PORT}, reached at 127.0.0.1:{port}; {count} keys from {}",
        home.join(".ssh").display()
    );
    Ok(config)
}

/// Every public key in `authorized_keys` and in the `*.pub` files beside it,
/// once each, as one `authorized_keys` file, and how many there are.
///
/// A line is a key when it is not blank and not a comment; one with options
/// in front of it is kept whole, as `sshd` would read it.
fn authorized_keys(dir: &Path) -> Result<(Vec<u8>, usize)> {
    let mut sources = vec![dir.join("authorized_keys")];
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut public: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "pub"))
            .collect();
        public.sort();
        sources.extend(public);
    }
    let mut lines: Vec<String> = Vec::new();
    for source in sources {
        let text = match std::fs::read_to_string(&source) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::new(format!("reading {}: {error}", source.display())));
            }
        };
        for line in text.lines().map(str::trim) {
            if !line.is_empty() && !line.starts_with('#') && !lines.iter().any(|kept| kept == line)
            {
                lines.push(line.to_owned());
            }
        }
    }
    let count = lines.len();
    let mut file = lines.join("\n");
    file.push('\n');
    Ok((file.into_bytes(), count))
}

/// The guest's host key, made with `ssh-keygen` the first time.
fn host_key(home: &Path) -> Result<Vec<u8>> {
    let dir = [".local", "share", "ferrix", "ssh"]
        .iter()
        .fold(home.to_path_buf(), |dir, name| dir.join(name));
    let path = dir.join("ssh_host_ed25519_key");
    if !path.is_file() {
        std::fs::create_dir_all(&dir)
            .map_err(|error| Error::new(format!("creating {}: {error}", dir.display())))?;
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "ferrix-guest", "-f"])
            .arg(&path)
            .status()
            .map_err(|error| Error::new(format!("--ssh: `ssh-keygen` would not start: {error}")))?;
        if !status.success() {
            return Err(Error::new(format!(
                "--ssh: `ssh-keygen` could not make {} ({status})",
                path.display()
            )));
        }
        println!("  ssh: made the guest's host key, {}", path.display());
    }
    std::fs::read(&path).map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own under the system's temporary one.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferrix-ssh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keys_are_gathered_once_each_without_comments() {
        let dir = scratch("keys");
        std::fs::write(
            dir.join("authorized_keys"),
            "# a comment\nssh-rsa AAAA one@a\n\nssh-ed25519 BBBB two@b\n",
        )
        .unwrap();
        std::fs::write(dir.join("id_ed25519.pub"), "ssh-ed25519 BBBB two@b\n").unwrap();
        std::fs::write(dir.join("id_other.pub"), "ssh-ed25519 CCCC three@c\n").unwrap();
        std::fs::write(dir.join("id_ed25519"), "PRIVATE, never read\n").unwrap();
        let (file, count) = authorized_keys(&dir).unwrap();
        assert_eq!(count, 3, "one line per key, the repeated one once");
        assert_eq!(
            String::from_utf8(file).unwrap(),
            "ssh-rsa AAAA one@a\nssh-ed25519 BBBB two@b\nssh-ed25519 CCCC three@c\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_keys_is_a_count_of_none() {
        let dir = scratch("none");
        let (_, count) = authorized_keys(&dir).unwrap();
        assert_eq!(count, 0, "an empty ~/.ssh authorizes nobody");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taken_port_costs_the_ssh_and_not_the_boot() {
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        let args = Args::parse(
            [
                "run-compositor",
                "--ssh",
                &port.to_string(),
                "--forward",
                "8080:80",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        let kept = checked(&args);
        assert_eq!(kept.ssh, None, "no SSH on a port somebody else holds");
        assert_eq!(
            kept.forwards,
            [crate::gateway::Forward {
                host: 8080,
                guest: 80
            }],
            "and no forward for it, while the others stay"
        );
        drop(holder);
        assert_eq!(checked(&args).ssh, Some(port), "a free port is kept");
    }

    #[test]
    fn without_the_server_nothing_is_started() {
        let args = Args {
            ssh: Some(2222),
            ..Args::default()
        };
        let mut carried = Vec::new();
        let config =
            with_server("exec-once = /bin/term\n".to_owned(), &args, &mut carried).unwrap();
        assert_eq!(
            config, "exec-once = /bin/term\n",
            "no sshdt, no line for it"
        );
        assert!(carried.is_empty(), "and no keys carried for it");
    }
}
