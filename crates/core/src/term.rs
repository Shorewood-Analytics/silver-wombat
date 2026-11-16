//! What the terminal shows.
//!
//! Programs write more than text: colour codes, carriage returns that redraw
//! a progress bar on one line, backspaces. [`Display`] takes program output
//! as it arrives and keeps the plain text a terminal would be showing. The
//! codes that only change colour or style are dropped, not drawn.

/// The text a terminal is showing, built from program output piece by piece.
#[derive(Debug, Default)]
pub struct Display {
    /// Finished lines, each ending in a newline.
    done: String,
    /// The line being written.
    line: Vec<char>,
    /// Where on that line the next character goes.
    column: usize,
    /// An escape sequence cut off at the end of the last piece.
    pending: String,
}

impl Display {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything on show.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = self.done.clone();
        text.extend(&self.line);
        text
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Forgets the oldest lines once more than `max_bytes` are kept. Whole
    /// lines go, so the first line shown is never half a line.
    pub fn keep_last(&mut self, max_bytes: usize) {
        // One endless line is cut from its start, like any other old text.
        if self.line.len() > max_bytes {
            let cut = self.line.len() - max_bytes;
            self.line.drain(..cut);
            self.column = self.column.saturating_sub(cut);
        }
        if self.done.len() <= max_bytes {
            return;
        }
        let mut cut = self.done.len() - max_bytes;
        while !self.done.is_char_boundary(cut) {
            cut += 1;
        }
        match self.done[cut..].find('\n') {
            Some(newline) => cut += newline + 1,
            None => cut = self.done.len(),
        }
        self.done.drain(..cut);
    }

    /// Adds program output.
    pub fn push(&mut self, output: &str) {
        let mut text = std::mem::take(&mut self.pending);
        text.push_str(output);
        let mut rest = text.as_str();
        while let Some(first) = rest.chars().next() {
            if first == '\u{1b}' {
                let Some((code, length)) = escape(rest) else {
                    // The rest of it has not arrived yet.
                    rest.clone_into(&mut self.pending);
                    return;
                };
                self.obey(&code);
                rest = &rest[length..];
                continue;
            }
            rest = &rest[first.len_utf8()..];
            match first {
                '\n' => {
                    self.done.extend(&self.line);
                    self.done.push('\n');
                    self.line.clear();
                    self.column = 0;
                }
                '\r' => self.column = 0,
                '\u{8}' => self.column = self.column.saturating_sub(1),
                '\t' => self.put('\t'),
                // Bell and the other control characters show nothing.
                other if other.is_control() => {}
                other => self.put(other),
            }
        }
    }

    fn put(&mut self, character: char) {
        if self.column < self.line.len() {
            self.line[self.column] = character;
        } else {
            self.line.push(character);
        }
        self.column += 1;
    }

    /// Carries out the few codes that change what text is on the line.
    fn obey(&mut self, code: &Escape) {
        let Escape::Control { arguments, action } = code else {
            return;
        };
        match (*action, arguments.as_str()) {
            // Erase from the cursor to the end of the line.
            ('K', "" | "0") => self.line.truncate(self.column),
            // Erase the whole line.
            ('K', "2") => {
                let width = self.column.min(self.line.len());
                self.line.clear();
                self.line.resize(width, ' ');
            }
            // Go to a column, counted from 1.
            ('G', column) => {
                let column = column.parse::<usize>().unwrap_or(1).max(1) - 1;
                while self.line.len() < column {
                    self.line.push(' ');
                }
                self.column = column;
            }
            _ => {}
        }
    }
}

/// One escape sequence.
#[derive(Debug, PartialEq, Eq)]
enum Escape {
    /// `ESC [ arguments action`: colours, cursor movement, erasing.
    Control { arguments: String, action: char },
    /// Anything else: window titles, character sets. Shows nothing.
    Other,
}

/// Reads the escape sequence at the start of `text` and says how many bytes
/// it takes. `None` means the text ends before the sequence does.
fn escape(text: &str) -> Option<(Escape, usize)> {
    let mut characters = text.char_indices().skip(1);
    let (_, kind) = characters.next()?;
    match kind {
        '[' => {
            let mut arguments = String::new();
            for (index, character) in characters {
                if ('\u{40}'..='\u{7e}').contains(&character) {
                    let code = Escape::Control {
                        arguments,
                        action: character,
                    };
                    return Some((code, index + character.len_utf8()));
                }
                arguments.push(character);
            }
            None
        }
        // An operating-system command, such as a window title. It ends with
        // a bell or with ESC \.
        ']' => {
            let mut previous = ' ';
            for (index, character) in characters {
                if character == '\u{7}' || (previous == '\u{1b}' && character == '\\') {
                    return Some((Escape::Other, index + character.len_utf8()));
                }
                previous = character;
            }
            None
        }
        // Two-character sequences, and the three-character ones that pick a
        // character set.
        '(' | ')' | '#' => {
            let (index, character) = characters.next()?;
            Some((Escape::Other, index + character.len_utf8()))
        }
        other => Some((Escape::Other, 1 + other.len_utf8())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(pieces: &[&str]) -> String {
        let mut display = Display::new();
        for piece in pieces {
            display.push(piece);
        }
        display.text()
    }

    #[test]
    fn plain_text_is_kept_as_it_is() {
        assert_eq!(shown(&["one\ntwo\n", "three"]), "one\ntwo\nthree");
        assert_eq!(shown(&["a\tb é 日本\n"]), "a\tb é 日本\n");
    }

    #[test]
    fn colour_codes_are_dropped() {
        assert_eq!(shown(&["\u{1b}[31mred\u{1b}[0m plain\n"]), "red plain\n");
        assert_eq!(
            shown(&["\u{1b}[1;38;5;208mbold orange\u{1b}[m\n"]),
            "bold orange\n"
        );
    }

    #[test]
    fn a_code_split_between_two_pieces_is_still_dropped() {
        assert_eq!(
            shown(&["ok \u{1b}[3", "2mgreen\u{1b}", "[0m\n"]),
            "ok green\n"
        );
        // Nothing of an unfinished code is shown while it is waited for.
        assert_eq!(shown(&["wait\u{1b}[3"]), "wait");
    }

    #[test]
    fn a_carriage_return_redraws_the_line() {
        // A progress bar: each update starts again from the left.
        assert_eq!(
            shown(&[" 10%|#   |\r", " 50%|##  |\r", "100%|####|\n"]),
            "100%|####|\n"
        );
        // Text shorter than what it overwrites leaves the rest, as a terminal does.
        assert_eq!(shown(&["abcdef\rXY\n"]), "XYcdef\n");
        // Windows line endings are ordinary lines.
        assert_eq!(shown(&["one\r\ntwo\r\n"]), "one\ntwo\n");
    }

    #[test]
    fn erasing_and_backspace_change_the_line() {
        assert_eq!(shown(&["abcdef\r\u{1b}[Kxy\n"]), "xy\n");
        assert_eq!(shown(&["abc\u{8}\u{8}X\n"]), "aXc\n");
        assert_eq!(shown(&["ab\u{1b}[2K\rnew\n"]), "new\n");
        assert_eq!(shown(&["ab\u{1b}[6Gz\n"]), "ab   z\n");
    }

    #[test]
    fn titles_bells_and_other_codes_show_nothing() {
        assert_eq!(shown(&["\u{1b}]0;a title\u{7}text\n"]), "text\n");
        assert_eq!(shown(&["\u{1b}]8;;http://x\u{1b}\\link\n"]), "link\n");
        assert_eq!(shown(&["ding\u{7}\u{1b}(B\u{1b}=\n"]), "ding\n");
    }

    #[test]
    fn old_lines_are_forgotten_whole() {
        let mut display = Display::new();
        display.push("first line\nsecond line\nthird");
        display.keep_last(15);
        assert_eq!(display.text(), "second line\nthird");
        display.keep_last(6);
        assert_eq!(display.text(), "third");
        display.clear();
        assert_eq!(display.text(), "");
        // A line that never ends is cut too.
        display.push(&"x".repeat(50));
        display.push("END");
        display.keep_last(10);
        assert_eq!(display.text(), "xxxxxxxEND");
        display.push("!\n");
        assert_eq!(display.text(), "xxxxxxxEND!\n");
    }
}
