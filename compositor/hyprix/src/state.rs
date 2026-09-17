//! The loop: accept, read, lay out, draw, show.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use compositor_config::{Config, NoSources};
use compositor_layout::{Monitor, MonitorId, Rect, Settings, State, WindowId};
use compositor_protocol::{core, xdg_shell};
use compositor_render::{Canvas, Damage, Style};
use compositor_server::{Client, Event, Globals, Role};
use compositor_socket::{Connection, Listener, RecvError};
use compositor_wire::ObjectId;

use crate::backend::{Backend, Headless};
use crate::frame::Source;
use crate::options::Options;
use crate::pool::Mapping;

/// One connection: the protocol side, the socket, and the memory it shared.
#[derive(Debug)]
pub struct Slot {
    client: Client,
    connection: Connection,
    pools: BTreeMap<ObjectId, Mapping>,
    /// Windows this connection owns, so they can be closed when it goes.
    windows: Vec<(ObjectId, WindowId)>,
    /// Whether it is finished and waiting to be dropped.
    gone: bool,
}

impl Slot {
    /// The protocol side of this connection.
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// The pools this connection has shared, by object.
    pub const fn pools(&self) -> &BTreeMap<ObjectId, Mapping> {
        &self.pools
    }
}

/// Run the compositor.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run(options: &Options) -> Result<String, String> {
    run_with(options, &mut |_| {})
}

/// The same, saying each thing as it happens.
///
/// A compositor's log line comes at the end, and a compositor does not end:
/// so whatever is watching -- a person, or `cargo xtask test-compositor` --
/// has to be told when the screen is up rather than when the run is over.
///
/// # Errors
///
/// A sentence saying what could not be done.
pub fn run_with(options: &Options, report: &mut dyn FnMut(&str)) -> Result<String, String> {
    let mut config = read_config(options)?;
    let mut settings = Settings::from_config(&config);
    let mut style = Style::from_config(&config);

    // The screen: memory when `--headless` asked for one, and the card
    // otherwise. The card's size is the mode's, not the compositor's to
    // choose.
    let mut backend: Box<dyn Backend> = match options.headless {
        Some((width, height)) => Box::new(Headless::new(width, height)),
        None => open_screen()?,
    };
    let (width, height) = backend.size();
    let mut canvas = Canvas::new(width, height).map_err(|error| format!("a canvas: {error:?}"))?;

    // Where the socket goes. Wayland's rule is `$XDG_RUNTIME_DIR/<name>`,
    // which is what a session manager sets; a compositor started as init on a
    // machine that has just booted has no session manager and no variable, so
    // it falls back to a directory that always exists and says so in its log
    // line. A name with a slash in it is an absolute path either way.
    let display = resolve_display(&options.display);
    let listener = Listener::bind(&display).map_err(|error| format!("the socket: {error}"))?;

    let mut state = State::new(settings);
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(1),
            rect: Rect::new(0, 0, i64::from(width), i64::from(height)),
            reserved: compositor_layout::Gaps::default(),
        })
        .map_err(|error| format!("the monitor: {error:?}"))?;

    // `exec-once` from the configuration, then anything --exec added.
    for command in config
        .exec_once
        .iter()
        .map(String::as_str)
        .chain(options.exec.iter().map(String::as_str))
    {
        start(command, listener.path());
    }

    // `hyprctl`'s socket, when one was asked for. Hyprland puts it under
    // $XDG_RUNTIME_DIR/hypr/<instance>/, and a program looks there.
    let control = match options.instance.as_deref() {
        Some(instance) => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            Some(
                crate::control::Control::bind(&runtime, instance)
                    .map_err(|error| format!("hyprctl's socket: {error}"))?,
            )
        }
        None => None,
    };

    let mut slots: Vec<Slot> = Vec::new();
    let mut sources: BTreeMap<WindowId, Source> = BTreeMap::new();
    let mut next_window = 1u32;
    let mut drawn = 0u32;
    // The most windows at once, not the count at the end: a client that ran
    // and closed leaves none behind, and "it never got a window" and "it got
    // one and gave it back" are not the same thing for a test to read.
    let mut most = 0usize;
    let started = Instant::now();
    let deadline = options.deadline.map(Duration::from_millis);

    loop {
        if let Some(limit) = deadline
            && started.elapsed() > limit
        {
            break;
        }
        if let Some(limit) = options.frames
            && drawn >= limit
        {
            break;
        }

        // New connections.
        while let Ok(Some(stream)) = listener.accept() {
            match Connection::new(stream) {
                Ok(connection) => slots.push(Slot {
                    client: {
                        let mut client = Client::new(globals());
                        // What the screen is, and what the seat has. There is
                        // no input path yet -- stage 17's L5 to L7 are the
                        // kernel side -- so the seat announces nothing rather
                        // than handing out a keyboard that never sends a key.
                        client.set_output(compositor_server::Output {
                            width: i32::try_from(width).unwrap_or(0),
                            height: i32::try_from(height).unwrap_or(0),
                            ..compositor_server::Output::default()
                        });
                        client.set_seat_capabilities(0);
                        client
                    },
                    connection,
                    pools: BTreeMap::new(),
                    windows: Vec::new(),
                    gone: false,
                }),
                Err(_) => continue,
            }
        }

        // `hyprctl`: one request a connection, answered and closed.
        let mut asked: Vec<compositor_ipc::Reply> = Vec::new();
        if let Some(control) = control.as_ref()
            && let Some(mut stream) = control.accept()
        {
            let snapshot = crate::control::snapshot(&state, &slots, &sources, (width, height));
            match crate::control::serve(&mut stream, &snapshot) {
                Ok(todo) => asked = todo,
                Err(_) => {
                    // A client that went away mid-request is not the
                    // compositor's problem.
                }
            }
        }

        let mut changed = false;
        for index in 0..slots.len() {
            if serve(
                &mut slots,
                index,
                &mut state,
                &mut sources,
                &mut next_window,
            )? {
                changed = true;
            }
        }
        for reply in asked {
            if run_ipc(
                &reply,
                &mut state,
                &mut config,
                &mut settings,
                &mut style,
                &mut slots,
                &sources,
            ) {
                changed = true;
            }
        }

        // A window arriving or leaving resizes every other window on the
        // workspace, and a window that is not told is one drawing at the
        // size it had before -- which the compositor then draws cropped.
        // Hyprland reconfigures the whole workspace for the same reason.
        if changed {
            reconfigure(&mut slots, &state);
        }

        // A connection that ended takes its windows with it.
        for index in 0..slots.len() {
            if slots.get(index).is_some_and(|slot| slot.gone) {
                let windows = slots
                    .get(index)
                    .map(|slot| slot.windows.clone())
                    .unwrap_or_default();
                for (_, window) in windows {
                    let _ = state.window_gone(window);
                    let _ = sources.remove(&window);
                    changed = true;
                }
            }
        }
        slots.retain(|slot| !slot.gone);

        most = most.max(sources.len());
        if changed || drawn == 0 {
            let outputs = state.layout();
            let Some(output) = outputs.first() else {
                continue;
            };
            let full = Damage::full(width, height);
            let mut target = crate::frame::Output {
                canvas: &mut canvas,
                backend: backend.as_mut(),
                origin: (0, 0),
                style: &style,
            };
            crate::frame::draw(&mut target, output, &slots, &sources, &full)?;
            drawn = drawn.saturating_add(1);
            if drawn == 1 {
                // The screen is up and the first frame is on it. This is what
                // a watcher waits for, in the shape `compositor/blank`'s
                // marker has.
                report(&format!("hyprix: {} {display}", backend.describe()));
            }
            if let Some(directory) = options.dump.as_ref() {
                let _ = std::fs::create_dir_all(directory);
                let path = directory.join(format!("frame-{drawn:04}.ppm"));
                crate::backend::write_ppm(backend.as_ref(), &path)
                    .map_err(|error| format!("writing {}: {error}", path.display()))?;
            }
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    Ok(format!(
        "hyprix: {} {display} frames {drawn} windows {} most {most}",
        backend.describe(),
        sources.len()
    ))
}

/// Read what one connection sent and act on it.
///
/// Gives whether the layout changed.
fn serve(
    slots: &mut [Slot],
    index: usize,
    state: &mut State,
    sources: &mut BTreeMap<WindowId, Source>,
    next_window: &mut u32,
) -> Result<bool, String> {
    let Some(slot) = slots.get_mut(index) else {
        return Ok(false);
    };
    match slot.connection.receive() {
        Ok(_) => {}
        Err(RecvError::WouldBlock) => {}
        Err(_) => {
            slot.gone = true;
            return Ok(false);
        }
    }

    let arrived = slot.connection.fds();
    let consumed = slot.client.read(slot.connection.bytes(), &arrived);
    let mut changed = false;
    if consumed > 0 {
        let events = slot.client.take_events();
        let mut claimed = 0;
        for event in events {
            match event {
                Event::PoolCreated { pool, memory } => {
                    claimed += 1;
                    match Mapping::new(memory.fd, memory.size) {
                        Ok(mapping) => {
                            let _ = slot.pools.insert(pool, mapping);
                        }
                        Err(_) => {
                            // A descriptor that is not memory: the client
                            // gets nothing drawn, and the protocol has no
                            // error the compositor may send after the fact.
                        }
                    }
                }
                Event::PoolResized { pool, size } => {
                    if let Some(mapping) = slot.pools.get_mut(&pool) {
                        let _ = mapping.resize(size);
                    }
                }
                Event::Destroyed {
                    object,
                    role: Role::ShmPool,
                } => {
                    let _ = slot.pools.remove(&object);
                }
                Event::ToplevelCreated { toplevel, .. } => {
                    let window = WindowId(u64::from(*next_window));
                    *next_window = next_window.saturating_add(1);
                    slot.windows.push((toplevel, window));
                    let _ = state
                        .open_window(window)
                        .map_err(|error| format!("placing a window: {error:?}"))?;
                    changed = true;
                }
                Event::SurfaceCommitted { surface, change } => {
                    if change.buffer.is_none()
                        && let Some((toplevel, window)) = slot
                            .windows
                            .iter()
                            .find(|(top, _)| {
                                slot.client
                                    .toplevel(*top)
                                    .is_some_and(|state| state.surface == surface)
                            })
                            .copied()
                    {
                        // The first commit of a window asks to be configured.
                        configure(&mut slot.client, state, toplevel, window);
                        let _ = sources.insert(
                            window,
                            Source {
                                client: index,
                                surface,
                            },
                        );
                    }
                    changed = true;
                    if let Some(old) = change.released {
                        slot.client.release_buffer(old);
                    }
                }
                _ => {}
            }
        }
        slot.connection.consume(consumed, claimed);
    }

    let outgoing = slot.client.take_outgoing();
    if !outgoing.bytes.is_empty()
        && slot
            .connection
            .send(&outgoing.bytes, &outgoing.descriptors)
            .is_err()
    {
        slot.gone = true;
    }
    if slot.client.is_finished() {
        slot.gone = true;
    }
    Ok(changed)
}

/// Where the Wayland socket goes.
///
/// A name with a `/` in it is an absolute path, as `wl_display_connect` reads
/// one. A bare name is joined to `XDG_RUNTIME_DIR` when there is one, and to
/// `/tmp` when there is not: a compositor started as init has no session
/// manager to set the variable, and refusing to start over it would be a
/// compositor that only runs where something else ran first.
fn resolve_display(display: &str) -> String {
    if display.contains('/') || std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        return display.to_owned();
    }
    format!("/tmp/{display}")
}

/// Open the screen, or say why not.
///
/// Only Linux, and Ferrix through its Linux ABI, have `/dev/dri`; elsewhere
/// the compositor is headless or it is nothing.
#[cfg(target_os = "linux")]
fn open_screen() -> Result<Box<dyn Backend>, String> {
    crate::backend::Drm::open()
        .map(|screen| Box::new(screen) as Box<dyn Backend>)
        .map_err(|error| format!("/dev/dri/card0: {error}"))
}

/// The same, where there is no `/dev/dri`.
#[cfg(not(target_os = "linux"))]
fn open_screen() -> Result<Box<dyn Backend>, String> {
    Err("this host has no /dev/dri; run with --headless".to_owned())
}

/// Do what a `hyprctl` request asked, and say whether the layout changed.
///
/// `dispatch` is `compositor/layout`'s own dispatcher table, so `hyprctl
/// dispatch movefocus l` and a keybind of the same name do the same thing.
/// `keyword` changes one option while the compositor runs, which is what
/// `hyprctl keyword general:gaps_in 10` is for.
fn run_ipc(
    reply: &compositor_ipc::Reply,
    state: &mut State,
    config: &mut Config,
    settings: &mut Settings,
    style: &mut Style,
    slots: &mut [Slot],
    sources: &BTreeMap<WindowId, Source>,
) -> bool {
    match reply {
        compositor_ipc::Reply::Dispatch { name, argument } => {
            match state.dispatch_str(name, argument) {
                Ok(changes) => {
                    // `killactive` asks a window to close, which is the
                    // client's to obey; the layout says which window.
                    for change in &changes {
                        if let compositor_layout::Change::Close(window) = change {
                            close(*window, slots, sources);
                        }
                    }
                    !changes.is_empty()
                }
                Err(_) => false,
            }
        }
        compositor_ipc::Reply::Keyword { name, value } => {
            if config.keyword(name, value).is_err() {
                return false;
            }
            *settings = Settings::from_config(config);
            *style = Style::from_config(config);
            let _ = state.set_settings(*settings);
            true
        }
        compositor_ipc::Reply::Reload | compositor_ipc::Reply::Text(_) => false,
    }
}

/// Ask the window's client to close it.
fn close(window: WindowId, slots: &mut [Slot], sources: &BTreeMap<WindowId, Source>) {
    let Some(source) = sources.get(&window) else {
        return;
    };
    let Some(slot) = slots.get_mut(source.client) else {
        return;
    };
    let toplevel = slot
        .client
        .toplevels()
        .find(|(_, top)| top.surface == source.surface)
        .map(|(id, _)| id);
    if let Some(toplevel) = toplevel {
        slot.client.close_toplevel(toplevel);
    }
}

/// Tell every window the size the layout gives it now.
///
/// A configure a client has already been given and has acked is not sent
/// again: an unchanged size is a round trip a client does not need, and
/// Hyprland does not send one either.
fn reconfigure(slots: &mut [Slot], state: &State) {
    let mut sizes: BTreeMap<WindowId, (i32, i32, bool)> = BTreeMap::new();
    for output in state.layout() {
        for placed in &output.windows {
            let _ = sizes.insert(
                placed.window,
                (
                    i32::try_from(placed.rect.width).unwrap_or(0),
                    i32::try_from(placed.rect.height).unwrap_or(0),
                    placed.focused,
                ),
            );
        }
    }
    for slot in slots.iter_mut() {
        let windows = slot.windows.clone();
        for (toplevel, window) in windows {
            let Some((width, height, focused)) = sizes.get(&window).copied() else {
                continue;
            };
            let already = slot
                .client
                .toplevel(toplevel)
                .is_some_and(|top| top.configured == (width, height));
            if already {
                continue;
            }
            slot.client
                .configure_toplevel(toplevel, width, height, &states(focused));
        }
    }
}

/// The `xdg_toplevel` states a tiled window is in.
fn states(focused: bool) -> Vec<u32> {
    let mut states = vec![
        xdg_shell::xdg_toplevel::state::TILED_LEFT,
        xdg_shell::xdg_toplevel::state::TILED_RIGHT,
        xdg_shell::xdg_toplevel::state::TILED_TOP,
        xdg_shell::xdg_toplevel::state::TILED_BOTTOM,
    ];
    if focused {
        states.push(xdg_shell::xdg_toplevel::state::ACTIVATED);
    }
    states
}

/// Tell a window the size the layout gave it.
fn configure(client: &mut Client, state: &State, toplevel: ObjectId, window: WindowId) {
    let mut width = 0;
    let mut height = 0;
    for output in state.layout() {
        for placed in &output.windows {
            if placed.window == window {
                width = i32::try_from(placed.rect.width).unwrap_or(0);
                height = i32::try_from(placed.rect.height).unwrap_or(0);
            }
        }
    }
    let focused = state.focused_window() == Some(window);
    client.configure_toplevel(toplevel, width, height, &states(focused));
}

/// The globals the compositor offers.
fn globals() -> Globals {
    let mut globals = Globals::new();
    for (interface, version, role) in [
        (&core::WL_COMPOSITOR, 6, Role::Compositor),
        (&core::WL_SUBCOMPOSITOR, 1, Role::Subcompositor),
        (&core::WL_SHM, 1, Role::Shm),
        (&core::WL_SEAT, 7, Role::Seat),
        (&core::WL_OUTPUT, 4, Role::Output),
        (&core::WL_DATA_DEVICE_MANAGER, 3, Role::DataDeviceManager),
        (&xdg_shell::XDG_WM_BASE, 6, Role::XdgWmBase),
    ] {
        let _ = globals.add(interface, version, role);
    }
    globals
}

/// Read the configuration, or take Hyprland's defaults.
fn read_config(options: &Options) -> Result<Config, String> {
    let Some(path) = options.config.as_ref() else {
        return Ok(Config::default());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let name = path.to_string_lossy().into_owned();
    Ok(compositor_config::parse(&name, &text, &mut NoSources).config)
}

/// Start a program with `WAYLAND_DISPLAY` pointing at this compositor.
fn start(command: &str, socket: &std::path::Path) {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return;
    };
    let _ = std::process::Command::new(program)
        .args(parts)
        .env("WAYLAND_DISPLAY", socket)
        .spawn();
}
