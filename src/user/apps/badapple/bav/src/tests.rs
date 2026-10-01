#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]

use super::{Encoder, Error, MAGIC, Picture, Video, grey_of, shade_of};

fn frames() -> Vec<Vec<u8>> {
    let (w, h) = (7_usize, 5_usize);
    let mut all = Vec::new();
    for n in 0..6_usize {
        let mut grey = vec![0_u8; w * h];
        for (i, pixel) in grey.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            *pixel = if x + y < n + 2 { 255 } else { 0 };
            if x == 6 && y == n % h {
                *pixel = 128;
            }
        }
        all.push(grey);
    }
    all
}

fn encoded(tolerance: u8) -> Vec<u8> {
    let mut encoder = Encoder::new(7, 5, 30, 1, tolerance).unwrap();
    for frame in frames() {
        encoder.push(&frame).unwrap();
    }
    encoder.finish()
}

#[test]
fn round_trip_is_exact_to_the_shade() {
    let file = encoded(0);
    let video = Video::parse(&file).unwrap();
    assert_eq!(video.header.frames, 6, "every frame is indexed");
    let mut picture = Picture::new(&video.header);
    for (n, source) in frames().iter().enumerate() {
        let _ = picture.apply(video.frame(n as u32).unwrap()).unwrap();
        let expected: Vec<u8> = source.iter().map(|g| shade_of(*g)).collect();
        assert_eq!(picture.shades(), &expected[..], "frame {n}");
    }
    assert!(video.frame(6).is_none(), "nothing past the last frame");
}

#[test]
fn tolerance_bounds_the_drift() {
    let file = encoded(1);
    let video = Video::parse(&file).unwrap();
    let mut picture = Picture::new(&video.header);
    for (n, source) in frames().iter().enumerate() {
        let _ = picture.apply(video.frame(n as u32).unwrap()).unwrap();
        for (shown, grey) in picture.shades().iter().zip(source) {
            assert!(shown.abs_diff(shade_of(*grey)) <= 1, "frame {n}");
        }
    }
}

#[test]
fn an_unchanged_frame_is_one_keep_and_no_damage() {
    let mut encoder = Encoder::new(4, 4, 30, 1, 0).unwrap();
    encoder.push(&[255; 16]).unwrap();
    encoder.push(&[255; 16]).unwrap();
    let file = encoder.finish();
    let video = Video::parse(&file).unwrap();
    assert_eq!(video.frame(1).unwrap(), &[15 << 1 | 1], "keep 16");
    let mut picture = Picture::new(&video.header);
    assert_eq!(
        picture.apply(video.frame(0).unwrap()),
        Ok(Some((0, 3))),
        "all rows"
    );
    assert_eq!(picture.apply(video.frame(1).unwrap()), Ok(None), "no rows");
}

#[test]
fn damage_is_the_rows_painted() {
    let mut encoder = Encoder::new(4, 4, 30, 1, 0).unwrap();
    encoder.push(&[0; 16]).unwrap();
    let mut second = [0_u8; 16];
    second[9] = 255;
    encoder.push(&second).unwrap();
    let file = encoder.finish();
    let video = Video::parse(&file).unwrap();
    let mut picture = Picture::new(&video.header);
    assert_eq!(
        picture.apply(video.frame(0).unwrap()),
        Ok(None),
        "black on black"
    );
    assert_eq!(
        picture.apply(video.frame(1).unwrap()),
        Ok(Some((2, 2))),
        "row 2"
    );
}

#[test]
fn damaged_files_are_refused() {
    let file = encoded(0);
    assert_eq!(
        Video::parse(&file[..10]).err(),
        Some(Error::Short),
        "cut header"
    );
    let mut wrong = file.clone();
    wrong[0] = b'X';
    assert_eq!(Video::parse(&wrong).err(), Some(Error::Magic), "magic");
    let mut backwards = file.clone();
    backwards[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(Video::parse(&backwards).err(), Some(Error::Index), "index");
    assert_eq!(
        Video::parse(&MAGIC).err(),
        Some(Error::Short),
        "magic alone"
    );
}

#[test]
fn a_frame_must_cover_the_picture() {
    let file = encoded(0);
    let video = Video::parse(&file).unwrap();
    let mut picture = Picture::new(&video.header);
    // Keep 34 of 35 pixels.
    assert_eq!(picture.apply(&[33 << 1 | 1]), Err(Error::Coverage), "short");
    // Paint 36.
    assert_eq!(picture.apply(&[0xe0, 0x08]), Err(Error::Coverage), "long");
    assert_eq!(picture.apply(&[0x80]), Err(Error::Token), "cut token");
}

#[test]
fn shades_span_black_to_white() {
    assert_eq!(
        (shade_of(0), shade_of(255), shade_of(128)),
        (0, 15, 8),
        "ends and middle"
    );
    assert_eq!((grey_of(0), grey_of(15)), (0, 255), "back to grey");
}

#[test]
fn frame_at_counts_whole_frames() {
    let header = Video::parse(&encoded(0)).unwrap().header;
    assert_eq!(header.frame_at(0), 0, "start");
    assert_eq!(header.frame_at(33_333), 0, "just before the second");
    assert_eq!(header.frame_at(33_334), 1, "the second");
    assert_eq!(header.frame_at(1_000_000), 30, "a second in");
}
