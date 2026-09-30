//! The bar as GTK3's widget and CSS node tree, which is what the stylesheet
//! is matched against and what is laid out and drawn.
//!
//! waybar builds (`bar.cpp`, `AModule.cpp`, `ALabel.cpp`, `AIconLabel.cpp`):
//!
//! ```text
//! window#waybar.<position>.<output name>[.<name>][.empty.solo…]
//! └ box                                   the bar's box, left/right packed, centre widget
//!   ├ box.modules-left                    each spaced by "spacing"
//!   │ ├ widget                            the module's GtkEventBox
//!   │ │ └ label#cpu.module[.<id>][.state]  an ALabel: the label is the named node
//!   │ └ widget
//!   │   └ box#custom-launcher.module[.class…]   an AIconLabel: the box is named,
//!   │     ├ image                               and holds a hidden image
//!   │     └ label[.flat.text-button]             and the label
//!   ├ box.modules-center
//!   └ box.modules-right
//! ```
//!
//! and a tooltip is a window of its own, outside `window#waybar`:
//! `tooltip.background > box > label`. So `#custom-launcher:hover` is the
//! `AIconLabel`'s box in the prelight state, which waybar sets on the event
//! box's child when the pointer enters (`AModule::handleMouseEnter`), and
//! `window#waybar .ws.active` is a custom module's box carrying the classes
//! its script printed.
//!
//! A hidden widget (`event_box_.hide()`: a custom module whose script
//! printed nothing) takes no room and no spacing, and is skipped when
//! siblings are counted for `:first-child`, as GTK3 skips invisible nodes.

use crate::css::Stylesheet;
use crate::css::selector::Node;
use crate::css::style::Style;

/// What a node is, for layout.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A toplevel: the bar's window, or a tooltip's.
    Window,
    /// A `GtkBox`: children in a row with `spacing` between them. `center`
    /// is the index among `children` of a centre widget (`set_center_widget`),
    /// and `end` is how many of the children at the end are packed at the
    /// end (`pack_end`), last first.
    Box {
        /// Pixels between visible children.
        spacing: f32,
        /// The child that is centred, if any.
        center: Option<usize>,
        /// How many trailing children are packed at the end.
        end: usize,
    },
    /// A `GtkEventBox`: one child, no box of its own.
    EventBox,
    /// A `GtkLabel` showing Pango markup.
    Label {
        /// The markup.
        markup: String,
        /// `max-width-chars`, which `max-length` sets.
        max_chars: Option<u32>,
        /// Whether it ellipsizes at the end.
        ellipsize: bool,
        /// Whether it wraps (a tooltip's label: 70 chars, wrapping).
        wrap: bool,
    },
    /// A `GtkImage`.
    Image,
}

/// One node.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeData {
    /// The CSS element name.
    pub element: &'static str,
    /// The widget's name, `#name`.
    pub name: Option<String>,
    /// Style classes.
    pub classes: Vec<String>,
    /// Under the pointer (`:hover`).
    pub hover: bool,
    /// Shown.
    pub visible: bool,
    /// What it is.
    pub kind: Kind,
    /// The parent's index.
    pub parent: Option<usize>,
    /// The children's indices, in order.
    pub children: Vec<usize>,
    /// Its computed style, once [`Tree::style`] has run.
    pub style: Style,
    /// Which module it belongs to, for pointer hits: an index into the
    /// bar's module list.
    pub module: Option<usize>,
}

impl NodeData {
    /// A node of `element` with no name, classes or children.
    #[must_use]
    pub fn new(element: &'static str, kind: Kind) -> Self {
        Self {
            element,
            name: None,
            classes: Vec::new(),
            hover: false,
            visible: true,
            kind,
            parent: None,
            children: Vec::new(),
            style: Style::default(),
            module: None,
        }
    }
}

/// A tree of nodes; index 0 is the root.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    /// The nodes.
    pub nodes: Vec<NodeData>,
}

/// A node in its tree, as the selector matcher sees it.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    tree: &'a Tree,
    index: usize,
}

impl Tree {
    /// Add `node` under `parent` (or as the root), returning its index.
    pub fn add(&mut self, parent: Option<usize>, mut node: NodeData) -> usize {
        let index = self.nodes.len();
        node.parent = parent;
        self.nodes.push(node);
        if let Some(parent) = parent.and_then(|p| self.nodes.get_mut(p)) {
            parent.children.push(index);
        }
        index
    }

    /// Node `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&NodeData> {
        self.nodes.get(index)
    }

    /// Node `index`, for the matcher.
    #[must_use]
    pub fn at(&self, index: usize) -> At<'_> {
        At { tree: self, index }
    }

    /// Compute every node's style, parents before children.
    pub fn style(&mut self, sheet: &Stylesheet) {
        for index in 0..self.nodes.len() {
            let parent_style = self
                .nodes
                .get(index)
                .and_then(|node| node.parent)
                .and_then(|parent| self.nodes.get(parent))
                .map(|parent| parent.style.clone());
            let style = sheet.compute(&self.at(index), parent_style.as_ref());
            if let Some(node) = self.nodes.get_mut(index) {
                node.style = style;
            }
        }
    }

    /// Whether a node and all its ancestors are visible.
    #[must_use]
    pub fn shown(&self, index: usize) -> bool {
        let mut at = Some(index);
        while let Some(current) = at {
            match self.nodes.get(current) {
                Some(node) if node.visible => at = node.parent,
                _ => return false,
            }
        }
        true
    }
}

impl At<'_> {
    fn data(&self) -> Option<&NodeData> {
        self.tree.nodes.get(self.index)
    }

    /// The visible siblings, this one included, in order.
    fn siblings(&self) -> Vec<usize> {
        let Some(parent) = self.data().and_then(|n| n.parent) else {
            return vec![self.index];
        };
        self.tree
            .nodes
            .get(parent)
            .map(|p| {
                p.children
                    .iter()
                    .copied()
                    .filter(|&c| {
                        self.tree.nodes.get(c).is_some_and(|n| n.visible) || c == self.index
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Node for At<'_> {
    fn element(&self) -> &str {
        self.data().map_or("", |n| n.element)
    }

    fn name(&self) -> Option<&str> {
        self.data().and_then(|n| n.name.as_deref())
    }

    fn has_class(&self, class: &str) -> bool {
        self.data()
            .is_some_and(|n| n.classes.iter().any(|c| c == class))
    }

    fn in_state(&self, state: &str) -> bool {
        state == "hover" && self.data().is_some_and(|n| n.hover)
    }

    fn parent(&self) -> Option<Self> {
        let parent = self.data()?.parent?;
        Some(Self {
            tree: self.tree,
            index: parent,
        })
    }

    fn position(&self) -> (usize, usize) {
        let siblings = self.siblings();
        let index = siblings.iter().position(|&c| c == self.index).unwrap_or(0);
        (index, siblings.len())
    }

    fn previous(&self) -> Option<Self> {
        let siblings = self.siblings();
        let at = siblings.iter().position(|&c| c == self.index)?;
        let before = *siblings.get(at.checked_sub(1)?)?;
        Some(Self {
            tree: self.tree,
            index: before,
        })
    }
}
