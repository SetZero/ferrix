//! The clipboard: what one client copied, and how another pastes it.
//!
//! Wayland's clipboard is a promise rather than a buffer. The client that
//! copies keeps the data and says which types it can give it in; the
//! compositor remembers who that is and tells every other client what is on
//! offer; a client that pastes asks for a type and hands over a pipe, and
//! the compositor passes that pipe to whoever copied, who writes to it and
//! closes it. Nothing here ever sees the data.
//!
//! That is the whole of `wl_data_device_manager`'s selection, and it is what
//! makes copy and paste work between two programs. Drag-and-drop is the
//! other half of the same interfaces and is not done: nothing here is
//! dragged.

use compositor_wire::{Fd, ObjectId};

/// What a client copied.
#[derive(Clone, Debug)]
pub struct Selection {
    /// Which connection owns it.
    pub client: usize,
    /// Its `wl_data_source`.
    pub source: ObjectId,
    /// The types it can give the data in, in the order it offered them.
    pub mimes: Vec<String>,
}

/// Which of the two selections a call is about.
///
/// Wayland has both, and so does X11 before it: the clipboard is what a copy
/// puts there and a paste takes out, and the *primary* selection is what
/// merely selecting text puts there and a middle click takes out. They are
/// the same protocol twice over, under different names, and they are kept
/// apart because a person uses them for different things.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// `wl_data_device`: copy and paste.
    Clipboard,
    /// `zwp_primary_selection_device_v1`: select and middle click.
    Primary,
}

/// The compositor's clipboard: at most one selection at a time, as X11's
/// `CLIPBOARD` and Wayland's selection both are.
#[derive(Debug, Default)]
pub struct Clipboard {
    selection: Option<Selection>,
    /// The primary selection, which is the same thing under another name.
    primary: Option<Selection>,
    /// How many times something has been copied and pasted, for the
    /// compositor's log line.
    copied: u32,
    pasted: u32,
}

impl Clipboard {
    /// Nothing copied yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            selection: None,
            primary: None,
            copied: 0,
            pasted: 0,
        }
    }

    /// What is on either selection, if anything.
    #[must_use]
    pub const fn selection(&self, which: Which) -> Option<&Selection> {
        self.held(which)
    }

    /// How many times something has been copied, and how many pasted.
    #[must_use]
    pub const fn counts(&self) -> (u32, u32) {
        (self.copied, self.pasted)
    }

    /// A client copied: it becomes the selection, and whoever held it
    /// before is told it no longer does.
    ///
    /// An empty `mimes` clears the clipboard, which is what a client sends
    /// when it no longer owns what it copied.
    pub fn copied(
        &mut self,
        which: Which,
        clients: &mut [crate::state::Slot],
        client: usize,
        source: Option<ObjectId>,
        mimes: Vec<String>,
    ) {
        let held = match which {
            Which::Clipboard => &mut self.selection,
            Which::Primary => &mut self.primary,
        };
        if let Some(previous) = held.take()
            && (previous.client != client || Some(previous.source) != source)
            && let Some(slot) = clients.get_mut(previous.client)
        {
            match which {
                Which::Clipboard => slot.client_mut().cancel_selection(previous.source),
                Which::Primary => slot.client_mut().cancel_primary(previous.source),
            }
        }
        *held = source.map(|source| Selection {
            client,
            source,
            mimes,
        });
        if held.is_some() {
            self.copied = self.copied.saturating_add(1);
        }
        self.offer_to_all(which, clients);
    }

    /// Tell every client that can paste what is on the clipboard.
    ///
    /// The client that owns the selection is told too, which is what
    /// Hyprland and every wlroots compositor do: a program that copies and
    /// then pastes gets its own data back.
    pub fn offer_to_all(&self, which: Which, clients: &mut [crate::state::Slot]) {
        let mimes = self
            .held(which)
            .map(|selection| selection.mimes.clone())
            .unwrap_or_default();
        for slot in clients.iter_mut() {
            match which {
                Which::Clipboard if slot.client().has_data_device() => {
                    slot.client_mut().offer_selection(&mimes);
                }
                Which::Primary if slot.client().has_primary_device() => {
                    slot.client_mut().offer_primary(&mimes);
                }
                _ => {}
            }
        }
    }

    /// Which of the two is held.
    const fn held(&self, which: Which) -> Option<&Selection> {
        match which {
            Which::Clipboard => self.selection.as_ref(),
            Which::Primary => self.primary.as_ref(),
        }
    }

    /// Tell one client, which is what a client that has just made a
    /// `wl_data_device` is owed.
    pub fn offer_to(&self, which: Which, clients: &mut [crate::state::Slot], client: usize) {
        let mimes = self
            .held(which)
            .map(|selection| selection.mimes.clone())
            .unwrap_or_default();
        if mimes.is_empty() {
            return;
        }
        if let Some(slot) = clients.get_mut(client) {
            match which {
                Which::Clipboard => slot.client_mut().offer_selection(&mimes),
                Which::Primary => slot.client_mut().offer_primary(&mimes),
            }
        }
    }

    /// A client pasted: the descriptor goes to whoever copied.
    ///
    /// Gives whether it went anywhere. A paste through an offer that is no
    /// longer the selection is dropped, which leaves the pasting client
    /// reading a pipe nothing writes to -- and its own end is closed here,
    /// so that read ends rather than hangs.
    pub fn pasted(
        &mut self,
        which: Which,
        clients: &mut [crate::state::Slot],
        asking: usize,
        offer: ObjectId,
        mime: &str,
        fd: Fd,
    ) -> bool {
        let Some(selection) = self.held(which).cloned() else {
            close(fd);
            return false;
        };
        let current = clients.get(asking).is_some_and(|slot| match which {
            Which::Clipboard => slot.client().holds_offer(offer),
            Which::Primary => slot.client().holds_primary_offer(offer),
        });
        if !current {
            close(fd);
            return false;
        }
        let Some(owner) = clients.get_mut(selection.client) else {
            close(fd);
            return false;
        };
        match which {
            Which::Clipboard => owner
                .client_mut()
                .send_selection(selection.source, mime, fd),
            Which::Primary => owner.client_mut().send_primary(selection.source, mime, fd),
        }
        // Sent now rather than at the end of the pass: the descriptor is
        // closed on the next line, and one let go of before the message
        // carrying it has been written is one the client never gets.
        let _ = owner.flush();
        self.pasted = self.pasted.saturating_add(1);
        // And closed here, because a descriptor the compositor keeps open is
        // one whose reader never sees the end of the data.
        close(fd);
        true
    }

    /// A connection went: if it owned the selection, there is no selection.
    pub fn client_gone(&mut self, clients: &mut [crate::state::Slot], client: usize) {
        for which in [Which::Clipboard, Which::Primary] {
            if self
                .held(which)
                .is_some_and(|selection| selection.client == client)
            {
                match which {
                    Which::Clipboard => self.selection = None,
                    Which::Primary => self.primary = None,
                }
                self.offer_to_all(which, clients);
            }
        }
    }
}

/// Close a descriptor the compositor was handed.
fn close(fd: Fd) {
    #[expect(
        unsafe_code,
        reason = "AUDIT: close is not in std for a raw descriptor; this one arrived over the \
                  socket and is the compositor's to close once it has been passed on"
    )]
    // SAFETY: a descriptor this process received and owns.
    let _ = unsafe { libc::close(fd.0) };
}
