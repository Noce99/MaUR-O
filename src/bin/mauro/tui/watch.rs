//! The screen of a job that runs on its own: what it is doing now, how its
//! loss is going, and the buttons to pause or stop it.
//!
//! ```text
//! ┌ Commands ─────────┐┌ Loss ────────────────────────────────┐
//! │ p  pause          ││ ╷   ╷   ╷                            │
//! │ s  stop           ││ │╲  │   │                            │
//! │ Esc back          ││ │ ╲╶┴───┴──── train  ·  valid        │
//! └───────────────────┘└──────────────────────────────────────┘
//! ┌ Epoch 3/10 ────────────────────────────── iteration 1234 ┐
//! │██████████████████████                          46%       │
//! └──────────────────────────────────────────────────────────┘
//! ┌ Whole run ───────────────────────────────────────────────┐
//! │██████                                          12%       │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! It owns nothing of the job: everything it shows is read from the job's
//! folder (see [`super::jobs`]) and everything it asks is written there, so
//! closing it, or the whole interface, changes nothing about the job.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, Gauge, GraphType, Paragraph, Wrap};
use ratatui::Frame;

use maur_o::net::monitor::{Control, LossPoint, State, Status, LOSSES_FILE};

use super::jobs::{Health, Job};
use super::Action;

/// The screen of one job.
pub struct Watch {
    job: Job,
    status: Option<Status>,
    health: Health,
    points: Vec<LossPoint>,
    /// How much of the losses file has been read, and the unfinished line
    /// at the end of it.
    offset: u64,
    partial: String,
    /// `s` pressed once, and waiting to be pressed again.
    confirming: bool,
    /// What the last command did, if it went wrong.
    message: Option<String>,
}

impl Watch {
    /// The screen of `job`, read once straight away.
    pub fn new(job: Job) -> Self {
        let mut watch = Self {
            health: job.health(),
            status: job.status(),
            job,
            points: Vec::new(),
            offset: 0,
            partial: String::new(),
            confirming: false,
            message: None,
        };
        watch.read_losses();
        watch
    }

    /// Takes in what the job wrote since the last tick.
    pub fn tick(&mut self) {
        self.status = self.job.status();
        self.health = self.job.health();
        self.read_losses();
    }

    fn read_losses(&mut self) {
        let Ok(mut file) = File::open(self.job.dir.join(LOSSES_FILE)) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return;
        }
        self.offset += text.len() as u64;
        self.partial.push_str(&text);
        // Only whole lines: the job may be midway through writing one.
        if let Some(end) = self.partial.rfind('\n') {
            let whole: String = self.partial.drain(..=end).collect();
            self.points
                .extend(whole.lines().filter_map(LossPoint::parse));
        }
    }

    fn running(&self) -> bool {
        matches!(self.health, Health::Alive(_) | Health::Waiting)
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let confirming = std::mem::take(&mut self.confirming);
        self.message = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => return Action::Back,
            KeyCode::Char('p') | KeyCode::Char(' ') if self.running() => {
                let paused = matches!(self.health, Health::Alive(State::Paused));
                let control = if paused { Control::Run } else { Control::Pause };
                if let Err(e) = self.job.ask(control) {
                    self.message = Some(format!("cannot reach the job: {e}"));
                }
            }
            KeyCode::Char('s') if self.running() => {
                if confirming {
                    if let Err(e) = self.job.ask(Control::Stop) {
                        self.message = Some(format!("cannot reach the job: {e}"));
                    }
                } else {
                    self.confirming = true;
                }
            }
            KeyCode::Char('x') if !self.running() => {
                if let Err(e) = self.job.forget() {
                    self.message = Some(format!("cannot forget the job: {e}"));
                } else {
                    return Action::Back;
                }
            }
            _ => {}
        }
        Action::None
    }

    pub fn draw(&self, frame: &mut Frame) {
        let [top, epoch, whole] = Layout::vertical([
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        let [commands, loss] =
            Layout::horizontal([Constraint::Length(46), Constraint::Min(20)]).areas(top);
        self.draw_commands(frame, commands);
        self.draw_loss(frame, loss);
        self.draw_bars(frame, epoch, whole);
    }

    fn state_line(&self) -> (String, Color) {
        match &self.health {
            Health::Waiting => ("Starting…".into(), Color::Cyan),
            Health::Lost => ("Lost: nothing heard from it for a while".into(), Color::Red),
            Health::Alive(State::Paused) => ("Paused".into(), Color::Yellow),
            Health::Alive(State::Stopping) => ("Stopping…".into(), Color::Yellow),
            Health::Alive(State::Starting) => ("Starting…".into(), Color::Cyan),
            Health::Alive(_) => ("Running".into(), Color::Green),
            Health::Over(State::Finished) => ("Finished".into(), Color::Green),
            Health::Over(State::Stopped) => ("Stopped".into(), Color::Yellow),
            Health::Over(_) => ("Failed".into(), Color::Red),
        }
    }

    fn draw_commands(&self, frame: &mut Frame, area: Rect) {
        let (state, color) = self.state_line();
        let mut lines = vec![
            Line::from(Span::styled(state, Style::new().fg(color).bold())),
            Line::from(self.job.command.clone()).dim(),
        ];
        if let Some(status) = &self.status {
            if let Some(run) = &status.run {
                lines.push(Line::from(format!("run: {}", run.display())).dim());
            }
            let loss = |value: Option<f64>| value.map_or("—".to_string(), |v| format!("{v:.4}"));
            lines.push(Line::from(format!(
                "loss: train {} · valid {}",
                loss(status.train_loss),
                loss(status.valid_loss)
            )));
            if let Some(message) = &status.message {
                lines.push(Line::styled(message.clone(), Style::new().fg(Color::Red)));
            }
        }
        if matches!(self.health, Health::Over(State::Failed) | Health::Lost) {
            for line in self.job.output_tail(4) {
                lines.push(Line::from(line).dim());
            }
        }
        lines.push(Line::raw(""));
        let key = |k: &'static str, what: String| {
            Line::from(vec![Span::raw(format!(" {k:<5}")).bold(), Span::raw(what)])
        };
        if self.running() {
            let paused = matches!(self.health, Health::Alive(State::Paused));
            lines.push(key("p", if paused { "resume" } else { "pause" }.into()));
            lines.push(key(
                "s",
                if self.confirming {
                    "press s again to stop; anything else cancels".into()
                } else {
                    "stop, keeping what it has".into()
                },
            ));
        } else {
            lines.push(key("x", "forget this job (the run's files stay)".into()));
        }
        lines.push(key(
            "Esc",
            "back — the job goes on without this screen".into(),
        ));
        if let Some(message) = &self.message {
            lines.push(Line::styled(message.clone(), Style::new().fg(Color::Red)));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" Commands ")),
            area,
        );
    }

    /// The training loss as at most `buckets` averaged points, the
    /// validation loss as a point per epoch, and where the epochs end.
    fn series(&self, buckets: usize) -> Series {
        let train: Vec<&LossPoint> = self.points.iter().filter(|p| !p.validation).collect();
        let per = train.len().div_ceil(buckets.max(1)).max(1);
        let train_line = train
            .chunks(per)
            .map(|chunk| {
                let n = chunk.len() as f64;
                (
                    chunk.iter().map(|p| p.iteration as f64).sum::<f64>() / n,
                    chunk.iter().map(|p| p.loss).sum::<f64>() / n,
                )
            })
            .collect();

        // One point per epoch, at the step the epoch ended on.
        let mut valid_points: Vec<(f64, f64)> = Vec::new();
        let epochs = self.points.iter().map(|p| p.epoch).max().unwrap_or(0);
        for epoch in 1..=epochs {
            let of_epoch: Vec<&LossPoint> = self
                .points
                .iter()
                .filter(|p| p.validation && p.epoch == epoch)
                .collect();
            if of_epoch.is_empty() {
                continue;
            }
            let mean = of_epoch.iter().map(|p| p.loss).sum::<f64>() / of_epoch.len() as f64;
            let at = of_epoch.iter().map(|p| p.iteration).max().unwrap_or(0);
            valid_points.push((at as f64, mean));
        }

        // The step each epoch began at: where the one before it ended.
        let mut starts: Vec<f64> = Vec::new();
        for epoch in 2..=epochs {
            if let Some(first) = train.iter().find(|p| p.epoch == epoch) {
                starts.push(first.iteration as f64);
            }
        }
        Series {
            train: train_line,
            valid: valid_points,
            epoch_ends: starts,
        }
    }

    fn draw_loss(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Loss ");
        let inner = block.inner(area);
        let series = self.series(inner.width.saturating_sub(8) as usize * 2);
        if series.train.is_empty() {
            frame.render_widget(
                Paragraph::new("No loss yet: the first steps are still being taken.")
                    .dim()
                    .block(block),
                area,
            );
            return;
        }

        let all = series.train.iter().chain(&series.valid);
        let low = all.clone().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let high = all.clone().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let pad = ((high - low) * 0.05).max(1e-6);
        let (low, high) = (low - pad, high + pad);
        let last = all.map(|p| p.0).fold(1.0, f64::max);

        // A line for every epoch where they are far enough apart to read:
        // every one, then every fifth, every tenth, and so on.
        let columns = inner.width.saturating_sub(8).max(1) as f64;
        let ends = &series.epoch_ends;
        let wanted = |every: usize| ends.len() / every.max(1);
        let every = [1, 5, 10, 25, 50, 100, 250, 500, 1000]
            .into_iter()
            .find(|&e| (wanted(e) as f64) * 4.0 <= columns)
            .unwrap_or(1000);
        let lines: Vec<[(f64, f64); 2]> = ends
            .iter()
            .enumerate()
            // `ends[0]` is where epoch 1 ended.
            .filter(|(i, _)| (i + 1) % every == 0)
            .map(|(_, &x)| [(x, low), (x, high)])
            .collect();

        let mut datasets: Vec<Dataset> = lines
            .iter()
            .map(|line| {
                Dataset::default()
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::new().fg(Color::DarkGray))
                    .data(line)
            })
            .collect();
        datasets.push(
            Dataset::default()
                .name("train")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::new().fg(Color::Cyan))
                .data(&series.train),
        );
        if !series.valid.is_empty() {
            datasets.push(
                Dataset::default()
                    .name("valid")
                    .marker(Marker::Dot)
                    .graph_type(GraphType::Line)
                    .style(Style::new().fg(Color::Yellow))
                    .data(&series.valid),
            );
        }

        let chart = Chart::new(datasets)
            .block(block)
            .x_axis(
                Axis::default()
                    .title("step")
                    .bounds([0.0, last])
                    .labels(["0".to_string(), format!("{last:.0}")]),
            )
            .y_axis(
                Axis::default()
                    .bounds([low, high])
                    .labels([format!("{low:.3}"), format!("{high:.3}")]),
            );
        frame.render_widget(chart, area);
    }

    fn draw_bars(&self, frame: &mut Frame, epoch_area: Rect, whole_area: Rect) {
        let status = self.status.clone().unwrap_or_default();
        let phase = if status.validating {
            "validating"
        } else {
            "training"
        };
        let title = if status.epochs == 0 {
            " Epoch ".to_string()
        } else {
            format!(" Epoch {}/{} · {phase} ", status.epoch, status.epochs)
        };
        let epoch_block = Block::bordered()
            .title(title)
            .title(Line::from(format!(" iteration {} ", status.iteration)).right_aligned());
        let fraction = status.epoch_fraction();
        frame.render_widget(bar(fraction, epoch_block, Color::Cyan), epoch_area);

        let whole_block = Block::bordered().title(" Whole run ");
        frame.render_widget(
            bar(status.overall_fraction(), whole_block, Color::Green),
            whole_area,
        );
    }
}

/// What the chart draws.
struct Series {
    train: Vec<(f64, f64)>,
    valid: Vec<(f64, f64)>,
    /// The step each epoch after the first began at, so the first is where
    /// epoch one ended.
    epoch_ends: Vec<f64>,
}

/// A bar `fraction` full, with the percentage written in the middle.
fn bar(fraction: f64, block: Block<'_>, color: Color) -> Gauge<'_> {
    Gauge::default()
        .block(block)
        .gauge_style(Style::new().fg(color).bg(Color::DarkGray))
        .ratio(fraction.clamp(0.0, 1.0))
        .label(format!("{:.0}%", fraction * 100.0))
        .use_unicode(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_in(dir: &std::path::Path) -> Job {
        std::fs::write(
            dir.join("job"),
            "kind=Training\ncommand=mauro net train d\n",
        )
        .unwrap();
        Job {
            id: "x".into(),
            kind: "Training".into(),
            command: "mauro net train d".into(),
            dir: dir.to_path_buf(),
        }
    }

    fn watch_with(points: &[&str]) -> Watch {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        watch.points = points.iter().filter_map(|l| LossPoint::parse(l)).collect();
        watch
    }

    #[test]
    fn it_draws_a_running_job_and_asks_it_to_pause_and_stop() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = tempfile::tempdir().unwrap();
        let status = Status {
            state: State::Running,
            epoch: 2,
            epochs: 4,
            done: 50,
            of: 100,
            iteration: 120,
            train_loss: Some(0.5),
            ..Status::default()
        };
        std::fs::write(dir.path().join("status"), status.to_text()).unwrap();
        std::fs::write(
            dir.path().join(LOSSES_FILE),
            "t 1 1 1.0\nt 1 2 0.9\nv 1 2 0.95\nt 2 3 0.8\nt 2",
        )
        .unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        // The half line at the end is waited for, not misread.
        assert_eq!(watch.points.len(), 4);

        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|frame| watch.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let shown: String = buffer.content.iter().map(|c| c.symbol()).collect();
        for wanted in [
            "Commands",
            "Loss",
            "Epoch 2/4",
            "iteration 120",
            "Whole run",
            "pause",
        ] {
            assert!(shown.contains(wanted), "{wanted} missing");
        }

        watch.handle(KeyEvent::from(KeyCode::Char('p')));
        assert_eq!(Control::read(dir.path()), Control::Pause);
        // One press asks, a second confirms.
        watch.handle(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(Control::read(dir.path()), Control::Pause);
        watch.handle(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(Control::read(dir.path()), Control::Stop);
        assert!(matches!(
            watch.handle(KeyEvent::from(KeyCode::Esc)),
            Action::Back
        ));
    }

    #[test]
    fn the_series_has_a_validation_point_and_an_end_per_epoch() {
        let watch = watch_with(&[
            "t 1 1 1.0",
            "t 1 2 0.9",
            "v 1 2 0.95",
            "t 2 3 0.8",
            "t 2 4 0.7",
            "v 2 4 0.75",
            "t 3 5 0.6",
        ]);
        let series = watch.series(100);
        assert_eq!(series.train.len(), 5);
        assert_eq!(series.valid, [(2.0, 0.95), (4.0, 0.75)]);
        assert_eq!(series.epoch_ends, [3.0, 5.0]);
    }

    #[test]
    fn a_long_run_is_averaged_into_the_buckets() {
        let lines: Vec<String> = (1..=1000).map(|i| format!("t 1 {i} 1.0")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let watch = watch_with(&refs);
        assert!(watch.series(100).train.len() <= 100);
    }
}
