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
    /// `drawn` is what of the buffer has been written since it was last
    /// handed over, in its own pixels: a screen that is sent its picture
    /// rather than scanning it out of this memory is sent that much.
    ///
    /// # Errors
    ///
    /// Whatever the screen said.
    fn present(&mut self, drawn: &compositor_render::Damage) -> io::Result<()>;

    /// Show what was drawn on the GPU, from now on, rather than this
    /// backend's own buffer: `fd` names the buffer object the renderer drew
    /// into, which the card is given and points its screen at.
    ///
    /// A backend that cannot -- one with no card, which is every backend
    /// but a real screen's -- says so, and the renderer fetches its frame
    /// into [`Backend::buffer`] as before. `docs/GPU.md` §3.5 piece 6 is
    /// what this is for: a frame that is already the device's, shown
    /// without being handed back to it.
    ///
    /// # Errors
    ///
    /// Whatever the card said. A backend that adopted nothing is unchanged.
    fn adopt(
        &mut self,
        fd: std::os::fd::BorrowedFd<'_>,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> io::Result<()> {
        let _ = (fd, width, height, pitch);
        Err(io::Error::other("this screen shows only its own buffer"))
    }

    /// The size of image this screen's cursor plane shows, if it has one: a
    /// pointer on a plane moves without a frame being drawn
    /// (`crate::plane`).
    fn cursor_plane(&self) -> Option<(u32, u32)> {
        None
    }

    /// Show `image` -- premultiplied `ARGB8888`, rows packed, the plane's
    /// size -- on the cursor plane with its hotspot at `hot` and its
    /// top-left corner at `at`, in the screen's own pixels. Returns once the
    /// image is on the screen.
    ///
    /// # Errors
    ///
    /// A screen with no plane, and whatever the screen said.
    fn set_cursor(&mut self, image: &[u8], hot: (i32, i32), at: (i32, i32)) -> io::Result<()> {
        let _ = (image, hot, at);
        Err(io::Error::other("this screen has no cursor plane"))
    }

    /// Put the cursor plane's image's top-left corner at `at`, waiting for
    /// nothing.
    ///
    /// # Errors
    ///
    /// A screen with no plane, and whatever the screen said.
    fn move_cursor(&mut self, at: (i32, i32)) -> io::Result<()> {
        let _ = at;
        Err(io::Error::other("this screen has no cursor plane"))
    }

    /// Whether [`Backend::adopt`] took: a frame that is shown from the
    /// device needs no fetching.
    fn adopted(&self) -> bool {
        false
    }

    /// How many frames old the buffer [`Backend::buffer`] gives is: one for
    /// a screen with one buffer, two for one that draws into two in turn.
    ///
    /// What a frame has to copy into it: its own damage, and that of every
    /// frame the buffer missed.
    fn age(&self) -> u32 {
        1
    }

    /// Whether the screen went away under the compositor: its card's driver
    /// died, and every call to it answers `ENODEV` until one is started
    /// again (`docs/DEVMGR.md` §4). A lost screen shows nothing and is not
    /// drawn for; the compositor and its clients carry on.
    fn lost(&self) -> bool {
        false
    }

    /// Try to have a lost screen back: open its card again and set the mode
    /// it had. Whether it is back, which a screen that was never lost always
    /// is. Cheap to call every frame: a backend tries only now and then.
    fn recover(&mut self) -> bool {
        true
    }

    /// A descriptor that becomes readable when the screen may have gone,
    /// for the compositor's wait: `None` for a screen that cannot go, and
    /// for one that already has.
    fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        None
    }

    /// Look, after [`Backend::raw_fd`] was readable, whether the screen has
    /// gone: `true` when it went just now. Without this a screen nothing is
    /// redrawn on would not find out until its next frame. A change of its
    /// modes found on the way is kept for [`Backend::modes_changed`].
    fn check(&mut self) -> bool {
        false
    }

    /// Whether the screen's modes changed since this was last asked: a
    /// virtio-gpu whose window on the host was resized, which now prefers
    /// the window's size.
    fn modes_changed(&mut self) -> bool {
        false
    }

    /// The size the screen prefers now, read from the card again: `None`
    /// for a screen that cannot say.
    fn preferred(&self) -> Option<(u32, u32)> {
        None
    }

    /// Run the screen at another of its modes, `size`, with buffers made
    /// afresh. A frame the GPU drew is no longer shown: the renderer's
    /// target is the old size, and has to be made again and adopted.
    ///
    /// # Errors
    ///
    /// A screen that has no such mode or cannot change, and whatever the
    /// card said; the screen is then as it was.
    fn resize(&mut self, size: (u32, u32)) -> io::Result<()> {
        let _ = size;
        Err(io::Error::other("this screen has one size"))
    }

    /// What to say about this backend in the compositor's log line.
    fn describe(&self) -> String;

    /// What the monitor on it is called: the connector's name, which
    /// `hyprctl monitors`, a `monitor =` line and `focusmonitor` all use.
    fn name(&self) -> String;

    /// What the monitor says it is: its make, model and serial with spaces
    /// between them, which is Hyprland's `m_shortDescription`.
    ///
    /// The thing `monitor = desc:...`, `hyprctl monitors`,
    /// `wl_output.description` and a bar's own `"output"` setting all match
    /// on. A connector's *name* moves when a cable does and a description
    /// does not, which is why a person writes the description.
    fn description(&self) -> String;

    /// The three parts of that description, for `hyprctl monitors`, which
    /// prints them apart as well as together.
    fn made(&self) -> (String, String, String) {
        (String::new(), String::new(), String::new())
    }
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

    fn present(&mut self, _drawn: &compositor_render::Damage) -> io::Result<()> {
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

    fn description(&self) -> String {
        // Hyprland's headless backend gives its outputs this description,
        // and a configuration written for one matches on it.
        "Headless output 1".to_owned()
    }
}

/// The screen: `/dev/dri/card0`, through the legacy mode-setting calls.
///
/// Two dumb buffers, drawn into in turn and shown with a page flip. One
/// buffer would work and would tear: the card scans out of the same memory
/// the compositor is writing, so half a frame is on screen while the other
/// half is being drawn.
///
/// Unless the card does not scan out of that memory at all. A virtio-gpu's
/// host holds a copy of the buffer and shows the copy, which changes only
/// when the guest says what to bring up to date -- so nothing tears, and a
/// flip is the expensive way to say it: the scanout is set again and the
/// *whole* buffer sent, whatever the frame changed. On that card the screen
/// is one buffer and a frame is `DRM_IOCTL_MODE_DIRTYFB` over its damage:
/// a pointer's frame sends a pointer's worth of pixels to the host rather
/// than two million. Linux's own `virtio_gpu` answers the call the same way.
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
    /// Whether the card shows a copy of the buffer it is told to bring up
    /// to date, so that the first buffer is the only one drawn into.
    copied: bool,
    /// What the GPU drew, once a renderer has handed it over: the screen is
    /// pointed at it and the buffers above are never shown again.
    adopted: Option<compositor_drm::Imported>,
    width: u32,
    height: u32,
    frames: u32,
    /// When the card went away, or when it was last looked for since: `None`
    /// while the screen is there.
    lost: Option<std::time::Instant>,
    /// The size of image the card's cursor plane shows, if it has one.
    cursor_size: Option<(u32, u32)>,
    /// The cursor plane's two images, each made the first time it is
    /// needed, and which the next image is drawn into.
    ///
    /// Two, because a card may show the plane from the buffer itself: the
    /// DK1's LTDC scans its second layer straight out of it, where
    /// virtio-gpu's host takes a copy. Drawn into while shown, the pointer
    /// would be half the old image and half the new, at the old hotspot's
    /// place, until the card was told; drawn into the other one, the image
    /// and its place change together at the card's next frame.
    cursors: [Option<compositor_drm::Dumb>; 2],
    next_cursor: usize,
    /// Whether the card said its connectors changed and nobody has asked
    /// [`Backend::modes_changed`] since.
    modes_changed: bool,
}

/// How often a lost screen's card is looked for.
#[cfg(target_os = "linux")]
const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether `error` is the card saying its driver has gone.
#[cfg(target_os = "linux")]
fn gone(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENODEV)
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
    ///
    /// `rules` are the configuration's `monitor =` lines. One that names a
    /// resolution has it taken before the mode is set, where the connector
    /// lists it; the rest of what a rule says -- where the monitor goes, its
    /// scale, whether it is used -- is `Screen::all`'s.
    pub fn open_all(rules: &[compositor_config::MonitorRule]) -> Vec<Self> {
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
        for (card, mut plan) in cards.into_iter().zip(plans) {
            let description = plan
                .edid
                .as_ref()
                .map(|edid| edid.describe(compositor_drm::registered))
                .unwrap_or_default();
            // The last rule that names this monitor, as everywhere else.
            let rule = rules
                .iter()
                .rev()
                .find(|rule| rule.matches(&plan.name, &description));
            if let Some(compositor_config::Mode::Fixed {
                width,
                height,
                refresh,
            }) = rule.map(|rule| rule.mode)
            {
                let _ = plan.take((width, height), refresh);
            }
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
        // A card that will not say what it is is drawn on the way that is
        // right for every card.
        let copied = card.driver().is_ok_and(|driver| driver == "virtio_gpu");
        let cursor_size = card.cursor_size();
        Ok(Self {
            card,
            plan,
            adopted: None,
            buffers: [first, second],
            back: usize::from(!copied),
            copied,
            width,
            height,
            frames: 0,
            lost: None,
            cursor_size,
            cursors: [None, None],
            next_cursor: 0,
            modes_changed: false,
        })
    }

    /// This screen's connector as the card describes it now, named as it
    /// was: `rename` numbered it among every card's.
    fn replanned(&self) -> io::Result<compositor_drm::Plan> {
        let mut plan = compositor_drm::plans(&self.card)?
            .into_iter()
            .find(|plan| plan.connector == self.plan.connector)
            .ok_or_else(|| io::Error::other("the connector is not there any more"))?;
        plan.name.clone_from(&self.plan.name);
        Ok(plan)
    }

    /// The screen again, on a card opened afresh: the same connector, at the
    /// same size, so nothing laid out on it moves. `None` while the card is
    /// not there yet, or is there with no such connector or mode.
    fn reopened(&self) -> Option<Self> {
        let index = self.card.name().strip_prefix("card")?.parse().ok()?;
        let card = std::rc::Rc::new(compositor_drm::Card::open_index(index).ok()?);
        let mut plans = compositor_drm::plans(&card).ok()?;
        let at = plans
            .iter()
            .position(|plan| plan.connector == self.plan.connector)
            .unwrap_or(0);
        let mut plan = (at < plans.len()).then(|| plans.swap_remove(at))?;
        let _ = plan.take((self.width, self.height), None);
        if plan.size() != (self.width, self.height) {
            return None;
        }
        // Named as it was: `rename` numbered it among every card's.
        plan.name.clone_from(&self.plan.name);
        let mut screen = Self::on(card, plan).ok()?;
        screen.frames = self.frames;
        Some(screen)
    }

    /// A cursor call's result, with a refusal taken to mean the card has no
    /// plane after all: asked again every pass, it would refuse every pass.
    fn refused(&mut self, result: io::Result<()>) -> io::Result<()> {
        if result.is_err() {
            self.cursor_size = None;
        }
        result
    }

    /// Note that the card went away, for [`Backend::recover`] to look for it
    /// from the next frame on.
    fn lose(&mut self) {
        self.lost = Some(
            std::time::Instant::now()
                .checked_sub(RETRY)
                .unwrap_or_else(std::time::Instant::now),
        );
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

    fn adopt(
        &mut self,
        fd: std::os::fd::BorrowedFd<'_>,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> io::Result<()> {
        let imported = compositor_drm::Imported::new(&self.card, fd, width, height, pitch)?;
        // Shown the long way round, as the first frame is: a page flip needs
        // a mode already set, and the mode is set to another buffer now.
        let _ = self.card.set_mode(&self.plan, &imported)?;
        self.adopted = Some(imported);
        Ok(())
    }

    fn adopted(&self) -> bool {
        self.adopted.is_some()
    }

    fn cursor_plane(&self) -> Option<(u32, u32)> {
        self.cursor_size.filter(|_| self.lost.is_none())
    }

    fn set_cursor(&mut self, image: &[u8], hot: (i32, i32), at: (i32, i32)) -> io::Result<()> {
        if self.lost.is_some() {
            return Ok(());
        }
        let (width, height) = self
            .cursor_size
            .ok_or_else(|| io::Error::other("this card has no cursor plane"))?;
        let Some(slot) = self.cursors.get_mut(self.next_cursor) else {
            return Ok(());
        };
        if slot.is_none() {
            *slot = Some(compositor_drm::Dumb::new(&self.card, width, height)?);
        }
        let Some(buffer) = slot.as_mut() else {
            return Ok(());
        };
        let pitch = buffer.pitch as usize;
        let row = width as usize * 4;
        let pixels = buffer.pixels();
        for (y, from) in image.chunks(row).take(height as usize).enumerate() {
            if let Some(to) = pixels.get_mut(y * pitch..y * pitch + from.len()) {
                to.copy_from_slice(from);
            }
        }
        let shown = self.cursors.get(self.next_cursor).and_then(Option::as_ref);
        let result = self.card.set_cursor(&self.plan, shown, hot, at);
        // The card now shows this one, and the call returned once it did:
        // the other is no longer read, and takes the next image.
        if result.is_ok() {
            self.next_cursor ^= 1;
        }
        match result {
            Err(error) if gone(&error) => {
                self.lose();
                Ok(())
            }
            result => self.refused(result),
        }
    }

    fn move_cursor(&mut self, at: (i32, i32)) -> io::Result<()> {
        if self.lost.is_some() {
            return Ok(());
        }
        match self.card.move_cursor(&self.plan, at) {
            Err(error) if gone(&error) => {
                self.lose();
                Ok(())
            }
            result => self.refused(result),
        }
    }

    fn present(&mut self, drawn: &compositor_render::Damage) -> io::Result<()> {
        if self.lost.is_some() {
            return Ok(());
        }
        // What the GPU drew is one texture, shown where it lies: there is no
        // second buffer to flip to and nothing to copy into it, so every
        // frame is a dirty rectangle -- which on this card costs the host
        // being told what changed and not one pixel.
        let shown: &dyn compositor_drm::Shown = match self.adopted.as_ref() {
            Some(imported) => imported,
            None => match self.buffers.get(self.back) {
                Some(buffer) => buffer,
                None => return Ok(()),
            },
        };
        let buffer = shown;
        let flipped = !(self.copied || self.adopted.is_some());
        let result = if !flipped {
            let edge = |value: i64| u32::try_from(value.max(0)).unwrap_or(u32::MAX);
            let clips: Vec<(u32, u32, u32, u32)> = drawn
                .rects()
                .iter()
                .map(|rect| {
                    (
                        edge(rect.x),
                        edge(rect.y),
                        edge(rect.width),
                        edge(rect.height),
                    )
                })
                .collect();
            // A frame that drew nothing has nothing to send, and an empty
            // list would say "all of it".
            if !clips.is_empty() {
                self.card.dirty(buffer, &clips)
            } else {
                Ok(())
            }
        } else {
            self.card.page_flip(&self.plan, buffer)
        };
        match result {
            Ok(()) => {
                if flipped {
                    self.back = 1 - self.back;
                }
                self.frames = self.frames.saturating_add(1);
                Ok(())
            }
            // The driver died. The compositor, its clients and the frame just
            // drawn are all still good; only the card is not, until one is
            // started again.
            Err(error) if gone(&error) => {
                self.lose();
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn lost(&self) -> bool {
        self.lost.is_some()
    }

    fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        // A lost card's descriptor is readable for good, and a wait on it
        // would never sleep: while lost, the frame loop looks for it instead.
        self.lost.is_none().then(|| self.card.raw_fd())
    }

    fn check(&mut self) -> bool {
        if self.lost.is_some() {
            return false;
        }
        let news = self.card.news();
        self.modes_changed |= news.connectors;
        if !news.gone {
            return false;
        }
        self.lose();
        true
    }

    fn modes_changed(&mut self) -> bool {
        core::mem::take(&mut self.modes_changed)
    }

    fn preferred(&self) -> Option<(u32, u32)> {
        self.replanned().ok().map(|plan| plan.size())
    }

    fn resize(&mut self, (width, height): (u32, u32)) -> io::Result<()> {
        let mut plan = self.replanned()?;
        if !plan.take((width, height), None) || plan.size() != (width, height) {
            return Err(io::Error::other(format!(
                "the connector has no {width}x{height} mode"
            )));
        }
        let first = compositor_drm::Dumb::new(&self.card, width, height)?;
        let second = compositor_drm::Dumb::new(&self.card, width, height)?;
        // Shown before anything old is let go: the card never points at a
        // buffer that is gone, the GPU's adopted one included.
        let _ = self.card.set_mode(&plan, &first)?;
        self.adopted = None;
        self.buffers = [first, second];
        self.back = usize::from(!self.copied);
        self.plan = plan;
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn recover(&mut self) -> bool {
        let Some(tried) = self.lost else {
            return true;
        };
        if tried.elapsed() < RETRY {
            return false;
        }
        self.lost = Some(std::time::Instant::now());
        match self.reopened() {
            Some(screen) => {
                *self = screen;
                true
            }
            None => false,
        }
    }

    fn age(&self) -> u32 {
        // What the GPU drew is one texture drawn into every frame, so a
        // frame owes it only its own damage, as a copied buffer does.
        if self.copied || self.adopted.is_some() {
            1
        } else {
            2
        }
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

    fn description(&self) -> String {
        self.plan
            .edid
            .as_ref()
            .map(|edid| edid.describe(compositor_drm::registered))
            .unwrap_or_default()
    }

    fn made(&self) -> (String, String, String) {
        self.plan
            .edid
            .as_ref()
            .map_or_else(Default::default, |edid| {
                (
                    compositor_drm::registered(&edid.manufacturer)
                        .unwrap_or_else(|| edid.manufacturer.clone()),
                    edid.model.clone(),
                    edid.serial.clone(),
                )
            })
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
