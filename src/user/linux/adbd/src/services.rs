//! The services a host can open a stream to, each on a thread of its own.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;

use ferrix_adb::sync::{self, Reader};

use crate::{Options, Outgoing};

/// The shell a `shell:` runs.
const SHELL: &str = "/bin/sh";

/// How much of a file or a program's output goes in one write.
const CHUNK: usize = sync::DATA_MAX;

/// Start the service `name` names on its stream, and say whether there is
/// one.
pub(crate) fn start(
    name: &str,
    out: Outgoing,
    from_host: Receiver<Vec<u8>>,
    options: Options,
) -> bool {
    let name = name.to_owned();
    let service: Box<dyn FnOnce() -> io::Result<()> + Send> =
        if let Some(command) = name.strip_prefix("shell:") {
            let command = command.to_owned();
            if command.is_empty() {
                Box::new(move || interactive(&out, from_host))
            } else {
                Box::new(move || shell(&command, &out, from_host))
            }
        } else if name == "sync:" {
            Box::new(move || sync_session(&out, &from_host))
        } else if name.starts_with("reboot:") {
            Box::new(move || reboot(&out, options))
        } else if let Some(port) = name.strip_prefix("tcp:") {
            let Ok(port) = port.parse::<u16>() else {
                return false;
            };
            Box::new(move || forward(port, &out, from_host))
        } else {
            return false;
        };
    let _running = thread::spawn(move || {
        if let Err(error) = service() {
            println!("adbd: {name}: {error}");
        }
    });
    true
}

/// Copy what the host sends into `sink` until the host closes the stream.
fn feed(from_host: Receiver<Vec<u8>>, mut sink: impl Write + Send + 'static) {
    let _feeding = thread::spawn(move || {
        for bytes in from_host {
            if sink.write_all(&bytes).is_err() {
                break;
            }
        }
    });
}

/// Copy `source` to the host until it ends, then close the stream.
fn drain(mut source: impl Read, out: &Outgoing) -> io::Result<()> {
    let mut buffer = vec![0; CHUNK];
    loop {
        let count = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            // A pty's master reads EIO once its last slave closes.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        out.write(buffer.get(..count).unwrap_or_default())?;
    }
    out.close();
    Ok(())
}

/// `shell:<command>`: the command under `/bin/sh -c`, its output and errors
/// both sent back as they come, what the host types given to it.
fn shell(command: &str, out: &Outgoing, from_host: Receiver<Vec<u8>>) -> io::Result<()> {
    let (reader, writer) = pipe()?;
    let errors = writer.try_clone()?;
    let mut child = Command::new(SHELL)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(errors))
        .spawn()?;
    if let Some(stdin) = child.stdin.take() {
        feed(from_host, stdin);
    }
    let result = drain(File::from(reader), out);
    let _ = child.wait();
    result
}

/// An interactive `shell:`: a login shell on a pseudo-terminal of its own.
fn interactive(out: &Outgoing, from_host: Receiver<Vec<u8>>) -> io::Result<()> {
    let (master, slave) = pty()?;
    let mut command = Command::new(SHELL);
    let _ = command
        .arg("-i")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: between fork and exec only async-signal-safe calls: a new
    // session, and the pty that stdin now is as its controlling terminal.
    unsafe {
        let _ = command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // The command holds the slave's copies until it goes, and while one is
    // open here the master never reads the end of the session.
    drop(command);
    let master = File::from(master);
    feed(from_host, master.try_clone()?);
    let result = drain(master, out);
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// A pipe, as two owned ends: read, write.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just made and are owned by no one else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// A new pseudo-terminal: its master, and its slave opened.
fn pty() -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: plain calls on a descriptor this function owns.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(master);
        if libc::grantpt(master.as_raw_fd()) < 0 || libc::unlockpt(master.as_raw_fd()) < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 64];
        if libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, OwnedFd::from_raw_fd(slave)))
    }
}

/// `sync:`: requests until `QUIT` or the host goes.
fn sync_session(out: &Outgoing, from_host: &Receiver<Vec<u8>>) -> io::Result<()> {
    let mut reader = Reader::new();
    loop {
        let request = loop {
            match reader.next_request() {
                Ok(Some(request)) => break request,
                Ok(None) => match from_host.recv() {
                    Ok(bytes) => reader.push(&bytes),
                    Err(_) => return Ok(()),
                },
                Err(error) => {
                    out.write(&sync::fail(&format!("{error:?}")))?;
                    out.close();
                    return Ok(());
                }
            }
        };
        let path = String::from_utf8_lossy(&request.data).into_owned();
        match &request.id {
            b"STAT" => out.write(&stat(&path))?,
            b"LIST" => list(&path, out)?,
            b"RECV" => recv(&path, out)?,
            b"SEND" => send(&request.data, &mut reader, from_host, out)?,
            b"QUIT" => {
                out.close();
                return Ok(());
            }
            _ => {
                out.write(&sync::fail("adbd does not know that request"))?;
                out.close();
                return Ok(());
            }
        }
    }
}

/// A path's `STAT` answer, all zero when it cannot be stat'ed.
fn stat(path: &str) -> Vec<u8> {
    match fs::symlink_metadata(path) {
        Ok(meta) => sync::stat(
            meta.mode(),
            u32::try_from(meta.size()).unwrap_or(u32::MAX),
            u32::try_from(meta.mtime()).unwrap_or(0),
        ),
        Err(_) => sync::stat(0, 0, 0),
    }
}

/// A directory's entries, then the end of the listing.
fn list(path: &str, out: &Outgoing) -> io::Result<()> {
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name();
            out.write(&sync::dent(
                meta.mode(),
                u32::try_from(meta.size()).unwrap_or(u32::MAX),
                u32::try_from(meta.mtime()).unwrap_or(0),
                std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str()),
            ))?;
        }
    }
    out.write(&sync::list_done())
}

/// Send a file back, or why not.
fn recv(path: &str, out: &Outgoing) -> io::Result<()> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return out.write(&sync::fail(&format!("{path}: {error}"))),
    };
    let mut buffer = vec![0; CHUNK];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => return out.write(&sync::fail(&format!("{path}: {error}"))),
        };
        out.write(&sync::data(buffer.get(..count).unwrap_or_default()))?;
    }
    out.write(&sync::recv_done())
}

/// Take a file the host sends: `DATA` until `DONE`, then `OKAY` or why not.
fn send(
    target: &[u8],
    reader: &mut Reader,
    from_host: &Receiver<Vec<u8>>,
    out: &Outgoing,
) -> io::Result<()> {
    let (path, mode) = sync::send_target(target);
    let path = String::from_utf8_lossy(path).into_owned();
    let mut file = File::create(&path);
    loop {
        let request = match reader.next_request() {
            Ok(Some(request)) => request,
            Ok(None) => match from_host.recv() {
                Ok(bytes) => {
                    reader.push(&bytes);
                    continue;
                }
                Err(_) => return Ok(()),
            },
            Err(error) => return out.write(&sync::fail(&format!("{error:?}"))),
        };
        match &request.id {
            b"DATA" => {
                if let Ok(open) = file.as_mut()
                    && let Err(error) = open.write_all(&request.data)
                {
                    file = Err(error);
                }
            }
            b"DONE" => break,
            _ => return out.write(&sync::fail("a SEND was cut short")),
        }
    }
    match file {
        Ok(_) => {
            if let Some(mode) = mode {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o7777));
            }
            out.write(&sync::okay())
        }
        Err(error) => out.write(&sync::fail(&format!("{path}: {error}"))),
    }
}

/// `reboot:`: restart the machine, or under `--test` end adbd.
fn reboot(out: &Outgoing, options: Options) -> io::Result<()> {
    out.close();
    if options.test {
        println!("adbd: the host asked for a reboot; ending (--test)");
        std::process::exit(0);
    }
    println!("adbd: the host asked for a reboot");
    // SAFETY: plain calls; `reboot` returns only if it was refused.
    unsafe {
        libc::sync();
        let _ = libc::reboot(libc::RB_AUTOBOOT);
    }
    Err(io::Error::last_os_error())
}

/// `tcp:<port>`: a stream to that port in Ferrix, both ways.
fn forward(port: u16, out: &Outgoing, from_host: Receiver<Vec<u8>>) -> io::Result<()> {
    let socket = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(socket) => socket,
        Err(error) => {
            out.close();
            return Err(error);
        }
    };
    feed(from_host, socket.try_clone()?);
    drain(socket, out)
}
