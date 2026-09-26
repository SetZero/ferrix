//! The line being typed: fuzzel's `prompt.c`.
//!
//! Text and a cursor in code points, and the editing actions the key
//! bindings name. Each answers whether anything changed, which is whether
//! fuzzel redraws.

/// The prompt, the placeholder and what has been typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    /// The prompt drawn before the input.
    pub prompt: Vec<char>,
    /// What is drawn while nothing is typed.
    pub placeholder: Vec<char>,
    /// What has been typed.
    pub text: Vec<char>,
    /// Where the cursor is, in code points.
    pub cursor: usize,
}

/// `iswspace`.
fn space(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_whitespace())
}

impl Prompt {
    /// A prompt with `text` typed already (`--search`), the cursor after it.
    #[must_use]
    pub fn new(prompt: &str, placeholder: &str, text: &str) -> Self {
        let text: Vec<char> = text.chars().collect();
        Self {
            prompt: prompt.chars().collect(),
            placeholder: placeholder.chars().collect(),
            cursor: text.len(),
            text,
        }
    }

    /// What has been typed, as a string.
    #[must_use]
    pub fn text_string(&self) -> String {
        self.text.iter().collect()
    }

    /// Insert `chars` at the cursor.
    pub fn insert(&mut self, chars: &str) {
        for c in chars.chars() {
            let at = self.cursor.min(self.text.len());
            self.text.insert(at, c);
            self.cursor = at + 1;
        }
    }

    fn prev_word(&self) -> usize {
        // From the character before the cursor: back over spaces, then over
        // the word, and one past where that stopped.
        let mut at = self.cursor.saturating_sub(1) as isize;
        while at >= 0 && space(self.text.get(at as usize)) {
            at -= 1;
        }
        while at >= 0 && !space(self.text.get(at as usize)) {
            at -= 1;
        }
        (at + 1).max(0) as usize
    }

    fn next_word(&self) -> usize {
        let mut at = self.cursor;
        while at < self.text.len() && !space(self.text.get(at)) {
            at += 1;
        }
        while at < self.text.len() && space(self.text.get(at)) {
            at += 1;
        }
        at
    }

    fn move_to(&mut self, at: usize) -> bool {
        if at == self.cursor {
            return false;
        }
        self.cursor = at;
        true
    }

    /// `cursor-home`.
    pub fn home(&mut self) -> bool {
        self.move_to(0)
    }

    /// `cursor-end`.
    pub fn end(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        self.move_to(self.text.len())
    }

    /// `cursor-right`.
    pub fn next_char(&mut self) -> bool {
        let at = if self.cursor < self.text.len() {
            self.cursor + 1
        } else {
            self.cursor
        };
        self.move_to(at)
    }

    /// `cursor-left`.
    pub fn prev_char(&mut self) -> bool {
        self.move_to(self.cursor.saturating_sub(1))
    }

    /// `cursor-left-word`.
    pub fn prev_word_move(&mut self) -> bool {
        let at = self.prev_word();
        self.move_to(at)
    }

    /// `cursor-right-word`.
    pub fn next_word_move(&mut self) -> bool {
        let at = self.next_word();
        self.move_to(at)
    }

    /// `delete-line`: always a change, as fuzzel counts it.
    pub fn erase_all(&mut self) -> bool {
        self.text.clear();
        self.cursor = 0;
        true
    }

    /// `delete-next`.
    pub fn erase_next_char(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        let _ = self.text.remove(self.cursor);
        true
    }

    /// `delete-prev`.
    pub fn erase_prev_char(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        if self.cursor < self.text.len() {
            let _ = self.text.remove(self.cursor);
        }
        true
    }

    /// `delete-next-word`.
    pub fn erase_next_word(&mut self) -> bool {
        let end = self.next_word();
        if end == self.cursor {
            return false;
        }
        let _ = self.text.drain(self.cursor..end);
        true
    }

    /// `delete-prev-word`.
    pub fn erase_prev_word(&mut self) -> bool {
        let start = self.prev_word();
        if start == self.cursor {
            return false;
        }
        let _ = self.text.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    /// `delete-line-forward`: always a change, as fuzzel counts it.
    pub fn erase_after_cursor(&mut self) -> bool {
        self.text.truncate(self.cursor);
        true
    }

    /// `delete-line-backward`.
    pub fn erase_before_cursor(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let _ = self.text.drain(..self.cursor);
        self.cursor = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Prompt;

    fn at(text: &str, cursor: usize) -> Prompt {
        let mut p = Prompt::new("> ", "", text);
        p.cursor = cursor;
        p
    }

    #[test]
    fn insert_and_move() {
        let mut p = Prompt::new("> ", "", "fox");
        assert_eq!(p.cursor, 3);
        assert!(p.home());
        p.insert("fire");
        assert_eq!((p.text_string(), p.cursor), ("firefox".to_owned(), 4));
        assert!(p.end() && !p.end());
        assert!(!p.next_char());
        assert!(p.prev_char());
        assert_eq!(p.cursor, 6);
    }

    #[test]
    fn words() {
        let mut p = at("open the  door", 14);
        assert!(p.erase_prev_word());
        assert_eq!(p.text_string(), "open the  ");
        assert!(p.erase_prev_word());
        assert_eq!((p.text_string(), p.cursor), ("open ".to_owned(), 5));
        let mut p = at("open the door", 0);
        assert!(p.next_word_move());
        assert_eq!(p.cursor, 5);
        assert!(p.erase_next_word());
        assert_eq!(p.text_string(), "open door");
        assert!(p.prev_word_move());
        assert_eq!(p.cursor, 0);
        assert!(!p.prev_word_move());
    }

    #[test]
    fn erasing() {
        let mut p = at("abcdef", 3);
        assert!(p.erase_prev_char());
        assert_eq!(p.text_string(), "abdef");
        assert!(p.erase_next_char());
        assert_eq!(p.text_string(), "abef");
        assert!(p.erase_before_cursor());
        assert_eq!((p.text_string(), p.cursor), ("ef".to_owned(), 0));
        assert!(!p.erase_before_cursor());
        assert!(p.erase_after_cursor());
        assert!(p.text.is_empty());
        assert!(!p.erase_prev_char() && !p.erase_next_char());
        assert!(p.erase_all());
    }
}
