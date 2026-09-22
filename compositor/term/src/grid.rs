//! The character grid, and the escape sequences that move about in it.
//!
//! A terminal emulator is two things: a grid of characters with a cursor in
//! it, and a parser for what a program writes at it. This is both, and it is
//! a pure function of the bytes -- no window, no font, no pseudoterminal --
//! so every rule here is host-tested.
//!
//! # What is understood
//!
//! The control characters a program uses without thinking: `\n`, `\r`, `\t`,
//! backspace and the bell (which is ignored, there being nothing to ring).
//! Then the `CSI` sequences a shell and the programs it starts actually
//! send, which is a much shorter list than the standard's:
//!
//! * `CSI n A|B|C|D` -- the cursor up, down, forward, back.
//! * `CSI r ; c H` and `CSI r ; c f` -- the cursor to a row and column,
//!   counted from one.
//! * `CSI n J` -- erase: to the end of the screen (0), to the start (1), or
//!   all of it (2).
//! * `CSI n K` -- the same for the line.
//! * `CSI n m` -- the graphic rendition, of which the bold and the eight
//!   colours of the foreground and the background (with their bright forms)
//!   are kept, a 256-colour or direct colour as the nearest of those, and the
//!   rest ignored.
//!
//! Text is UTF-8, as everything a shell on Ferrix writes is: a character is
//! the bytes that encode it, and a sequence that is not UTF-8 is drawn as
//! U+FFFD, once for each place it goes wrong.
//! * `CSI ? n h|l` -- the private modes, of which only the cursor's own
//!   visibility (25) is kept.
//!
//! An escape sequence this does not know is dropped rather than drawn: a
//! terminal that printed the bytes of a sequence it did not understand would
//! fill its screen with rubbish the first time a program asked about the
//! cursor.

/// One cell: what is in it, and how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character, a space for an empty cell.
    pub ch: char,
    /// Its colour, as an index into the eight the terminal has.
    pub colour: u8,
    /// Whether it is drawn bright.
    pub bold: bool,
    /// What is behind it: one of the eight colours, or eight to fifteen for
    /// their bright forms, or the terminal's own background when `None`.
    pub background: Option<u8>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            // Seven is white, which is what a terminal starts in.
            colour: 7,
            bold: false,
            background: None,
        }
    }
}

/// A rectangular group of cells whose pixels need drawing again.
///
/// This is in cell coordinates, rather than pixels: the terminal learns its
/// output scale only when it makes its Wayland buffer. Keeping the small
/// logical rectangle here lets the client redraw and report the same precise
/// buffer damage after that conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellDamage {
    /// First column in the region.
    pub left: usize,
    /// First row in the region.
    pub top: usize,
    /// Number of columns in the region.
    pub width: usize,
    /// Number of rows in the region.
    pub height: usize,
}

impl CellDamage {
    /// Every cell in a `columns` by `rows` grid.
    #[must_use]
    pub const fn full(columns: usize, rows: usize) -> Self {
        Self {
            left: 0,
            top: 0,
            width: columns,
            height: rows,
        }
    }

    /// The smallest region covering this and `other`.
    #[must_use]
    pub fn joined(self, other: Self) -> Self {
        let (left, top) = (self.left.min(other.left), self.top.min(other.top));
        let right = self
            .left
            .saturating_add(self.width)
            .max(other.left.saturating_add(other.width));
        let bottom = self
            .top
            .saturating_add(self.height)
            .max(other.top.saturating_add(other.height));
        Self {
            left,
            top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        }
    }
}

/// Where the parser is between bytes of an escape sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Parsing {
    /// Ordinary text.
    Text,
    /// An `ESC` has arrived.
    Escape,
    /// A `CSI` has arrived, and these are its parameter bytes.
    Csi(Vec<u8>),
    /// The first bytes of a UTF-8 character have arrived: the bits so far,
    /// how many continuation bytes are still to come, and the least code
    /// point a sequence of this length may encode, below which it is an
    /// overlong encoding and not a character.
    Utf8 { code: u32, needed: u8, least: u32 },
}

/// A grid of characters, with a cursor.
#[derive(Clone, Debug)]
pub struct Grid {
    columns: usize,
    rows: usize,
    cells: Vec<Cell>,
    /// Where the next character goes.
    cursor: (usize, usize),
    /// Whether the cursor is drawn.
    visible: bool,
    /// The colour and weight the next character takes.
    pen: Cell,
    parsing: Parsing,
    /// How many times the grid has scrolled, which a test uses to say that
    /// it did.
    scrolled: u64,
}

impl Grid {
    /// An empty grid.
    ///
    /// A grid with no columns or no rows would have nowhere to put a
    /// character, so both are at least one.
    #[must_use]
    pub fn new(columns: usize, rows: usize) -> Self {
        let (columns, rows) = (columns.max(1), rows.max(1));
        Self {
            columns,
            rows,
            cells: vec![Cell::default(); columns * rows],
            cursor: (0, 0),
            visible: true,
            pen: Cell::default(),
            parsing: Parsing::Text,
            scrolled: 0,
        }
    }

    /// How many columns and rows it has.
    #[must_use]
    pub const fn size(&self) -> (usize, usize) {
        (self.columns, self.rows)
    }

    /// Where the cursor is: the column and the row, counting from zero.
    #[must_use]
    pub const fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Whether the cursor is drawn.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.visible
    }

    /// The smallest visual region that differs from `before`.
    ///
    /// A terminal normally changes one cell and moves its block cursor one
    /// cell. Repainting the whole shared-memory buffer for that common case
    /// turns one typed byte into a full-window copy and compositor frame. The
    /// comparison is at the grid level, so scrolling, erasing and every escape
    /// sequence retain their existing implementation and damage exactly what
    /// changed.
    #[must_use]
    pub fn damage_since(&self, before: &Self) -> Option<CellDamage> {
        if self.size() != before.size() {
            return Some(CellDamage::full(self.columns, self.rows));
        }

        let mut damage: Option<CellDamage> = None;
        for (at, (was, now)) in before.cells.iter().zip(&self.cells).enumerate() {
            if was != now {
                let one = CellDamage {
                    left: at % self.columns,
                    top: at / self.columns,
                    width: 1,
                    height: 1,
                };
                damage = Some(damage.map_or(one, |held| held.joined(one)));
            }
        }

        // The cursor is painted by the terminal rather than held in a cell,
        // so both its old and new positions are damaged separately -- but
        // only if one of them changed. An idle terminal must not manufacture
        // a cursor-sized frame on every pass through its event loop.
        if before.cursor != self.cursor || before.visible != self.visible {
            for (grid, cursor) in [(before, before.cursor), (self, self.cursor)] {
                if grid.visible && cursor.0 < grid.columns && cursor.1 < grid.rows {
                    let one = CellDamage {
                        left: cursor.0,
                        top: cursor.1,
                        width: 1,
                        height: 1,
                    };
                    damage = Some(damage.map_or(one, |held| held.joined(one)));
                }
            }
        }
        damage
    }

    /// How many times the grid has scrolled.
    #[must_use]
    pub const fn scrolled(&self) -> u64 {
        self.scrolled
    }

    /// The cell at a column and a row, if it is on the grid.
    #[must_use]
    pub fn cell(&self, column: usize, row: usize) -> Option<&Cell> {
        if column >= self.columns || row >= self.rows {
            return None;
        }
        self.cells.get(row * self.columns + column)
    }

    /// One row as text, with the trailing spaces cut: what a test reads.
    #[must_use]
    pub fn line(&self, row: usize) -> String {
        let mut text = String::new();
        for column in 0..self.columns {
            match self.cell(column, row) {
                Some(cell) => text.push(cell.ch),
                None => break,
            }
        }
        text.trim_end().to_owned()
    }

    /// Resize the grid, keeping what is in the cells that are still there.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let (columns, rows) = (columns.max(1), rows.max(1));
        if (columns, rows) == (self.columns, self.rows) {
            return;
        }
        let mut cells = vec![Cell::default(); columns * rows];
        for row in 0..rows.min(self.rows) {
            for column in 0..columns.min(self.columns) {
                if let (Some(cell), Some(slot)) = (
                    self.cell(column, row).copied(),
                    cells.get_mut(row * columns + column),
                ) {
                    *slot = cell;
                }
            }
        }
        self.cells = cells;
        self.columns = columns;
        self.rows = rows;
        self.cursor = (
            self.cursor.0.min(columns.saturating_sub(1)),
            self.cursor.1.min(rows.saturating_sub(1)),
        );
    }

    /// Take what a program wrote.
    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }

    /// One byte.
    fn byte(&mut self, byte: u8) {
        match core::mem::replace(&mut self.parsing, Parsing::Text) {
            Parsing::Text => self.text(byte),
            // `ESC [` starts a `CSI`; `ESC` and any other byte is a
            // sequence nothing here needs, and a sequence that is dropped is
            // better than one that is printed.
            Parsing::Escape => {
                if byte == b'[' {
                    self.parsing = Parsing::Csi(Vec::new());
                }
            }
            Parsing::Csi(mut parameters) => {
                // Parameters and the bytes between them, then one byte that
                // says what the sequence is.
                if byte.is_ascii_digit() || byte == b';' || byte == b'?' {
                    parameters.push(byte);
                    self.parsing = Parsing::Csi(parameters);
                } else {
                    self.csi(&parameters, byte);
                }
            }
            Parsing::Utf8 {
                code,
                needed,
                least,
            } => {
                if byte & 0xC0 != 0x80 {
                    // Cut short: what arrived is one bad character, and this
                    // byte starts whatever comes next.
                    self.put(char::REPLACEMENT_CHARACTER);
                    self.text(byte);
                    return;
                }
                let code = code << 6 | u32::from(byte & 0x3F);
                if needed > 1 {
                    self.parsing = Parsing::Utf8 {
                        code,
                        needed: needed - 1,
                        least,
                    };
                } else if code < least {
                    self.put(char::REPLACEMENT_CHARACTER);
                } else {
                    // `from_u32` refuses the surrogates and anything past
                    // U+10FFFF, which UTF-8 cannot carry either.
                    self.put(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                }
            }
        }
    }

    /// An ordinary byte, or one of the control characters.
    fn text(&mut self, byte: u8) {
        match byte {
            0x1B => self.parsing = Parsing::Escape,
            b'\n' => self.line_feed(),
            b'\r' => self.cursor.0 = 0,
            0x08 => self.cursor.0 = self.cursor.0.saturating_sub(1),
            b'\t' => {
                // The next multiple of eight, which is every terminal's tab.
                let next = (self.cursor.0 / 8 + 1) * 8;
                self.cursor.0 = next.min(self.columns.saturating_sub(1));
            }
            // The bell, and every other control character: nothing to do.
            byte if byte < 0x20 || byte == 0x7F => {}
            byte if byte < 0x80 => self.put(char::from(byte)),
            // The first byte of a UTF-8 character, which says how many more
            // there are. A continuation byte with nothing before it, and the
            // bytes no UTF-8 starts with, are a character each that is not
            // one.
            0xC2..=0xDF => self.utf8(byte & 0x1F, 1, 0x80),
            0xE0..=0xEF => self.utf8(byte & 0x0F, 2, 0x800),
            0xF0..=0xF4 => self.utf8(byte & 0x07, 3, 0x1_0000),
            _ => self.put(char::REPLACEMENT_CHARACTER),
        }
    }

    /// Start a UTF-8 character: its lead byte's bits, and what is to come.
    fn utf8(&mut self, bits: u8, needed: u8, least: u32) {
        self.parsing = Parsing::Utf8 {
            code: u32::from(bits),
            needed,
            least,
        };
    }

    /// Put a character where the cursor is, and move on.
    fn put(&mut self, ch: char) {
        if self.cursor.0 >= self.columns {
            self.cursor.0 = 0;
            self.line_feed();
        }
        let (column, row) = self.cursor;
        if let Some(slot) = self.cells.get_mut(row * self.columns + column) {
            *slot = Cell { ch, ..self.pen };
        }
        self.cursor.0 += 1;
    }

    /// Down one row, scrolling when there is no row to go down to.
    fn line_feed(&mut self) {
        if self.cursor.1 + 1 < self.rows {
            self.cursor.1 += 1;
            return;
        }
        self.scroll();
    }

    /// Everything up one row, the bottom row emptied.
    fn scroll(&mut self) {
        let _ = self.cells.drain(..self.columns);
        self.cells
            .extend(core::iter::repeat_n(Cell::default(), self.columns));
        self.scrolled = self.scrolled.saturating_add(1);
    }

    /// A `CSI` sequence, by its final byte.
    fn csi(&mut self, parameters: &[u8], final_byte: u8) {
        let text = String::from_utf8_lossy(parameters);
        let private = text.starts_with('?');
        let numbers: Vec<usize> = text
            .trim_start_matches('?')
            .split(';')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        let at = |index: usize| numbers.get(index).copied().unwrap_or(0);
        match final_byte {
            b'A' => self.cursor.1 = self.cursor.1.saturating_sub(first.max(1)),
            b'B' => {
                self.cursor.1 = (self.cursor.1 + first.max(1)).min(self.rows.saturating_sub(1));
            }
            b'C' => {
                self.cursor.0 = (self.cursor.0 + first.max(1)).min(self.columns.saturating_sub(1));
            }
            b'D' => self.cursor.0 = self.cursor.0.saturating_sub(first.max(1)),
            // Counted from one, and a missing number is one.
            b'H' | b'f' => {
                self.cursor = (
                    at(1).max(1).saturating_sub(1).min(self.columns - 1),
                    first.max(1).saturating_sub(1).min(self.rows - 1),
                );
            }
            b'J' => self.erase_screen(first),
            b'K' => self.erase_line(first),
            b'm' => self.rendition(&numbers),
            b'h' if private && first == 25 => self.visible = true,
            b'l' if private && first == 25 => self.visible = false,
            // Every other sequence: dropped.
            _ => {}
        }
    }

    /// `CSI n J`.
    fn erase_screen(&mut self, what: usize) {
        let (column, row) = self.cursor;
        let at = row * self.columns + column;
        let range = match what {
            0 => at..self.cells.len(),
            1 => 0..at.saturating_add(1),
            _ => 0..self.cells.len(),
        };
        for index in range {
            if let Some(slot) = self.cells.get_mut(index) {
                *slot = Cell::default();
            }
        }
        if what >= 2 {
            self.cursor = (0, 0);
        }
    }

    /// `CSI n K`.
    fn erase_line(&mut self, what: usize) {
        let (column, row) = self.cursor;
        let start = row * self.columns;
        let range = match what {
            0 => start + column..start + self.columns,
            1 => start..start + column + 1,
            _ => start..start + self.columns,
        };
        for index in range {
            if let Some(slot) = self.cells.get_mut(index) {
                *slot = Cell::default();
            }
        }
    }

    /// `CSI n m`: the pen's colour, weight and background.
    fn rendition(&mut self, numbers: &[usize]) {
        let mut numbers = numbers.iter().copied();
        while let Some(number) = numbers.next() {
            match number {
                0 => self.pen = Cell::default(),
                1 => self.pen.bold = true,
                22 => self.pen.bold = false,
                30..=37 => self.pen.colour = u8::try_from(number - 30).unwrap_or(7),
                39 => self.pen.colour = 7,
                // Bright foregrounds are the same eight, drawn bold.
                90..=97 => {
                    self.pen.colour = u8::try_from(number - 90).unwrap_or(7);
                    self.pen.bold = true;
                }
                40..=47 => self.pen.background = u8::try_from(number - 40).ok(),
                49 => self.pen.background = None,
                100..=107 => self.pen.background = u8::try_from(number - 100 + 8).ok(),
                // `38;5;n`, `38;2;r;g;b` and the same after 48: one colour
                // in several numbers, which must be taken together or the
                // `5` and the `n` would be read as codes of their own.
                38 | 48 => {
                    let Some(index) = extended(&mut numbers) else {
                        continue;
                    };
                    if number == 38 {
                        self.pen.colour = index & 7;
                        self.pen.bold = index >= 8;
                    } else {
                        self.pen.background = Some(index);
                    }
                }
                // Everything else: not kept.
                _ => {}
            }
        }
    }
}

/// The rest of an extended colour, `5;n` or `2;r;g;b`, as the nearest of the
/// sixteen this terminal has: which of red, green and blue are on, and
/// whether it is bright.
fn extended(numbers: &mut impl Iterator<Item = usize>) -> Option<u8> {
    let (r, g, b) = match numbers.next()? {
        5 => {
            let index = numbers.next()?;
            if index < 16 {
                return u8::try_from(index).ok();
            }
            if index < 232 {
                // The six-level cube: 16 + 36r + 6g + b, each 0 to 5.
                let cube = index - 16;
                let level = |value: usize| value * 51;
                (level(cube / 36), level(cube / 6 % 6), level(cube % 6))
            } else {
                // The grey ramp, from 8 to 238.
                let grey = 8 + (index.min(255) - 232) * 10;
                (grey, grey, grey)
            }
        }
        2 => (numbers.next()?, numbers.next()?, numbers.next()?),
        _ => return None,
    };
    let on = |value: usize| value >= 0x80;
    let hue = u8::from(on(r)) | u8::from(on(g)) << 1 | u8::from(on(b)) << 2;
    // Bright when the strongest channel is near full; a dark grey is black.
    let bright = r.max(g).max(b) >= 0xE0 || (hue == 0 && r.max(g).max(b) >= 0x60);
    Some(hue | u8::from(bright) << 3)
}
