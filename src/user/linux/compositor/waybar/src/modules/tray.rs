//! `tray`: `StatusNotifierItem` icons (`modules/sni/`), which need D-Bus.
//!
//! waybar's tray is a `StatusNotifierWatcher` and host on the session bus,
//! and each icon is an item a program registered there. Ferrix has no
//! D-Bus, so no program can register one: the tray has no items, and
//! waybar hides a tray with no items (`Tray::update`). So this module is
//! always hidden, and says once why. `icon-size`, `spacing` and the rest
//! of its options have nothing to apply to.

use super::{Common, Host, Module};
use crate::json::Value;
use crate::view::{ModuleView, Shape};

/// The `tray` module.
#[derive(Debug)]
pub struct Tray {
    common: Common,
    view: ModuleView,
}

impl Tray {
    /// Make it, and say why it will stay empty.
    pub fn new(name: &str, config: &Value, host: &mut dyn Host) -> Self {
        let common = Common::new(name, config, "", 0);
        let mut view = common.view("tray", Shape::Box);
        view.visible = false;
        host.diag().warn(
            "tray: StatusNotifierItem needs a D-Bus session bus, which Ferrix does not have; the tray stays empty and hidden".to_owned(),
        );
        Self { common, view }
    }
}

impl Module for Tray {
    fn start(&mut self, _host: &mut dyn Host) {}

    fn view(&self) -> &ModuleView {
        &self.view
    }

    fn common(&mut self) -> &mut Common {
        &mut self.common
    }

    fn clickable(&mut self) -> bool {
        false
    }
}
