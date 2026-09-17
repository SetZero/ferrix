//! `.socket2.sock`: a line for every state change.
//!
//! The second of Hyprland's two sockets. A bar connects and reads; it never
//! writes, and the compositor never reads. Every state change becomes one
//! line, and a program that wants to know when the workspace changed reads
//! that line rather than polling `hyprctl`.
//!
//! # The line
//!
//! `CEventManager::formatEvent` (`src/managers/EventManager.cpp:126`, at
//! efb5099) is three rules and no more:
//!
//! * The line is `"{event}>>{data}\n"`.
//! * The data is cut to its first 1024 bytes.
//! * Every newline *inside the data* becomes a space, so that one event is
//!   always one line however a client titled its window.
//!
//! Nothing is escaped and nothing is quoted: a title with a comma in it
//! makes a line a reader cannot take apart, and Hyprland's own readers live
//! with that. Matching it exactly is the point, so this does too.
//!
//! # The `v2` events
//!
//! Hyprland sends most events twice, once in an old shape and once in a
//! `v2` shape that carries an id as well as a name. Both are sent, because
//! both have readers: a bar written against `workspace` still works and one
//! written against `workspacev2` gets the id it needs.
//!
//! # What is here
//!
//! The events this compositor can produce, with the payload each carries in
//! Hyprland's own source, cited on each variant. Nothing is invented: an
//! event with no counterpart here is an event a bar will not see, which is
//! better than one it sees in the wrong shape.

use crate::Snapshot;

/// The longest payload a line carries, after which Hyprland cuts it.
pub const MAX_DATA: usize = 1024;

/// One thing that happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The workspace shown changed. `workspace` carries its name and
    /// `workspacev2` its id and name (`src/output/Monitor.cpp:1505`).
    Workspace {
        /// Its number.
        id: i32,
        /// Its name.
        name: String,
    },
    /// A workspace came into being (`src/desktop/Workspace.cpp:72`).
    CreateWorkspace {
        /// Its number.
        id: i32,
        /// Its name.
        name: String,
    },
    /// A workspace went (`src/desktop/Workspace.cpp:81`).
    DestroyWorkspace {
        /// Its number.
        id: i32,
        /// Its name.
        name: String,
    },
    /// The focused monitor changed. `focusedmon` carries the monitor's name
    /// and the workspace's; `focusedmonv2` the name and the workspace's id
    /// (`src/desktop/state/FocusState.cpp:287`).
    FocusedMonitor {
        /// The monitor's name.
        monitor: String,
        /// The workspace it shows: its id and its name.
        workspace: (i32, String),
    },
    /// The focused window changed. `activewindow` carries the class and the
    /// title, `activewindowv2` the address
    /// (`src/desktop/state/FocusState.cpp:209`). Both are sent empty when
    /// nothing is focused, which is `activewindow>>,` and `activewindowv2>>`
    /// -- the comma is Hyprland's own (`:243`).
    ActiveWindow(Option<WindowRef>),
    /// A window was mapped (`src/desktop/view/Window.cpp:2369`).
    OpenWindow {
        /// Its address.
        address: u64,
        /// The workspace it opened on.
        workspace: String,
        /// Its class, which is `xdg_toplevel.set_app_id`.
        class: String,
        /// Its title.
        title: String,
    },
    /// A window went (`src/desktop/view/Window.cpp:2574`).
    CloseWindow {
        /// Its address.
        address: u64,
    },
    /// A window moved to another workspace. `movewindow` carries the address
    /// and the workspace's name, `movewindowv2` the address, the id and the
    /// name (`src/desktop/view/Window.cpp:579`).
    MoveWindow {
        /// Its address.
        address: u64,
        /// The workspace it is on now.
        workspace: (i32, String),
    },
    /// A window's title changed. `windowtitle` carries the address,
    /// `windowtitlev2` the address and the title
    /// (`src/desktop/view/Window.cpp:1452`).
    WindowTitle {
        /// Its address.
        address: u64,
        /// Its title now.
        title: String,
    },
    /// A window started or stopped floating
    /// (`src/layout/LayoutManager.cpp:47`).
    FloatingMode {
        /// Its address.
        address: u64,
        /// Whether it floats now.
        floating: bool,
    },
    /// The focused window's fullscreen changed. The payload is `1` or `0`
    /// and names no window (`src/managers/fullscreen/FullscreenController.cpp:485`).
    Fullscreen(bool),
    /// A monitor appeared. `monitoradded` carries the name, `monitoraddedv2`
    /// the id, the name and the short description
    /// (`src/output/Monitor.cpp:387`).
    MonitorAdded {
        /// Its number.
        id: i32,
        /// Its name.
        name: String,
        /// Its short description, which is empty here.
        description: String,
    },
    /// A monitor went (`src/output/Monitor.cpp:397`).
    MonitorRemoved {
        /// Its number.
        id: i32,
        /// Its name.
        name: String,
        /// Its short description.
        description: String,
    },
    /// The configuration was read again (`src/config/legacy/ConfigManager.cpp:1074`).
    ConfigReloaded,
    /// The submap changed; the payload is its name, empty for the global map
    /// (`src/config/shared/actions/ConfigActions.cpp:1667`).
    Submap(String),
    /// A layer surface was mapped (`src/desktop/view/LayerSurface.cpp:218`).
    OpenLayer(String),
    /// A layer surface went (`src/desktop/view/LayerSurface.cpp:227`).
    CloseLayer(String),
    /// A window asked for attention (`src/desktop/view/Window.cpp:1371`).
    Urgent {
        /// Its address.
        address: u64,
    },
    /// A group was made or dissolved. The payload is `1` or `0` and the
    /// head's address (`src/desktop/view/Group.cpp:66` and `:82`).
    ToggleGroup {
        /// Whether the group is there now.
        on: bool,
        /// The head's address: the window whose place in the tiling the
        /// group holds.
        address: u64,
    },
    /// A window joined a group
    /// (`src/config/shared/actions/ConfigActions.cpp:1320`).
    MoveIntoGroup {
        /// The window that joined.
        address: u64,
    },
    /// A window left one
    /// (`src/config/shared/actions/ConfigActions.cpp:1341`).
    MoveOutOfGroup {
        /// The window that left.
        address: u64,
    },
}

/// A window as `activewindow` names it: what it is, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRef {
    /// Its address.
    pub address: u64,
    /// Its class.
    pub class: String,
    /// Its title.
    pub title: String,
}

impl Event {
    /// The lines this event puts on the socket, in order.
    ///
    /// More than one, because Hyprland sends an event and its `v2` form
    /// together and a reader may be subscribed to either.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        match self {
            Self::Workspace { id, name } => {
                vec![
                    line("workspace", name),
                    line("workspacev2", &format!("{id},{name}")),
                ]
            }
            Self::CreateWorkspace { id, name } => vec![
                line("createworkspace", name),
                line("createworkspacev2", &format!("{id},{name}")),
            ],
            Self::DestroyWorkspace { id, name } => vec![
                line("destroyworkspace", name),
                line("destroyworkspacev2", &format!("{id},{name}")),
            ],
            Self::FocusedMonitor { monitor, workspace } => vec![
                line("focusedmon", &format!("{monitor},{}", workspace.1)),
                line("focusedmonv2", &format!("{monitor},{}", workspace.0)),
            ],
            Self::ActiveWindow(Some(window)) => vec![
                line(
                    "activewindow",
                    &format!("{},{}", window.class, window.title),
                ),
                line("activewindowv2", &format!("{:x}", window.address)),
            ],
            // Hyprland's own empty forms: a bare comma, and nothing.
            Self::ActiveWindow(None) => {
                vec![line("activewindow", ","), line("activewindowv2", "")]
            }
            Self::OpenWindow {
                address,
                workspace,
                class,
                title,
            } => vec![line(
                "openwindow",
                &format!("{address:x},{workspace},{class},{title}"),
            )],
            Self::CloseWindow { address } => {
                vec![line("closewindow", &format!("{address:x}"))]
            }
            Self::MoveWindow { address, workspace } => vec![
                line("movewindow", &format!("{address:x},{}", workspace.1)),
                line(
                    "movewindowv2",
                    &format!("{address:x},{},{}", workspace.0, workspace.1),
                ),
            ],
            Self::WindowTitle { address, title } => vec![
                line("windowtitle", &format!("{address:x}")),
                line("windowtitlev2", &format!("{address:x},{title}")),
            ],
            Self::FloatingMode { address, floating } => vec![line(
                "changefloatingmode",
                &format!("{address:x},{}", u8::from(*floating)),
            )],
            Self::Fullscreen(on) => {
                vec![line("fullscreen", &format!("{}", u8::from(*on)))]
            }
            Self::MonitorAdded {
                id,
                name,
                description,
            } => vec![
                line("monitoradded", name),
                line("monitoraddedv2", &format!("{id},{name},{description}")),
            ],
            Self::MonitorRemoved {
                id,
                name,
                description,
            } => vec![
                line("monitorremoved", name),
                line("monitorremovedv2", &format!("{id},{name},{description}")),
            ],
            Self::ConfigReloaded => vec![line("configreloaded", "")],
            Self::Submap(name) => vec![line("submap", name)],
            Self::OpenLayer(namespace) => vec![line("openlayer", namespace)],
            Self::CloseLayer(namespace) => vec![line("closelayer", namespace)],
            Self::Urgent { address } => {
                vec![line("urgent", &format!("{address:x}"))]
            }
            Self::ToggleGroup { on, address } => vec![line(
                "togglegroup",
                &format!("{},{address:x}", u8::from(*on)),
            )],
            Self::MoveIntoGroup { address } => {
                vec![line("moveintogroup", &format!("{address:x}"))]
            }
            Self::MoveOutOfGroup { address } => {
                vec![line("moveoutofgroup", &format!("{address:x}"))]
            }
        }
    }
}

/// One line, by `formatEvent`'s three rules.
fn line(event: &str, data: &str) -> String {
    let cut = data
        .char_indices()
        .take_while(|(at, character)| at + character.len_utf8() <= MAX_DATA)
        .map(|(at, character)| at + character.len_utf8())
        .last()
        .map_or("", |end| data.get(..end).unwrap_or(""));
    let flattened: String = cut
        .chars()
        .map(|character| if character == '\n' { ' ' } else { character })
        .collect();
    format!("{event}>>{flattened}\n")
}

/// What has been seen, so that what changed can be said.
///
/// Hyprland posts an event at the point of the change, from inside whatever
/// did it. This compositor's loop reaches the end of a pass knowing the state
/// before and the state after, and works out the difference: nothing can
/// change without being noticed, which is the failure a scattering of
/// `postEvent` calls has. The cost is that two changes in one pass are
/// reported in one go and in this function's order rather than in the order
/// they happened, which no reader of a bar can tell apart from two changes a
/// millisecond apart.
#[derive(Clone, Debug, Default)]
pub struct Watcher {
    seen: Option<Snapshot>,
}

impl Watcher {
    /// A watcher that has seen nothing, so the first snapshot is all new.
    #[must_use]
    pub const fn new() -> Self {
        Self { seen: None }
    }

    /// Take the state now, and say what changed since the last time.
    ///
    /// The first call reports everything as having appeared -- the monitor,
    /// the workspace, each window -- which is what a bar that connected
    /// before the compositor finished starting needs to hear.
    pub fn changed(&mut self, now: &Snapshot) -> Vec<Event> {
        let mut events = Vec::new();
        let before = self.seen.take().unwrap_or_default();

        // Monitors, then workspaces, then windows, then the focus: a reader
        // is told a thing exists before it is told that thing is focused.
        for monitor in &now.monitors {
            if !before.monitors.iter().any(|old| old.id == monitor.id) {
                events.push(Event::MonitorAdded {
                    id: monitor.id,
                    name: monitor.name.clone(),
                    description: String::new(),
                });
            }
        }
        for monitor in &before.monitors {
            if !now.monitors.iter().any(|new| new.id == monitor.id) {
                events.push(Event::MonitorRemoved {
                    id: monitor.id,
                    name: monitor.name.clone(),
                    description: String::new(),
                });
            }
        }

        for workspace in &now.workspaces {
            if !before.workspaces.iter().any(|old| old.id == workspace.id) {
                events.push(Event::CreateWorkspace {
                    id: workspace.id,
                    name: workspace.name.clone(),
                });
            }
        }
        for workspace in &before.workspaces {
            if !now.workspaces.iter().any(|new| new.id == workspace.id) {
                events.push(Event::DestroyWorkspace {
                    id: workspace.id,
                    name: workspace.name.clone(),
                });
            }
        }

        for window in &now.windows {
            match before
                .windows
                .iter()
                .find(|old| old.address == window.address)
            {
                None => events.push(Event::OpenWindow {
                    address: window.address,
                    workspace: window.workspace_name.clone(),
                    class: window.class.clone(),
                    title: window.title.clone(),
                }),
                Some(old) => {
                    if old.title != window.title {
                        events.push(Event::WindowTitle {
                            address: window.address,
                            title: window.title.clone(),
                        });
                    }
                    if old.workspace != window.workspace {
                        events.push(Event::MoveWindow {
                            address: window.address,
                            workspace: (window.workspace, window.workspace_name.clone()),
                        });
                    }
                    if old.floating != window.floating {
                        events.push(Event::FloatingMode {
                            address: window.address,
                            floating: window.floating,
                        });
                    }
                }
            }
        }
        for window in &before.windows {
            if !now.windows.iter().any(|new| new.address == window.address) {
                events.push(Event::CloseWindow {
                    address: window.address,
                });
            }
        }

        group_events(&before, now, &mut events);

        if before.active_workspace != now.active_workspace {
            let name = now
                .workspaces
                .iter()
                .find(|workspace| workspace.id == now.active_workspace)
                .map_or_else(
                    || now.active_workspace.to_string(),
                    |workspace| workspace.name.clone(),
                );
            events.push(Event::Workspace {
                id: now.active_workspace,
                name,
            });
        }

        if before.active_window != now.active_window {
            events.push(Event::ActiveWindow(now.active().map(|window| WindowRef {
                address: window.address,
                class: window.class.clone(),
                title: window.title.clone(),
            })));
        }

        // Fullscreen is a property of the focused window here, as the event
        // is: Hyprland's payload names no window.
        let was = before.active().is_some_and(|window| window.fullscreen);
        let is = now.active().is_some_and(|window| window.fullscreen);
        if was != is {
            events.push(Event::Fullscreen(is));
        }

        self.seen = Some(now.clone());
        events
    }
}

/// What the groups did between two snapshots.
///
/// Hyprland posts these where they happen: a group appearing or going is
/// `togglegroup` with the head's address, and a window joining or leaving one
/// that stays is `moveintogroup` or `moveoutofgroup`. Told apart here by
/// whether the group itself is still there, since a snapshot names each
/// window's group rather than the groups.
fn group_events(before: &Snapshot, now: &Snapshot, events: &mut Vec<Event>) {
    let heads = |snapshot: &Snapshot| -> Vec<u64> {
        let mut heads: Vec<u64> = snapshot
            .windows
            .iter()
            .filter_map(|window| window.grouped.first().copied())
            .collect();
        heads.sort_unstable();
        heads.dedup();
        heads
    };
    let (was, is) = (heads(before), heads(now));
    for head in &is {
        if !was.contains(head) {
            events.push(Event::ToggleGroup {
                on: true,
                address: *head,
            });
        }
    }
    for head in &was {
        if !is.contains(head) {
            events.push(Event::ToggleGroup {
                on: false,
                address: *head,
            });
        }
    }
    for window in &now.windows {
        let Some(old) = before
            .windows
            .iter()
            .find(|old| old.address == window.address)
        else {
            continue;
        };
        let (left, joined) = (old.grouped.first(), window.grouped.first());
        match (left, joined) {
            // Into a group it does not head. A window that heads the group
            // it joined made it, which is the `togglegroup` above and not a
            // second event: `moveintogroup` names the window that came to
            // an existing head, whether the head was there a frame ago or a
            // batch put both in one pass.
            (None, Some(head)) if *head != window.address => {
                events.push(Event::MoveIntoGroup {
                    address: window.address,
                });
            }
            // Out of one that still is.
            (Some(head), None) if is.contains(head) => {
                events.push(Event::MoveOutOfGroup {
                    address: window.address,
                });
            }
            _ => {}
        }
    }
}
