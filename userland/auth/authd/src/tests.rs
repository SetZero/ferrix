//! The engine's rules, driven with no socket: each test builds a root of
//! its own with `/etc/passwd`, the shipped policies, a gate policy and the
//! credentials it needs, and plays one peer or another.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ferrix_auth_proto::{Record, Response, method};

use crate::engine::{Conversation, Engine, FAILED, Out, Peer, Reply};
use crate::password::{FLOOR, Hasher};
use crate::paths::Paths;
use crate::store::{Credential, Store};

const ROOT: Peer = Peer { pid: 1, uid: 0 };
const FERRIX: Peer = Peer { pid: 2, uid: 1000 };
const OTHER: Peer = Peer { pid: 3, uid: 1001 };

/// A root under the temporary directory, with accounts and policies.
struct Machine {
    root: PathBuf,
    engine: Engine,
    now_ms: u64,
}

impl Machine {
    fn new(name: &str) -> Machine {
        let root = std::env::temp_dir().join(format!("authd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(
            &root.join("etc/passwd"),
            "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:ferrix:/home/ferrix:/bin/sh\nother:x:1001:1001::/:/bin/sh\nauth:x:90:90::/:/sbin/nologin\n",
        );
        let services = root.join("lib/ferrix/auth/services");
        for service in ["hyprlock", "passwd", "login"] {
            let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../services")
                .join(service);
            write(
                &services.join(service),
                &std::fs::read_to_string(shipped).unwrap(),
            );
        }
        write(
            &services.join("gate"),
            "[Service]\nAccount=any\nMethods=password\nFailDelaySec=2\n",
        );
        let paths = Paths::under(&root);
        let engine = Engine::new(paths, Hasher::fixed(FLOOR), [9; 32]);
        engine.store().prepare().unwrap();
        Machine {
            root,
            engine,
            now_ms: 1_790_000_000_000,
        }
    }

    /// Give `account` (uid `uid`) the password `password`.
    fn set(&self, account: &str, uid: u32, password: &str) {
        let mut hasher = Hasher::fixed(FLOOR);
        let secret = ferrix_auth_proto::Secret::from_bytes(password.as_bytes()).unwrap();
        let hash = hasher.make(&secret, &mut |_| {}).unwrap();
        Store::new(Paths::under(&self.root))
            .set_credential(&Credential {
                account: account.to_owned(),
                uid,
                password: Some(hash),
                locked: None,
                changed: 0,
            })
            .unwrap();
    }

    /// Run a conversation for `service` and `account` as `peer`, answering
    /// each prompt with the next of `answers`; the replies in order.
    fn converse(&mut self, peer: Peer, service: &str, account: &str, answers: &[&str]) -> Vec<Out> {
        let mut conversation = Conversation::new(peer);
        let mut all = self.engine.handle(
            &mut conversation,
            &Record::Begin {
                service,
                account,
                method: "",
            },
            self.now_ms,
        );
        let mut answers = answers.iter();
        while matches!(
            all.last(),
            Some(Out {
                reply: Reply::Prompt { .. },
                ..
            })
        ) {
            let answer = answers.next().expect("a prompt the test did not expect");
            let more = self.engine.handle(
                &mut conversation,
                &Record::Respond(Response(answer.as_bytes())),
                self.now_ms,
            );
            all.extend(more);
        }
        // A held reply is sent only when its delay has passed, and a client
        // asks again only after it has its answer.
        let held = all.iter().map(|out| out.after).max().unwrap_or_default();
        self.now_ms += u64::try_from(held.as_millis()).unwrap();
        all
    }

    fn verdict(&mut self, peer: Peer, service: &str, account: &str, answers: &[&str]) -> Out {
        self.converse(peer, service, account, answers)
            .pop()
            .unwrap()
    }

    fn one(&mut self, peer: Peer, record: &Record<'_>) -> Reply {
        let mut conversation = Conversation::new(peer);
        let mut outs = self.engine.handle(&mut conversation, record, self.now_ms);
        assert_eq!(outs.len(), 1, "{outs:?}");
        outs.pop().unwrap().reply
    }

    fn audit(&self) -> String {
        std::fs::read_to_string(self.root.join("var/log/ferrix/auth.log")).unwrap_or_default()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn accepted(account: &str, uid: u32) -> Out {
    Out {
        reply: Reply::Accepted {
            uid,
            account: account.to_owned(),
        },
        after: Duration::ZERO,
    }
}

fn failed(retry_after_ms: u32) -> Out {
    Out {
        reply: Reply::Failed {
            retry_after_ms,
            text: FAILED.to_owned(),
        },
        after: Duration::from_secs(2),
    }
}

fn unavailable(text: &str) -> Out {
    Out {
        reply: Reply::Unavailable(text.to_owned()),
        after: Duration::ZERO,
    }
}

#[test]
fn the_right_password_is_accepted_and_a_wrong_one_fails_after_the_delay() {
    let mut m = Machine::new("right-wrong");
    m.set("ferrix", 1000, "correct horse");
    let outs = m.converse(FERRIX, "hyprlock", "", &["correct horse"]);
    assert_eq!(
        outs.first().map(|out| &out.reply),
        Some(&Reply::Prompt {
            visible: false,
            text: "Password: ".to_owned()
        })
    );
    assert_eq!(outs.last(), Some(&accepted("ferrix", 1000)));
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["correct hors"]),
        failed(0)
    );
    let log = m.audit();
    assert!(log.contains("service=hyprlock account=ferrix peer-pid=2 peer-uid=1000 method=password result=accepted"), "{log}");
    assert!(log.contains("result=failed why=failures:1"), "{log}");
    assert!(
        !log.contains("correct"),
        "the audit log holds no password: {log}"
    );
}

#[test]
fn no_credential_is_unavailable_never_an_empty_password() {
    let mut m = Machine::new("none");
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &[]),
        unavailable("no password is set for ferrix")
    );
    assert_eq!(
        m.one(FERRIX, &Record::Status { account: "" }),
        Reply::State {
            credential: false,
            methods: 0,
            throttled_ms: 0
        }
    );
    m.set("ferrix", 1000, "x");
    assert_eq!(
        m.one(FERRIX, &Record::Status { account: "" }),
        Reply::State {
            credential: true,
            methods: method::PASSWORD,
            throttled_ms: 0
        }
    );
}

#[test]
fn only_root_names_another_account_and_never_skips_the_password() {
    let mut m = Machine::new("naming");
    m.set("ferrix", 1000, "correct horse");
    assert_eq!(
        m.verdict(OTHER, "gate", "ferrix", &[]),
        unavailable("only root may name another account")
    );
    assert_eq!(
        m.verdict(OTHER, "hyprlock", "ferrix", &[]),
        unavailable("this service checks only the caller's own account")
    );
    assert_eq!(
        m.one(OTHER, &Record::Status { account: "ferrix" }),
        Reply::Unavailable("only root may name another account".to_owned())
    );
    // Root names ferrix, and is still asked for ferrix's password.
    assert_eq!(m.verdict(ROOT, "gate", "ferrix", &["wrong"]), failed(0));
    assert_eq!(
        m.verdict(ROOT, "gate", "ferrix", &["correct horse"]),
        accepted("ferrix", 1000)
    );
    // login is root's alone.
    assert_eq!(
        m.verdict(FERRIX, "login", "", &[]),
        unavailable("only root may use the login service")
    );
    assert_eq!(
        m.verdict(FERRIX, "nosuch", "", &[]),
        unavailable("there is no nosuch service")
    );
}

#[test]
fn an_unknown_account_answers_as_a_real_one_does_over_six_failures() {
    let mut m = Machine::new("unknown");
    m.set("ferrix", 1000, "correct horse");
    let start = m.now_ms;
    let real: Vec<Vec<Out>> = (0..6)
        .map(|_| m.converse(ROOT, "gate", "ferrix", &["guess"]))
        .collect();
    m.now_ms = start;
    let unknown: Vec<Vec<Out>> = (0..6)
        .map(|_| m.converse(ROOT, "gate", "nobody", &["guess"]))
        .collect();
    assert_eq!(
        real, unknown,
        "the same prompts, texts, delays and throttles, answer by answer"
    );
    assert!(
        matches!(real.last().and_then(|outs| outs.last()), Some(Out { reply: Reply::Failed { text, .. }, .. }) if text.starts_with("wait ")),
        "six in a row reach the throttle: {real:?}"
    );
    assert!(
        !m.root.join("var/lib/ferrix/auth/state/nobody").exists(),
        "an unknown account leaves no file behind"
    );
}

#[test]
fn the_fourth_failure_throttles_and_the_throttle_is_kept_in_the_store() {
    let mut m = Machine::new("throttle");
    m.set("ferrix", 1000, "correct horse");
    for (attempt, wait_ms) in [(1, 0), (2, 0), (3, 0), (4, 2000)] {
        assert_eq!(
            m.verdict(ROOT, "gate", "ferrix", &["wrong"]),
            failed(wait_ms),
            "attempt {attempt}"
        );
    }
    // The fourth FAILED went out two seconds after its guess, and the
    // throttle runs two more from then. Inside them, an attempt is refused
    // unlooked-at, with the right password too, and with no prompt.
    m.now_ms += 500;
    let outs = m.converse(ROOT, "gate", "ferrix", &[]);
    assert_eq!(
        outs,
        vec![Out {
            reply: Reply::Failed {
                retry_after_ms: 1500,
                text: "wait 2 s".to_owned()
            },
            after: Duration::ZERO
        }]
    );
    // A fresh engine over the same store keeps the throttle.
    let paths = Paths::under(&m.root);
    m.engine = Engine::new(paths, Hasher::fixed(FLOOR), [9; 32]);
    assert!(matches!(
        m.one(ROOT, &Record::Status { account: "ferrix" }),
        Reply::State {
            throttled_ms: 1500,
            ..
        }
    ));
    // Once it has passed, the right password opens and resets the count.
    m.now_ms += 1500;
    assert_eq!(
        m.verdict(ROOT, "gate", "ferrix", &["correct horse"]),
        accepted("ferrix", 1000)
    );
    assert_eq!(m.verdict(ROOT, "gate", "ferrix", &["wrong"]), failed(0));
    // Only root resets a throttle.
    assert!(matches!(
        m.one(FERRIX, &Record::Reset { account: "ferrix" }),
        Reply::Unavailable(_)
    ));
    assert!(matches!(
        m.one(ROOT, &Record::Reset { account: "ferrix" }),
        Reply::State {
            throttled_ms: 0,
            ..
        }
    ));
}

#[test]
fn passwd_asks_for_the_current_password_unless_root_asks() {
    let mut m = Machine::new("passwd");
    m.set("ferrix", 1000, "old one");
    let outs = m.converse(FERRIX, "passwd", "", &["old one", "new one", "new one"]);
    let prompts: Vec<&Reply> = outs.iter().map(|out| &out.reply).collect();
    assert_eq!(prompts.len(), 4, "{prompts:?}");
    assert_eq!(outs.last(), Some(&accepted("ferrix", 1000)));
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["new one"]),
        accepted("ferrix", 1000)
    );
    assert_eq!(m.verdict(FERRIX, "hyprlock", "", &["old one"]), failed(0));
    // A wrong current password changes nothing.
    assert_eq!(m.verdict(FERRIX, "passwd", "", &["nope"]), failed(0));
    // Mismatched and empty new passwords are refused.
    assert!(matches!(
        m.verdict(FERRIX, "passwd", "", &["new one", "a", "b"]).reply,
        Reply::Failed { ref text, .. } if text == "the passwords did not match"
    ));
    assert!(matches!(
        m.verdict(FERRIX, "passwd", "", &["new one", ""]).reply,
        Reply::Failed { ref text, .. } if text == "a password may not be empty"
    ));
    // Root sets another account's first password with no current one.
    let outs = m.converse(ROOT, "passwd", "other", &["theirs", "theirs"]);
    assert_eq!(outs.len(), 3);
    assert_eq!(outs.last(), Some(&accepted("other", 1001)));
    // Nobody else may.
    assert_eq!(
        m.verdict(FERRIX, "passwd", "other", &[]),
        unavailable("only root may name another account")
    );
    assert!(m.audit().contains(
        "service=passwd account=other peer-pid=1 peer-uid=0 method=password result=changed"
    ));
}

#[test]
fn a_seed_is_imported_once_and_a_changed_password_survives_it() {
    let mut m = Machine::new("seed");
    let mut hasher = Hasher::fixed(FLOOR);
    let secret = ferrix_auth_proto::Secret::from_bytes(b"seeded").unwrap();
    let hash = hasher.make(&secret, &mut |_| {}).unwrap();
    write(
        &m.root.join("lib/ferrix/auth/seed/ferrix"),
        &format!("{hash}\n"),
    );
    write(
        &m.root.join("lib/ferrix/auth/seed/ghost"),
        &format!("{hash}\n"),
    );
    m.engine.import_seeds(m.now_ms);
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["seeded"]),
        accepted("ferrix", 1000)
    );
    assert!(
        !m.root.join("var/lib/ferrix/auth/users/ghost").exists(),
        "no account, no import"
    );
    assert_eq!(
        m.verdict(FERRIX, "passwd", "", &["seeded", "mine", "mine"]),
        accepted("ferrix", 1000)
    );
    m.engine.import_seeds(m.now_ms);
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["mine"]),
        accepted("ferrix", 1000)
    );
}

#[test]
fn a_sha512_crypt_seed_is_rehashed_as_argon2id_at_its_first_success() {
    let mut m = Machine::new("rehash");
    write(
        &m.root.join("lib/ferrix/auth/seed/ferrix"),
        "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1\n",
    );
    m.engine.import_seeds(m.now_ms);
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["Hello world!"]),
        accepted("ferrix", 1000)
    );
    let record = std::fs::read_to_string(m.root.join("var/lib/ferrix/auth/users/ferrix")).unwrap();
    assert!(record.contains("password $argon2id$v=19$"), "{record}");
    assert_eq!(
        m.verdict(FERRIX, "hyprlock", "", &["Hello world!"]),
        accepted("ferrix", 1000)
    );
}

#[test]
fn unlock_seat_is_root_only_and_waits_for_phase_2() {
    let mut m = Machine::new("seat");
    assert_eq!(
        m.one(FERRIX, &Record::UnlockSeat),
        Reply::Unavailable("only root may let the seat's lock go".to_owned())
    );
    assert!(
        matches!(m.one(ROOT, &Record::UnlockSeat), Reply::Unavailable(ref t) if t.contains("phase 2"))
    );
    assert!(m.audit().contains("service=unlock-seat"));
}

#[test]
fn records_out_of_turn_end_the_conversation() {
    let mut m = Machine::new("turns");
    let mut conversation = Conversation::new(FERRIX);
    let outs = m.engine.handle(
        &mut conversation,
        &Record::Respond(Response(b"x")),
        m.now_ms,
    );
    assert!(matches!(
        outs.as_slice(),
        [Out {
            reply: Reply::Unavailable(_),
            ..
        }]
    ));
    let after = m
        .engine
        .handle(&mut conversation, &Record::Status { account: "" }, m.now_ms);
    assert!(after.is_empty(), "nothing after the end");
}
