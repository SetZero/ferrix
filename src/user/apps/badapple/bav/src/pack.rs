//! The host's side of `.bav`, which xtask runs on the build machine:
//!
//! * `bav-pack WIDTH HEIGHT RATE_NUM RATE_DEN TOLERANCE OUT`: grey frames on
//!   standard input, as `ffmpeg -i video -f rawvideo -pix_fmt gray -` writes
//!   them, and a `.bav` file out;
//! * `bav-pack frame FILE N OUT`: frame `N` of `FILE`, decoded, one shade a
//!   byte, row after row -- what `xtask test-badapple` expects the screen
//!   to show.

use std::io::{Read as _, Write as _};
use std::process::ExitCode;

use bav::{Encoder, Picture, Video};

fn main() -> ExitCode {
    match run() {
        Ok(line) => {
            let _ = writeln!(std::io::stdout(), "{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "bav-pack: {error}");
            ExitCode::FAILURE
        }
    }
}

type Outcome = Result<String, Box<dyn std::error::Error>>;

fn run() -> Outcome {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match &args[..] {
        [frame, file, n, out] if frame == "frame" => decode(file, n.parse()?, out),
        [width, height, rate_num, rate_den, tolerance, out] => {
            let mut encoder = Encoder::new(
                width.parse()?,
                height.parse()?,
                rate_num.parse()?,
                rate_den.parse()?,
                tolerance.parse()?,
            )?;
            pack(
                &mut encoder,
                usize::from(width.parse::<u16>()?) * usize::from(height.parse::<u16>()?),
            )?;
            let frames = encoder.frames();
            let file = encoder.finish();
            std::fs::write(out, &file)?;
            Ok(format!(
                "bav-pack: {frames} frames, {} bytes, {out}",
                file.len()
            ))
        }
        _ => Err(
            "usage: bav-pack WIDTH HEIGHT RATE_NUM RATE_DEN TOLERANCE OUT\n       \
                  bav-pack frame FILE N OUT"
                .into(),
        ),
    }
}

fn pack(encoder: &mut Encoder, size: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = vec![0_u8; size];
    let mut input = std::io::stdin().lock();
    loop {
        match input.read_exact(&mut frame) {
            Ok(()) => encoder.push(&frame)?,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

fn decode(file: &str, n: u32, out: &str) -> Outcome {
    let bytes = std::fs::read(file)?;
    let video = Video::parse(&bytes)?;
    if n >= video.header.frames {
        return Err(format!("{file} has {} frames, not {}", video.header.frames, n + 1).into());
    }
    let mut picture = Picture::new(&video.header);
    for k in 0..=n {
        let _ = picture.apply(video.frame(k).ok_or("a frame the index lost")?)?;
    }
    std::fs::write(out, picture.shades())?;
    Ok(format!(
        "bav-pack: frame {n} of {file}, {}x{}, {out}",
        video.header.width, video.header.height
    ))
}
