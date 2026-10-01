//! The screen: one dumb buffer on `/dev/dri/card0`, shown once and then
//! brought up to date a band of rows at a time.
//!
//! One buffer, not two, because the card this runs on is a virtio-gpu,
//! which scans out of the host's copy of the buffer: drawing into it is
//! invisible until `DIRTYFB` copies a band across, so nothing half-drawn is
//! ever shown.

use std::io;

use compositor_drm::{Card, Dumb, Plan, plan};

/// The card, the mode and the buffer.
#[derive(Debug)]
pub(crate) struct Screen {
    card: Card,
    dumb: Dumb,
    /// Pixels across.
    pub(crate) width: usize,
    /// Pixels down.
    pub(crate) height: usize,
    plan: Plan,
}

impl Screen {
    /// Open the card, make a black buffer the size of the screen, and show
    /// it.
    pub(crate) fn open() -> io::Result<Self> {
        let card = Card::open()?;
        let plan = plan(&card)?;
        let (width, height) = (u32::from(plan.mode.hdisplay), u32::from(plan.mode.vdisplay));
        let mut dumb = Dumb::new(&card, width, height)?;
        dumb.pixels().fill(0);
        let _ = card.set_mode(&plan, &dumb)?;
        Ok(Self {
            card,
            dumb,
            width: width as usize,
            height: height as usize,
            plan,
        })
    }

    /// Bytes from one row to the next.
    pub(crate) const fn pitch(&self) -> usize {
        self.dumb.pitch as usize
    }

    /// The memory to draw into.
    pub(crate) fn pixels(&mut self) -> &mut [u8] {
        self.dumb.pixels()
    }

    /// Show what was drawn into rows `top` to `top + rows` of the screen.
    pub(crate) fn show(&self, top: usize, rows: usize) -> io::Result<()> {
        let edge = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        self.card
            .dirty(&self.dumb, &[(0, edge(top), edge(self.width), edge(rows))])
    }

    /// The mode's name, for the log.
    pub(crate) fn mode(&self) -> String {
        format!("{}x{} {}", self.width, self.height, self.plan.name)
    }
}
