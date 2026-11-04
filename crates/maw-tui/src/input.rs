//! Single-line text input with correct unicode cursor handling (cursor counts chars; the
//! visible column accounts for wide glyphs).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextInput {
    text: String,
    /// Cursor position in chars (0..=char count).
    cursor: usize,
}

/// A text laid out on rows (see `TextInput::wrap`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wrapped {
    pub rows: Vec<String>,
    /// Row and column of the cursor.
    pub row: usize,
    pub col: usize,
}

impl Wrapped {
    /// The first row to show when only `room` rows fit, so that the cursor row is visible.
    pub fn first_visible(&self, room: usize) -> usize {
        (self.row + 1).saturating_sub(room.max(1))
    }
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_text(s: &str) -> Self {
        let mut t = TextInput {
            text: s.to_string(),
            cursor: 0,
        };
        t.cursor = t.char_len();
        t
    }

    pub fn value(&self) -> &str {
        &self.text
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.char_len();
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    pub fn insert(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        let b = self.byte_at(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let b = self.byte_at(self.cursor - 1);
        self.text.remove(b);
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.char_len() {
            return;
        }
        let b = self.byte_at(self.cursor);
        self.text.remove(b);
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }
    pub fn home(&mut self) {
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.cursor = self.char_len();
    }

    pub fn word_left(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor = i;
    }

    pub fn word_right(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor = i;
    }

    /// Delete the word before the cursor (Ctrl+W).
    pub fn kill_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        let (s, e) = (self.byte_at(self.cursor), self.byte_at(end));
        self.text.replace_range(s..e, "");
    }

    /// Delete from the start to the cursor (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        let e = self.byte_at(self.cursor);
        self.text.replace_range(..e, "");
        self.cursor = 0;
    }

    /// Apply an editing key. Returns true if the key was consumed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.home(),
                'e' => self.end(),
                'u' => self.kill_to_start(),
                'w' => self.kill_word(),
                'b' => self.left(),
                'f' => self.right(),
                _ => return false,
            },
            KeyCode::Char('b') if alt => self.word_left(),
            KeyCode::Char('f') if alt => self.word_right(),
            KeyCode::Char(c) => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if ctrl || alt => self.word_left(),
            KeyCode::Right if ctrl || alt => self.word_right(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }

    /// The text laid out on rows `width` columns wide: lines break after the last space that fits
    /// (a word longer than the row is cut), and the cursor is given as (row, column).
    pub fn wrap(&self, width: usize) -> Wrapped {
        let width = width.max(2);
        let chars: Vec<char> = self.text.chars().collect();
        let w = |c: char| c.width().unwrap_or(0);
        let mut ranges: Vec<(usize, usize)> = vec![];
        let (mut start, mut used, mut i) = (0usize, 0usize, 0usize);
        let mut after_space: Option<usize> = None;
        while i < chars.len() {
            let cw = w(chars[i]);
            if used + cw > width {
                let end = match after_space {
                    Some(s) if s > start => s,
                    _ => i,
                };
                ranges.push((start, end));
                start = end;
                i = end;
                used = 0;
                after_space = None;
                continue;
            }
            used += cw;
            if chars[i] == ' ' {
                after_space = Some(i + 1);
            }
            i += 1;
        }
        ranges.push((start, chars.len()));
        let rows: Vec<String> = ranges
            .iter()
            .map(|&(s, e)| chars[s..e].iter().collect())
            .collect();
        let mut rows = rows;
        let cur = self.cursor.min(chars.len());
        let mut row = ranges.len() - 1;
        for (r, &(s, e)) in ranges.iter().enumerate() {
            if cur >= s && cur < e {
                row = r;
                break;
            }
        }
        let (s, _) = ranges[row];
        let mut col: usize = chars[s..cur].iter().map(|&c| w(c)).sum();
        if col >= width && row + 1 == ranges.len() {
            // the cursor sits after a full last row: it goes to a fresh row
            rows.push(String::new());
            row += 1;
            col = 0;
        }
        Wrapped { rows, row, col }
    }

    /// The visible slice for a field `width` columns wide and the cursor column inside it.
    /// The window scrolls so the cursor is always visible.
    pub fn view(&self, width: usize) -> (String, usize) {
        if width == 0 {
            return (String::new(), 0);
        }
        let chars: Vec<char> = self.text.chars().collect();
        let w = |c: char| c.width().unwrap_or(0);
        // walk back from the cursor so that the text before it fits in width-1 columns
        let mut start = self.cursor;
        let mut used = 0;
        while start > 0 {
            let cw = w(chars[start - 1]);
            if used + cw > width.saturating_sub(1) {
                break;
            }
            used += cw;
            start -= 1;
        }
        let mut out = String::new();
        let mut col = 0;
        for &c in &chars[start..] {
            let cw = w(c);
            if col + cw > width {
                break;
            }
            out.push(c);
            col += cw;
        }
        (out, used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(c, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn edits_around_multibyte_chars() {
        let mut t = TextInput::new();
        for c in "héllo wörld ✓".chars() {
            t.insert(c);
        }
        assert_eq!(t.value(), "héllo wörld ✓");
        t.left();
        t.backspace(); // removes ' '
        assert_eq!(t.value(), "héllo wörld✓");
        t.home();
        t.delete(); // removes 'h'
        assert_eq!(t.value(), "éllo wörld✓");
        t.end();
        assert_eq!(t.cursor(), t.char_len());
        t.backspace();
        assert_eq!(t.value(), "éllo wörld");
    }

    #[test]
    fn word_navigation_and_kill() {
        let mut t = TextInput::with_text("alpha beta gamma");
        t.word_left();
        assert_eq!(t.cursor(), 11);
        t.kill_word();
        assert_eq!(t.value(), "alpha gamma");
        t.kill_to_start();
        assert_eq!(t.value(), "gamma");
        assert_eq!(t.cursor(), 0);
    }

    #[test]
    fn view_scrolls_to_keep_cursor_visible_and_counts_wide_glyphs() {
        let mut t = TextInput::with_text("abcdefghijklmnopqrstuvwxyz");
        let (s, col) = t.view(10);
        assert!(s.len() <= 10 && col < 10);
        assert!(s.ends_with("xyz") || s.ends_with("z"));
        t.home();
        assert_eq!(t.view(10).1, 0);
        let wide = TextInput::with_text("日本語");
        let (s, col) = wide.view(20);
        assert_eq!(s, "日本語");
        assert_eq!(col, 6, "each CJK glyph is two columns wide");
        assert_eq!(wide.view(0), (String::new(), 0));
    }

    #[test]
    fn long_text_wraps_at_spaces_and_the_cursor_follows() {
        let t = TextInput::with_text("write a bubble sort in python and add tests");
        let w = t.wrap(16);
        assert_eq!(
            w.rows,
            ["write a bubble ", "sort in python ", "and add tests"]
        );
        assert!(w.rows.iter().all(|r| r.chars().count() <= 16));
        assert_eq!((w.row, w.col), (2, 13), "cursor at the end");
        let mut t = t;
        t.home();
        assert_eq!(t.wrap(16).row, 0);
        // a word longer than the row is cut instead of hidden
        let long = TextInput::with_text("abcdefghijklmnopqrstuvwxyz");
        let w = long.wrap(10);
        assert_eq!(w.rows, ["abcdefghij", "klmnopqrst", "uvwxyz"]);
        // a full last row sends the cursor to a new one
        let full = TextInput::with_text("abcdefghij");
        let w = full.wrap(10);
        assert_eq!((w.rows.len(), w.row, w.col), (2, 1, 0));
        // nothing typed is one empty row; wide glyphs count two columns
        assert_eq!(TextInput::new().wrap(10).rows, [""]);
        assert_eq!(
            TextInput::with_text("日本語日本").wrap(6).rows,
            ["日本語", "日本"]
        );
        // showing only the last rows that fit keeps the cursor visible
        let w = TextInput::with_text("one two three four five six").wrap(8);
        assert!(w.first_visible(2) + 2 > w.row);
    }

    #[test]
    fn handle_key_consumes_editing_keys_only() {
        let mut t = TextInput::new();
        assert!(t.handle_key(key(KeyCode::Char('x'))));
        assert!(!t.handle_key(key(KeyCode::Esc)));
        assert!(!t.handle_key(key(KeyCode::Enter)));
        assert_eq!(t.value(), "x");
    }

    #[test]
    fn control_characters_are_not_inserted() {
        let mut t = TextInput::new();
        t.insert('\n');
        t.insert('\u{7}');
        assert!(t.is_empty());
    }
}
