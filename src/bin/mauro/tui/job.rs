//! A subcommand run from the terminal UI: `mauro` itself, started again as a
//! child process with the command line a screen built, its output shown as
//! it comes.
//!
//! A child process rather than a call into the subcommand's module, because
//! the subcommands print as they go — straight to a terminal the UI is
//! drawing on — and a child's output can be caught whole. It also makes
//! cancelling a kill, and keeps whatever the subcommand does, a panic
//! included, from taking the UI down with it.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::Action;

/// What to run, as a screen hands it over.
pub struct JobSpec {
    /// The arguments after `mauro`: `["render", "map.omap", …]`.
    pub args: Vec<String>,
    /// What an exit code of this subcommand means.
    pub explain: fn(i32) -> Option<&'static str>,
}

/// Which of the child's two outputs a line came out of.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stream {
    Out,
    Err,
}

/// A piece of output, as the reader threads split it.
#[derive(Debug, PartialEq)]
enum Piece {
    /// A line that ended with a newline.
    Line(String),
    /// A line that ended with a lone carriage return: a progress bar, which
    /// the next line from the same stream draws over.
    Redrawn(String),
}

/// Splits a byte stream into [`Piece`]s, whatever chunks it arrives in.
#[derive(Default)]
struct Splitter {
    current: Vec<u8>,
    /// A `\r` seen last: a lone one, unless a `\n` follows (Windows).
    carriage_return: bool,
}

impl Splitter {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Piece>) {
        for &byte in bytes {
            if self.carriage_return {
                self.carriage_return = false;
                if byte == b'\n' {
                    out.push(Piece::Line(self.take()));
                    continue;
                }
                out.push(Piece::Redrawn(self.take()));
            }
            match byte {
                b'\n' => out.push(Piece::Line(self.take())),
                b'\r' => self.carriage_return = true,
                _ => self.current.push(byte),
            }
        }
    }

    /// Whatever is left when the stream ends.
    fn finish(&mut self, out: &mut Vec<Piece>) {
        if self.carriage_return || !self.current.is_empty() {
            self.carriage_return = false;
            out.push(Piece::Line(self.take()));
        }
    }

    fn take(&mut self) -> String {
        strip_ansi(&String::from_utf8_lossy(&std::mem::take(&mut self.current)))
    }
}

/// `text` without its terminal escape sequences (colours, cursor moves),
/// which mean nothing inside the UI's own drawing.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // Parameters and intermediates, up to the final byte.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The output so far, both streams interleaved as they arrived.
#[derive(Default)]
struct Log {
    lines: Vec<(Stream, String)>,
    /// Per stream, the line a [`Piece::Redrawn`] left to be drawn over.
    open: [Option<usize>; 2],
}

impl Log {
    fn push(&mut self, stream: Stream, piece: Piece) {
        let slot = stream as usize;
        let (text, redrawn) = match piece {
            Piece::Line(text) => (text, false),
            Piece::Redrawn(text) => (text, true),
        };
        let at = match self.open[slot] {
            Some(at) => {
                self.lines[at].1 = text;
                at
            }
            None => {
                self.lines.push((stream, text));
                self.lines.len() - 1
            }
        };
        self.open[slot] = redrawn.then_some(at);
    }
}

/// Where the child is.
#[derive(Debug, PartialEq)]
enum Status {
    Running,
    /// Ended by itself, with this exit code (`None`: killed by a signal).
    Finished(Option<i32>),
    Cancelled,
    /// Could not be started at all.
    Failed(String),
}

/// A running (or run) subcommand, and the screen showing it.
pub struct Job {
    /// The command line, as it would be typed.
    command_line: String,
    explain: fn(i32) -> Option<&'static str>,
    child: Option<Child>,
    output: Receiver<(Stream, Piece)>,
    log: Log,
    status: Status,
    /// Lines scrolled up from the end; 0 follows the output as it comes.
    scroll_back: usize,
    /// Ticks seen while running, for the spinner.
    ticks: usize,
}

impl Job {
    /// Starts `mauro` again, with `spec`'s arguments.
    pub fn start(spec: JobSpec) -> Self {
        let (sender, output) = channel();
        let mut job = Self {
            command_line: command_line(&spec.args),
            explain: spec.explain,
            child: None,
            output,
            log: Log::default(),
            status: Status::Running,
            scroll_back: 0,
            ticks: 0,
        };
        let spawned = std::env::current_exe().and_then(|exe| {
            Command::new(exe)
                .args(&spec.args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        });
        match spawned {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    read_into(out, Stream::Out, sender.clone());
                }
                if let Some(err) = child.stderr.take() {
                    read_into(err, Stream::Err, sender);
                }
                job.child = Some(child);
            }
            Err(e) => job.status = Status::Failed(e.to_string()),
        }
        job
    }

    fn running(&self) -> bool {
        self.status == Status::Running
    }

    /// Takes in what the child said since the last tick, and whether it
    /// has ended.
    pub fn tick(&mut self) {
        self.ticks += 1;
        while let Ok((stream, piece)) = self.output.try_recv() {
            self.log.push(stream, piece);
        }
        if let (Status::Running, Some(child)) = (&self.status, &mut self.child) {
            if let Ok(Some(status)) = child.try_wait() {
                self.status = Status::Finished(status.code());
            }
        }
    }

    /// Stops the child, if it is still running.
    fn cancel(&mut self) {
        if let (Status::Running, Some(child)) = (&self.status, &mut self.child) {
            child.kill().ok();
            child.wait().ok();
            self.status = Status::Cancelled;
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let lines = self.log.lines.len();
        match key.code {
            KeyCode::Up => self.scroll_back = (self.scroll_back + 1).min(lines),
            KeyCode::Down => self.scroll_back = self.scroll_back.saturating_sub(1),
            KeyCode::PageUp => self.scroll_back = (self.scroll_back + 10).min(lines),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            KeyCode::End => self.scroll_back = 0,
            KeyCode::Esc | KeyCode::Char('c') if self.running() => self.cancel(),
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') if !self.running() => {
                return Action::Back
            }
            _ => {}
        }
        Action::None
    }

    pub fn draw(&self, frame: &mut Frame) {
        let [body, status] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(frame.area());

        let block = Block::bordered().title(Line::from(format!(" {} ", self.command_line)).bold());
        let inner = block.inner(body);
        // Wrapped here rather than by the paragraph, so that which rows
        // are the last ones — the ones followed as output comes — is known.
        let width = (inner.width as usize).max(1);
        let mut lines: Vec<Line> = Vec::new();
        for (stream, text) in &self.log.lines {
            let style = if text.starts_with("Error") {
                Style::new().fg(Color::Red)
            } else if text.starts_with("Warning") {
                Style::new().fg(Color::Yellow)
            } else if *stream == Stream::Err {
                Style::new().fg(Color::Gray)
            } else {
                Style::new()
            };
            for row in wrap(text, width) {
                lines.push(Line::styled(row, style));
            }
        }
        let visible = inner.height as usize;
        let top = lines
            .len()
            .saturating_sub(visible)
            .saturating_sub(self.scroll_back);
        let log = Paragraph::new(lines)
            .block(block)
            .scroll((top.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(log, body);

        const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
        let (text, color, hint) = match &self.status {
            Status::Running => (
                format!("{} Running…", SPINNER[(self.ticks / 2) % SPINNER.len()]),
                Color::Cyan,
                "Esc or c cancel · ↑↓ scroll",
            ),
            Status::Finished(Some(0)) => ("Done.".to_string(), Color::Green, "Enter back"),
            Status::Finished(Some(code)) => (
                match (self.explain)(*code) {
                    Some(meaning) => format!("Failed with exit code {code}: {meaning}."),
                    None => format!("Failed with exit code {code}."),
                },
                Color::Red,
                "Enter back · ↑↓ scroll",
            ),
            Status::Finished(None) => {
                ("Stopped by a signal.".to_string(), Color::Red, "Enter back")
            }
            Status::Cancelled => ("Cancelled.".to_string(), Color::Yellow, "Enter back"),
            Status::Failed(e) => (
                format!("Could not start mauro: {e}"),
                Color::Red,
                "Enter back",
            ),
        };
        let status_line = Line::from(vec![
            Span::styled(text, Style::new().fg(color).bold()),
            Span::raw("   "),
            Span::raw(hint).dim(),
        ]);
        frame.render_widget(Paragraph::new(status_line).block(Block::bordered()), status);
    }
}

impl Drop for Job {
    /// Leaving the UI while a job runs does not leave it running unseen.
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Reads `source` on a thread of its own until it ends, sending what it
/// says, split into lines.
fn read_into(
    mut source: impl Read + Send + 'static,
    stream: Stream,
    sink: Sender<(Stream, Piece)>,
) {
    std::thread::spawn(move || {
        let mut splitter = Splitter::default();
        let mut buffer = [0u8; 4096];
        let mut pieces = Vec::new();
        loop {
            match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => splitter.feed(&buffer[..n], &mut pieces),
            }
            for piece in pieces.drain(..) {
                if sink.send((stream, piece)).is_err() {
                    return;
                }
            }
        }
        splitter.finish(&mut pieces);
        for piece in pieces {
            sink.send((stream, piece)).ok();
        }
    });
}

/// `text` cut into rows of at most `width` characters; an empty line is
/// one empty row.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// `args` as a `mauro` command line, quoted where a shell would need it.
pub fn command_line(args: &[String]) -> String {
    let mut line = String::from("mauro");
    for arg in args {
        line.push(' ');
        let plain = !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_alphanumeric() || "-_./,:=+@%".contains(c));
        if plain {
            line.push_str(arg);
        } else {
            line.push('\'');
            line.push_str(&arg.replace('\'', r"'\''"));
            line.push('\'');
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<Piece> {
        let mut splitter = Splitter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk, &mut out);
        }
        splitter.finish(&mut out);
        out
    }

    #[test]
    fn output_splits_into_lines_whatever_the_chunks() {
        assert_eq!(
            split(&[b"one\ntw", b"o\r", b"\nthree"]),
            [
                Piece::Line("one".into()),
                Piece::Line("two".into()),
                Piece::Line("three".into())
            ]
        );
    }

    #[test]
    fn a_lone_carriage_return_is_a_line_drawn_over() {
        let pieces = split(&[b"10%\r50%\r", b"100%\ndone\n"]);
        assert_eq!(
            pieces,
            [
                Piece::Redrawn("10%".into()),
                Piece::Redrawn("50%".into()),
                Piece::Line("100%".into()),
                Piece::Line("done".into())
            ]
        );
        let mut log = Log::default();
        for piece in pieces {
            log.push(Stream::Err, piece);
        }
        let texts: Vec<&str> = log.lines.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["100%", "done"]);
    }

    #[test]
    fn a_progress_bar_on_one_stream_leaves_the_other_alone() {
        let mut log = Log::default();
        log.push(Stream::Err, Piece::Redrawn("1/3".into()));
        log.push(Stream::Out, Piece::Line("said meanwhile".into()));
        log.push(Stream::Err, Piece::Line("3/3".into()));
        let texts: Vec<&str> = log.lines.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["3/3", "said meanwhile"]);
    }

    #[test]
    fn long_lines_wrap_to_the_width() {
        assert_eq!(wrap("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(wrap("", 3), [""]);
    }

    #[test]
    fn escape_sequences_are_dropped() {
        assert_eq!(
            strip_ansi("\u{1b}[1;31mError\u{1b}[0m: x\u{1b}[2K"),
            "Error: x"
        );
    }

    #[test]
    fn command_lines_quote_what_needs_it() {
        let args = ["render", "my map.omap", "-r", "3", "it's.png"].map(String::from);
        assert_eq!(
            command_line(&args),
            r"mauro render 'my map.omap' -r 3 'it'\''s.png'"
        );
    }
}
