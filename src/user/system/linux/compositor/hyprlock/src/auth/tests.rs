//! The `authd` client against a pretend `authd` on the far end of a socket
//! pair, which answers as the real one's `hyprlock` policy does.

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrix_auth_client::Connection;
use ferrix_auth_proto::{MAX_RECORD, Record};

use super::{
    Account, Backend, Missing, NO_SERVICE, Next, Prompt, REJECTED, Secret, Service, Verdict,
    account,
};

/// What the pretend `authd` knows.
#[derive(Clone, Copy)]
struct Pretend {
    password: &'static [u8],
    credential: bool,
    /// An INFO sent before every FAILED, as a throttled account has.
    info: Option<&'static str>,
}

/// A connected pair: the client's end, and a thread answering on the other.
fn pair(pretend: Pretend, opened: &Arc<AtomicUsize>) -> std::io::Result<Connection> {
    let mut fds = [0; 2];
    #[expect(
        unsafe_code,
        reason = "AUDIT: socketpair(2) is not in std; a test's two ends of a seqpacket pair"
    )]
    // SAFETY: `fds` has room for the two descriptors socketpair writes.
    let status =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[expect(unsafe_code, reason = "AUDIT: taking the pair socketpair just made")]
    // SAFETY: the descriptor was just made and nothing else owns it.
    let client = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    #[expect(unsafe_code, reason = "AUDIT: taking the pair socketpair just made")]
    // SAFETY: as above, for the other end.
    let server = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    let _ = opened.fetch_add(1, Ordering::SeqCst);
    let _ = std::thread::spawn(move || serve(&Connection::from_fd(server), pretend));
    Ok(Connection::from_fd(client))
}

/// One connection's worth of the real `authd`'s answers.
fn serve(connection: &Connection, pretend: Pretend) {
    let mut buffer = [0_u8; MAX_RECORD + 1];
    let Ok(first) = connection.receive(&mut buffer) else {
        return;
    };
    match first {
        Record::Status { .. } => {
            let _ = connection.send(&Record::State {
                credential: pretend.credential,
                methods: 1,
                throttled_ms: 0,
            });
        }
        Record::Begin { service, .. } => {
            assert_eq!(service, "hyprlock");
            if !pretend.credential {
                let _ = connection.send(&Record::Unavailable("no password is set for root"));
                return;
            }
            let _ = connection.send(&Record::Prompt {
                visible: false,
                text: "Password: ",
            });
            let mut buffer = [0_u8; MAX_RECORD + 1];
            let Ok(Record::Respond(answer)) = connection.receive(&mut buffer) else {
                return;
            };
            if answer.bytes() == pretend.password {
                let _ = connection.send(&Record::Accepted {
                    uid: 0,
                    account: "root",
                });
            } else {
                if let Some(info) = pretend.info {
                    let _ = connection.send(&Record::Info(info));
                }
                let _ = connection.send(&Record::Failed {
                    retry_after_ms: 0,
                    text: REJECTED,
                });
            }
        }
        _ => {}
    }
}

fn service(pretend: Pretend) -> (Service, Arc<AtomicUsize>) {
    let opened = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&opened);
    (
        Service::with_opener(Box::new(move || pair(pretend, &counted))),
        opened,
    )
}

const ROOT: Pretend = Pretend {
    password: b"gatez",
    credential: true,
    info: None,
};

fn secret(text: &str) -> Secret {
    Secret::from_bytes(text.as_bytes()).expect("a short secret")
}

#[test]
fn a_wrong_password_then_the_right_one() {
    let (backend, opened) = service(ROOT);
    assert_eq!(backend.ready(), Ok(()));
    assert_eq!(
        backend.begin(),
        Ok(Prompt {
            text: "Password: ".to_owned(),
            secret: true,
        })
    );
    assert_eq!(
        backend.respond(&secret("wrong")),
        Next::Verdict(Verdict::Failed {
            text: REJECTED.to_owned(),
            retry_after_ms: 0,
        })
    );
    // A verdict closes the conversation: an answer now has nowhere to go.
    assert!(matches!(
        backend.respond(&secret("gatez")),
        Next::Verdict(Verdict::Unavailable(_))
    ));
    assert!(backend.begin().is_ok());
    assert_eq!(
        backend.respond(&secret("gatez")),
        Next::Verdict(Verdict::Accepted)
    );
    // STATUS, and one connection a conversation.
    assert_eq!(opened.load(Ordering::SeqCst), 3);
}

#[test]
fn no_credential_is_no_lock() {
    let (backend, _) = service(Pretend {
        credential: false,
        ..ROOT
    });
    let Err(Verdict::Unavailable(text)) = backend.ready() else {
        panic!("ready with no credential");
    };
    assert!(text.starts_with("no password is set for "), "{text}");
    assert!(text.ends_with(": run `passwd` first"), "{text}");
    assert_eq!(
        backend.begin(),
        Err(Verdict::Unavailable(
            "no password is set for root".to_owned()
        ))
    );
}

#[test]
fn an_info_before_a_refusal_is_what_shows() {
    let (backend, _) = service(Pretend {
        info: Some("wait 16 s"),
        ..ROOT
    });
    assert!(backend.begin().is_ok());
    assert_eq!(
        backend.respond(&secret("nope")),
        Next::Verdict(Verdict::Failed {
            text: "wait 16 s".to_owned(),
            retry_after_ms: 0,
        })
    );
}

#[test]
fn no_authd_is_no_service() {
    let backend = Service::with_opener(Box::new(|| {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }));
    assert_eq!(
        backend.ready(),
        Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
    );
    assert_eq!(
        backend.begin(),
        Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
    );
    assert_eq!(
        Missing.begin(),
        Err(Verdict::Unavailable(NO_SERVICE.to_owned()))
    );
}

#[test]
fn the_account_is_the_uid_s_line() {
    let dir = std::env::temp_dir().join(format!("hyprlock-auth-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("etc")).expect("a test directory");
    std::fs::write(
        dir.join("etc/passwd"),
        "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:Ferrix User:/home/ferrix:/bin/zsh\n",
    )
    .expect("a test file");
    let found = (account(&dir, 1000), account(&dir, 0), account(&dir, 7));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        found.0,
        Some(Account {
            name: "ferrix".to_owned(),
            gecos: "Ferrix User".to_owned(),
        })
    );
    assert_eq!(found.1.map(|account| account.name), Some("root".to_owned()));
    assert_eq!(found.2, None);
}
