//! The compositor's shaders, as TGSI assembly.
//!
//! virgl carries a shader as the text gallium's `tgsi_dump` writes and
//! `tgsi_text_translate` reads, and virglrenderer turns it into GLSL for the
//! host's driver. It is an assembly: declarations, then numbered
//! instructions over four-component registers. A vertex shader's `OUT` and a
//! fragment shader's `IN` are joined by their *semantic* -- `GENERIC[0]` to
//! `GENERIC[0]` -- and not by their register's number.
//!
//! # Where the text comes from
//!
//! Nobody wrote it. The shaders are GLSL, under `shaders/`, and
//! `tools/regenerate.sh` compiles each on a Linux host with Mesa's own virgl
//! driver and keeps the TGSI that driver sends -- the script says how, and
//! why there is no other compiler to ask. What is committed under `src/tgsi`
//! is its output, and `tests/host.rs` runs every one of them on
//! virglrenderer. A change to a shader is a change to its GLSL and a run of
//! the script, never an edit here.
//!
//! # The conventions every shader keeps
//!
//! * A vertex is two attributes of two floats: its place in *pixels* of the
//!   target, and its texture coordinate from 0 to 1.
//! * The vertex stage's `CONST[0]` is `(2/width, 2/height, -1, -1)`, which
//!   takes pixels to clip space with nothing flipped: row 0 of a texture is
//!   the row gallium calls 0, in a transfer and in a draw alike.
//! * A fragment stage's constants are one array of `vec4`, which each
//!   shader's own GLSL lays out in its opening comment, and its one texture
//!   is sampler 0.
//! * Colours are premultiplied, as Wayland's are and the software renderer's
//!   are, so an opacity is one multiply of all four channels.
//! * A shape is cut in pixels, all or nothing, by the superellipse
//!   `compositor/render` cuts its corners with: the GPU's frame and the
//!   software one differ at a corner by a pixel's rounding and no more.

/// An upper bound on any shader here, in TGSI tokens: how much room
/// virglrenderer gives its parse. The longest is a few hundred instructions
/// of a few tokens each.
pub const TOKENS: u32 = 4096;

/// The one vertex shader: `shaders/quad.vert`.
pub const VERTEX: &str = include_str!("tgsi/quad.tgsi");

/// A premultiplied colour inside a rounded rectangle: `shaders/solid.frag`.
/// Constants: the colour; the rectangle; `(radius, power, 0, 0)`.
pub const SOLID: &str = include_str!("tgsi/solid.tgsi");

/// A surface's texture, faded and cut to a rounded rectangle:
/// `shaders/surface.frag`. Constants: `(opacity, 0, 0, 0)`; the rectangle;
/// `(radius, power, 0, 0)`.
pub const SURFACE: &str = include_str!("tgsi/surface.tgsi");

/// A border's gradient, read from a ramp texture: `shaders/gradient.frag`.
/// Constants: the box the gradient runs across; `(sine, flip x, flip y, 0)`;
/// the rectangle drawn; `(radius, power, last / length, 0.5 / length)` of the
/// ramp.
pub const GRADIENT: &str = include_str!("tgsi/gradient.tgsi");

/// A drop shadow's falloff: `shaders/shadow.frag`. Constants: the colour;
/// the shadow's box; `(inset, range, power, 0)`.
pub const SHADOW: &str = include_str!("tgsi/shadow.tgsi");

/// `blurprepare.glsl`: `shaders/blur_prepare.frag`. Constants:
/// `(1/width, 1/height, contrast, max(1, brightness))`.
pub const BLUR_PREPARE: &str = include_str!("tgsi/blur_prepare.tgsi");

/// `blur1.glsl`, the downsample: `shaders/blur_down.frag`. Constants:
/// `(1/source width, 1/source height, radius, 0)`;
/// `(vibrancy / passes, 1 - vibrancy_darkness, 0, 0)`.
pub const BLUR_DOWN: &str = include_str!("tgsi/blur_down.tgsi");

/// `blur2.glsl`, the upsample: `shaders/blur_up.frag`. Constants:
/// `(1/source width, 1/source height, radius / 4, 0)`.
pub const BLUR_UP: &str = include_str!("tgsi/blur_up.tgsi");

/// `blurFinish.glsl` written into a window's shape:
/// `shaders/blur_finish.frag`. Constants:
/// `(1/width, 1/height, noise, min(1, brightness))`; the rectangle;
/// `(radius, power, 0, 0)`.
pub const BLUR_FINISH: &str = include_str!("tgsi/blur_finish.tgsi");

/// Every fragment shader, by name, for the tests that run them all.
pub const FRAGMENTS: [(&str, &str); 8] = [
    ("solid", SOLID),
    ("surface", SURFACE),
    ("gradient", GRADIENT),
    ("shadow", SHADOW),
    ("blur_prepare", BLUR_PREPARE),
    ("blur_down", BLUR_DOWN),
    ("blur_up", BLUR_UP),
    ("blur_finish", BLUR_FINISH),
];
