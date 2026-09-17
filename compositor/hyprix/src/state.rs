//! The loop: accept, read, lay out, draw, show.

use std::collections::BTreeMap;
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
    let config = read_config(options)?;
    let settings = Settings::from_config(&config);
    let style = Style::from_config(&config);

    let (width, height) = options.headless.unwrap_or((1920, 1080));
    let mut backend = Headless::new(width, height);
    let mut canvas = Canvas::new(width, height).map_err(|error| format!("a canvas: {error:?}"))?;

    let listener =
        Listener::bind(&options.display).map_err(|error| format!("the socket: {error}"))?;

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

    let mut slots: Vec<Slot> = Vec::new();
    let mut sources: BTreeMap<WindowId, Source> = BTreeMap::new();
    let mut next_window = 1u32;
    let mut drawn = 0u32;
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
                    client: Client::new(globals()),
                    connection,
                    pools: BTreeMap::new(),
                    windows: Vec::new(),
                    gone: false,
                }),
                Err(_) => continue,
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

        if changed || drawn == 0 {
            let outputs = state.layout();
            let Some(output) = outputs.first() else {
                continue;
            };
            let full = Damage::full(width, height);
            let mut target = crate::frame::Output {
                canvas: &mut canvas,
                backend: &mut backend,
                origin: (0, 0),
                style: &style,
            };
            crate::frame::draw(&mut target, output, &slots, &sources, &full)?;
            drawn = drawn.saturating_add(1);
            if let Some(directory) = options.dump.as_ref() {
                let _ = std::fs::create_dir_all(directory);
                let path = directory.join(format!("frame-{drawn:04}.ppm"));
                backend
                    .write_ppm(&path)
                    .map_err(|error| format!("writing {}: {error}", path.display()))?;
            }
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    Ok(format!(
        "hyprix: {} {} frames {drawn} windows {}",
        backend.describe(),
        options.display,
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
