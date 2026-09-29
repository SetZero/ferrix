//! What to play.

/// The command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Options {
    /// The `.bav` file.
    pub(crate) video: String,
    /// The song: an MP4 audio file.
    pub(crate) song: String,
    /// Stop after this many seconds, rather than at the end.
    pub(crate) seconds: Option<u32>,
}

/// The arguments meant, from `argv` less the program name.
///
/// As init, Ferrix starts a program as `sh -i` or `sh -c SCRIPT`
/// (`src/kernel/src/init.rs`), so `-i` alone is nothing asked for and a script
/// is split into the arguments it names: one a line when it has a newline,
/// otherwise at whitespace. The rule is `compositor_evecho::init::unshell`'s.
pub(crate) fn unshell(args: &[String]) -> Vec<String> {
    match args.split_first() {
        Some((first, rest)) if first == "-i" && rest.is_empty() => Vec::new(),
        Some((first, rest)) if first == "-c" => rest.iter().flat_map(|s| split(s)).collect(),
        _ => args.to_vec(),
    }
}

fn split(script: &str) -> Vec<String> {
    if script.contains('\n') {
        script
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        script.split_whitespace().map(str::to_owned).collect()
    }
}

/// Read `VIDEO SONG [SECONDS]`.
pub(crate) fn parse(args: &[String]) -> Result<Options, String> {
    match args {
        [video, song] => Ok(Options {
            video: video.clone(),
            song: song.clone(),
            seconds: None,
        }),
        [video, song, seconds] => Ok(Options {
            video: video.clone(),
            song: song.clone(),
            seconds: Some(
                seconds
                    .parse()
                    .map_err(|_| format!("not a number of seconds: {seconds}"))?,
            ),
        }),
        _ => Err(format!(
            "usage: badapple VIDEO.bav SONG.m4a [SECONDS], not {args:?}"
        )),
    }
}
