//! Text and pictures for the widgets, through the desktop clients' shared
//! crates: hyprgraphics' `CTextResource` and `CImageResource`.
//!
//! A text is Pango markup in a Pango font description at a point size, laid
//! out and drawn by `src/user/system/linux/compositor/text` into a pixmap the size of its
//! logical rectangle -- what upstream's label texture is. Markup that does
//! not parse is drawn as the plain text it is, as upstream falls back when
//! `pango_parse_markup` refuses it. Pictures are `src/user/system/linux/compositor/image`'s:
//! PNG, JPEG and SVG, where upstream also reads WebP, JPEG XL and BMP.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use compositor_text::markup::{self, Rgba};
use compositor_text::{Align, FontDescription, Fonts, LayoutOptions, Size};
use tiny_skia::Pixmap;

use crate::scene::{Assets, TextAlign, TextRequest};

/// How many drawn texts are kept: a clock makes a new one every minute.
const KEPT: usize = 64;

/// The machine's fonts and pictures, with what has been drawn kept.
#[derive(Debug)]
pub struct System {
    /// Scanned at the first text, not at start: a screen with no text
    /// never reads a font directory.
    fonts: Option<Fonts>,
    dirs: Option<Vec<PathBuf>>,
    texts: HashMap<TextRequest, Option<Rc<Pixmap>>>,
    images: HashMap<PathBuf, (Option<SystemTime>, Option<Rc<Pixmap>>)>,
}

impl System {
    /// Every font under the usual directories.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fonts: None,
            dirs: None,
            texts: HashMap::new(),
            images: HashMap::new(),
        }
    }

    /// Only the fonts under `dirs`: what a test that must draw the same
    /// picture on every machine uses.
    #[must_use]
    pub fn with_font_dirs(dirs: &[&Path]) -> Self {
        Self {
            fonts: None,
            dirs: Some(dirs.iter().map(|dir| dir.to_path_buf()).collect()),
            texts: HashMap::new(),
            images: HashMap::new(),
        }
    }

    /// The fonts, scanned the first time they are wanted.
    fn fonts(&mut self) -> &mut Fonts {
        let dirs = &self.dirs;
        self.fonts.get_or_insert_with(|| match dirs {
            Some(dirs) => {
                let mut fonts = Fonts::new();
                for dir in dirs {
                    let _ = fonts.add_dir(dir);
                }
                fonts
            }
            None => Fonts::system(),
        })
    }

    fn draw_text(&mut self, request: &TextRequest) -> Option<Rc<Pixmap>> {
        let parsed = markup::parse(&request.text).unwrap_or_else(|_| markup::plain(&request.text));
        let mut font = FontDescription::pango(&request.font);
        #[expect(clippy::cast_precision_loss, reason = "a point size")]
        let points = request.size as f32;
        font.size = Size::Points(points);
        let byte = |shift: u32| u8::try_from((request.color >> shift) & 0xFF).unwrap_or(0);
        let options = LayoutOptions {
            font,
            color: Rgba::rgb(byte(16), byte(8), byte(0)),
            align: match request.align {
                TextAlign::Left => Align::Left,
                TextAlign::Center => Align::Center,
                TextAlign::Right => Align::Right,
            },
            ..LayoutOptions::default()
        };
        let fonts = self.fonts();
        let layout = fonts.layout(&parsed.spans, &options);
        fonts.render(&layout).map(Rc::new)
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

impl Assets for System {
    fn text(&mut self, request: &TextRequest) -> Option<Rc<Pixmap>> {
        if request.text.is_empty() {
            return None;
        }
        if let Some(kept) = self.texts.get(request) {
            return kept.clone();
        }
        if self.texts.len() >= KEPT {
            self.texts.clear();
        }
        let drawn = self.draw_text(request);
        let _ = self.texts.insert(request.clone(), drawn.clone());
        drawn
    }

    fn image(&mut self, path: &Path) -> Option<Rc<Pixmap>> {
        let path = crate::scene::absolute(&path.to_string_lossy());
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        if let Some((when, kept)) = self.images.get(&path)
            && *when == modified
        {
            return kept.clone();
        }
        let loaded = match compositor_image::load(&path, compositor_image::Fit::Natural) {
            Ok(pixmap) => Some(Rc::new(pixmap)),
            Err(error) => {
                crate::say(&format!("image: {}: {error}", path.display()));
                None
            }
        };
        let _ = self.images.insert(path, (modified, loaded.clone()));
        loaded
    }
}
