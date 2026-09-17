//! Where the frame goes: memory, or a screen.

use std::io;
use std::path::Path;

/// A place a frame can be put.
///
/// `Debug` so a caller holding one can derive it; a backend's own debug
/// output is its name and size, never its pixels.
pub trait Backend: core::fmt::Debug {
    /// The size of the screen in pixels.
    fn size(&self) -> (u32, u32);

    /// Bytes of an `XRGB8888` buffer of that size, to draw into.
    fn buffer(&mut self) -> &mut [u8];

    /// The bytes just drawn, to read back.
    fn drawn(&self) -> &[u8];

    /// Bytes from one row's start to the next.
    fn stride(&self) -> u32;

    /// Show what was drawn.
    ///
    /// # Errors
    ///
    /// Whatever the screen said.
    fn present(&mut self) -> io::Result<()>;

    /// What to say about this backend in the compositor's log line.
    fn describe(&self) -> String;

    /// What the monitor on it is called: the connector's name, which
    /// `hyprctl monitors`, a `monitor =` line and `focusmonitor` all use.
    fn name(&self) -> String;
}

/// A screen that is only memory: the everyday one, and the one a test reads.
#[derive(Debug)]
pub struct Headless {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
    frames: u32,
}

impl Headless {
    /// A `width` by `height` screen of memory.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let bytes = vec![0; width as usize * height as usize * 4];
        Self {
            width,
            height,
            bytes,
            frames: 0,
        }
    }

    /// How many frames have been shown.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }
}

impl Backend for Headless {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    fn drawn(&self) -> &[u8] {
        &self.bytes
    }

    fn stride(&self) -> u32 {
        self.width.saturating_mul(4)
    }

    fn present(&mut self) -> io::Result<()> {
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    fn describe(&self) -> String {
        format!("headless {}x{}", self.width, self.height)
    }

    fn name(&self) -> String {
        // What Hyprland's own headless backend calls its output.
        "HEADLESS-1".to_owned()
    }
}

/// The screen: `/dev/dri/card0`, through the legacy mode-setting calls.
///
/// Two dumb buffers, drawn into in turn and shown with a page flip. One
/// buffer would work and would tear: the card scans out of the same memory
/// the compositor is writing, so half a frame is on screen while the other
/// half is being drawn.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Drm {
    /// The card, shared with the other screens on it: only one open of a
    /// card is allowed, and a card with two connectors is two screens.
    card: std::rc::Rc<compositor_drm::Card>,
    plan: compositor_drm::Plan,
    buffers: [compositor_drm::Dumb; 2],
    /// Which buffer is being drawn into.
    back: usize,
    width: u32,
    height: u32,
    frames: u32,
}

#[cfg(target_os = "linux")]
impl Drm {
    /// Open the card, set its preferred mode and make the buffers.
    ///
    /// # Errors
    ///
    /// Whatever the card said. A compositor with no screen is the one thing
    /// it cannot do without, so this is fatal.
    pub fn open() -> io::Result<Self> {
        let card = std::rc::Rc::new(compositor_drm::Card::open()?);
        let plan = compositor_drm::plan(&card)?;
        Self::on(card, plan)
    }

    /// Every screen the machine has: each connected connector of each card,
    /// in card and connector order.
    ///
    /// A machine with two monitors has them on one card's two connectors or
    /// on a card each, and both are two screens here. The order is the
    /// order the kernel lists them in, which is what decides which monitor
    /// is the first.
    ///
    /// # Errors
    ///
    /// Nothing: a card that will not open or a connector that will not take
    /// a mode is left out, and a machine with no screen at all is the
    /// caller's to report.
    pub fn open_all() -> Vec<Self> {
        let mut cards = Vec::new();
        let mut plans = Vec::new();
        for card in compositor_drm::cards() {
            let card = std::rc::Rc::new(card);
            let Ok(found) = compositor_drm::plans(&card) else {
                continue;
            };
            for plan in found {
                cards.push(std::rc::Rc::clone(&card));
                plans.push(plan);
            }
        }
        // The names are numbered across every card, so two cards with a
        // `Virtual-1` each become `Virtual-1` and `Virtual-2`.
        compositor_drm::rename(&mut plans);
        let mut screens = Vec::new();
        for (card, plan) in cards.into_iter().zip(plans) {
            if let Ok(screen) = Self::on(card, plan) {
                screens.push(screen);
            }
        }
        screens
    }

    /// One screen: two buffers on `plan`'s connector, with its mode set.
    fn on(card: std::rc::Rc<compositor_drm::Card>, plan: compositor_drm::Plan) -> io::Result<Self> {
        let (width, height) = plan.size();
        let first = compositor_drm::Dumb::new(&card, width, height)?;
        let second = compositor_drm::Dumb::new(&card, width, height)?;
        // A page flip needs a mode already set, so the first buffer is shown
        // the long way round.
        let _ = card.set_mode(&plan, &first)?;
        Ok(Self {
            card,
            plan,
            buffers: [first, second],
            back: 1,
            width,
            height,
            frames: 0,
        })
    }

    /// How many frames have been shown.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }
}

#[cfg(target_os = "linux")]
impl Backend for Drm {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn buffer(&mut self) -> &mut [u8] {
        match self.buffers.get_mut(self.back) {
            Some(buffer) => buffer.pixels(),
            // `back` is 0 or 1 and the array has two; an empty slice would
            // draw nothing rather than draw somewhere else.
            None => &mut [],
        }
    }

    fn drawn(&self) -> &[u8] {
        // The buffer just drawn into, which `present` has not yet swapped
        // away from.
        match self.buffers.get(self.back) {
            Some(buffer) => buffer.shown(),
            None => &[],
        }
    }

    fn stride(&self) -> u32 {
        self.buffers
            .get(self.back)
            .map_or_else(|| self.width.saturating_mul(4), |buffer| buffer.pitch)
    }

    fn present(&mut self) -> io::Result<()> {
        if let Some(buffer) = self.buffers.get(self.back) {
            self.card.page_flip(&self.plan, buffer)?;
        }
        self.back = 1 - self.back;
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    fn describe(&self) -> String {
        format!(
            "{} {} {} {}x{}",
            self.card.name(),
            self.plan.name,
            compositor_drm::modeset::mode_name(&self.plan.mode),
            self.width,
            self.height
        )
    }

    fn name(&self) -> String {
        self.plan.name.clone()
    }
}

/// Write a backend's last frame as a binary PPM, which every image viewer
/// reads and which needs no library to produce.
///
/// The rows are taken a stride at a time: a card's pitch is whatever the
/// kernel chose and is usually wider than the mode, so a dump that walked
/// the buffer straight through would slant.
///
/// # Errors
///
/// Whatever the write said.
pub fn write_ppm(backend: &dyn Backend, path: &Path) -> io::Result<()> {
    use std::io::Write;
    let (width, height) = backend.size();
    let stride = backend.stride() as usize;
    let bytes = backend.drawn();
    let mut out = Vec::with_capacity(width as usize * height as usize * 3 + 32);
    write!(out, "P6\n{width} {height}\n255\n")?;
    for y in 0..height as usize {
        let row = bytes
            .get(y * stride..y * stride + width as usize * 4)
            .unwrap_or(&[]);
        for pixel in row.chunks_exact(4) {
            // The buffer is XRGB8888 little-endian: blue, green, red, unused.
            if let [blue, green, red, _] = pixel {
                out.extend_from_slice(&[*red, *green, *blue]);
            }
        }
    }
    std::fs::write(path, out)
}
