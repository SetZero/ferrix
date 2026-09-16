//! What an object is, beyond which interface it speaks.
//!
//! The interface says how to decode a message; the role says what to do with
//! it. They are not the same thing -- two globals could one day share an
//! interface -- and keeping them apart means a handler never has to compare
//! interface pointers to find out what it is holding.

/// What a live object is to the compositor.
///
/// Carried in `wire::Objects`' per-object slot, so there is one map of live
/// objects and not two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Role {
    /// `wl_display`, object 1 of every connection.
    Display,
    /// `wl_registry`, made by `wl_display.get_registry`.
    Registry,
    /// `wl_callback`, made by `wl_display.sync`. It is destroyed by the
    /// server the moment it has fired, which is why nothing else may be
    /// addressed to it.
    Callback,
    /// `wl_compositor`, bound from the registry.
    Compositor,
    /// `wl_subcompositor`.
    Subcompositor,
    /// `wl_shm`.
    Shm,
    /// `wl_seat`.
    Seat,
    /// `wl_output`.
    Output,
    /// `wl_data_device_manager`.
    DataDeviceManager,
    /// `xdg_wm_base`.
    XdgWmBase,
    /// `zxdg_decoration_manager_v1`.
    DecorationManager,
    /// `zwlr_layer_shell_v1`.
    LayerShell,
    /// `wl_surface`, made by `wl_compositor.create_surface`.
    Surface,
    /// `wl_region`, made by `wl_compositor.create_region`.
    Region,
    /// `wl_shm_pool`, made by `wl_shm.create_pool`.
    ShmPool,
    /// `wl_buffer`, made by `wl_shm_pool.create_buffer`.
    Buffer,
    /// A `wl_callback` a `wl_surface.frame` asked for. Unlike the one
    /// `wl_display.sync` makes, it lives until a frame is drawn.
    FrameCallback,
}

impl Role {
    /// Whether a client may address requests to an object of this role.
    ///
    /// `wl_callback` is the one that cannot: the protocol gives it no
    /// requests at all, and a client that sends one to it has either lost
    /// track of an id the server already took back or is guessing.
    #[must_use]
    pub const fn takes_requests(self) -> bool {
        !matches!(self, Self::Callback | Self::FrameCallback)
    }
}
