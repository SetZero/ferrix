//! Capabilities and the directory (`docs/INIT.md` §6, landing L8's init
//! side): what init does with the native calls K2 to K6 made.
//!
//! * Pid 1's own bootstrap channel ([`Directory::open`]): taken with
//!   `process_bootstrap`, and the kernel's hello read from it (K2). Init
//!   keeps it; later versions carry handles over it.
//! * One port, as a descriptor in init's epoll (`port_fd`, K4): every
//!   service channel and every native process is watched through it.
//! * A bootstrap channel for each service that declares `Uses=` or
//!   `Offers=`, and for every `Type=native` one. A Linux service is given its
//!   end with `process_give` (K3) while it waits, before `execve`, on a pipe
//!   init writes once the give is done (§5.2 step 3); a native one gets it
//!   from `process_start`.
//! * `Type=native` services, made with `process_create` in the job behind
//!   their cgroup (`job_for_cgroup`, C8) from the ELF file `ExecStart=`
//!   names, and watched for `TERMINATED`; how one ended is read with
//!   `process_status` (K6).
//! * The directory's messages on those channels ([`directory`]): READY
//!   is readiness, OFFER a provider's channel for a name, OPEN a client's
//!   request, which the manager routes ([`Action::Route`]) or refuses
//!   ([`Action::Refuse`]); init forwards the client's end as CONNECT, or
//!   answers REFUSED.
//!
//! [`directory`]: ferrix_native_abi::directory
//! [`Action::Route`]: ferrix_svc::event::Action::Route
//! [`Action::Refuse`]: ferrix_svc::event::Action::Refuse

use std::collections::BTreeMap;
use std::os::fd::{BorrowedFd, RawFd};

use ferrix_native::channel::{self, Channel, ReadError};
use ferrix_native::job::{self, Job};
use ferrix_native::pending::{self, Process};
use ferrix_native::port::{self, Port};
use ferrix_native::{
    Deadline, Error, Handle, Object, OwnedHandle, Requested, Rights, Signals, vmo,
};
use ferrix_native_abi::bootstrap::init_hello_version;
use ferrix_native_abi::directory::{Kind, MAX_MESSAGE, Message};
use ferrix_native_abi::types::{PACKET_SIGNAL, PROCESS_EXITED, PROCESS_KILLED};
use ferrix_svc::event::{Event, Exit, Name, Pid, Token, UnitId};
use ferrix_svc::value::Signal;

use crate::sys::Native;

/// What a port key stands for.
#[derive(Debug)]
enum Watched {
    /// A service's bootstrap channel: init's end.
    Channel(UnitId, Channel<Native>),
    /// A native service's process.
    Process(UnitId, u32, Process<Native>),
}

/// A client's end in transit, from its OPEN to the provider's CONNECT.
#[derive(Debug)]
struct End {
    from: UnitId,
    handle: OwnedHandle<Native>,
}

/// The directory's state.
#[derive(Debug)]
pub(crate) struct Directory {
    /// Pid 1's own channel, held for later versions' handles.
    #[expect(
        dead_code,
        reason = "held open, not read: version 1 sends nothing after its hello"
    )]
    own: Option<Channel<Native>>,
    port: Port<Native>,
    /// What each port key stands for.
    watched: BTreeMap<u64, Watched>,
    /// Each unit's bootstrap channel, by its port key.
    channel_of: BTreeMap<UnitId, u64>,
    /// The channel ends providers offered, by name.
    offers: BTreeMap<String, Channel<Native>>,
    /// Client ends waiting for their route.
    ends: BTreeMap<u64, End>,
    next: u64,
}

/// A spawn's native half, for the caller to finish.
#[derive(Debug)]
pub(crate) struct Given {
    /// The service's end, to give it.
    pub(crate) end: Channel<Native>,
}

impl Directory {
    /// Take pid 1's bootstrap channel and read the kernel's hello, and make
    /// the port. Returns the directory, the port's descriptor to watch, and
    /// a line saying what the kernel said.
    pub(crate) fn open() -> Result<(Directory, RawFd, String), Error> {
        let own = pending::take_bootstrap(Native)?.map(Channel::from_owned);
        let said = match &own {
            Some(channel) => {
                let mut bytes = [0_u8; 64];
                match channel.read(&mut bytes, &mut []) {
                    Ok(received) => match init_hello_version(
                        bytes.get(..received.bytes).unwrap_or_default(),
                    ) {
                        Some(version) => format!("the kernel greeted init, version {version}"),
                        None => {
                            "init's bootstrap channel held something that is not the kernel's hello"
                                .to_owned()
                        }
                    },
                    Err(_) => "init's bootstrap channel held no hello".to_owned(),
                }
            }
            None => "init was started with no bootstrap channel".to_owned(),
        };
        let port = port::create(Native)?;
        let fd = port.descriptor(true)?;
        Ok((
            Directory {
                own,
                port,
                watched: BTreeMap::new(),
                channel_of: BTreeMap::new(),
                offers: BTreeMap::new(),
                ends: BTreeMap::new(),
                next: 1,
            },
            fd,
            said,
        ))
    }

    fn key(&mut self) -> u64 {
        let key = self.next;
        self.next += 1;
        key
    }

    /// A new bootstrap channel for `unit`: init keeps and watches one end,
    /// and the other is returned to hand to the service.
    pub(crate) fn channel_for(&mut self, unit: UnitId) -> Result<Given, Error> {
        let (ours, theirs) = channel::create(Native)?;
        let key = self.key();
        ours.wait_async(&self.port, Signals::READABLE | Signals::PEER_CLOSED, key)?;
        if let Some(old) = self.channel_of.insert(unit, key) {
            let _ = self.watched.remove(&old);
        }
        let _ = self.watched.insert(key, Watched::Channel(unit, ours));
        Ok(Given { end: theirs })
    }

    /// `process_give`: hand a Linux child `end` before it runs its program.
    pub(crate) fn give(pid: u32, end: Channel<Native>) -> Result<(), Error> {
        pending::give_bootstrap(Native, pid, end.into_owned()).map_err(|(error, _)| error)
    }

    /// Start `program` as `unit`'s native main process, in the job behind
    /// the cgroup `cgroup` is open on, with `end` as its bootstrap channel.
    /// Returns its key, for the pid the caller finds in the cgroup.
    pub(crate) fn start_native(
        &mut self,
        unit: UnitId,
        cgroup: BorrowedFd<'_>,
        program: &str,
        end: Channel<Native>,
    ) -> Result<u64, String> {
        use std::os::fd::AsRawFd as _;
        let image =
            std::fs::read(program).map_err(|error| format!("reading {program}: {error}"))?;
        let job: Job<Native> =
            job::for_cgroup(Native, cgroup.as_raw_fd(), Requested::Exactly(Rights::JOB))
                .map_err(|error| format!("job_for_cgroup: {error:?}"))?;
        let elf =
            vmo::create(Native, image.len()).map_err(|error| format!("vmo_create: {error:?}"))?;
        elf.write(&image, 0)
            .map_err(|error| format!("vmo_write: {error:?}"))?;
        let name = program.rsplit('/').next().unwrap_or(program);
        let name = name
            .get(..name.len().min(ferrix_native_abi::nr::PROCESS_NAME_MAX))
            .unwrap_or(name);
        let process = pending::create_process(&job, &elf, name)
            .map_err(|error| format!("process_create: {error:?}"))?;
        let key = self.key();
        process
            .notify_on_exit(&self.port, key)
            .map_err(|error| format!("waiting for its end: {error:?}"))?;
        process
            .start(end.into_owned())
            .map_err(|(error, _)| format!("process_start: {error:?}"))?;
        let _ = self.watched.insert(key, Watched::Process(unit, 0, process));
        Ok(key)
    }

    /// Record the pid a native process was found under in its cgroup.
    pub(crate) fn found_pid(&mut self, key: u64, pid: u32) {
        if let Some(Watched::Process(_, found, _)) = self.watched.get_mut(&key) {
            *found = pid;
        }
    }

    /// Drain the port: the events its packets are, and lines to say.
    pub(crate) fn drain(&mut self) -> (Vec<Event>, Vec<String>) {
        let mut events = Vec::new();
        let mut lines = Vec::new();
        while let Ok(packet) = self.port.wait(Deadline::At(0)) {
            if packet.kind != PACKET_SIGNAL {
                continue;
            }
            match self.watched.remove(&packet.key) {
                Some(Watched::Channel(unit, channel)) => {
                    let open = self.read_channel(unit, &channel, &mut events, &mut lines);
                    if open
                        && channel
                            .wait_async(
                                &self.port,
                                Signals::READABLE | Signals::PEER_CLOSED,
                                packet.key,
                            )
                            .is_ok()
                    {
                        let _ = self
                            .watched
                            .insert(packet.key, Watched::Channel(unit, channel));
                    } else if self.channel_of.get(&unit) == Some(&packet.key) {
                        let _ = self.channel_of.remove(&unit);
                    }
                }
                Some(Watched::Process(unit, pid, process)) => {
                    let how = match process.status() {
                        Ok(status) if status.state == PROCESS_EXITED => {
                            Exit::Code(i32::try_from(status.value).unwrap_or(255))
                        }
                        Ok(status) if status.state == PROCESS_KILLED => Exit::Signal {
                            signal: Signal(u8::try_from(status.value).unwrap_or(9)),
                            core: false,
                        },
                        Ok(_) | Err(_) => Exit::Signal {
                            signal: Signal(9),
                            core: false,
                        },
                    };
                    let _ = unit;
                    events.push(Event::Exited { pid: Pid(pid), how });
                }
                None => {}
            }
        }
        (events, lines)
    }

    /// Read every message a unit's channel holds; whether it is still open.
    fn read_channel(
        &mut self,
        unit: UnitId,
        channel: &Channel<Native>,
        events: &mut Vec<Event>,
        lines: &mut Vec<String>,
    ) -> bool {
        loop {
            let mut bytes = [0_u8; MAX_MESSAGE];
            let mut handles = [Handle::INVALID; 1];
            let received = match channel.read(&mut bytes, &mut handles) {
                Ok(received) => received,
                Err(ReadError::Failed(Error::ShouldWait)) => return true,
                Err(ReadError::Failed(Error::PeerClosed)) => return false,
                Err(ReadError::TooSmall { .. }) => {
                    discard(channel);
                    lines.push("a service sent a message too big to be the directory's".to_owned());
                    continue;
                }
                Err(ReadError::Failed(_)) => return false,
            };
            let [handle] = handles;
            let handle = (received.handles == 1).then(|| OwnedHandle::from_raw(Native, handle));
            let Some(message) = Message::decode(bytes.get(..received.bytes).unwrap_or_default())
            else {
                lines.push("a service sent something that is not a directory message".to_owned());
                continue;
            };
            match (message.kind, handle) {
                (Kind::Ready, _) => events.push(Event::Ready { unit, status: None }),
                (Kind::Offer, Some(handle)) => {
                    let _ = self
                        .offers
                        .insert(message.name.to_owned(), Channel::from_owned(handle));
                }
                (Kind::Open, Some(handle)) => {
                    let token = self.key();
                    let _ = self.ends.insert(token, End { from: unit, handle });
                    events.push(Event::Open {
                        from: unit,
                        name: Name(message.name.to_owned()),
                        end: Token(token),
                    });
                }
                (kind, _) => lines.push(format!(
                    "a service sent {kind:?}, which is not a service's to send"
                )),
            }
        }
    }

    /// CONNECT: forward the client's end to the provider, down the channel
    /// it offered for the name, or else its bootstrap channel. `client` is
    /// the asking unit's name.
    pub(crate) fn route(
        &mut self,
        to: UnitId,
        name: &str,
        end: Token,
        client: impl Fn(UnitId) -> String,
    ) -> Result<(), String> {
        let End { from, handle } = self
            .ends
            .remove(&end.0)
            .ok_or_else(|| format!("no end {} to route for {name}", end.0))?;
        let detail = client(from);
        let mut bytes = [0_u8; MAX_MESSAGE];
        let len = Message::named(Kind::Connect, name, &detail)
            .encode(&mut bytes)
            .ok_or_else(|| format!("a CONNECT for {name} does not fit a message"))?;
        let bytes = bytes.get(..len).unwrap_or_default();
        let sent = match self.offers.get(name) {
            Some(offered) => offered.write_with(bytes, [handle]),
            None => match self.channel(to) {
                Some(channel) => channel.write_with(bytes, [handle]),
                None => return Err(format!("the provider of {name} has no channel")),
            },
        };
        sent.map_err(|(error, _)| format!("CONNECT for {name}: {error:?}"))
    }

    /// REFUSED: close the end and tell the asking unit why.
    pub(crate) fn refuse(&mut self, to: UnitId, name: &str, end: Token, why: &str) {
        let mut bytes = [0_u8; MAX_MESSAGE];
        if let (Some(len), Some(channel)) = (
            Message::named(Kind::Refused, name, why).encode(&mut bytes),
            self.channel(to),
        ) {
            let _ = channel.write(bytes.get(..len).unwrap_or_default());
        }
        // The end closes after REFUSED is sent, so a client that sees its
        // end close finds the reason already waiting.
        drop(self.ends.remove(&end.0));
    }

    /// A unit's bootstrap channel, while it is open.
    fn channel(&self, unit: UnitId) -> Option<&Channel<Native>> {
        let key = self.channel_of.get(&unit)?;
        match self.watched.get(key)? {
            Watched::Channel(_, channel) => Some(channel),
            Watched::Process(..) => None,
        }
    }

    /// A unit has stopped: close its channel, so its processes see
    /// `PEER_CLOSED`, and drop what it offered.
    pub(crate) fn forget(&mut self, unit: UnitId, offered: &[String]) {
        if let Some(key) = self.channel_of.remove(&unit) {
            let _ = self.watched.remove(&key);
        }
        for name in offered {
            let _ = self.offers.remove(name);
        }
    }
}

/// Take a message that is not the directory's off `channel`, and close the
/// handles it carried.
fn discard(channel: &Channel<Native>) {
    let mut bytes = vec![0_u8; 65536];
    let mut handles = [Handle::INVALID; 64];
    let Ok(got) = channel.read(&mut bytes, &mut handles) else {
        return;
    };
    for handle in handles.iter().take(got.handles) {
        drop(OwnedHandle::from_raw(Native, *handle));
    }
}
