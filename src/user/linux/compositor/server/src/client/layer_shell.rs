//! `wlr-layer-shell-unstable-v1`: bars, docks, wallpapers and launchers.
//!
//! A layer surface is not a window. It is anchored to the edges of a
//! screen, in one of four layers -- background, bottom, top and overlay --
//! and may reserve an exclusive zone the tiling keeps clear, which is how a
//! bar keeps windows from sliding under it. Waybar, fuzzel, swaybg and
//! every notification daemon are made of these.
//!
//! The client says what it wants and the compositor above works out where
//! that puts it; [`Client::configure_layer`] tells the client the size it
//! came to. A bar's menus and tooltips are `xdg_popup`s made with no
//! parent, which `get_popup` hands to the layer surface to be placed
//! against.

use compositor_protocol::layer_shell::{self, zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use compositor_protocol::xdg_shell::{xdg_surface, xdg_wm_base};
use compositor_wire::{Arg, ArgType, ObjectId};

use crate::client::{Client, Event, Fatal};
use crate::layer::{Anchors, Layer, LayerSurface, Margin};
use crate::role::Role;

impl Client {
    /// `zwlr_layer_shell_v1`: `get_layer_surface`.
    ///
    /// The surface must have no buffer and no other role, as every
    /// role-giving request requires, and the layer must be one of the four
    /// the protocol defines. A client that asks for a fifth is refused with
    /// `invalid_layer` rather than being given a surface nothing draws.
    pub(super) fn layer_shell(&mut self, version: u32, opcode: u16, args: &[Arg<'_>]) {
        if opcode != zwlr_layer_shell_v1::request::GET_LAYER_SURFACE {
            return;
        }
        let (Some(id), Some(surface)) = (
            args.first().and_then(Arg::as_object),
            args.get(1).and_then(Arg::as_object),
        ) else {
            return;
        };
        // The output is nullable: `None` means "you choose", and this
        // compositor has one monitor to choose.
        let output = args.get(2).and_then(Arg::as_object).filter(|id| id.0 != 0);
        let Some(layer) = args.get(3).and_then(Arg::as_uint).and_then(Layer::from_raw) else {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::INVALID_LAYER,
                text: "that is not one of the four layers".to_owned(),
            });
            return;
        };
        let namespace = args.get(4).and_then(Arg::as_str).unwrap_or("").to_owned();

        if !self.surfaces.contains_key(&surface) {
            self.fail(Fatal::WrongInterface {
                object: surface,
                wanted: "wl_surface",
            });
            return;
        }
        let has_buffer = self
            .surfaces
            .get(&surface)
            .is_some_and(|state| state.is_mapped() || state.pending.buffer.is_some());
        if has_buffer {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::ALREADY_CONSTRUCTED,
                text: "a surface with a buffer cannot be given a role".to_owned(),
            });
            return;
        }
        let taken = self.xdg_surfaces.values().any(|xdg| xdg.surface == surface)
            || self.layers.values().any(|live| live.surface == surface);
        if taken {
            self.fail(Fatal::Interface {
                object: id,
                code: zwlr_layer_shell_v1::error::ROLE,
                text: "that surface already has a role".to_owned(),
            });
            return;
        }
        if !self.make(
            id,
            &layer_shell::ZWLR_LAYER_SURFACE_V1,
            version,
            Role::LayerSurface,
        ) {
            return;
        }
        let _ = self
            .layers
            .insert(id, LayerSurface::new(surface, output, layer, namespace));
        self.events.push(Event::LayerSurfaceCreated {
            layer_surface: id,
            surface,
        });
    }

    /// `zwlr_layer_surface_v1`: everything a bar says about itself.
    ///
    /// Each request records what the client asked for and tells the
    /// compositor above that the placement has to be worked out again. The
    /// compositor answers with [`Client::configure_layer`], which is where
    /// the size the client is given is decided.
    pub(super) fn layer_surface_request(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        args: &[Arg<'_>],
    ) {
        let uint = |at: usize| args.get(at).and_then(Arg::as_uint).unwrap_or(0);
        let int = |at: usize| args.get(at).and_then(Arg::as_int).unwrap_or(0);
        match opcode {
            zwlr_layer_surface_v1::request::SET_SIZE => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.size = (uint(0), uint(1));
                }
            }
            zwlr_layer_surface_v1::request::SET_ANCHOR => {
                let raw = uint(0);
                if !Anchors::is_valid(raw) {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_surface_v1::error::INVALID_ANCHOR,
                        text: "that anchor has a bit the protocol does not define".to_owned(),
                    });
                    return;
                }
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.anchor = raw;
                }
            }
            zwlr_layer_surface_v1::request::SET_EXCLUSIVE_ZONE => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.exclusive_zone = int(0);
                }
            }
            zwlr_layer_surface_v1::request::SET_MARGIN => {
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.margin = Margin {
                        top: int(0),
                        right: int(1),
                        bottom: int(2),
                        left: int(3),
                    };
                }
            }
            zwlr_layer_surface_v1::request::SET_KEYBOARD_INTERACTIVITY => {
                let wanted = uint(0);
                if wanted > zwlr_layer_surface_v1::keyboard_interactivity::ON_DEMAND {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_surface_v1::error::INVALID_KEYBOARD_INTERACTIVITY,
                        text: "that is not a keyboard interactivity".to_owned(),
                    });
                    return;
                }
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.keyboard_interactivity = wanted;
                }
            }
            zwlr_layer_surface_v1::request::SET_LAYER => {
                let Some(wanted) = Layer::from_raw(uint(0)) else {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: zwlr_layer_shell_v1::error::INVALID_LAYER,
                        text: "that is not one of the four layers".to_owned(),
                    });
                    return;
                };
                if let Some(layer) = self.layers.get_mut(&sender) {
                    layer.layer = wanted;
                }
            }
            zwlr_layer_surface_v1::request::ACK_CONFIGURE => {
                let serial = uint(0);
                let known = self
                    .layers
                    .get_mut(&sender)
                    .is_some_and(|layer| layer.acked(serial));
                if !known {
                    self.fail(Fatal::Interface {
                        object: sender,
                        code: xdg_surface::error::INVALID_SERIAL,
                        text: format!("{serial} is not a configure this surface was sent"),
                    });
                }
                return;
            }
            zwlr_layer_surface_v1::request::GET_POPUP => {
                self.layer_popup(sender, args.first().and_then(Arg::as_object));
                return;
            }
            // `set_exclusive_edge` is recorded nowhere: it only matters for
            // a surface anchored to more than one edge with a zone, which
            // `place` does not reserve for anyway.
            _ => return,
        }
        self.events.push(Event::LayerSurfaceChanged {
            layer_surface: sender,
        });
    }

    /// `zwlr_layer_surface_v1.get_popup`: the layer surface `sender` takes
    /// a popup made with a null parent, which is then placed against it.
    ///
    /// The protocol says the popup must have been made with a null parent
    /// and not yet committed; one that already hangs off an `xdg_surface`
    /// is refused as `xdg_wm_base.invalid_popup_parent`, which is the error
    /// wlroots gives it.
    fn layer_popup(&mut self, sender: ObjectId, popup: Option<ObjectId>) {
        let Some(popup) = popup.filter(|popup| self.popups.contains_key(popup)) else {
            self.fail(Fatal::WrongInterface {
                object: popup.unwrap_or(ObjectId::NULL),
                wanted: "xdg_popup",
            });
            return;
        };
        let Some(held) = self.popups.get_mut(&popup) else {
            return;
        };
        if !held.parent.is_null() || held.layer_parent.is_some() {
            let object = held.xdg_surface;
            self.fail(Fatal::Interface {
                object,
                code: xdg_wm_base::error::INVALID_POPUP_PARENT,
                text: "that popup already has a parent".to_owned(),
            });
            return;
        }
        held.layer_parent = Some(sender);
        let surface = held.surface;
        self.events.push(Event::PopupCreated {
            popup,
            surface,
            parent: ObjectId::NULL,
        });
    }

    /// The layer surface `id` names, if it is one.
    #[must_use]
    pub fn layer_surface(&self, id: ObjectId) -> Option<&LayerSurface> {
        self.layers.get(&id)
    }

    /// Every layer surface, in the order they were created, which is the
    /// order wlroots places them in.
    pub fn layer_surfaces(&self) -> impl Iterator<Item = (ObjectId, &LayerSurface)> {
        self.layers.iter().map(|(id, layer)| (*id, layer))
    }

    /// Tell a layer surface the size it is to be.
    ///
    /// A size of zero on an axis the client is not anchored to both edges of
    /// is `invalid_size`: the protocol says the client must be told a real
    /// number, and a bar that forgot `set_size` finds out here rather than
    /// by drawing nothing.
    pub fn configure_layer(&mut self, layer_surface: ObjectId, width: u32, height: u32) {
        let Some(layer) = self.layers.get(&layer_surface) else {
            return;
        };
        let anchors = Anchors::from_raw(layer.anchor);
        if !layer.size_is_valid(&anchors) {
            self.fail(Fatal::Interface {
                object: layer_surface,
                code: zwlr_layer_surface_v1::error::INVALID_SIZE,
                text: "a surface not anchored to both edges of an axis must set a size on it"
                    .to_owned(),
            });
            return;
        }
        let serial = self.next_serial();
        if let Some(layer) = self.layers.get_mut(&layer_surface) {
            layer.configure_sent(serial, (width, height));
        }
        let _ = self.out.write(
            layer_surface,
            zwlr_layer_surface_v1::event::CONFIGURE,
            &[ArgType::Uint, ArgType::Uint, ArgType::Uint],
            &[Arg::Uint(serial), Arg::Uint(width), Arg::Uint(height)],
        );
    }

    /// Tell a layer surface to close, and forget it.
    ///
    /// `closed` is final, unlike `xdg_toplevel.close`: the protocol says the
    /// surface is no longer shown and the client should destroy it.
    pub fn close_layer(&mut self, layer_surface: ObjectId) {
        if self.layers.remove(&layer_surface).is_none() {
            return;
        }
        let _ = self.out.write(
            layer_surface,
            zwlr_layer_surface_v1::event::CLOSED,
            &[],
            &[],
        );
    }
}
