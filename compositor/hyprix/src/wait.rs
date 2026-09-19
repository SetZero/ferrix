//! Waiting for the compositor's next reason to work.
//!
//! Every descriptor the compositor owns is non-blocking.  That makes each
//! dispatcher short, but used to mean the loop woke every two milliseconds
//! merely to find nothing to do.  `poll` lets the loop sleep until a client,
//! input device, control socket, or plugin is ready, while a timeout keeps
//! frame pacing and protocol timers on their clocks.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

/// Wait for input descriptors or a timer, and return the descriptors that
/// need servicing.
///
/// `None` waits until a descriptor is ready.  A zero duration asks whether a
/// descriptor is ready without sleeping.  Interrupted waits simply begin
/// again: a signal has not made any compositor work ready.
pub(crate) fn wait(fds: &[RawFd], timeout: Option<Duration>) -> io::Result<Vec<RawFd>> {
    let mut fds: Vec<libc::pollfd> = fds
        .iter()
        .copied()
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let timeout = timeout.map_or(-1, timeout_millis);
    loop {
        // SAFETY: `fds` points to `pollfd`s for the duration of this call;
        // an empty vector's pointer is valid because its count is zero.
        let waited = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if waited >= 0 {
            return Ok(fds
                .iter()
                .filter(|poll| {
                    poll.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
                        != 0
                })
                .map(|poll| poll.fd)
                .collect());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `poll` takes whole milliseconds.  Round a nonzero fraction up so a frame
/// timer cannot turn into a busy loop just before it is due.
fn timeout_millis(timeout: Duration) -> i32 {
    let mut milliseconds = timeout.as_millis();
    if !timeout.subsec_nanos().is_multiple_of(1_000_000) {
        milliseconds = milliseconds.saturating_add(1);
    }
    i32::try_from(milliseconds).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn poll_timeouts_round_up() {
        assert_eq!(timeout_millis(Duration::ZERO), 0);
        assert_eq!(timeout_millis(Duration::from_nanos(1)), 1);
        assert_eq!(timeout_millis(Duration::from_micros(1_001)), 2);
        assert_eq!(timeout_millis(Duration::from_millis(8)), 8);
    }

    #[test]
    fn no_descriptors_can_still_wait_for_a_timer() {
        assert_eq!(wait(&[], Some(Duration::ZERO)).expect("zero timeout"), []);
    }

    #[test]
    fn only_the_descriptor_that_woke_is_returned() {
        let (mut sender, receiver) = UnixStream::pair().expect("pair");
        let (_quiet_sender, quiet) = UnixStream::pair().expect("quiet pair");
        sender.write_all(b"ready").expect("write");
        let fds = [receiver.as_raw_fd(), quiet.as_raw_fd()];
        assert_eq!(
            wait(&fds, Some(Duration::ZERO)).expect("ready descriptor"),
            [receiver.as_raw_fd()]
        );
    }
}
