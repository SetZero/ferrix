//! The enumerations a stream's words are written in: gallium's, from
//! `p_defines.h`, and virgl's own, from `virgl_hw.h` (virglrenderer 1.2.0).
//!
//! Only what the compositor writes is here. Each is the header's number
//! written out, so a test that compares it against the header's text pins
//! the two together.

/// `PIPE_BUFFER`: a resource with no shape.
pub const BUFFER: u32 = 0;
/// `PIPE_TEXTURE_2D`.
pub const TEXTURE_2D: u32 = 2;

/// `VIRGL_FORMAT_B8G8R8A8_UNORM`: what `wl_shm`'s `ARGB8888` is in memory.
pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;
/// `VIRGL_FORMAT_B8G8R8X8_UNORM`: `XRGB8888`, and the scanout's format.
pub const FORMAT_B8G8R8X8_UNORM: u32 = 2;
/// `VIRGL_FORMAT_R32G32_FLOAT`: a vertex's position, or its coordinate.
pub const FORMAT_R32G32_FLOAT: u32 = 29;
/// `VIRGL_FORMAT_R32G32B32A32_FLOAT`.
pub const FORMAT_R32G32B32A32_FLOAT: u32 = 31;
/// `VIRGL_FORMAT_R8_UNORM`: how a buffer's bytes are counted.
pub const FORMAT_R8_UNORM: u32 = 64;
/// `VIRGL_FORMAT_R8G8B8A8_UNORM`.
pub const FORMAT_R8G8B8A8_UNORM: u32 = 67;

/// `VIRGL_BIND_RENDER_TARGET`: may be drawn into.
pub const BIND_RENDER_TARGET: u32 = 1 << 1;
/// `VIRGL_BIND_SAMPLER_VIEW`: may be sampled from.
pub const BIND_SAMPLER_VIEW: u32 = 1 << 3;
/// `VIRGL_BIND_VERTEX_BUFFER`.
pub const BIND_VERTEX_BUFFER: u32 = 1 << 4;
/// `VIRGL_BIND_SCANOUT`: may be shown on a screen.
pub const BIND_SCANOUT: u32 = 1 << 18;

/// `VIRGL_RESOURCE_Y_0_TOP`: row 0 is the top row, as a screen's is.
pub const RESOURCE_Y_0_TOP: u32 = 1 << 0;

/// `PIPE_SHADER_VERTEX`.
pub const SHADER_VERTEX: u32 = 0;
/// `PIPE_SHADER_FRAGMENT`.
pub const SHADER_FRAGMENT: u32 = 1;

/// `PIPE_PRIM_TRIANGLES`.
pub const PRIM_TRIANGLES: u32 = 4;
/// `PIPE_PRIM_TRIANGLE_STRIP`.
pub const PRIM_TRIANGLE_STRIP: u32 = 5;

/// `PIPE_CLEAR_COLOR0`: the first colour buffer.
pub const CLEAR_COLOR0: u32 = 1 << 2;

/// `PIPE_BLENDFACTOR_ONE`.
pub const BLENDFACTOR_ONE: u32 = 1;
/// `PIPE_BLENDFACTOR_SRC_ALPHA`.
pub const BLENDFACTOR_SRC_ALPHA: u32 = 3;
/// `PIPE_BLENDFACTOR_ZERO`.
pub const BLENDFACTOR_ZERO: u32 = 0x11;
/// `PIPE_BLENDFACTOR_INV_SRC_ALPHA`.
pub const BLENDFACTOR_INV_SRC_ALPHA: u32 = 0x13;
/// `PIPE_BLEND_ADD`.
pub const BLEND_ADD: u32 = 0;
/// `PIPE_MASK_RGBA`: every channel is written.
pub const MASK_RGBA: u32 = 0xf;

/// `PIPE_TEX_WRAP_CLAMP_TO_EDGE`.
pub const TEX_WRAP_CLAMP_TO_EDGE: u32 = 2;
/// `PIPE_TEX_FILTER_NEAREST`.
pub const TEX_FILTER_NEAREST: u32 = 0;
/// `PIPE_TEX_FILTER_LINEAR`.
pub const TEX_FILTER_LINEAR: u32 = 1;
/// `PIPE_TEX_MIPFILTER_NONE`.
pub const TEX_MIPFILTER_NONE: u32 = 2;

/// `PIPE_SWIZZLE_RED` to `PIPE_SWIZZLE_ONE`: which channel a view's channel
/// reads.
pub const SWIZZLE_RED: u32 = 0;
/// See [`SWIZZLE_RED`].
pub const SWIZZLE_GREEN: u32 = 1;
/// See [`SWIZZLE_RED`].
pub const SWIZZLE_BLUE: u32 = 2;
/// See [`SWIZZLE_RED`].
pub const SWIZZLE_ALPHA: u32 = 3;
/// See [`SWIZZLE_RED`]: reads as one, which is how `XRGB8888` is opaque.
pub const SWIZZLE_ONE: u32 = 5;

/// `PIPE_TRANSFER_WRITE`, an inline write's usage.
pub const TRANSFER_WRITE: u32 = 1 << 1;

/// `enum virgl_object_type`: what `CREATE_OBJECT`, `BIND_OBJECT` and
/// `DESTROY_OBJECT` are about.
pub mod object {
    /// `VIRGL_OBJECT_BLEND`.
    pub const BLEND: u32 = 1;
    /// `VIRGL_OBJECT_RASTERIZER`.
    pub const RASTERIZER: u32 = 2;
    /// `VIRGL_OBJECT_DSA`: depth, stencil and alpha test.
    pub const DSA: u32 = 3;
    /// `VIRGL_OBJECT_SHADER`.
    pub const SHADER: u32 = 4;
    /// `VIRGL_OBJECT_VERTEX_ELEMENTS`.
    pub const VERTEX_ELEMENTS: u32 = 5;
    /// `VIRGL_OBJECT_SAMPLER_VIEW`.
    pub const SAMPLER_VIEW: u32 = 6;
    /// `VIRGL_OBJECT_SAMPLER_STATE`.
    pub const SAMPLER_STATE: u32 = 7;
    /// `VIRGL_OBJECT_SURFACE`.
    pub const SURFACE: u32 = 8;
}

/// `enum virgl_context_cmd`: a command's number.
pub mod command {
    /// `VIRGL_CCMD_CREATE_OBJECT`.
    pub const CREATE_OBJECT: u32 = 1;
    /// `VIRGL_CCMD_BIND_OBJECT`.
    pub const BIND_OBJECT: u32 = 2;
    /// `VIRGL_CCMD_DESTROY_OBJECT`.
    pub const DESTROY_OBJECT: u32 = 3;
    /// `VIRGL_CCMD_SET_VIEWPORT_STATE`.
    pub const SET_VIEWPORT_STATE: u32 = 4;
    /// `VIRGL_CCMD_SET_FRAMEBUFFER_STATE`.
    pub const SET_FRAMEBUFFER_STATE: u32 = 5;
    /// `VIRGL_CCMD_SET_VERTEX_BUFFERS`.
    pub const SET_VERTEX_BUFFERS: u32 = 6;
    /// `VIRGL_CCMD_CLEAR`.
    pub const CLEAR: u32 = 7;
    /// `VIRGL_CCMD_DRAW_VBO`.
    pub const DRAW_VBO: u32 = 8;
    /// `VIRGL_CCMD_RESOURCE_INLINE_WRITE`.
    pub const RESOURCE_INLINE_WRITE: u32 = 9;
    /// `VIRGL_CCMD_SET_SAMPLER_VIEWS`.
    pub const SET_SAMPLER_VIEWS: u32 = 10;
    /// `VIRGL_CCMD_SET_CONSTANT_BUFFER`.
    pub const SET_CONSTANT_BUFFER: u32 = 12;
    /// `VIRGL_CCMD_SET_SCISSOR_STATE`.
    pub const SET_SCISSOR_STATE: u32 = 15;
    /// `VIRGL_CCMD_BLIT`.
    pub const BLIT: u32 = 16;
    /// `VIRGL_CCMD_BIND_SAMPLER_STATES`.
    pub const BIND_SAMPLER_STATES: u32 = 18;
    /// `VIRGL_CCMD_BIND_SHADER`.
    pub const BIND_SHADER: u32 = 31;
}
