//! The words, against `virgl_protocol.h`.
//!
//! Every number below is written out from the header's text and not taken
//! from this crate's constants: a constant tested against itself proves
//! nothing, and what is being pinned is what virglrenderer will read.

use crate::pipe;
use crate::{Blend, Rasterizer, Region, Sampler, Stream, VertexBuffer, VertexElement, View};

/// `VIRGL_CMD0`.
const fn header(command: u32, object: u32, len: u32) -> u32 {
    command | (object << 8) | (len << 16)
}

#[test]
fn a_surface_and_a_framebuffer() {
    let mut stream = Stream::new();
    stream.create_surface(7, 0x4000_0001, 1);
    stream.set_framebuffer(7);
    assert_eq!(
        stream.words(),
        [
            // CREATE_OBJECT, SURFACE (8), VIRGL_OBJ_SURFACE_SIZE 5.
            header(1, 8, 5),
            7,
            0x4000_0001,
            1,
            0,
            0,
            // SET_FRAMEBUFFER_STATE: one colour buffer, no depth.
            header(5, 0, 3),
            1,
            0,
            7,
        ]
    );
    assert_eq!(stream.len_bytes(), 40);
}

#[test]
fn a_clear_is_eight_words() {
    let mut stream = Stream::new();
    stream.clear([1.0, 0.5, 0.25, 1.0]);
    assert_eq!(
        stream.words(),
        [
            header(7, 0, 8),
            // PIPE_CLEAR_COLOR0.
            4,
            0x3f80_0000,
            0x3f00_0000,
            0x3e80_0000,
            0x3f80_0000,
            0,
            0,
            0,
        ]
    );
}

#[test]
fn a_viewport_is_scale_then_translate() {
    let mut stream = Stream::new();
    stream.set_viewport(64, 32);
    let (w, h, z) = (32.0_f32.to_bits(), 16.0_f32.to_bits(), 0.5_f32.to_bits());
    assert_eq!(stream.words(), [header(4, 0, 7), 0, w, h, z, w, h, z]);
}

#[test]
fn a_scissor_packs_its_corners() {
    let mut stream = Stream::new();
    stream.set_scissor(Region {
        x: 1,
        y: 2,
        width: 10,
        height: 20,
    });
    assert_eq!(
        stream.words(),
        [header(15, 0, 3), 0, 1 | (2 << 16), 11 | (22 << 16)]
    );
}

#[test]
fn the_state_objects() {
    let mut stream = Stream::new();
    stream.create_blend(1, Blend::PREMULTIPLIED_OVER);
    let words = stream.words();
    // VIRGL_OBJ_BLEND_SIZE is VIRGL_MAX_COLOR_BUFS + 3.
    assert_eq!(words.first(), Some(&header(1, 1, 11)));
    assert_eq!(words.get(1..4), Some(&[1, 0, 0][..]));
    // Enabled; ADD; ONE (1) and INV_SRC_ALPHA (0x13) for both; all channels.
    let target = 1 | (1 << 4) | (0x13 << 9) | (1 << 17) | (0x13 << 22) | (0xf << 27);
    assert_eq!(words.get(4), Some(&target));
    assert_eq!(words.len(), 12);

    let mut stream = Stream::new();
    stream.create_dsa(2);
    assert_eq!(stream.words(), [header(1, 3, 5), 2, 0, 0, 0, 0]);

    let mut stream = Stream::new();
    stream.create_rasterizer(3, Rasterizer { scissor: true });
    let one = 1.0_f32.to_bits();
    assert_eq!(
        stream.words(),
        [
            header(1, 2, 9),
            3,
            // DEPTH_CLIP, SCISSOR, HALF_PIXEL_CENTER.
            (1 << 1) | (1 << 14) | (1 << 29),
            one,
            0,
            0,
            one,
            0,
            0,
            0,
        ]
    );

    let mut stream = Stream::new();
    stream.create_sampler_state(
        4,
        Sampler {
            filter: pipe::TEX_FILTER_LINEAR,
        },
    );
    // CLAMP_TO_EDGE (2) three times, linear min and mag, no mip filter (2).
    let s0 = 2 | (2 << 3) | (2 << 6) | (1 << 9) | (2 << 11) | (1 << 13);
    assert_eq!(
        stream.words(),
        [header(1, 7, 9), 4, s0, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn a_view_of_an_opaque_texture_reads_alpha_as_one() {
    let mut stream = Stream::new();
    stream.create_sampler_view(
        5,
        View {
            resource: 9,
            format: 2,
            opaque: true,
        },
    );
    // RED, GREEN, BLUE, ONE (5).
    let swizzle = (1 << 3) | (2 << 6) | (5 << 9);
    assert_eq!(stream.words(), [header(1, 6, 6), 5, 9, 2, 0, 0, swizzle]);
}

#[test]
fn vertices_and_a_draw() {
    let mut stream = Stream::new();
    stream.create_vertex_elements(
        6,
        &[
            VertexElement {
                offset: 0,
                buffer: 0,
                format: 29,
            },
            VertexElement {
                offset: 8,
                buffer: 0,
                format: 29,
            },
        ],
    );
    stream.set_vertex_buffers(&[VertexBuffer {
        stride: 16,
        offset: 0,
        resource: 9,
    }]);
    stream.draw(5, 0, 4);
    assert_eq!(
        stream.words(),
        [
            header(1, 5, 9),
            6,
            0,
            0,
            0,
            29,
            8,
            0,
            0,
            29,
            header(6, 0, 3),
            16,
            0,
            9,
            // VIRGL_DRAW_VBO_SIZE 12: TRIANGLE_STRIP, one instance, max 3.
            header(8, 0, 12),
            0,
            4,
            5,
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            3,
            0,
        ]
    );
}

#[test]
fn a_shader_is_its_text_a_zero_and_padding() {
    let mut stream = Stream::new();
    assert!(stream.create_shader(8, 1, "FRAG\nEND", 300));
    assert_eq!(
        stream.words(),
        [
            // Five words of header, and nine bytes in three words.
            header(1, 4, 8),
            8,
            1,
            9,
            300,
            0,
            u32::from_le_bytes(*b"FRAG"),
            u32::from_le_bytes(*b"\nEND"),
            0,
        ]
    );

    // Text that stops short of a word still ends in a zero.
    let mut stream = Stream::new();
    assert!(stream.create_shader(8, 0, "VERT\nE", 300));
    assert_eq!(stream.words().get(3), Some(&7));
    assert_eq!(
        stream.words().get(6..),
        Some(
            &[
                u32::from_le_bytes(*b"VERT"),
                u32::from_le_bytes(*b"\nE\0\0")
            ][..]
        )
    );
}

#[test]
fn constants_samplers_and_a_bound_shader() {
    let mut stream = Stream::new();
    stream.bind_shader(8, 1);
    stream.set_constants(1, &[0.5, 0.0, 0.0, 1.0]);
    stream.set_sampler_views(1, &[5]);
    stream.bind_sampler_states(1, &[4]);
    stream.bind_object(1, 2);
    assert_eq!(
        stream.words(),
        [
            header(31, 0, 2),
            8,
            1,
            header(12, 0, 6),
            1,
            0,
            0x3f00_0000,
            0,
            0,
            0x3f80_0000,
            header(10, 0, 3),
            1,
            0,
            5,
            header(18, 0, 3),
            1,
            0,
            4,
            header(2, 1, 1),
            2,
        ]
    );
}

#[test]
fn bytes_written_into_a_buffer_are_padded_to_words() {
    let mut stream = Stream::new();
    assert!(stream.write_buffer(9, 16, &[1, 2, 3, 4, 5]));
    assert_eq!(
        stream.words(),
        [
            header(9, 0, 13),
            9,
            0,
            // PIPE_TRANSFER_WRITE.
            2,
            0,
            0,
            16,
            0,
            0,
            5,
            1,
            1,
            0x0403_0201,
            5,
        ]
    );
}

/// The shaders are text virglrenderer's parser takes: a stage, declarations,
/// and instructions numbered from zero with no gap, ending in `END`.
#[test]
fn every_shader_is_numbered_and_ends() {
    use crate::shaders;
    for (text, stage) in [
        (shaders::VERTEX, "VERT"),
        (shaders::SOLID, "FRAG"),
        (shaders::TEXTURED, "FRAG"),
    ] {
        assert_eq!(text.lines().next(), Some(stage));
        let numbered: Vec<&str> = text
            .lines()
            .filter_map(|line| line.trim_start().split_once(": "))
            .map(|(number, _)| number)
            .collect();
        for (at, number) in numbered.iter().enumerate() {
            assert_eq!(number.parse::<usize>().ok(), Some(at), "{text}");
        }
        assert!(text.trim_end().ends_with("END"), "{text}");
        assert!(text.is_ascii());
    }
}
