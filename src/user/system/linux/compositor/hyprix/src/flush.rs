//! A screen's flushes on a thread of their own.
//!
//! On a card that shows a copy of the frame -- a virtio-gpu, whose host
//! holds it -- the frame is shown by `DIRTYFB`, and the call returns once
//! the host has shown it: under QEMU's GL window at the window's next
//! repaint, under VNC after the whole screen was read back. That took 5 to
//! 15 ms a frame, and the compositor's loop waited for it, so it answered
//! no client and drew no frame meanwhile. Linux's compositors do not wait:
//! they commit without blocking and hear of the flip by an event.
//!
//! Here the loop hands the flush to this thread and goes on. At most one
//! flush waits behind the one under way; a frame drawn while one waits is
//! merged into it, its rectangles added, so the card is never more than one
//! frame behind and the newest frame is the one shown. The order the card
//! sees is safe: the GPU's drawing and the flush go to the device on one
//! queue, which the host serves in order and each command whole, so a
//! flush that goes after the next frame's drawing shows that frame entire,
//! never a mix.
//!
//! A card whose driver has gone answers `ENODEV`; the thread says so
//! ([`FlushThread::gone`]) and stops, and the screen is lost as it would
//! have been at the call.

use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

/// More rectangles than one flush carries: past it a flush is of the whole
/// screen, as `DIRTYFB` takes too many.
const MERGED_CLIPS: usize = 64;

/// A flush: the framebuffer, and what changed as `(x, y, width, height)`,
/// an empty list for all of it.
type Job = (u32, Vec<(u32, u32, u32, u32)>);

/// What the loop and the thread share.
#[derive(Debug, Default)]
struct State {
    /// The flush waiting to be made: the framebuffer and what changed, an
    /// empty list for all of it.
    waiting: Option<Job>,
    /// The thread is to end.
    stop: bool,
    /// The card's driver has gone.
    gone: bool,
    /// What the card refused, said once.
    refused: Option<String>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// A screen's flushing thread.
#[derive(Debug)]
pub(crate) struct FlushThread {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl FlushThread {
    /// Start flushing on `flusher`.
    ///
    /// # Errors
    ///
    /// The thread could not be started.
    pub(crate) fn start(flusher: compositor_drm::Flusher) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let theirs = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("flush".to_owned())
            .spawn(move || run(&theirs, &flusher))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Show `clips` of `framebuffer`, each `(x, y, width, height)`, without
    /// waiting: merged into the flush that waits, if one does.
    pub(crate) fn flush(&self, framebuffer: u32, clips: &[(u32, u32, u32, u32)]) {
        let mut state = self.lock();
        match state.waiting.as_mut() {
            Some((waiting, merged)) if *waiting == framebuffer => {
                if !merged.is_empty() {
                    merged.extend_from_slice(clips);
                    if merged.len() > MERGED_CLIPS || clips.is_empty() {
                        merged.clear();
                    }
                }
            }
            _ => state.waiting = Some((framebuffer, clips.to_vec())),
        }
        drop(state);
        self.shared.wake.notify_one();
    }

    /// Whether the card's driver went away under a flush.
    pub(crate) fn gone(&self) -> bool {
        self.lock().gone
    }

    /// What the card refused since this was last asked, if anything.
    pub(crate) fn refused(&self) -> Option<String> {
        self.lock().refused.take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for FlushThread {
    fn drop(&mut self) {
        self.lock().stop = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread: each waiting flush in turn, until told to stop or the card
/// goes.
fn run(shared: &Shared, flusher: &compositor_drm::Flusher) {
    loop {
        let (framebuffer, clips) = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if state.stop {
                    return;
                }
                if let Some(job) = state.waiting.take() {
                    break job;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if let Err(error) = flusher.dirty(framebuffer, &clips) {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            if error.raw_os_error() == Some(libc::ENODEV) {
                state.gone = true;
                return;
            }
            state.refused = Some(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A waiting flush takes in the next frame's rectangles; a flush of the
    /// whole screen stays one; and too many become the whole screen.
    #[test]
    fn a_waiting_flush_takes_in_the_next() {
        let shared = Arc::new(Shared::default());
        let thread = FlushThread {
            shared: Arc::clone(&shared),
            thread: None,
        };
        thread.flush(7, &[(0, 0, 10, 10)]);
        thread.flush(7, &[(5, 5, 10, 10)]);
        assert_eq!(
            shared.state.lock().unwrap().waiting,
            Some((7, vec![(0, 0, 10, 10), (5, 5, 10, 10)]))
        );
        thread.flush(7, &[]);
        assert_eq!(shared.state.lock().unwrap().waiting, Some((7, vec![])));
        thread.flush(7, &[(1, 1, 1, 1)]);
        assert_eq!(
            shared.state.lock().unwrap().waiting,
            Some((7, vec![])),
            "all of it stays all of it"
        );
        shared.state.lock().unwrap().waiting = None;
        for at in 0..=MERGED_CLIPS as u32 {
            thread.flush(9, &[(at, 0, 1, 1)]);
        }
        assert_eq!(shared.state.lock().unwrap().waiting, Some((9, vec![])));
        thread.flush(3, &[(0, 0, 1, 1)]);
        assert_eq!(
            shared.state.lock().unwrap().waiting,
            Some((3, vec![(0, 0, 1, 1)])),
            "another framebuffer replaces it"
        );
    }
}
