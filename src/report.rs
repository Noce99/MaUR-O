//! Shared shaping for the plain text files a benchmark run leaves behind.
//!
//! Those files are read in whatever a person happens to open a `.txt` with,
//! which is to say something with no idea how to reflow a paragraph. So they
//! arrive already wrapped, at a width that fits a terminal without one.

/// How wide the run's text files are wrapped.
pub const WIDTH: usize = 80;

/// Wraps `text` to `width` columns, putting `indent` in front of every line.
///
/// Each line of the input is a paragraph of its own, so a caller writes a
/// paragraph as one long line and gets it back folded; blank lines survive as
/// blank lines. Counted in characters rather than bytes, since these reports
/// hold a `±` and the occasional dash.
pub fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = indent.to_string();
        let mut empty = true;
        for word in paragraph.split_whitespace() {
            let extended = line.chars().count() + usize::from(!empty) + word.chars().count();
            if !empty && extended > width {
                lines.push(line);
                line = indent.to_string();
                empty = true;
            }
            if !empty {
                line.push(' ');
            }
            line.push_str(word);
            empty = false;
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// A paragraph wrapped to the standard width, with a trailing blank line.
pub fn paragraph(text: &str) -> String {
    format!("{}\n\n", wrap(text, WIDTH, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_folded_at_the_width() {
        let text = "one two three four five six seven eight nine ten";
        assert_eq!(
            wrap(text, 20, ""),
            "one two three four\nfive six seven eight\nnine ten"
        );
    }

    #[test]
    fn the_indent_counts_towards_the_width() {
        assert_eq!(
            wrap("one two three four", 12, "  "),
            "  one two\n  three four"
        );
    }

    #[test]
    fn a_word_longer_than_the_width_is_left_alone_rather_than_cut() {
        // Breaking a path or a file name in half helps nobody.
        assert_eq!(
            wrap("a supercalifragilistic b", 10, ""),
            "a\nsupercalifragilistic\nb"
        );
    }

    #[test]
    fn blank_lines_stay_blank_and_lines_wrap_on_their_own() {
        assert_eq!(wrap("one two\n\nthree", 4, ""), "one\ntwo\n\nthree");
    }

    #[test]
    fn the_width_is_counted_in_characters_not_bytes() {
        // "±" is two bytes and one column; counting bytes would fold early.
        assert_eq!(wrap("± ± ±", 5, ""), "± ± ±");
    }
}
