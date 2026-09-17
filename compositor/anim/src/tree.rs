//! Hyprland's animation tree: the names, and what each inherits.
//!
//! `animation = NAME, ONOFF, SPEED, CURVE[, STYLE]` sets one node. The nodes
//! are a tree -- `windowsMove` under `windows` under `global` -- and a node
//! that has not been set takes its parent's settings, which is what makes
//! `animation = global, 1, 3, myCurve` change everything at once.
//!
//! The names and their parents are
//! `src/config/shared/animation/AnimationTree.cpp`'s, at efb5099, and the
//! roots' settings are that file's `setConfigForNode` calls: `global` is on,
//! speed 8, curve `default`.
//!
//! # Speed is in deciseconds
//!
//! `getPercent` is `clamp((milliseconds / 100) / speed, 0, 1)`
//! (`hyprutils/src/animation/AnimatedVariable.cpp`), so speed 8 is 800
//! milliseconds and speed 3 is 300. Nothing in Hyprland's configuration says
//! so; it is only in that line.

use std::collections::BTreeMap;

/// What one node says.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Whether the animation runs at all. A node that is off warps: the
    /// value goes straight to its goal.
    pub enabled: bool,
    /// In deciseconds: how long the animation takes, times ten.
    pub speed: f32,
    /// The bezier's name.
    pub curve: String,
    /// The style, such as `slide` or `popin 80%`, kept as written. Nothing
    /// reads it yet.
    pub style: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            speed: 8.0,
            curve: "default".to_owned(),
            style: String::new(),
        }
    }
}

/// Every node Hyprland's tree has, with its parent.
///
/// In the order `AnimationTree.cpp` creates them, so a parent is always
/// before its children.
pub const NODES: &[(&str, &str)] = &[
    ("global", ""),
    ("windows", "global"),
    ("layers", "global"),
    ("fade", "global"),
    ("border", "global"),
    ("borderangle", "global"),
    ("shadowangle", "global"),
    ("glowangle", "global"),
    ("workspaces", "global"),
    ("zoomFactor", "global"),
    ("monitorAdded", "global"),
    ("layersIn", "layers"),
    ("layersOut", "layers"),
    ("windowsIn", "windows"),
    ("windowsOut", "windows"),
    ("windowsMove", "windows"),
    ("fadeIn", "fade"),
    ("fadeOut", "fade"),
    ("fadeSwitch", "fade"),
    ("fadeShadow", "fade"),
    ("fadeGlow", "fade"),
    ("fadeDim", "fade"),
    ("fadeLayers", "fade"),
    ("fadeLayersIn", "fadeLayers"),
    ("fadeLayersOut", "fadeLayers"),
    ("fadePopups", "fade"),
    ("fadePopupsIn", "fadePopups"),
    ("fadePopupsOut", "fadePopups"),
    ("fadeDpms", "fade"),
    ("workspacesIn", "workspaces"),
    ("workspacesOut", "workspaces"),
    ("specialWorkspace", "workspaces"),
    ("specialWorkspaceIn", "specialWorkspace"),
    ("specialWorkspaceOut", "specialWorkspace"),
];

/// The tree, with whatever the configuration set.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    set: BTreeMap<String, Settings>,
}

impl Tree {
    /// A tree with nothing set, so every node is `global`'s.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `name` is a node of the tree.
    #[must_use]
    pub fn has(name: &str) -> bool {
        NODES.iter().any(|(node, _)| *node == name)
    }

    /// Set one node, as an `animation` line does.
    ///
    /// Says whether the node exists; Hyprland answers `no such animation`
    /// for one that does not, and a compositor that quietly took it would
    /// leave a person's typo doing nothing for ever.
    pub fn set(&mut self, name: &str, settings: Settings) -> bool {
        if !Self::has(name) {
            return false;
        }
        let _ = self.set.insert(name.to_owned(), settings);
        true
    }

    /// What `name` ends up with: its own settings, or the nearest ancestor's
    /// that was set, or `global`'s defaults.
    #[must_use]
    pub fn get(&self, name: &str) -> Settings {
        let mut at = name;
        loop {
            if let Some(settings) = self.set.get(at) {
                return settings.clone();
            }
            let Some((_, parent)) = NODES.iter().find(|(node, _)| *node == at) else {
                return Settings::default();
            };
            if parent.is_empty() {
                return Settings::default();
            }
            at = parent;
        }
    }

    /// How long `name`'s animation takes, in milliseconds.
    ///
    /// Zero for a node that is off, which is a value that warps.
    #[must_use]
    pub fn duration(&self, name: &str) -> f32 {
        let settings = self.get(name);
        if !settings.enabled {
            return 0.0;
        }
        (settings.speed * 100.0).max(0.0)
    }
}
