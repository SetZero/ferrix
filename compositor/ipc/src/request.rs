//! Reading one `hyprctl` request.

/// How an answer should be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Format {
    /// The readable form `hyprctl` prints without `-j`.
    #[default]
    Readable,
    /// JSON, which `hyprctl -j` and every bar asks for.
    Json,
}

/// The leading flags of a request, as Hyprland reads them.
///
/// They come before the command, separated from it and from each other by
/// `/`: `j/clients`, `jr/clients`, `a/clients`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Flags {
    /// `j`: answer in JSON.
    pub json: bool,
    /// `r`: JSON without the pretty-printing, which Hyprland spells
    /// "raw"; on its own it does not ask for JSON.
    pub raw: bool,
    /// `a`: include windows that are not mapped.
    pub all: bool,
    /// `-`: leave the trailing newline off.
    pub no_newline: bool,
}

impl Flags {
    /// What format these ask for.
    #[must_use]
    pub const fn format(self) -> Format {
        if self.json {
            Format::Json
        } else {
            Format::Readable
        }
    }
}

/// One request: what to do, and how to answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// The flags in front of it.
    pub flags: Flags,
    /// The command's name, lower-cased as Hyprland lower-cases it.
    pub command: String,
    /// What followed the name, trimmed; empty when there was nothing.
    pub argument: String,
}

impl Request {
    /// Read one line.
    ///
    /// A line that is only flags, or empty, is a request for the empty
    /// command, which is answered as an unknown one. Nothing here fails:
    /// Hyprland answers every request it can read, and a request it cannot
    /// read is one it answers with "unknown request".
    #[must_use]
    pub fn parse(line: &str) -> Self {
        let line = line.trim_end_matches(['\n', '\r']);
        let (flags, rest) = Self::flags(line);
        let (command, argument) = match rest.split_once(char::is_whitespace) {
            Some((command, argument)) => (command, argument.trim()),
            None => (rest, ""),
        };
        Self {
            flags,
            command: command.trim().to_ascii_lowercase(),
            argument: argument.to_owned(),
        }
    }

    /// Read several, as `[[BATCH]]` sends them.
    ///
    /// The flags in front of the batch apply to every request in it, which is
    /// what `hyprctl -j --batch` relies on; a request inside a batch may not
    /// carry its own.
    #[must_use]
    pub fn parse_batch(line: &str) -> Vec<Self> {
        let line = line.trim_end_matches(['\n', '\r']);
        let (flags, rest) = Self::flags(line);
        let Some(batch) = rest.strip_prefix("[[BATCH]]") else {
            return vec![Self::parse(line)];
        };
        batch
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut request = Self::parse(part);
                request.flags = flags;
                request
            })
            .collect()
    }

    /// Split the leading flags from the rest.
    ///
    /// Hyprland looks for a `/` before the first space and reads everything
    /// in front of it as flag letters. A `/` inside an argument -- a path in
    /// `keyword`, say -- is past the first space and is left alone.
    fn flags(line: &str) -> (Flags, &str) {
        let head = line.split_whitespace().next().unwrap_or("");
        let Some(slash) = head.find('/') else {
            return (Flags::default(), line);
        };
        let (letters, _) = line.split_at(slash);
        let mut flags = Flags::default();
        for letter in letters.chars() {
            match letter {
                'j' => flags.json = true,
                'r' => flags.raw = true,
                'a' => flags.all = true,
                '-' => flags.no_newline = true,
                // An unknown letter is not an error: Hyprland adds them and a
                // client built against a newer one should still be answered.
                _ => {}
            }
        }
        (flags, line.get(slash + 1..).unwrap_or(""))
    }
}
