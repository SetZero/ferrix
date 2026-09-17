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
    /// Its `wl_data_source`, or its data-control source.
    pub source: ObjectId,
    /// The types it can give the data in, in the order it offered them.
    pub mimes: Vec<String>,
    /// Which protocol the owner set it through, which is the one it is
    /// asked for the data through: a clipboard manager's source hears
    /// `send` on its own interface and not on `wl_data_source`'s.
    pub through: Through,
}

/// Which protocol a selection was set or asked for through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Through {
    /// `wl_data_device` or `zwp_primary_selection_device_v1`: a client with
    /// a window, told only while it has the keyboard.
    Window,
    /// `zwlr_data_control_device_v1` or `ext_data_control_device_v1`: a
    /// clipboard manager, which has no window and is told regardless.
    Manager,
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
        through: Through,
    ) {
        let held = match which {
            Which::Clipboard => &mut self.selection,
            Which::Primary => &mut self.primary,
        };
        if let Some(previous) = held.take()
            && (previous.client != client || Some(previous.source) != source)
            && let Some(slot) = clients.get_mut(previous.client)
        {
            match (previous.through, which) {
                (Through::Manager, _) => slot.client_mut().control_cancel(previous.source),
                (Through::Window, Which::Clipboard) => {
                    slot.client_mut().cancel_selection(previous.source);
                }
                (Through::Window, Which::Primary) => {
                    slot.client_mut().cancel_primary(previous.source);
                }
            }
        }
        *held = source.map(|source| Selection {
            client,
            source,
            mimes,
            through,
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
            // And every clipboard manager, whether or not it has a window
            // or the keyboard. That is the whole difference between the two
            // protocols.
            if slot.client().watches_clipboard() {
                slot.client_mut()
                    .control_offer_selection(which == Which::Primary, &mimes);
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

    /// Tell one clipboard manager what both selections hold, which is what
    /// a manager that has just made a device is owed.
    pub fn offer_both_to(&self, clients: &mut [crate::state::Slot], client: usize) {
        for which in [Which::Clipboard, Which::Primary] {
            let mimes = self
                .held(which)
                .map(|selection| selection.mimes.clone())
                .unwrap_or_default();
            if let Some(slot) = clients.get_mut(client) {
                slot.client_mut()
                    .control_offer_selection(which == Which::Primary, &mimes);
            }
        }
    }

    /// A client pasted: the descriptor goes to whoever copied.
    ///
    /// Gives whether it went anywhere. A paste through an offer that is no
    /// longer the selection is dropped, which leaves the pasting client
    /// reading a pipe nothing writes to -- and its own end is closed here,
    /// so that read ends rather than hangs.
    #[expect(
        clippy::too_many_arguments,
        reason = "a paste names both selections, both protocols, who asked, through what, for which type, and the pipe"
    )]
    pub fn pasted(
        &mut self,
        which: Which,
        clients: &mut [crate::state::Slot],
        asking: usize,
        offer: ObjectId,
        mime: &str,
        fd: Fd,
        through: Through,
    ) -> bool {
        let Some(selection) = self.held(which).cloned() else {
            close(fd);
            return false;
        };
        let current = clients
            .get(asking)
            .is_some_and(|slot| match (through, which) {
                // A manager's offer is the compositor's to remember, so the
                // check is the same one and the device holds it.
                (Through::Manager, _) => slot.client().holds_control_offer(offer),
                (Through::Window, Which::Clipboard) => slot.client().holds_offer(offer),
                (Through::Window, Which::Primary) => slot.client().holds_primary_offer(offer),
            });
        if !current {
            close(fd);
            return false;
        }
        let Some(owner) = clients.get_mut(selection.client) else {
            close(fd);
            return false;
        };
        match (selection.through, which) {
            (Through::Manager, _) => owner.client_mut().control_send(selection.source, mime, fd),
            (Through::Window, Which::Clipboard) => {
                owner
                    .client_mut()
                    .send_selection(selection.source, mime, fd);
            }
            (Through::Window, Which::Primary) => {
                owner.client_mut().send_primary(selection.source, mime, fd);
            }
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

    /// A connection went, so every slot after it moved: `places[old]` is
    /// where the client that was at `old` is now.
    ///
    /// A selection holds its owner by its *place* in the list, so a place
    /// that is no longer that client is a paste answered by a stranger --
    /// or, as the clipboard boot found, one answered by nobody while the
    /// program that copied waits to be asked. The same hazard a window's
    /// `Source` and the focus have, and the same fix.
    pub fn renumber(&mut self, places: &[Option<usize>]) {
        for held in [&mut self.selection, &mut self.primary] {
            let moved = held
                .as_ref()
                .map(|selection| places.get(selection.client).copied().flatten());
            match (held.as_mut(), moved) {
                (Some(selection), Some(Some(at))) => selection.client = at,
                // Its owner is the client that went, which `client_gone`
                // has already dealt with; this is the belt to that brace.
                (Some(_), Some(None)) => *held = None,
                _ => {}
            }
        }
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
pub fn close(fd: Fd) {
    #[expect(
        unsafe_code,
        reason = "AUDIT: close is not in std for a raw descriptor; this one arrived over the \
                  socket and is the compositor's to close once it has been passed on"
    )]
    // SAFETY: a descriptor this process received and owns.
    let _ = unsafe { libc::close(fd.0) };
}

#[cfg(test)]
mod tests {
    use super::{Clipboard, Which};
    use compositor_wire::ObjectId;

    /// A clipboard whose owner is the client at `client`.
    fn holding(client: usize) -> Clipboard {
        let mut clipboard = Clipboard::default();
        for which in [Which::Clipboard, Which::Primary] {
            clipboard.copied(
                which,
                &mut [],
                client,
                Some(ObjectId(7)),
                vec!["text/plain".to_owned()],
                super::Through::Window,
            );
        }
        clipboard
    }

    /// A connection ending moves every slot after it, and the selection
    /// holds its owner by that place.
    ///
    /// This is the bug the clipboard boot found: with the owner left at its
    /// old place, a paste is answered by whichever client took that number
    /// -- or, as it happened, by nobody, while the program that copied waits
    /// to be asked.
    #[test]
    fn a_selection_follows_its_owner_when_a_client_before_it_goes() {
        let mut clipboard = holding(3);
        // Client 1 went: 0 stays, 2 becomes 1, 3 becomes 2.
        clipboard.renumber(&[Some(0), None, Some(1), Some(2)]);
        for which in [Which::Clipboard, Which::Primary] {
            assert_eq!(
                clipboard.held(which).map(|held| held.client),
                Some(2),
                "{which:?}"
            );
        }
    }

    /// And a selection whose own owner went is no selection.
    #[test]
    fn a_selection_whose_owner_went_is_gone() {
        let mut clipboard = holding(1);
        clipboard.renumber(&[Some(0), None, Some(1)]);
        for which in [Which::Clipboard, Which::Primary] {
            assert!(clipboard.held(which).is_none(), "{which:?}");
        }
    }

    /// A place nobody moved leaves the selection where it was, which is
    /// every pass in which no connection ended.
    #[test]
    fn nothing_moves_when_nothing_went() {
        let mut clipboard = holding(2);
        clipboard.renumber(&[Some(0), Some(1), Some(2)]);
        assert_eq!(
            clipboard.held(Which::Clipboard).map(|held| held.client),
            Some(2)
        );
    }
}
