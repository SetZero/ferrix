//! The compositor's shaders, as TGSI assembly.
//!
//! virgl carries a shader as the text gallium's `tgsi_dump` writes and
//! `tgsi_text_translate` reads, and virglrenderer turns it into GLSL for the
//! host's driver. It is an assembly: declarations, then numbered
//! instructions over four-component registers, each source with a swizzle
//! and each destination with a write mask. A vertex shader's `OUT` and a
//! fragment shader's `IN` are joined by their *semantic* -- `GENERIC[0]` to
//! `GENERIC[0]` -- and not by their register's number.
//!
//! # The conventions every shader here keeps
//!
//! * A vertex is two attributes of two floats: `IN[0]` its place in
//!   *pixels* of the target, `IN[1]` its texture coordinate from 0 to 1.
//! * The vertex stage's `CONST[0]` is `(2/width, 2/height, -1, -1)`, which
//!   takes pixels to clip space with nothing flipped: row 0 of a texture is
//!   the row gallium calls 0, in a transfer and in a draw alike.
//! * Colours are premultiplied, as Wayland's are and the software renderer's
//!   are, so an opacity is one multiply of all four channels.

/// An upper bound on any shader here, in TGSI tokens: how much room
/// virglrenderer gives its parse. A few tokens an instruction; these shaders
/// are a handful of instructions.
pub const TOKENS: u32 = 512;

/// The one vertex shader: pixels in, clip space out, the texture coordinate
/// handed on.
pub const VERTEX: &str = "VERT
DCL IN[0]
DCL IN[1]
DCL OUT[0], POSITION
DCL OUT[1], GENERIC[0]
DCL CONST[0]
DCL TEMP[0]
IMM[0] FLT32 { 0.0000, 1.0000, 0.0000, 0.0000 }
  0: MAD TEMP[0].xy, IN[0].xyyy, CONST[0].xyyy, CONST[0].zwww
  1: MOV TEMP[0].zw, IMM[0].xxxy
  2: MOV OUT[0], TEMP[0]
  3: MOV OUT[1], IN[1]
  4: END
";

/// One colour everywhere: `CONST[0]`, premultiplied.
pub const SOLID: &str = "FRAG
DCL OUT[0], COLOR
DCL CONST[0]
  0: MOV OUT[0], CONST[0]
  1: END
";

/// A texture, times an opacity in `CONST[0].x`.
pub const TEXTURED: &str = "FRAG
DCL IN[0], GENERIC[0], PERSPECTIVE
DCL OUT[0], COLOR
DCL SAMP[0]
DCL SVIEW[0], 2D, FLOAT
DCL CONST[0]
DCL TEMP[0]
  0: TEX TEMP[0], IN[0], SAMP[0], 2D
  1: MUL OUT[0], TEMP[0], CONST[0].xxxx
  2: END
";
