//! The pieces the screens are built from: a one-line text input and a file
//! picker.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

/// A one-line text field: its text, and where the cursor is in it.
#[derive(Clone, Debug, Default)]
pub struct TextInput {
    text: String,
    /// In characters, not bytes: 0 is before the first one.
    cursor: usize,
}

impl TextInput {
    /// A field holding `text`, the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let mut input = Self::default();
        input.set(text);
        input
    }

    /// What the field holds.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces what the field holds, the cursor at its end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.chars().count();
    }

    /// Where the cursor is, in characters from the start.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Applies an editing key, and says whether it was one.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let chars = self.text.chars().count();
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let at = self.byte_index(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_index(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < chars => {
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(chars),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = chars,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }

    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }
}

/// One row of a [`FilePicker`].
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// What is shown: the file name, a folder's with a `/` after it.
    label: String,
    path: PathBuf,
    is_dir: bool,
}

/// What a key did to a [`FilePicker`].
#[derive(Debug, PartialEq)]
pub enum Picked {
    /// Still picking.
    Not,
    /// This file was picked.
    File(PathBuf),
    /// Given up on.
    Cancelled,
}

/// Picks a file by walking the folders, showing only the files with one of
/// the suffixes it is given (and every folder, to walk into). Hidden files
/// and folders are left out.
pub struct FilePicker {
    dir: PathBuf,
    entries: Vec<Entry>,
    state: ListState,
    extensions: &'static [&'static str],
    /// Why `dir` could not be listed, if it could not.
    error: Option<String>,
    /// The name being typed, to jump to; and when its last letter was.
    typed: String,
    typed_at: Instant,
}

/// Letters typed further apart than this start a new name.
const TYPING_PAUSE: Duration = Duration::from_secs(1);

impl FilePicker {
    /// A picker starting at `start`: a folder, or a file, which is then the
    /// one selected in its folder.
    pub fn new(start: &Path, extensions: &'static [&'static str]) -> Self {
        let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
        let (dir, selected) = if start.is_dir() {
            (start, None)
        } else {
            let dir = start
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            (dir, Some(start))
        };
        let mut picker = Self {
            dir,
            entries: Vec::new(),
            state: ListState::default(),
            extensions,
            error: None,
            typed: String::new(),
            typed_at: Instant::now(),
        };
        picker.load(selected.as_deref());
        picker
    }

    /// The folder being shown.
    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lists `self.dir` again, selecting `selected` where it is in it.
    fn load(&mut self, selected: Option<&Path>) {
        self.entries.clear();
        self.error = None;
        if let Some(parent) = self.dir.parent() {
            self.entries.push(Entry {
                label: "../".to_string(),
                path: parent.to_path_buf(),
                is_dir: true,
            });
        }
        match std::fs::read_dir(&self.dir) {
            Ok(read) => {
                let mut dirs = Vec::new();
                let mut files = Vec::new();
                for entry in read.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(Entry {
                            label: format!("{name}/"),
                            path,
                            is_dir: true,
                        });
                    } else if self.wanted(&path) {
                        files.push(Entry {
                            label: name,
                            path,
                            is_dir: false,
                        });
                    }
                }
                let by_name =
                    |a: &Entry, b: &Entry| a.label.to_lowercase().cmp(&b.label.to_lowercase());
                dirs.sort_by(by_name);
                files.sort_by(by_name);
                self.entries.extend(dirs);
                self.entries.extend(files);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        let at = selected
            .and_then(|s| self.entries.iter().position(|e| e.path == s))
            .unwrap_or(0);
        self.state.select((!self.entries.is_empty()).then_some(at));
    }

    fn wanted(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| {
            self.extensions
                .iter()
                .any(|wanted| e.eq_ignore_ascii_case(wanted))
        })
    }

    /// Walks into `dir`, selecting `selected` there where it is given.
    fn enter(&mut self, dir: PathBuf, selected: Option<PathBuf>) {
        self.dir = dir;
        self.load(selected.as_deref());
    }

    /// Up to the parent folder, the folder just left selected in it.
    fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            let left = std::mem::replace(&mut self.dir, parent);
            self.load(Some(&left));
        }
    }

    /// Moves, walks or picks.
    pub fn handle(&mut self, key: KeyEvent) -> Picked {
        // Any key but a letter ends the name being typed.
        if !matches!(key.code, KeyCode::Char(_)) {
            self.typed.clear();
        }
        let last = self.entries.len().saturating_sub(1);
        let at = self.state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => return Picked::Cancelled,
            KeyCode::Up => self.state.select(Some(at.saturating_sub(1))),
            KeyCode::Down => self.state.select(Some((at + 1).min(last))),
            KeyCode::PageUp => self.state.select(Some(at.saturating_sub(10))),
            KeyCode::PageDown => self.state.select(Some((at + 10).min(last))),
            KeyCode::Home => self.state.select(Some(0)),
            KeyCode::End => self.state.select(Some(last)),
            KeyCode::Backspace | KeyCode::Left => self.up(),
            KeyCode::Char(c) => self.jump(c, Instant::now()),
            KeyCode::Enter | KeyCode::Right => {
                if let Some(entry) = self.entries.get(at).cloned() {
                    if entry.label == "../" {
                        self.up();
                    } else if entry.is_dir {
                        self.enter(entry.path, None);
                    } else if key.code == KeyCode::Enter {
                        return Picked::File(entry.path);
                    }
                }
            }
            _ => {}
        }
        Picked::Not
    }

    /// Finds a name by its start, typed: the letters typed less than a
    /// second apart make one prefix, searched from the selected entry on and
    /// round to the top. One letter typed again moves on to the next name
    /// with it, as file managers do.
    fn jump(&mut self, c: char, now: Instant) {
        if now.duration_since(self.typed_at) > TYPING_PAUSE {
            self.typed.clear();
        }
        self.typed_at = now;
        self.typed.extend(c.to_lowercase());
        let first = self.typed.chars().next().unwrap_or(c);
        let (prefix, skip) = if self.typed.chars().all(|t| t == first) {
            (first.to_string(), 1)
        } else {
            (self.typed.clone(), 0)
        };
        let at = self.state.selected().unwrap_or(0);
        let count = self.entries.len().max(1);
        if let Some(next) = (skip..count + skip).map(|i| (at + i) % count).find(|&i| {
            self.entries
                .get(i)
                .is_some_and(|e| e.label.to_lowercase().starts_with(&prefix))
        }) {
            self.state.select(Some(next));
        }
    }

    /// Draws the picker as a box over the middle of `area`.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, title: &str) {
        let area = centered(area, 80, 80);
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .title(Line::from(format!(" {title} ")).bold())
            .title_bottom(Line::from(
                " ↑↓ move · a-z jump · Enter open/pick · ← up · Esc cancel ",
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [head, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        frame.render_widget(
            Paragraph::new(self.dir.display().to_string()).italic(),
            head,
        );
        if let Some(error) = &self.error {
            frame.render_widget(Paragraph::new(error.as_str()).red(), body);
            return;
        }
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|e| {
                let item = ListItem::new(e.label.as_str());
                if e.is_dir {
                    item.blue()
                } else {
                    item
                }
            })
            .collect();
        let list = List::new(items).highlight_style(Style::new().reversed());
        frame.render_stateful_widget(list, body, &mut self.state);
    }
}

/// `path` relative to the working folder where it is inside it: shorter to
/// show, and the command line it goes into reads as one typed there.
pub fn relative(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|here| path.strip_prefix(here).ok().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| path.to_path_buf())
}

/// What of `text` fits in `width` columns, and where the cursor is in it:
/// the part round the cursor where there is one, else the end — of a path,
/// the file name.
pub fn visible(text: &str, cursor: Option<usize>, width: usize) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let width = width.max(2);
    match cursor {
        _ if chars.len() < width => (text.to_string(), cursor.unwrap_or(0)),
        Some(cursor) => {
            let start = cursor.saturating_sub(width - 1);
            let end = (start + width).min(chars.len());
            (chars[start..end].iter().collect(), cursor - start)
        }
        None => {
            let tail: String = chars[chars.len() - (width - 1)..].iter().collect();
            (format!("…{tail}"), 0)
        }
    }
}

/// A rectangle `percent_x` by `percent_y` of `area`, in its middle.
pub fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Min(0),
    ])
    .areas(area);
    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Min(0),
    ])
    .areas(middle);
    center
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn text_input_edits_at_the_cursor_in_characters() {
        let mut input = TextInput::new("città");
        assert_eq!(input.cursor(), 5);
        input.handle(key(KeyCode::Left));
        input.handle(key(KeyCode::Backspace));
        assert_eq!(input.text(), "cità");
        input.handle(key(KeyCode::Char('t')));
        assert_eq!(input.text(), "città");
        input.handle(key(KeyCode::Home));
        input.handle(key(KeyCode::Delete));
        assert_eq!(input.text(), "ittà");
        assert!(!input.handle(key(KeyCode::Enter)));
        assert!(!input.handle(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)));
        assert_eq!(input.text(), "ittà");
    }

    #[test]
    fn file_picker_shows_folders_then_wanted_files_and_picks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        for name in ["b.omap", "A.XMAP", "notes.txt", ".c.omap"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        std::fs::write(dir.path().join("sub").join("deep.ocd"), "").unwrap();

        let mut picker = FilePicker::new(dir.path(), &["omap", "xmap", "ocd"]);
        let labels: Vec<&str> = picker.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["../", "sub/", "A.XMAP", "b.omap"]);

        // Into sub/, pick its map.
        picker.handle(key(KeyCode::Down));
        assert_eq!(picker.handle(key(KeyCode::Enter)), Picked::Not);
        assert!(picker.dir().ends_with("sub"));
        picker.handle(key(KeyCode::Down));
        assert_eq!(
            picker.handle(key(KeyCode::Enter)),
            Picked::File(picker.dir().join("deep.ocd"))
        );

        // Back up: the folder just left is the one selected.
        picker.handle(key(KeyCode::Backspace));
        let at = picker.state.selected().unwrap();
        assert_eq!(picker.entries[at].label, "sub/");
        assert_eq!(picker.handle(key(KeyCode::Esc)), Picked::Cancelled);
    }

    #[test]
    fn typing_a_name_jumps_to_it() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["alpha.omap", "beta.omap", "bravo.omap", "brown.omap"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let mut picker = FilePicker::new(dir.path(), &["omap"]);
        let label = |p: &FilePicker| p.entries[p.state.selected().unwrap()].label.clone();
        let t0 = Instant::now();
        let later = |ms| t0 + Duration::from_millis(ms);

        // Typed together, the letters are one prefix.
        for (i, c) in "BRO".chars().enumerate() {
            picker.jump(c, later(100 * i as u64));
        }
        assert_eq!(label(&picker), "brown.omap");

        // After a pause, a new name; one letter again moves on.
        picker.jump('b', later(3000));
        assert_eq!(label(&picker), "beta.omap");
        picker.jump('b', later(3100));
        assert_eq!(label(&picker), "bravo.omap");
        picker.jump('b', later(3200));
        assert_eq!(label(&picker), "brown.omap");
        picker.jump('b', later(3300));
        assert_eq!(label(&picker), "beta.omap");

        // Any other key ends a name too: "r" right after Up is not "br".
        picker.handle(key(KeyCode::Up));
        picker.jump('r', later(3400));
        assert_eq!(label(&picker), "alpha.omap", "no name starts with r");
    }

    #[test]
    fn long_values_show_the_cursor_or_the_end() {
        assert_eq!(visible("short", Some(2), 10), ("short".to_string(), 2));
        assert_eq!(
            visible("maps/forest.omap", None, 8),
            ("…st.omap".to_string(), 0)
        );
        assert_eq!(visible("abcdefghij", Some(10), 5), ("ghij".to_string(), 4));
        assert_eq!(visible("abcdefghij", Some(0), 5), ("abcde".to_string(), 0));
    }

    #[test]
    fn file_picker_started_on_a_file_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.omap", "b.omap"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let picker = FilePicker::new(&dir.path().join("b.omap"), &["omap"]);
        let at = picker.state.selected().unwrap();
        assert_eq!(picker.entries[at].label, "b.omap");
    }
}
