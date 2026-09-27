//! The running bar: outputs, layer surfaces, the modules' loop, the
//! pointer and tooltips, over `compositor/toolkit`.
//!
//! As `client.cpp` does, a bar is made for every output a bar object asks
//! for (`config::bars_for`), when the output is described and again when
//! one is added; a bar is dropped with its output. Each bar is a
//! `zwlr_layer_surface_v1` on its output, anchored to its edge and the two
//! beside it, `height` tall (or as tall as its content: GTK never makes a
//! window smaller than its widgets ask, and waybar says so with
//! `Requested height: … is less than the minimum height: …`), reserving its
//! height unless it is not `exclusive`.
//!
//! The pointer is GTK's: the module under it is prelit (`:hover` on its
//! named node), the cursor is a hand over a module that does something when
//! clicked, a button press is a `GDK_BUTTON_PRESS` and a second or third one
//! within 400 ms a `2BUTTON`/`3BUTTON` press as well, and a wheel notch is
//! one scroll. A tooltip opens after GTK's 500 ms hover delay, at once when
//! one was already open (GTK's browse mode), as an `xdg_popup` the bar's
//! layer surface takes, placed below the pointer as `gtk_tooltip_position`
//! places it: centred under a cursor-sized rectangle at the pointer,
//! flipped above where there is no room below.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use compositor_toolkit::{
    Anchor, Client, Command, CursorShape, Event, KeyboardInteractivity, Layer, LayerOptions,
    Margin, OutputId, PointerEvent, PopupOptions, Rect as ToolkitRect, SurfaceId, TimerId, WatchId,
};

use crate::bar::{self, LayerName, Position};
use crate::config::{self, Output, System};
use crate::css::Stylesheet;
use crate::diag::{Diagnostics, Level};
use crate::json::Value;
use crate::layout::{self, Measure, Placement};
use crate::modules::{self, ChildKey, Host, Module, Press, Scroll, TimerKey, network::Interface};
use crate::paint::{self, Images, Text};
use crate::tree::Tree;
use crate::view::{self, BarView, Built};

/// What lays out and draws text, both halves, and what it has to say.
pub trait Engine: Measure + Text {
    /// Its diagnostics, taken with the bar's.
    fn diagnostics(&mut self) -> Option<&mut Diagnostics> {
        None
    }
}

/// GTK's tooltip delay (`gtk-tooltip-timeout`'s old default, which GTK 3.24
/// keeps as a constant).
pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// GTK's double-click time.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The cursor size GTK anchors a tooltip against.
pub const CURSOR_SIZE: i32 = 24;

/// An open tooltip.
#[derive(Debug)]
struct Tip {
    module: usize,
    surface: SurfaceId,
    markup: String,
    tree: Tree,
    size: (u32, u32),
}

/// One bar on one output.
#[derive(Debug)]
struct BarState {
    output: OutputId,
    output_name: String,
    config: Value,
    options: bar::Options,
    surface: Option<SurfaceId>,
    modules: Vec<Box<dyn Module>>,
    /// Each module's name in the config, beside `modules`.
    names: Vec<String>,
    sections: [Vec<usize>; 3],
    size: (u32, u32),
    tree: Tree,
    built: Built,
    placement: Placement,
    hover: Option<usize>,
    pointer: (f64, f64),
    tip: Option<Tip>,
    tip_timer: Option<TimerId>,
    dirty: bool,
    base_classes: Vec<String>,
    module_classes: BTreeSet<String>,
    scrolled: f64,
}

impl std::fmt::Debug for dyn Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module({})", self.view().name)
    }
}

/// Who owns a child, a timer or a watched descriptor.
#[derive(Debug, Default)]
struct Owners {
    children: BTreeMap<u32, (usize, usize)>,
    timers: BTreeMap<u32, (usize, usize)>,
    watches: BTreeMap<u32, (usize, usize)>,
}

/// Hyprland's sockets, once found.
#[derive(Debug)]
struct Hyprland {
    dir: PathBuf,
    events: Option<(UnixStream, WatchId)>,
    pending: String,
}

/// The bar program.
pub struct App<E: Engine> {
    client: Client,
    root: Value,
    sheet: Stylesheet,
    engine: E,
    images: Box<dyn Images>,
    bars: Vec<BarState>,
    owners: Owners,
    diag: Diagnostics,
    level: Level,
    hyprland: Option<Hyprland>,
    start: Instant,
    last_press: Option<(Instant, u32, u8)>,
    signals: Vec<i32>,
    /// Outputs bars have been made for: the toolkit reports the outputs
    /// there are at connection as added, too, and a bar is made once.
    seen: BTreeSet<OutputId>,
}

impl<E: Engine> std::fmt::Debug for App<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "App({} bars)", self.bars.len())
    }
}

/// The C library's `SIGRTMIN`.
#[must_use]
pub fn sigrtmin() -> i32 {
    libc::SIGRTMIN()
}

/// What the loop gives a module while it handles one event.
struct Ctx<'a> {
    client: &'a mut Client,
    owners: &'a mut Owners,
    bar: usize,
    module: usize,
    output: &'a str,
    diag: &'a mut Diagnostics,
    hyprland: Option<&'a Path>,
    start: Instant,
}

impl Host for Ctx<'_> {
    fn run(&mut self, command: &str, lines: bool) -> Option<ChildKey> {
        let command = Command {
            line: command.to_owned(),
            env: vec![("WAYBAR_OUTPUT_NAME".to_owned(), self.output.to_owned())],
            output: if lines {
                compositor_toolkit::ChildOutput::Lines
            } else {
                compositor_toolkit::ChildOutput::Whole
            },
        };
        match self.client.run(&command) {
            Ok(child) => {
                let _ = self
                    .owners
                    .children
                    .insert(child.0, (self.bar, self.module));
                Some(u64::from(child.0))
            }
            Err(error) => {
                self.diag.error(format!(
                    "Unable to exec cmd {}, error {error}",
                    command.line
                ));
                None
            }
        }
    }

    fn kill(&mut self, child: ChildKey) {
        if let Ok(id) = u32::try_from(child) {
            self.client.kill(compositor_toolkit::ChildId(id));
        }
    }

    fn spawn(&mut self, command: &str) {
        if let Err(error) = compositor_toolkit::spawn(command) {
            self.diag
                .error(format!("Unable to exec cmd {command}, error {error}"));
        }
    }

    fn timer(&mut self, after: Duration) -> TimerKey {
        let timer = self.client.add_timer(after, None);
        let _ = self.owners.timers.insert(timer.0, (self.bar, self.module));
        u64::from(timer.0)
    }

    fn cancel(&mut self, timer: TimerKey) {
        if let Ok(id) = u32::try_from(timer) {
            self.client.cancel_timer(TimerId(id));
            let _ = self.owners.timers.remove(&id);
        }
    }

    fn read(&mut self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn watch(&mut self, fd: i32) -> modules::WatchKey {
        let watch = self.client.watch_fd(fd);
        let _ = self.owners.watches.insert(watch.0, (self.bar, self.module));
        u64::from(watch.0)
    }

    fn unwatch(&mut self, watch: modules::WatchKey) {
        if let Ok(id) = u32::try_from(watch) {
            self.client.unwatch_fd(WatchId(id));
            let _ = self.owners.watches.remove(&id);
        }
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn wall(&self) -> (i64, i64) {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        (seconds, local_offset())
    }

    fn hyprland(&mut self, request: &str) -> Option<String> {
        let dir = self.hyprland?;
        let mut stream = UnixStream::connect(dir.join(".socket.sock")).ok()?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        stream.write_all(request.as_bytes()).ok()?;
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        Some(answer)
    }

    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn diag(&mut self) -> &mut Diagnostics {
        self.diag
    }

    fn interface(&mut self, name: &str) -> Option<Interface> {
        interface(name)
    }

    fn sigrtmin(&self) -> i32 {
        sigrtmin()
    }
}

/// The local time's offset from UTC: `TZ` of the form `UTC±h` or `UTC`,
/// else none. Ferrix has no zoneinfo; the clock module says what it does
/// not carry out.
fn local_offset() -> i64 {
    0
}

/// The `ifreq` ioctls for `name`: flags, address, netmask.
fn interface(name: &str) -> Option<Interface> {
    let bytes = name.as_bytes();
    if bytes.len() >= libc::IFNAMSIZ {
        return None;
    }
    // SAFETY: a plain socket call with constant arguments; the descriptor
    // is closed below.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return None;
    }
    let ask = |request: libc::c_ulong| -> Option<libc::ifreq> {
        // SAFETY: `ifreq` is plain data; all zeros is a valid value.
        let mut ifreq: libc::ifreq = unsafe { core::mem::zeroed() };
        for (slot, &byte) in ifreq.ifr_name.iter_mut().zip(bytes) {
            *slot = libc::c_char::from_ne_bytes([byte]);
        }
        // libc types the `SIOCGIF*` numbers `c_ulong` but musl's `ioctl`
        // takes a `c_int`; every one fits in either.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "the SIOCGIF* numbers are below 0x10000"
        )]
        let request = request as libc::Ioctl;
        // SAFETY: `ifreq` is a live, writable `struct ifreq` and the request
        // is one of the `SIOCGIF*` getters, which write only into it.
        let answer = unsafe { libc::ioctl(fd, request, &raw mut ifreq) };
        (answer == 0).then_some(ifreq)
    };
    let flags = ask(libc::SIOCGIFFLAGS).map(|ifreq| {
        // SAFETY: SIOCGIFFLAGS filled the union's flags member.
        let flags = unsafe { ifreq.ifr_ifru.ifru_flags };
        u32::from(flags.cast_unsigned())
    });
    let address = |request| {
        ask(request).and_then(|ifreq| {
            // SAFETY: the getter filled the union's address member.
            let address = unsafe { ifreq.ifr_ifru.ifru_addr };
            if i32::from(address.sa_family) != libc::AF_INET {
                return None;
            }
            // sockaddr_in: family, port, then the address.
            let octets = address.sa_data;
            let byte = |at: usize| {
                octets
                    .get(at)
                    .map_or(0, |b| u8::from_ne_bytes(b.to_ne_bytes()))
            };
            Some(std::net::Ipv4Addr::new(byte(2), byte(3), byte(4), byte(5)))
        })
    };
    let found = flags.map(|flags| Interface {
        flags,
        address: address(libc::SIOCGIFADDR),
        netmask: address(libc::SIOCGIFNETMASK),
    });
    // SAFETY: `fd` is the socket made above, closed once.
    let _ = unsafe { libc::close(fd) };
    found
}

impl<E: Engine> App<E> {
    /// Connect, read the files, and make the bars for the outputs there are.
    ///
    /// # Errors
    ///
    /// No compositor, no layer shell, or no config: what waybar would stop at.
    pub fn new(
        client: Client,
        root: Value,
        sheet: Stylesheet,
        engine: E,
        images: Box<dyn Images>,
        level: Level,
    ) -> Result<Self, String> {
        Self::with_hyprland(client, root, sheet, engine, images, level, None)
    }

    /// [`App::new`], with Hyprland's socket directory given rather than
    /// found from `$HYPRLAND_INSTANCE_SIGNATURE`: a test's compositor's.
    ///
    /// # Errors
    ///
    /// As [`App::new`].
    pub fn with_hyprland(
        client: Client,
        root: Value,
        sheet: Stylesheet,
        engine: E,
        images: Box<dyn Images>,
        level: Level,
        hyprland: Option<PathBuf>,
    ) -> Result<Self, String> {
        if client.bound_version("zwlr_layer_shell_v1").is_none() {
            return Err(
                "The Wayland compositor does not support wlr-layer-shell protocol".to_owned(),
            );
        }
        let mut app = Self {
            client,
            root,
            sheet,
            engine,
            images,
            bars: Vec::new(),
            owners: Owners::default(),
            diag: Diagnostics::default(),
            level,
            hyprland: None,
            start: Instant::now(),
            last_press: None,
            signals: Vec::new(),
            seen: BTreeSet::new(),
        };
        app.find_hyprland(hyprland);
        let outputs: Vec<OutputId> = app.client.outputs().iter().filter_map(|o| o.id).collect();
        for output in outputs {
            app.add_output(output);
        }
        app.watch_signals();
        Ok(app)
    }

    /// The diagnostics said so far, at or above the level.
    pub fn take_lines(&mut self) -> Vec<String> {
        if let Some(engine) = self.engine.diagnostics() {
            self.diag.lines.append(&mut engine.lines);
        }
        self.diag.drain(self.level)
    }

    /// The surfaces of the bars, for tests.
    #[must_use]
    pub fn bar_surfaces(&self) -> Vec<SurfaceId> {
        self.bars.iter().filter_map(|bar| bar.surface).collect()
    }

    /// The open tooltip's surface, for tests.
    #[must_use]
    pub fn tooltip_surface(&self) -> Option<SurfaceId> {
        self.bars
            .iter()
            .find_map(|bar| bar.tip.as_ref().map(|tip| tip.surface))
    }

    fn find_hyprland(&mut self, given: Option<PathBuf>) {
        let needs = match &self.root {
            Value::Array(bars) => bars.iter().any(uses_hyprland),
            bar => uses_hyprland(bar),
        };
        if !needs {
            return;
        }
        let dir = if let Some(given) = given {
            given
        } else {
            let Ok(signature) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") else {
                self.diag.warn(
                    "Hyprland is not running, Hyprland IPC will not be available.".to_owned(),
                );
                return;
            };
            let runtime = std::env::var("XDG_RUNTIME_DIR").ok();
            if !runtime
                .as_deref()
                .is_some_and(|r| Path::new(r).join("hypr").exists())
            {
                self.diag.warn(
                    "$XDG_RUNTIME_DIR/hypr does not exist, falling back to /tmp/hypr".to_owned(),
                );
            }
            PathBuf::from(modules::hyprland::socket_folder(
                runtime.as_deref(),
                &signature,
                &|path| Path::new(path).exists(),
            ))
        };
        self.diag.info("Hyprland IPC starting".to_owned());
        let events = match UnixStream::connect(dir.join(".socket2.sock")) {
            Ok(stream) => {
                let _ = stream.set_nonblocking(true);
                let watch = self.client.watch_fd(stream.as_raw_fd());
                Some((stream, watch))
            }
            Err(error) => {
                self.diag
                    .error(format!("Hyprland IPC: Unable to connect? {error}"));
                None
            }
        };
        self.hyprland = Some(Hyprland {
            dir,
            events,
            pending: String::new(),
        });
    }

    fn watch_signals(&mut self) {
        let base = sigrtmin();
        let mut wanted: BTreeSet<i32> = [libc::SIGUSR1, libc::SIGUSR2].into_iter().collect();
        for bar in &self.bars {
            for (_, value) in bar.config.members() {
                let signal = value.get("signal");
                if signal.is_int()
                    && let Some(n) = signal.as_i64().and_then(|n| i32::try_from(n).ok())
                {
                    let _ = wanted.insert(base + n);
                }
            }
        }
        let wanted: Vec<i32> = wanted.into_iter().collect();
        if wanted != self.signals {
            if let Err(error) = self.client.watch_signals(&wanted) {
                self.diag.error(format!("signals: {error}"));
            }
            self.signals = wanted;
        }
    }

    fn describe(&self, output: OutputId) -> Option<Output> {
        let found = self.client.output(output)?;
        let pick = |a: &str, b: &str| {
            if a.is_empty() {
                b.to_owned()
            } else {
                a.to_owned()
            }
        };
        let (width, height) = found.logical_size();
        Some(Output {
            name: pick(&found.xdg_name, &found.name),
            description: pick(&found.xdg_description, &found.description),
            width,
            height,
        })
    }

    fn add_output(&mut self, output: OutputId) {
        let Some(described) = self.describe(output) else {
            return;
        };
        if !self.seen.insert(output) {
            return;
        }
        self.diag.debug(format!(
            "Output detection done: {} ({})",
            described.name,
            described.identifier()
        ));
        let configs: Vec<Value> = config::bars_for(&self.root, &described, &System, &mut self.diag)
            .into_iter()
            .cloned()
            .collect();
        if configs.is_empty() {
            self.diag.info(format!(
                "No bar configuration asks for output {} ({}); none is made there",
                described.name,
                described.identifier()
            ));
        }
        for config in configs {
            self.make_bar(output, &described, config);
        }
    }

    fn make_bar(&mut self, output: OutputId, described: &Output, config: Value) {
        let options = bar::Options::read(&config, &mut self.diag);
        let index = self.bars.len();
        let mut state = BarState {
            output,
            output_name: described.name.clone(),
            config: config.clone(),
            options: options.clone(),
            surface: None,
            modules: Vec::new(),
            names: Vec::new(),
            sections: [Vec::new(), Vec::new(), Vec::new()],
            size: (0, 0),
            tree: Tree::default(),
            built: Built::default(),
            placement: Placement::default(),
            hover: None,
            pointer: (0.0, 0.0),
            tip: None,
            tip_timer: None,
            dirty: true,
            base_classes: vec![options.position.name().to_owned(), described.name.clone()],
            module_classes: BTreeSet::new(),
            scrolled: 0.0,
        };
        if !options.name.is_empty() {
            state.base_classes.push(options.name.clone());
        }
        state.base_classes.push("mode-default".to_owned());
        self.bars.push(state);
        for (section, names) in options.modules.iter().enumerate() {
            if section == 1 && options.no_center {
                continue;
            }
            for name in names {
                self.add_module(index, section, name, &config);
            }
        }
        self.create_surface(index);
    }

    fn add_module(&mut self, index: usize, section: usize, name: &str, config: &Value) {
        let Some(bar) = self.bars.get(index) else {
            return;
        };
        let module_index = bar.modules.len();
        let output = bar.output_name.clone();
        let hyprland = self.hyprland.as_ref().map(|h| h.dir.clone());
        let mut ctx = Ctx {
            client: &mut self.client,
            owners: &mut self.owners,
            bar: index,
            module: module_index,
            output: &output,
            diag: &mut self.diag,
            hyprland: hyprland.as_deref(),
            start: self.start,
        };
        match modules::make(name, config.get(name), &output, &mut ctx) {
            Ok(mut module) => {
                module.start(&mut ctx);
                if let Some(bar) = self.bars.get_mut(index) {
                    bar.modules.push(module);
                    bar.names.push(name.to_owned());
                    if let Some(list) = bar.sections.get_mut(section) {
                        list.push(module_index);
                    }
                }
            }
            Err(error) => self.diag.warn(format!("module {name}: {error}")),
        }
    }

    fn natural_height(&mut self, index: usize) -> u32 {
        self.rebuild(index);
        let Some(bar) = self.bars.get(index) else {
            return 0;
        };
        let (_, height) = layout::natural_size(&bar.tree, &mut self.engine);
        #[expect(clippy::cast_possible_truncation, reason = "a bar's height in pixels")]
        #[expect(clippy::cast_sign_loss, reason = "a size is not negative")]
        let height = height.ceil().max(0.0) as u32;
        height
    }

    fn create_surface(&mut self, index: usize) {
        let natural = self.natural_height(index);
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        let options = &bar.options;
        let horizontal = matches!(options.position, Position::Top | Position::Bottom);
        let requested = if horizontal {
            options.height
        } else {
            options.width
        };
        if requested > 1 && requested < natural {
            self.diag.warn(format!(
                "Requested height: {requested} is less than the minimum height: {natural} required by the modules"
            ));
        }
        let height = requested.max(natural);
        let mut anchor = match options.position {
            Position::Top => Anchor::TOP.with(Anchor::LEFT).with(Anchor::RIGHT),
            Position::Bottom => Anchor::BOTTOM.with(Anchor::LEFT).with(Anchor::RIGHT),
            Position::Left => Anchor::LEFT.with(Anchor::TOP).with(Anchor::BOTTOM),
            Position::Right => Anchor::RIGHT.with(Anchor::TOP).with(Anchor::BOTTOM),
        };
        if horizontal && options.width > 1 {
            anchor = match options.position {
                Position::Bottom => Anchor::BOTTOM,
                _ => Anchor::TOP,
            };
        }
        let [top, right, bottom, left] = options.margin;
        let layer_options = LayerOptions {
            output: Some(bar.output),
            layer: match options.layer {
                LayerName::Bottom => Layer::Bottom,
                LayerName::Top => Layer::Top,
                LayerName::Overlay => Layer::Overlay,
            },
            namespace: options.namespace().to_owned(),
            size: (
                if horizontal { options.width } else { height },
                if horizontal { height } else { options.height },
            ),
            anchor,
            exclusive_zone: if options.exclusive {
                i32::try_from(height).unwrap_or(0)
                    + if horizontal {
                        top.max(bottom)
                    } else {
                        left.max(right)
                    }
            } else {
                0
            },
            margin: Margin {
                top,
                right,
                bottom,
                left,
            },
            keyboard: KeyboardInteractivity::None,
        };
        match self.client.layer_surface(&layer_options) {
            Ok(surface) => {
                if bar.options.passthrough {
                    self.client.set_input_region(surface, Some(&[]));
                }
                bar.surface = Some(surface);
            }
            Err(error) => self.diag.warn(format!("Error creating bar: {error}")),
        }
    }

    /// The tree of a bar, from its modules' views, styled.
    fn rebuild(&mut self, index: usize) {
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        let mut classes: BTreeSet<String> = bar.module_classes.clone();
        for module in &bar.modules {
            for (class, on) in module.window_classes() {
                if on {
                    let _ = classes.insert(class);
                } else {
                    let _ = classes.remove(&class);
                }
            }
        }
        bar.module_classes = classes;
        let mut window_classes = bar.base_classes.clone();
        window_classes.extend(bar.module_classes.iter().cloned());
        let views: Vec<_> = bar.modules.iter().map(|m| m.view().clone()).collect();
        let bar_view = BarView {
            window_classes,
            spacing: f32::from(i16::try_from(bar.options.spacing).unwrap_or(0)),
            center: !bar.options.no_center,
            fixed_center: bar.options.fixed_center,
            sections: bar.sections.clone(),
        };
        let (mut tree, built) = view::build(&bar_view, &views);
        if let Some(hovered) = bar.hover
            && let Some(Some((event_box, _))) = built.modules.get(hovered)
            && let Some(child) = tree
                .get(*event_box)
                .and_then(|n| n.children.first().copied())
            && let Some(node) = tree.nodes.get_mut(child)
        {
            node.hover = true;
        }
        tree.style(&self.sheet);
        bar.tree = tree;
        bar.built = built;
    }

    /// Lay out and draw a bar.
    fn redraw(&mut self, index: usize) {
        self.rebuild(index);
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        let Some(surface) = bar.surface else {
            return;
        };
        let (width, height) = bar.size;
        if width == 0 || height == 0 {
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "a surface's size in pixels")]
        let placement = layout::layout(&bar.tree, &mut self.engine, width as f32, height as f32);
        bar.placement = placement;
        #[expect(clippy::cast_precision_loss, reason = "an integer scale")]
        let scale = self.client.scale(surface) as f32;
        let (tree, placement) = (&bar.tree, &bar.placement);
        let (images, engine) = (&mut self.images, &mut self.engine);
        if let Err(error) = self.client.draw(surface, |pixmap| {
            paint::draw_tree(pixmap, tree, placement, images.as_mut(), engine, scale);
        }) {
            self.diag.error(format!("drawing the bar: {error}"));
        }
        bar.dirty = false;
    }

    /// The module under a point of a bar, by its event box's allocation.
    fn hit(&self, index: usize, x: f64, y: f64) -> Option<usize> {
        let bar = self.bars.get(index)?;
        #[expect(clippy::cast_possible_truncation, reason = "a point on a bar")]
        let (x, y) = (x as f32, y as f32);
        bar.built
            .modules
            .iter()
            .enumerate()
            .find_map(|(module, nodes)| {
                let (event_box, _) = (*nodes)?;
                if !bar.tree.shown(event_box) {
                    return None;
                }
                let placed = bar.placement.nodes.get(event_box)?;
                placed.allocation.contains(x, y).then_some(module)
            })
    }

    fn bar_of(&self, surface: SurfaceId) -> Option<usize> {
        self.bars
            .iter()
            .position(|bar| bar.surface == Some(surface))
    }

    fn with_module<T>(
        &mut self,
        index: usize,
        module: usize,
        mut each: impl FnMut(&mut dyn Module, &mut Ctx<'_>) -> T,
    ) -> Option<T> {
        let hyprland = self.hyprland.as_ref().map(|h| h.dir.clone());
        let bar = self.bars.get_mut(index)?;
        let output = bar.output_name.clone();
        let target = bar.modules.get_mut(module)?;
        let mut ctx = Ctx {
            client: &mut self.client,
            owners: &mut self.owners,
            bar: index,
            module,
            output: &output,
            diag: &mut self.diag,
            hyprland: hyprland.as_deref(),
            start: self.start,
        };
        let answer = each(target.as_mut(), &mut ctx);
        bar.dirty = true;
        Some(answer)
    }

    fn close_tip(&mut self, index: usize) {
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        if let Some(timer) = bar.tip_timer.take() {
            self.client.cancel_timer(timer);
        }
        if let Some(tip) = bar.tip.take() {
            self.client.destroy(tip.surface);
        }
    }

    fn tooltip_markup(&self, index: usize, module: usize) -> Option<String> {
        let bar = self.bars.get(index)?;
        let markup = bar.modules.get(module)?.view().tooltip.clone()?;
        (!markup.is_empty()).then_some(markup)
    }

    fn open_tip(&mut self, index: usize, module: usize) {
        let Some(markup) = self.tooltip_markup(index, module) else {
            return;
        };
        let Some(bar) = self.bars.get(index) else {
            return;
        };
        let Some(parent) = bar.surface else {
            return;
        };
        let mut tree = view::tooltip(&markup);
        tree.style(&self.sheet);
        let (width, height) = layout::natural_size(&tree, &mut self.engine);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a tooltip's size in pixels"
        )]
        #[expect(clippy::cast_sign_loss, reason = "a size is not negative")]
        let size = (width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32);
        #[expect(clippy::cast_possible_truncation, reason = "a point on the bar")]
        let (px, py) = (bar.pointer.0 as i32, bar.pointer.1 as i32);
        let options = PopupOptions {
            size: (
                i32::try_from(size.0).unwrap_or(1),
                i32::try_from(size.1).unwrap_or(1),
            ),
            anchor_rect: ToolkitRect {
                x: px - 4,
                y: py - 4,
                width: CURSOR_SIZE,
                height: CURSOR_SIZE,
            },
            // The anchor rectangle's bottom edge's middle, growing down; the
            // compositor may slide it sideways and flip it above.
            anchor: 2,
            gravity: 2,
            constraint_adjustment: 1 | 4,
            offset: (0, 0),
            grab: false,
        };
        match self.client.popup(parent, &options) {
            Ok(surface) => {
                if let Some(bar) = self.bars.get_mut(index) {
                    bar.tip = Some(Tip {
                        module,
                        surface,
                        markup,
                        tree,
                        size,
                    });
                }
            }
            Err(error) => self.diag.warn(format!("tooltip: {error}")),
        }
    }

    fn draw_tip(&mut self, index: usize) {
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        let Some(tip) = &bar.tip else {
            return;
        };
        let surface = tip.surface;
        let Some((width, height)) = self.client.size(surface) else {
            return;
        };
        #[expect(clippy::cast_precision_loss, reason = "a surface's size in pixels")]
        let placement = layout::layout(&tip.tree, &mut self.engine, width as f32, height as f32);
        #[expect(clippy::cast_precision_loss, reason = "an integer scale")]
        let scale = self.client.scale(surface) as f32;
        let (images, engine) = (&mut self.images, &mut self.engine);
        let tree = &tip.tree;
        if let Err(error) = self.client.draw(surface, |pixmap| {
            paint::draw_tree(pixmap, tree, &placement, images.as_mut(), engine, scale);
        }) {
            self.diag.error(format!("drawing a tooltip: {error}"));
        }
    }

    /// If the open tooltip's text changed, redraw it; if its module has
    /// none now, close it.
    fn refresh_tip(&mut self, index: usize) {
        let Some(bar) = self.bars.get(index) else {
            return;
        };
        let Some(tip) = &bar.tip else {
            return;
        };
        let module = tip.module;
        let old = tip.markup.clone();
        let old_size = tip.size;
        match self.tooltip_markup(index, module) {
            None => self.close_tip(index),
            Some(markup) if markup != old => {
                let mut tree = view::tooltip(&markup);
                tree.style(&self.sheet);
                let (width, height) = layout::natural_size(&tree, &mut self.engine);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a tooltip's size in pixels"
                )]
                #[expect(clippy::cast_sign_loss, reason = "a size is not negative")]
                let size = (width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32);
                if size == old_size {
                    if let Some(bar) = self.bars.get_mut(index)
                        && let Some(tip) = bar.tip.as_mut()
                    {
                        tip.markup = markup;
                        tip.tree = tree;
                    }
                    self.draw_tip(index);
                } else {
                    self.close_tip(index);
                    self.open_tip(index, module);
                }
            }
            Some(_) => {}
        }
    }

    fn pointer_at(&mut self, index: usize, x: f64, y: f64) {
        let hit = self.hit(index, x, y);
        let Some(bar) = self.bars.get_mut(index) else {
            return;
        };
        bar.pointer = (x, y);
        if hit == bar.hover {
            return;
        }
        bar.hover = hit;
        bar.dirty = true;
        let browsing = bar.tip.is_some();
        self.close_tip(index);
        let clickable = hit
            .and_then(|module| {
                self.bars
                    .get_mut(index)
                    .and_then(|bar| bar.modules.get_mut(module))
                    .map(|m| m.clickable())
            })
            .unwrap_or(false);
        self.client.set_cursor(if clickable {
            CursorShape::Pointer
        } else {
            CursorShape::Default
        });
        if let Some(module) = hit
            && self.tooltip_markup(index, module).is_some()
        {
            if browsing {
                self.open_tip(index, module);
            } else {
                let timer = self.client.add_timer(TOOLTIP_DELAY, None);
                if let Some(bar) = self.bars.get_mut(index) {
                    bar.tip_timer = Some(timer);
                }
            }
        }
    }

    fn pointer(&mut self, event: PointerEvent) {
        self.diag.debug(format!("pointer: {event:?}"));
        match event {
            PointerEvent::Enter { surface, x, y } | PointerEvent::Motion { surface, x, y } => {
                if let Some(index) = self.bar_of(surface) {
                    self.pointer_at(index, x, y);
                }
            }
            PointerEvent::Leave { surface } => {
                if let Some(index) = self.bar_of(surface)
                    && let Some(bar) = self.bars.get_mut(index)
                {
                    bar.hover = None;
                    bar.dirty = true;
                    self.close_tip(index);
                }
            }
            PointerEvent::Button {
                surface,
                x,
                y,
                button,
                pressed,
                ..
            } => self.button(surface, x, y, button, pressed),
            PointerEvent::Axis {
                surface,
                vertical,
                horizontal,
                discrete,
            } => self.axis(surface, vertical, horizontal, discrete),
        }
    }

    fn fraction(&self, index: usize, module: usize, x: f64, y: f64) -> (f32, f32) {
        let Some(bar) = self.bars.get(index) else {
            return (0.0, 0.0);
        };
        let rect = bar
            .built
            .modules
            .get(module)
            .copied()
            .flatten()
            .and_then(|(event_box, _)| bar.placement.nodes.get(event_box))
            .map(|placed| placed.allocation)
            .unwrap_or_default();
        #[expect(clippy::cast_possible_truncation, reason = "a point on a bar")]
        let (x, y) = (x as f32, y as f32);
        (
            if rect.width > 0.0 {
                (x - rect.x) / rect.width
            } else {
                0.0
            },
            if rect.height > 0.0 {
                (y - rect.y) / rect.height
            } else {
                0.0
            },
        )
    }

    fn button(&mut self, surface: SurfaceId, x: f64, y: f64, evdev: u32, pressed: bool) {
        let Some(index) = self.bar_of(surface) else {
            return;
        };
        let Some(button) = modules::gdk_button(evdev) else {
            return;
        };
        let Some(module) = self.hit(index, x, y) else {
            return;
        };
        let at = self.fraction(index, module, x, y);
        self.close_tip(index);
        let mut presses = Vec::new();
        if pressed {
            let now = Instant::now();
            let count = match self.last_press {
                Some((when, last, count)) if last == button && now - when < DOUBLE_CLICK => {
                    count + 1
                }
                _ => 1,
            };
            self.last_press = Some((now, button, if count >= 3 { 0 } else { count }));
            presses.push(Press::Single);
            match count {
                2 => presses.push(Press::Double),
                3 => presses.push(Press::Triple),
                _ => {}
            }
        } else {
            presses.push(Press::Release);
        }
        for press in presses {
            let _ = self.with_module(index, module, |target, ctx| {
                target.click(ctx, button, press, at)
            });
        }
    }

    fn axis(&mut self, surface: SurfaceId, vertical: f64, horizontal: f64, discrete: (i32, i32)) {
        let Some(index) = self.bar_of(surface) else {
            return;
        };
        let Some(bar) = self.bars.get(index) else {
            return;
        };
        let Some(module) = bar.hover else {
            return;
        };
        let mut scrolls = Vec::new();
        if discrete.0 != 0 {
            let notch = if discrete.0 < 0 {
                Scroll::Up
            } else {
                Scroll::Down
            };
            scrolls.extend(core::iter::repeat_n(
                notch,
                discrete.0.unsigned_abs() as usize,
            ));
        } else if discrete.1 != 0 {
            let notch = if discrete.1 < 0 {
                Scroll::Left
            } else {
                Scroll::Right
            };
            scrolls.extend(core::iter::repeat_n(
                notch,
                discrete.1.unsigned_abs() as usize,
            ));
        } else {
            let threshold = bar
                .config
                .get(bar.names.get(module).map_or("", String::as_str))
                .get("smooth-scrolling-threshold")
                .as_f64()
                .unwrap_or(0.0);
            let total = bar.scrolled + vertical;
            if total < -threshold {
                scrolls.push(Scroll::Up);
            } else if total > threshold {
                scrolls.push(Scroll::Down);
            } else if horizontal > threshold {
                scrolls.push(Scroll::Right);
            } else if horizontal < -threshold {
                scrolls.push(Scroll::Left);
            }
            if let Some(bar) = self.bars.get_mut(index) {
                bar.scrolled = if scrolls.is_empty() { total } else { 0.0 };
            }
        }
        for scroll in scrolls {
            let _ = self.with_module(index, module, |target, ctx| target.scroll(ctx, scroll));
        }
    }

    fn hyprland_readable(&mut self) {
        let mut lines = Vec::new();
        if let Some(hyprland) = self.hyprland.as_mut()
            && let Some((stream, _)) = hyprland.events.as_mut()
        {
            let mut buffer = [0u8; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => hyprland.pending.push_str(&String::from_utf8_lossy(
                        buffer.get(..n).unwrap_or_default(),
                    )),
                    Err(_) => break,
                }
            }
            while let Some(at) = hyprland.pending.find('\n') {
                let line: String = hyprland.pending.drain(..=at).collect();
                let line = line.trim_end_matches('\n').to_owned();
                if !line.is_empty() {
                    lines.push(line);
                }
            }
        }
        for line in lines {
            let (name, data) = line.split_once(">>").unwrap_or((line.as_str(), ""));
            self.every_module(|target, ctx| {
                let _ = target.hyprland_event(ctx, name, data);
            });
        }
    }

    fn signal(&mut self, signal: i32) {
        if signal == libc::SIGUSR1 {
            // waybar's default for SIGUSR1: show or hide every bar.
            for index in 0..self.bars.len() {
                let surface = self.bars.get_mut(index).and_then(|bar| bar.surface.take());
                match surface {
                    Some(surface) => {
                        self.close_tip(index);
                        self.client.destroy(surface);
                    }
                    None => self.create_surface(index),
                }
            }
            return;
        }
        if signal == libc::SIGUSR2 {
            self.diag.info("Reloading...".to_owned());
            self.diag.warn(
                "SIGUSR2's reload is not carried out: restart waybar to read changed files"
                    .to_owned(),
            );
            return;
        }
        self.every_module(|target, ctx| {
            let _ = target.signal(ctx, signal);
        });
    }

    /// Hand every module of every bar to `each`.
    fn every_module(&mut self, mut each: impl FnMut(&mut dyn Module, &mut Ctx<'_>)) {
        for index in 0..self.bars.len() {
            let count = self.bars.get(index).map_or(0, |bar| bar.modules.len());
            for module in 0..count {
                let _ = self.with_module(index, module, &mut each);
            }
        }
    }

    /// Forget bar `index`: its surface, its tooltip, and its children's and
    /// timers' owners; later bars move down one.
    fn remove_bar(&mut self, index: usize) {
        self.close_tip(index);
        if let Some(surface) = self.bars.get(index).and_then(|bar| bar.surface) {
            self.client.destroy(surface);
        }
        let name = self
            .bars
            .get(index)
            .map(|bar| bar.output_name.clone())
            .unwrap_or_default();
        let _ = self.bars.remove(index);
        self.owners.children.retain(|_, (bar, _)| *bar != index);
        self.owners.timers.retain(|_, (bar, _)| *bar != index);
        let owners = self
            .owners
            .children
            .values_mut()
            .chain(self.owners.timers.values_mut());
        for (bar, _) in owners.filter(|(bar, _)| *bar > index) {
            *bar -= 1;
        }
        self.diag.info(format!("Bar removed from output: {name}"));
    }

    /// Close the tooltip whose surface is `surface`.
    fn tip_closed(&mut self, surface: SurfaceId) {
        let owner = self
            .bars
            .iter()
            .position(|bar| bar.tip.as_ref().is_some_and(|tip| tip.surface == surface));
        if let Some(index) = owner {
            self.close_tip(index);
        }
    }

    /// Handle one event.
    pub fn handle(&mut self, event: Event) {
        match event {
            Event::OutputAdded(output) => self.add_output(output),
            Event::OutputRemoved(output) => {
                let gone: Vec<usize> = self
                    .bars
                    .iter()
                    .enumerate()
                    .filter(|(_, bar)| bar.output == output)
                    .map(|(index, _)| index)
                    .collect();
                for index in gone.into_iter().rev() {
                    self.remove_bar(index);
                }
                let _ = self.seen.remove(&output);
            }
            Event::Configure {
                surface,
                width,
                height,
            } => {
                if let Some(index) = self.bar_of(surface) {
                    if let Some(bar) = self.bars.get_mut(index) {
                        bar.size = (width, height);
                        bar.dirty = true;
                    }
                    self.diag.info(format!(
                        "Bar configured (width: {width}, height: {height}) for output: {}",
                        self.bars
                            .get(index)
                            .map(|b| b.output_name.clone())
                            .unwrap_or_default()
                    ));
                } else if let Some(index) = self
                    .bars
                    .iter()
                    .position(|bar| bar.tip.as_ref().is_some_and(|tip| tip.surface == surface))
                {
                    self.draw_tip(index);
                }
            }
            Event::Closed(surface) => {
                if let Some(index) = self.bar_of(surface) {
                    self.client.destroy(surface);
                    if let Some(bar) = self.bars.get_mut(index) {
                        bar.surface = None;
                    }
                } else {
                    self.tip_closed(surface);
                }
            }
            Event::Pointer(event) => self.pointer(event),
            Event::Timer(timer) => {
                if let Some(index) = self
                    .bars
                    .iter()
                    .position(|bar| bar.tip_timer == Some(timer))
                {
                    if let Some(bar) = self.bars.get_mut(index) {
                        bar.tip_timer = None;
                    }
                    if let Some(module) = self.bars.get(index).and_then(|bar| bar.hover) {
                        self.open_tip(index, module);
                    }
                    return;
                }
                if let Some((index, module)) = self.owners.timers.remove(&timer.0) {
                    let _ = self.with_module(index, module, |target, ctx| {
                        target.timer(ctx, u64::from(timer.0))
                    });
                }
            }
            Event::ChildLine { child, line } => {
                if let Some(&(index, module)) = self.owners.children.get(&child.0) {
                    let _ = self.with_module(index, module, |target, ctx| {
                        target.child_line(ctx, u64::from(child.0), &line)
                    });
                }
            }
            Event::ChildExited {
                child,
                status,
                output,
            } => {
                if let Some((index, module)) = self.owners.children.remove(&child.0) {
                    let _ = self.with_module(index, module, |target, ctx| {
                        target.child_exit(ctx, u64::from(child.0), status, &output)
                    });
                }
            }
            Event::Signal(signal) => self.signal(signal),
            Event::Readable(watch) => {
                let hyprland = self
                    .hyprland
                    .as_ref()
                    .and_then(|h| h.events.as_ref())
                    .is_some_and(|(_, mine)| *mine == watch);
                if hyprland {
                    self.hyprland_readable();
                } else if let Some(&(index, module)) = self.owners.watches.get(&watch.0) {
                    let _ = self.with_module(index, module, |target, ctx| {
                        target.readable(ctx, u64::from(watch.0))
                    });
                }
            }
            _ => {}
        }
    }

    /// Redraw what changed, after a batch of events.
    pub fn settle(&mut self) {
        self.watch_signals();
        for index in 0..self.bars.len() {
            if self.bars.get(index).is_some_and(|bar| bar.dirty) {
                self.redraw(index);
                self.refresh_tip(index);
            }
        }
    }

    /// Turn the loop once: wait for events, handle them, redraw.
    ///
    /// # Errors
    ///
    /// The connection to the compositor ending.
    pub fn turn(&mut self, timeout: Option<Duration>) -> Result<(), String> {
        let events = self
            .client
            .dispatch(timeout)
            .map_err(|error| error.to_string())?;
        for event in events {
            self.handle(event);
        }
        self.settle();
        Ok(())
    }
}

/// Whether a bar object names a `hyprland/…` module.
fn uses_hyprland(bar: &Value) -> bool {
    ["modules-left", "modules-center", "modules-right"]
        .iter()
        .flat_map(|list| bar.get(list).items())
        .any(|name| name.as_str().is_some_and(|n| n.starts_with("hyprland/")))
}
