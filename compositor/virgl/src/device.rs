//! What a renderer needs of whatever runs its streams.
//!
//! Two things do: `/dev/dri/renderD128` in a guest, and virglrenderer's test
//! server on a host. A renderer written against this trait is the same code
//! on both, which is the point -- every shader and every pass is tested on
//! the host, in a second, against the very renderer that a guest's frames
//! will reach through QEMU.

use std::io;

use crate::Region;

/// A texture to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture {
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// `VIRGL_FORMAT_*`.
    pub format: u32,
    /// `VIRGL_BIND_*`: what it may be used as.
    pub bind: u32,
    /// Whether pixels will be moved to or from it. One that is only ever
    /// drawn into and sampled from -- a blur's scratch -- needs no memory on
    /// this side at all, and on a guest that is the difference between a
    /// page and a screen's worth of pinned pages.
    pub moved: bool,
}

/// Somewhere streams run.
pub trait Device {
    /// Make a texture, and answer the number a stream names it by.
    ///
    /// # Errors
    ///
    /// The device's.
    fn texture(&mut self, texture: Texture) -> io::Result<u32>;

    /// Make a buffer of `bytes`, for vertices.
    ///
    /// # Errors
    ///
    /// The device's.
    fn buffer(&mut self, bytes: u32) -> io::Result<u32>;

    /// Write pixels into `region` of a texture made to be `moved`. `data`
    /// begins at the region's first pixel and its rows are `stride` bytes
    /// apart, which is how a client's padded buffer is handed over without
    /// being gathered first.
    ///
    /// # Errors
    ///
    /// The device's; a `data` too short for the region.
    fn upload(&mut self, resource: u32, region: Region, stride: u32, data: &[u8])
    -> io::Result<()>;

    /// Run a stream. It may still be running when this returns; a
    /// [`Device::read`] comes after it.
    ///
    /// # Errors
    ///
    /// The device's. What the *renderer* made of the words is not among
    /// them: a stream it could not run is a picture that is wrong.
    fn submit(&mut self, words: &[u32]) -> io::Result<()>;

    /// Read `region` of a texture made to be `moved` back: four bytes a
    /// pixel, rows packed.
    ///
    /// # Errors
    ///
    /// The device's.
    fn read(&mut self, resource: u32, region: Region) -> io::Result<Vec<u8>>;
}
