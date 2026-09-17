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
//!   foreground colours are kept and the rest ignored.
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
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            // Seven is white, which is what a terminal starts in.
            colour: 7,
            bold: false,
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
            byte => self.put(char::from(byte)),
        }
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

    /// `CSI n m`: the pen's colour and weight.
    fn rendition(&mut self, numbers: &[usize]) {
        for number in numbers {
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
                // Backgrounds and everything else: not kept.
                _ => {}
            }
        }
    }
}
