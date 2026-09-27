//! `authd` over its socket: the built program, started with a root of its
//! own, and the client library every program uses, so the plumbing -- the
//! listener, `SO_PEERCRED`, the held FAILED, the records on the wire -- is
//! what is tested, not only the rules behind it.

#![allow(
    clippy::unwrap_used,
    reason = "a test's helpers fail it by panicking, as its #[test] functions may"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use ferrix_auth_client::{Answer, Connection, Person, State, Verdict, converse};
use ferrix_auth_proto::{Record, Secret, method};

/// A person who gives one answer to every prompt.
struct Answers(&'static str);

impl Person for Answers {
    fn ask(&mut self, _visible: bool, _text: &str) -> Option<Secret> {
        Secret::from_bytes(self.0.as_bytes())
    }

    fn tell(&mut self, _text: &str, _error: bool) {}
}

struct Running {
    root: PathBuf,
    socket: PathBuf,
    child: Child,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A floor-cost Argon2id of `password`.
fn hash(password: &str) -> String {
    let params = ferrix_argon2::Params {
        memory_kib: 19_456,
        passes: 2,
        lanes: 1,
    };
    let salt = *b"socket-test-salt";
    let mut memory = vec![ferrix_argon2::Block::ZERO; params.blocks().unwrap()];
    let mut tag = [0_u8; 32];
    ferrix_argon2::hash(
        &params,
        &ferrix_argon2::Inputs {
            password: password.as_bytes(),
            salt: &salt,
            secret: &[],
            associated: &[],
        },
        &mut memory,
        &mut tag,
    )
    .unwrap();
    ferrix_argon2::phc::Encoded::new(params, &salt, &tag)
        .unwrap()
        .to_string()
}

/// Start `authd` over a fresh root in which the caller is `tester` with the
/// password `secret`.
fn start(name: &str) -> Running {
    let root = std::env::temp_dir().join(format!("authd-sock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // SAFETY: `getuid` reads the caller's ids and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as for `getuid`.
    let gid = unsafe { libc::getgid() };
    write(
        &root.join("etc/passwd"),
        &format!("root:x:0:0:root:/:/bin/sh\ntester:x:{uid}:{gid}::/:/bin/sh\n"),
    );
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../services/hyprlock");
    write(
        &root.join("lib/ferrix/auth/services/hyprlock"),
        &std::fs::read_to_string(shipped).unwrap(),
    );
    write(
        &root.join("lib/ferrix/auth/seed/tester"),
        &format!("{}\n", hash("secret")),
    );
    let socket = root.join("s");
    let child = Command::new(env!("CARGO_BIN_EXE_authd"))
        .arg("--root")
        .arg(&root)
        .arg("--socket")
        .arg(&socket)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "authd never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    Running {
        root,
        socket,
        child,
    }
}

#[test]
fn a_conversation_over_the_socket() {
    let authd = start("conversation");
    let open = || Connection::open_at(&authd.socket).unwrap();

    // STATUS finds the seed.
    let state = open().request(&Record::Status { account: "" }).unwrap();
    assert_eq!(
        state,
        Answer::State(State {
            credential: true,
            methods: method::PASSWORD,
            throttled_ms: 0
        })
    );

    // The right password.
    let verdict = converse(&open(), "hyprlock", "", &mut Answers("secret")).unwrap();
    assert!(
        matches!(verdict, Verdict::Accepted { ref account, .. } if account == "tester"),
        "{verdict:?}"
    );

    // A wrong one, held for the policy's two seconds.
    let started = Instant::now();
    let verdict = converse(&open(), "hyprlock", "", &mut Answers("guess")).unwrap();
    assert_eq!(
        verdict,
        Verdict::Failed {
            retry_after_ms: 0,
            text: "Authentication failed".to_owned()
        }
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    // SO_PEERCRED said who asked: this test is not root, so it may not name
    // root, and is told so before any prompt.
    let verdict = converse(&open(), "hyprlock", "root", &mut Answers("x")).unwrap();
    assert!(matches!(verdict, Verdict::Unavailable(_)), "{verdict:?}");

    // A packet that is not a record is answered, not crashed on.
    let raw = open();
    let odd = Record::Info("a client may not send this");
    raw.send(&odd).unwrap();
    let mut buffer = [0_u8; ferrix_auth_proto::MAX_RECORD + 1];
    assert!(matches!(
        raw.receive(&mut buffer),
        Ok(Record::Unavailable(_))
    ));

    // The audit log has every attempt, and neither password.
    let log = std::fs::read_to_string(authd.root.join("var/log/ferrix/auth.log")).unwrap();
    assert!(
        log.contains("result=accepted") && log.contains("result=failed"),
        "{log}"
    );
    assert!(!log.contains("secret") && !log.contains("guess"), "{log}");
}
