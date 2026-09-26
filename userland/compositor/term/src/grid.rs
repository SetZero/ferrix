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
//! * `CSI ? n h|l` -- the private modes, of which the cursor's own
//!   visibility (25) and bracketed paste (2004) are kept.
//! * `CSI 3 J` -- forget the scrollback, which `clear` asks for.
//!
//! An escape sequence this does not know is dropped rather than drawn: a
//! terminal that printed the bytes of a sequence it did not understand would
//! fill its screen with rubbish the first time a program asked about the
//! cursor.
//!
//! # Scrollback and selection
//!
//! A row that scrolls off the top is kept, up to [`HISTORY`] of them, and
//! the grid can be *viewed* some rows back into them. What is on the screen
//! is then [`Grid::shown`], not [`Grid::cell`]: the program still writes at
//! the live rows, and the person reads older ones.
//!
//! Every row, kept or live, has an absolute line number that never changes
//! while the row exists: the live row `r` is line `scrolled + r`. A
//! selection is held in those numbers, so text that scrolls on while it is
//! selected stays selected, rather than the highlight staying where it was
//! on the glass and the text moving out from under it.

use std::collections::VecDeque;

/// How many rows that scrolled off the top are kept.
///
/// Ten thousand is what most terminals keep by default, and at 150 columns
/// of eight-byte cells it is about twelve megabytes -- a price paid only by
/// a terminal whose program has written that much.
pub const HISTORY: usize = 10_000;

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
    /// it did, and which is the absolute line number of the top live row.
    scrolled: u64,
    /// Whether each live row ran on into the next, rather than ending in a
    /// line feed: what copying joins back into one line.
    wrapped: Vec<bool>,
    /// The rows that scrolled off the top, oldest first.
    history: VecDeque<Line>,
    /// How many rows back into `history` the screen is looking; zero is the
    /// live rows.
    view: usize,
    /// What is selected, if anything.
    selection: Option<Selection>,
    /// Whether the program asked for pasted text to be bracketed, `CSI ?
    /// 2004 h`, so it can tell a paste from typing: zsh's line editor does,
    /// so that a pasted line is not run the moment its newline arrives.
    bracketed_paste: bool,
}

/// A row that scrolled off the top: its cells at the width it had then, and
/// whether it ran on into the next.
#[derive(Clone, Debug)]
struct Line {
    cells: Vec<Cell>,
    wrapped: bool,
}

/// Where the pointer is, in the grid's absolute lines: the line, the cell,
/// and whether it is in that cell's right half -- which says which side of
/// the cell a character-wise selection's edge falls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// The absolute line number.
    pub line: u64,
    /// The column of the cell.
    pub column: usize,
    /// Whether it is in the cell's right half.
    pub right: bool,
}

/// What one press selects: the cells dragged over, or the whole words or
/// whole lines they touch -- one, two or three clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Character by character.
    Cell,
    /// Whole words.
    Word,
    /// Whole lines.
    Line,
}

/// A selection: where the press was, where the pointer is now, and by what.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: Point,
    head: Point,
    unit: Unit,
}

/// What the screen showed at one moment, cheaply: the cells in view and
/// which of them were selected, and where the cursor was drawn.
///
/// [`Grid::damage_since`] compares against this rather than a whole second
/// grid, which would copy the scrollback on every batch of output.
#[derive(Clone, Debug)]
pub struct Snapshot {
    size: (usize, usize),
    shown: Vec<(Cell, bool)>,
    cursor: Option<(usize, usize)>,
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
            wrapped: vec![false; rows],
            history: VecDeque::new(),
            view: 0,
            selection: None,
            bracketed_paste: false,
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

    /// What the screen shows now, to compare a later one against.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let spans = self.spans();
        let mut shown = Vec::with_capacity(self.columns * self.rows);
        for row in 0..self.rows {
            let line = self.line_at(row);
            for column in 0..self.columns {
                let cell = self.row_cells(line).and_then(|cells| cells.get(column));
                shown.push((
                    cell.copied().unwrap_or_default(),
                    Self::in_spans(spans, line, column),
                ));
            }
        }
        Snapshot {
            size: self.size(),
            shown,
            cursor: self.shown_cursor(),
        }
    }

    /// The smallest visual region that differs from `before`.
    ///
    /// A terminal normally changes one cell and moves its block cursor one
    /// cell. Repainting the whole shared-memory buffer for that common case
    /// turns one typed byte into a full-window copy and compositor frame. The
    /// comparison is of what is on the screen, so scrolling, erasing, every
    /// escape sequence, looking back into the scrollback and selecting all
    /// damage exactly what changed.
    #[must_use]
    pub fn damage_since(&self, before: &Snapshot) -> Option<CellDamage> {
        if self.size() != before.size {
            return Some(CellDamage::full(self.columns, self.rows));
        }
        let now = self.snapshot();

        let mut damage: Option<CellDamage> = None;
        for (at, (was, is)) in before.shown.iter().zip(&now.shown).enumerate() {
            if was != is {
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
        if before.cursor != now.cursor {
            for (column, row) in [before.cursor, now.cursor].into_iter().flatten() {
                let one = CellDamage {
                    left: column,
                    top: row,
                    width: 1,
                    height: 1,
                };
                damage = Some(damage.map_or(one, |held| held.joined(one)));
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

    /// The cell the screen shows at a column and a row, and whether it is
    /// selected: a live cell, or one from the scrollback when the screen is
    /// looking back. A kept row narrower than the screen is blank past its
    /// end.
    #[must_use]
    pub fn shown(&self, column: usize, row: usize) -> Option<(Cell, bool)> {
        if column >= self.columns || row >= self.rows {
            return None;
        }
        let line = self.line_at(row);
        let cell = self
            .row_cells(line)
            .and_then(|cells| cells.get(column))
            .copied()
            .unwrap_or_default();
        Some((cell, Self::in_spans(self.spans(), line, column)))
    }

    /// Where the cursor is drawn, if it is: it moves down the screen with
    /// the live rows as the screen looks back, and off it.
    #[must_use]
    pub fn shown_cursor(&self) -> Option<(usize, usize)> {
        let row = self.cursor.1 + self.view;
        (self.visible && row < self.rows && self.cursor.0 < self.columns)
            .then_some((self.cursor.0, row))
    }

    /// How many rows back the screen is looking.
    #[must_use]
    pub const fn view(&self) -> usize {
        self.view
    }

    /// How many rows the scrollback holds.
    #[must_use]
    pub fn history(&self) -> usize {
        self.history.len()
    }

    /// Look `rows` further back into the scrollback, or forward for a
    /// negative number, stopping at either end.
    pub fn scroll_view(&mut self, rows: isize) {
        self.view = self
            .view
            .saturating_add_signed(rows)
            .min(self.history.len());
    }

    /// Look at the live rows again.
    pub fn view_live(&mut self) {
        self.view = 0;
    }

    /// Whether the program asked for pasted text to be bracketed.
    #[must_use]
    pub const fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// The point the screen shows at a column and a row, and which half of
    /// the cell: what a pointer at that place is over.
    #[must_use]
    pub fn point(&self, column: usize, row: usize, right: bool) -> Point {
        Point {
            line: self.line_at(row.min(self.rows.saturating_sub(1))),
            column: column.min(self.columns.saturating_sub(1)),
            right,
        }
    }

    /// Start a selection at `at`, by `unit`.
    pub fn select(&mut self, at: Point, unit: Unit) {
        self.selection = Some(Selection {
            anchor: at,
            head: at,
            unit,
        });
    }

    /// Move the selection's free end to `at`.
    pub fn extend(&mut self, at: Point) {
        if let Some(selection) = &mut self.selection {
            selection.head = at;
        }
    }

    /// Select nothing.
    pub fn deselect(&mut self) {
        self.selection = None;
    }

    /// The selected text: a line break between rows, except where a row ran
    /// on into the next, and each row's trailing blanks left off -- which is
    /// what the program wrote, rather than the spaces the grid filled in.
    /// `None` when nothing is selected.
    #[must_use]
    pub fn selected(&self) -> Option<String> {
        let ((first, from), (last, to)) = self.spans()?;
        let mut text = String::new();
        for line in first..=last {
            let Some(cells) = self.row_cells(line) else {
                continue;
            };
            let start = if line == first { from } else { 0 };
            let end = if line == last { to } else { usize::MAX };
            let row: String = cells
                .iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .map(|cell| cell.ch)
                .collect();
            let wrapped = self.wrapped_at(line) && line != last;
            if wrapped {
                text.push_str(&row);
            } else {
                text.push_str(row.trim_end());
                if line != last {
                    text.push('\n');
                }
            }
        }
        Some(text)
    }

    /// The absolute line the screen shows at `row`.
    fn line_at(&self, row: usize) -> u64 {
        (self.scrolled + row as u64).saturating_sub(self.view as u64)
    }

    /// The cells of an absolute line, if it is still held.
    fn row_cells(&self, line: u64) -> Option<&[Cell]> {
        if let Some(row) = line.checked_sub(self.scrolled) {
            let row = usize::try_from(row).ok()?;
            if row >= self.rows {
                return None;
            }
            return self.cells.get(row * self.columns..(row + 1) * self.columns);
        }
        let back = usize::try_from(self.scrolled - line).ok()?;
        let index = self.history.len().checked_sub(back)?;
        self.history.get(index).map(|kept| kept.cells.as_slice())
    }

    /// Whether an absolute line ran on into the next.
    fn wrapped_at(&self, line: u64) -> bool {
        if let Some(row) = line.checked_sub(self.scrolled) {
            return usize::try_from(row)
                .ok()
                .and_then(|row| self.wrapped.get(row))
                .copied()
                .unwrap_or(false);
        }
        let Ok(back) = usize::try_from(self.scrolled - line) else {
            return false;
        };
        self.history
            .len()
            .checked_sub(back)
            .and_then(|index| self.history.get(index))
            .is_some_and(|kept| kept.wrapped)
    }

    /// The selection as cells: the first line and column, and the last line
    /// and the column after the last cell. `None` when it selects nothing,
    /// which is a press that has not moved yet.
    fn spans(&self) -> Option<((u64, usize), (u64, usize))> {
        let selection = self.selection?;
        let key = |point: Point| (point.line, point.column, point.right);
        let (start, end) = if key(selection.anchor) <= key(selection.head) {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        match selection.unit {
            // An edge falls between cells: after a cell when the pointer is
            // in its right half, before it in the left.
            Unit::Cell => {
                let from = start.column + usize::from(start.right);
                let to = end.column + usize::from(end.right);
                if (start.line, from) >= (end.line, to) {
                    return None;
                }
                Some(((start.line, from), (end.line, to)))
            }
            Unit::Word => {
                let from = self.word_edge(start.line, start.column, false);
                let to = self.word_edge(end.line, end.column, true);
                Some(((start.line, from), (end.line, to)))
            }
            Unit::Line => Some(((start.line, 0), (end.line, usize::MAX))),
        }
    }

    /// Where the word around a cell starts, or ends (the column after it).
    /// A cell that is not part of a word is a word of one cell.
    fn word_edge(&self, line: u64, column: usize, end: bool) -> usize {
        let Some(cells) = self.row_cells(line) else {
            return column + usize::from(end);
        };
        let word = |at: usize| cells.get(at).is_some_and(|cell| in_word(cell.ch));
        if !word(column) {
            return column + usize::from(end);
        }
        let mut at = column;
        if end {
            while word(at + 1) {
                at += 1;
            }
            at + 1
        } else {
            while at > 0 && word(at - 1) {
                at -= 1;
            }
            at
        }
    }

    /// Whether a cell is inside the selection's cells.
    fn in_spans(spans: Option<((u64, usize), (u64, usize))>, line: u64, column: usize) -> bool {
        let Some((start, end)) = spans else {
            return false;
        };
        (line, column) >= start && (line, column) < end
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
    ///
    /// A grid that loses rows below the cursor loses them from the bottom,
    /// which is empty; one that would lose the cursor's row instead moves
    /// its top rows into the scrollback, so the line being typed on stays in
    /// view and what was above it can still be scrolled back to.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let (columns, rows) = (columns.max(1), rows.max(1));
        if (columns, rows) == (self.columns, self.rows) {
            return;
        }
        let over = (self.cursor.1 + 1).saturating_sub(rows);
        for _ in 0..over {
            self.keep_top_row();
        }
        let mut cells = vec![Cell::default(); columns * rows];
        let mut wrapped = vec![false; rows];
        for row in 0..rows.min(self.rows) {
            if let (Some(was), Some(slot)) = (self.wrapped.get(row), wrapped.get_mut(row)) {
                *slot = *was;
            }
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
        self.wrapped = wrapped;
        self.columns = columns;
        self.rows = rows;
        self.cursor = (
            self.cursor.0.min(columns.saturating_sub(1)),
            self.cursor
                .1
                .saturating_sub(over)
                .min(rows.saturating_sub(1)),
        );
        self.view = 0;
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
            if let Some(wrapped) = self.wrapped.get_mut(self.cursor.1) {
                *wrapped = true;
            }
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
        self.keep_top_row();
        self.cells
            .extend(core::iter::repeat_n(Cell::default(), self.columns));
        self.wrapped.push(false);
    }

    /// Move the top live row into the scrollback, leaving one row fewer.
    ///
    /// A screen looking back keeps looking at the same text, one row further
    /// back, rather than having it scroll away under the person reading it.
    fn keep_top_row(&mut self) {
        let cells: Vec<Cell> = self.cells.drain(..self.columns).collect();
        let wrapped = if self.wrapped.is_empty() {
            false
        } else {
            self.wrapped.remove(0)
        };
        if self.history.len() == HISTORY {
            let _ = self.history.pop_front();
        }
        self.history.push_back(Line { cells, wrapped });
        self.scrolled = self.scrolled.saturating_add(1);
        if self.view > 0 {
            self.view = (self.view + 1).min(self.history.len());
        }
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
            b'h' if private && first == 2004 => self.bracketed_paste = true,
            b'l' if private && first == 2004 => self.bracketed_paste = false,
            // Every other sequence: dropped.
            _ => {}
        }
    }

    /// `CSI n J`.
    fn erase_screen(&mut self, what: usize) {
        if what == 3 {
            // The scrollback, and only that: `clear` sends this after the
            // `2 J` that clears the screen.
            self.history.clear();
            self.view = 0;
            return;
        }
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
        let rows = match what {
            0 => row + 1..self.rows,
            1 => 0..row,
            _ => 0..self.rows,
        };
        for index in rows {
            if let Some(wrapped) = self.wrapped.get_mut(index) {
                *wrapped = false;
            }
        }
        if what >= 2 {
            self.cursor = (0, 0);
        }
    }

    /// `CSI n K`.
    fn erase_line(&mut self, what: usize) {
        let (column, row) = self.cursor;
        if what != 1
            && let Some(wrapped) = self.wrapped.get_mut(row)
        {
            *wrapped = false;
        }
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

/// Whether a character is part of a word, for a double click: anything but
/// blanks and the punctuation that surrounds a word rather than being in it.
/// A path, a URL and an option like `--arch=x86_64` are each one word.
fn in_word(ch: char) -> bool {
    !ch.is_whitespace() && !"\"'`()[]{}<>|;,".contains(ch)
}
