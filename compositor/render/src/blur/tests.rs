//! What a blur must do: spread a colour, keep the total, and leave a flat
//! block flat.

use super::blur;

/// A block of one colour, as premultiplied `RGBA` bytes.
fn flat(width: usize, height: usize, colour: [u8; 4]) -> Vec<u8> {
    colour
        .into_iter()
        .cycle()
        .take(width * height * 4)
        .collect()
}

fn pixel(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    let at = (y * width + x) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

/// A blur of one colour is that colour. Every tap reads the same value and
/// the weights sum to one, so anything else is an arithmetic mistake.
#[test]
fn a_flat_block_survives_every_pass() {
    for passes in 1..=3 {
        let mut pixels = flat(64, 64, [40, 80, 120, 255]);
        blur(&mut pixels, 64, 64, 8, passes);
        for (x, y) in [(0, 0), (32, 32), (63, 63), (1, 62)] {
            assert_eq!(
                pixel(&pixels, 64, x, y),
                [40, 80, 120, 255],
                "{passes} passes changed ({x}, {y})"
            );
        }
    }
}

/// A bright block spreads past its own edge, and its middle stays bright.
/// That is the whole of what a blur is.
///
/// A block rather than a single pixel: one pixel's worth of light spread
/// over a blur's reach is less than a byte, so a test that asked for it
/// would be asking the arithmetic to be wrong.
#[test]
fn a_block_spreads_past_its_own_edge() {
    let mut pixels = flat(64, 64, [0, 0, 0, 255]);
    for y in 24..40 {
        for x in 24..40 {
            let at = (y * 64 + x) * 4;
            pixels[at] = 255;
            pixels[at + 1] = 255;
            pixels[at + 2] = 255;
        }
    }
    blur(&mut pixels, 64, 64, 8, 2);

    // The middle is still the brightest thing there is, and light has gone
    // where there was none. How much is the radius's business, and a
    // radius of 8 over two passes reaches most of a 64-pixel square.
    let middle = pixel(&pixels, 64, 32, 32)[0];
    let past = pixel(&pixels, 64, 42, 32)[0];
    let far = pixel(&pixels, 64, 1, 1)[0];
    assert!(
        middle > past,
        "the middle is not the brightest: {middle} vs {past}"
    );
    assert!(past > far, "nothing spread past the edge: {past} vs {far}");
    // And its edge is no longer an edge.
    let inside = pixel(&pixels, 64, 38, 32)[0];
    let outside = pixel(&pixels, 64, 41, 32)[0];
    assert!(inside > outside, "{inside} is not brighter than {outside}");
}

/// An edge between two colours becomes a gradient, and the gradient runs the
/// right way.
#[test]
fn an_edge_becomes_a_gradient() {
    let (width, height) = (64, 16);
    let mut pixels = Vec::with_capacity(width * height * 4);
    for _ in 0..height {
        for x in 0..width {
            let value = if x < width / 2 { 0 } else { 255 };
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    blur(&mut pixels, width, height, 8, 2);

    let row: Vec<u8> = (0..width).map(|x| pixel(&pixels, width, x, 8)[0]).collect();
    assert!(
        row.windows(2).all(|pair| pair[1] >= pair[0]),
        "the gradient is not monotonic: {row:?}"
    );
    assert!(row[0] < 128 && row[width - 1] > 128, "{row:?}");
    // The middle is somewhere between, which a sharp edge is not.
    let middle = row[width / 2];
    assert!(
        (16..240).contains(&middle),
        "the edge stayed sharp: {middle}"
    );
}

/// Zero passes or zero size is Hyprland's way of turning the blur off, and
/// it must leave the pixels exactly as they were.
#[test]
fn nothing_is_blurred_with_no_passes_or_no_size() {
    let original = flat(8, 8, [1, 2, 3, 255]);
    for (size, passes) in [(0, 1), (8, 0), (0, 0)] {
        let mut pixels = original.clone();
        blur(&mut pixels, 8, 8, size, passes);
        assert_eq!(pixels, original, "size {size}, {passes} passes");
    }
}

/// A block smaller than the pass chain still comes back the size it went in,
/// rather than the empty vector a division that reached zero would give.
#[test]
fn a_tiny_block_survives_more_passes_than_it_has_pixels() {
    let mut pixels = flat(3, 2, [9, 9, 9, 255]);
    blur(&mut pixels, 3, 2, 8, 5);
    assert_eq!(pixels.len(), 3 * 2 * 4);
    assert_eq!(pixel(&pixels, 3, 1, 1), [9, 9, 9, 255]);
}
