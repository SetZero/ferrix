//! What the modules say they show, and the tree built from it.
//!
//! Each module keeps a [`ModuleView`]: its widget's name and classes, the
//! markup of its label, whether it is shown, and its tooltip. [`build`]
//! turns a bar's views into the [`Tree`] waybar's GTK widgets make, which
//! is what the stylesheet is matched against.

use crate::tree::{Kind, NodeData, Tree};

/// Which of waybar's label classes a module is built on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `ALabel`: the label itself is the named node (`cpu`, `memory`,
    /// `network`, `pulseaudio`, `clock`).
    Label,
    /// `AIconLabel`: a named box holding a hidden image and the label
    /// (`custom/*`, `hyprland/window`).
    IconLabel,
    /// A bare named box with children of its own (`tray`).
    Box,
}

/// What one module shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleView {
    /// The widget name: `custom-launcher`, `window`, `cpu`.
    pub name: String,
    /// Label or icon-label.
    pub shape: Shape,
    /// Classes on the named node: the `#id` part of the module's name,
    /// `module`, and the module's state classes.
    pub classes: Vec<String>,
    /// Classes on the label only, where it is not the named node (custom
    /// modules add `flat` and `text-button`).
    pub label_classes: Vec<String>,
    /// The label's Pango markup.
    pub markup: String,
    /// Whether the module's event box is shown.
    pub visible: bool,
    /// Whether the label within is shown.
    pub label_visible: bool,
    /// `max-length`.
    pub max_chars: Option<u32>,
    /// Whether the label ellipsizes (`max-length`, or the module's own
    /// choice, as `hyprland/window` makes).
    pub ellipsize: bool,
    /// The tooltip's markup, `None` or empty for none.
    pub tooltip: Option<String>,
}

impl ModuleView {
    /// A shown module of `shape` named `name`, with the `module` class.
    #[must_use]
    pub fn new(name: &str, shape: Shape) -> Self {
        Self {
            name: name.to_owned(),
            shape,
            classes: vec!["module".to_owned()],
            label_classes: Vec::new(),
            markup: String::new(),
            visible: true,
            label_visible: true,
            max_chars: None,
            ellipsize: false,
            tooltip: None,
        }
    }
}

/// A bar's window and its three sections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarView {
    /// Classes on `window#waybar`: the position, the output's name, the
    /// bar's `name`, `mode-default`, and modules' (`empty`, `solo`, …).
    pub window_classes: Vec<String>,
    /// `spacing`.
    pub spacing: f32,
    /// Whether there is a centre section (`no-center` false).
    pub center: bool,
    /// Whether the centre is the box's centre widget (`fixed-center`,
    /// the default) or packed after the left (`fixed-center: false`).
    pub fixed_center: bool,
    /// The modules of each section, by index into the bar's module list.
    pub sections: [Vec<usize>; 3],
}

/// The indices of the interesting nodes of a built bar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Built {
    /// The window.
    pub window: usize,
    /// For each module, its event box and the label.
    pub modules: Vec<Option<(usize, usize)>>,
}

/// Build the tree for `bar` from `modules`.
#[must_use]
pub fn build(bar: &BarView, modules: &[ModuleView]) -> (Tree, Built) {
    let mut tree = Tree::default();
    let mut window = NodeData::new("window", Kind::Window);
    window.name = Some("waybar".to_owned());
    window.classes.clone_from(&bar.window_classes);
    let window = tree.add(None, window);
    let main = tree.add(
        Some(window),
        NodeData::new(
            "box",
            Kind::Box {
                spacing: 0.0,
                center: None,
                end: 0,
            },
        ),
    );
    let mut built = Built {
        window,
        modules: vec![None; modules.len()],
    };
    let names = ["modules-left", "modules-center", "modules-right"];
    let mut children_of_main = 0usize;
    let mut center_child = None;
    for (section, (list, class)) in bar.sections.iter().zip(names).enumerate() {
        if section == 1 && !bar.center {
            continue;
        }
        let mut node = NodeData::new(
            "box",
            Kind::Box {
                spacing: bar.spacing,
                center: None,
                end: 0,
            },
        );
        node.classes.push(class.to_owned());
        let section_node = tree.add(Some(main), node);
        if section == 1 && bar.fixed_center {
            center_child = Some(children_of_main);
        }
        children_of_main += 1;
        for &index in list {
            let Some(module) = modules.get(index) else {
                continue;
            };
            let mut event_box = NodeData::new("widget", Kind::EventBox);
            event_box.visible = module.visible;
            event_box.module = Some(index);
            let event_box = tree.add(Some(section_node), event_box);
            let label_kind = Kind::Label {
                markup: module.markup.clone(),
                max_chars: module.max_chars,
                ellipsize: module.ellipsize || module.max_chars.is_some(),
                wrap: false,
            };
            let label = match module.shape {
                Shape::Label => {
                    let mut label = NodeData::new("label", label_kind);
                    label.name = Some(module.name.clone());
                    label.classes.clone_from(&module.classes);
                    label.visible = module.label_visible;
                    label.module = Some(index);
                    tree.add(Some(event_box), label)
                }
                Shape::IconLabel | Shape::Box => {
                    let mut named = NodeData::new(
                        "box",
                        Kind::Box {
                            spacing: 8.0,
                            center: None,
                            end: 0,
                        },
                    );
                    named.name = Some(module.name.clone());
                    named.classes.clone_from(&module.classes);
                    named.module = Some(index);
                    let named = tree.add(Some(event_box), named);
                    if module.shape == Shape::IconLabel {
                        let mut image = NodeData::new("image", Kind::Image);
                        image.visible = false;
                        let _ = tree.add(Some(named), image);
                    }
                    let mut label = NodeData::new("label", label_kind);
                    label.classes.clone_from(&module.label_classes);
                    label.visible = module.label_visible && module.shape == Shape::IconLabel;
                    label.module = Some(index);
                    tree.add(Some(named), label)
                }
            };
            if let Some(slot) = built.modules.get_mut(index) {
                *slot = Some((event_box, label));
            }
        }
    }
    if let Some(node) = tree.nodes.get_mut(main) {
        node.kind = Kind::Box {
            spacing: 0.0,
            center: center_child,
            end: 1,
        };
    }
    (tree, built)
}

/// A tooltip's tree: `tooltip.background > box > label`, the label
/// wrapping at 70 characters as GTK's tooltip window's does.
#[must_use]
pub fn tooltip(markup: &str) -> Tree {
    let mut tree = Tree::default();
    let mut window = NodeData::new("tooltip", Kind::Window);
    window.classes.push("background".to_owned());
    let window = tree.add(None, window);
    let inner = tree.add(
        Some(window),
        NodeData::new(
            "box",
            Kind::Box {
                spacing: 0.0,
                center: None,
                end: 0,
            },
        ),
    );
    let _ = tree.add(
        Some(inner),
        NodeData::new(
            "label",
            Kind::Label {
                markup: markup.to_owned(),
                max_chars: Some(70),
                ellipsize: false,
                wrap: true,
            },
        ),
    );
    tree
}
