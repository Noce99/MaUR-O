//! A progress bar for the long stages of a benchmark run, and the little bit
//! of colour the stage headings use.
//!
//! Modelled on `tqdm`: a bar, a count, how long it has taken and how long it
//! looks like taking. A suite is a couple of hundred maps and a few minutes,
//! which is exactly long enough to want to know whether to wait for it.
//!
//! Everything here degrades rather than breaks when the output is not a
//! terminal: the bar is drawn on standard error only when that is a
//! terminal, and is replaced by a single summary line when it is not, so
//! that a redirected log holds one line per stage instead of a few thousand
//! carriage returns. Colour is decided separately, on standard output.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

/// The eighths of a block, for the partly filled cell at the end of the bar.
const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// How wide the bar itself is drawn, in cells.
const WIDTH: usize = 32;

/// How often the bar is redrawn at most. Redrawing it for every one of a few
/// thousand quick steps costs more than the steps do.
const REDRAW: Duration = Duration::from_millis(60);

/// Whether the stage headings should be coloured.
pub fn colour() -> bool {
    // Both the usual opt-outs; `NO_COLOR` is the conventional one, and
    // `TERM=dumb` is what a terminal without escape sequences reports.
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |term| term != "dumb")
}

/// Prints a numbered stage heading, in green where colour is wanted.
pub fn stage(number: usize, of: usize, title: &str) {
    if colour() {
        println!("\x1b[1;32m==> [{number}/{of}] {title}\x1b[0m");
    } else {
        println!("==> [{number}/{of}] {title}");
    }
}

/// `seconds` as `mm:ss`, or `h:mm:ss` once there is an hour of it.
fn clock(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "??:??".to_string();
    }
    let seconds = seconds as u64;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// A progress bar over a known number of steps.
///
/// Call [`tick`](Self::tick) once per step and [`finish`](Self::finish) at the
/// end. Drops to a single summary line where standard error is not a
/// terminal.
pub struct Progress {
    label: String,
    total: usize,
    current: usize,
    start: Instant,
    drawn: Instant,
    /// The count the bar on screen is showing, so that finishing an
    /// already-complete bar does not redraw the same frame.
    shown: Option<usize>,
    /// Whether the bar is drawn at all, i.e. whether stderr is a terminal.
    live: bool,
}

impl Progress {
    /// A bar named `label`, over `total` steps.
    pub fn new(label: &str, total: usize) -> Self {
        let mut progress = Progress {
            label: label.to_string(),
            total,
            current: 0,
            start: Instant::now(),
            drawn: Instant::now(),
            shown: None,
            live: std::io::stderr().is_terminal(),
        };
        progress.draw();
        progress
    }

    /// Counts one step done, redrawing the bar if it is time to.
    pub fn tick(&mut self) {
        self.current += 1;
        if self.current >= self.total || self.drawn.elapsed() >= REDRAW {
            self.draw();
        }
    }

    fn draw(&mut self) {
        if !self.live {
            return;
        }
        self.drawn = Instant::now();
        self.shown = Some(self.current);

        let fraction = if self.total > 0 {
            self.current as f64 / self.total as f64
        } else {
            1.0
        };
        let eighths = (fraction * (WIDTH * 8) as f64).round() as usize;
        let (full, part) = (eighths / 8, eighths % 8);
        let bar = format!(
            "{}{}{}",
            "█".repeat(full.min(WIDTH)),
            if full < WIDTH { PARTIAL[part] } else { "" },
            " ".repeat(WIDTH.saturating_sub(full + usize::from(part > 0)))
        );

        let elapsed = self.start.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            self.current as f64 / elapsed
        } else {
            0.0
        };
        let remaining = if rate > 0.0 {
            (self.total - self.current) as f64 / rate
        } else {
            f64::NAN
        };
        let speed = if rate >= 1.0 || rate == 0.0 {
            format!("{rate:.2}it/s")
        } else {
            format!("{:.2}s/it", 1.0 / rate)
        };

        // \x1b[K clears whatever the previous, possibly longer, line left.
        eprint!(
            "\r{}: {:>3}%|{bar}| {}/{} [{}<{}, {speed}]\x1b[K",
            self.label,
            (fraction * 100.0).round() as u64,
            self.current,
            self.total,
            clock(elapsed),
            clock(remaining),
        );
        std::io::stderr().flush().ok();
    }

    /// Leaves the finished bar on screen, or, where there is none, a single
    /// line saying what it would have said.
    pub fn finish(mut self) {
        if self.live {
            if self.shown != Some(self.current) {
                self.draw();
            }
            eprintln!();
        } else {
            eprintln!(
                "{}: {}/{} in {}",
                self.label,
                self.current,
                self.total,
                clock(self.start.elapsed().as_secs_f64())
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_grows_a_field_only_when_it_needs_one() {
        assert_eq!(clock(0.0), "00:00");
        assert_eq!(clock(9.7), "00:09");
        assert_eq!(clock(75.0), "01:15");
        assert_eq!(clock(3600.0), "1:00:00");
        assert_eq!(clock(f64::NAN), "??:??");
    }
}
