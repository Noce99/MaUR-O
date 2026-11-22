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
//! Above the bars, while the job runs on a GPU, a bar of that GPU's memory
//! with its load, temperature and power in the title (see [`super::gpu`]).
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
use ratatui::widgets::{
    Axis, Block, Chart, Dataset, Gauge, GraphType, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::Frame;

use maur_o::net::monitor::{
    Control, LossPoint, State, Status, ACCURACY_FILE, LINE_IOU_FILE, LOSSES_FILE, POINT_F1_FILE,
    SCALE_ERROR_FILE,
};

use super::form::{CHANGED_COLOR, DEFAULT_COLOR};
use super::gpu::{Gpu, Probe};
use super::jobs::{Health, Job};
use super::Action;
use crate::defaults::{Parameter, Source};

/// What the chart shows. Tab goes through the ones the run has numbers for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Loss,
    Accuracy,
    /// How much of the lines the network found: only a run with a lines layer.
    LineIou,
    /// How well the network found the point symbols: only a run with a
    /// points layer.
    PointF1,
    /// How far out the network was about the density: only a run with a
    /// scale head.
    ScaleError,
}

impl View {
    /// What the chart is titled, and what it says there is none of yet.
    fn name(self) -> &'static str {
        match self {
            View::Loss => "Loss",
            View::Accuracy => "Accuracy (% of pixels right)",
            View::LineIou => "Line IoU (% of the lines found)",
            View::PointF1 => "Point F1 (% of the point symbols found, and found right)",
            View::ScaleError => "Scale error (% out about how dense the picture is)",
        }
    }

    fn short(self) -> &'static str {
        match self {
            View::Loss => "loss",
            View::Accuracy => "accuracy",
            View::LineIou => "line IoU",
            View::PointF1 => "point F1",
            View::ScaleError => "scale error",
        }
    }
}

/// The screen of one job.
pub struct Watch {
    job: Job,
    status: Option<Status>,
    health: Health,
    points: Vec<LossPoint>,
    /// The accuracy file's points (in %; the `loss` field holds the value).
    accuracy_points: Vec<LossPoint>,
    /// How much of the losses file has been read, and the unfinished line
    /// at the end of it.
    offset: u64,
    partial: String,
    /// The same for the accuracy file.
    accuracy_offset: u64,
    accuracy_partial: String,
    /// The line IoU file's points, where the run has a lines layer.
    line_points: Vec<LossPoint>,
    line_offset: u64,
    line_partial: String,
    /// The point F1 file's points, where the run has a points layer.
    point_f1_points: Vec<LossPoint>,
    point_f1_offset: u64,
    point_f1_partial: String,
    /// The scale error file's points, where the run has a scale head.
    scale_points: Vec<LossPoint>,
    scale_offset: u64,
    scale_partial: String,
    /// What the chart shows (Tab).
    view: View,
    /// `s` pressed once, and waiting to be pressed again.
    confirming: bool,
    /// What the last command did, if it went wrong.
    message: Option<String>,
    /// Asks the driver what the GPUs are doing.
    gpu: Probe,
    /// What the job was started with, where it wrote that down.
    parameters: Option<Vec<Parameter>>,
    /// How far down the parameters panel is scrolled, in rows.
    parameters_scroll: usize,
}

impl Watch {
    /// The screen of `job`, read once straight away.
    pub fn new(job: Job) -> Self {
        let parameters = job.parameters();
        let mut watch = Self {
            health: job.health(),
            status: job.status(),
            job,
            points: Vec::new(),
            accuracy_points: Vec::new(),
            offset: 0,
            partial: String::new(),
            accuracy_offset: 0,
            accuracy_partial: String::new(),
            line_points: Vec::new(),
            line_offset: 0,
            line_partial: String::new(),
            point_f1_points: Vec::new(),
            point_f1_offset: 0,
            point_f1_partial: String::new(),
            scale_points: Vec::new(),
            scale_offset: 0,
            scale_partial: String::new(),
            view: View::Loss,
            confirming: false,
            message: None,
            gpu: Probe::start(),
            parameters,
            parameters_scroll: 0,
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
        read_points(
            &self.job.dir.join(LOSSES_FILE),
            &mut self.offset,
            &mut self.partial,
            &mut self.points,
        );
        read_points(
            &self.job.dir.join(ACCURACY_FILE),
            &mut self.accuracy_offset,
            &mut self.accuracy_partial,
            &mut self.accuracy_points,
        );
        read_points(
            &self.job.dir.join(LINE_IOU_FILE),
            &mut self.line_offset,
            &mut self.line_partial,
            &mut self.line_points,
        );
        read_points(
            &self.job.dir.join(POINT_F1_FILE),
            &mut self.point_f1_offset,
            &mut self.point_f1_partial,
            &mut self.point_f1_points,
        );
        read_points(
            &self.job.dir.join(SCALE_ERROR_FILE),
            &mut self.scale_offset,
            &mut self.scale_partial,
            &mut self.scale_points,
        );
    }

    /// The numbers behind a view.
    fn points_of(&self, view: View) -> &[LossPoint] {
        match view {
            View::Loss => &self.points,
            View::Accuracy => &self.accuracy_points,
            View::LineIou => &self.line_points,
            View::PointF1 => &self.point_f1_points,
            View::ScaleError => &self.scale_points,
        }
    }

    /// The views the run has anything for, in the order Tab goes through
    /// them: the loss always, the others once their first number is in.
    fn views(&self) -> Vec<View> {
        [
            View::Loss,
            View::Accuracy,
            View::LineIou,
            View::PointF1,
            View::ScaleError,
        ]
        .into_iter()
        .filter(|&v| v == View::Loss || !self.points_of(v).is_empty())
        .collect()
    }

    /// The view after the current one.
    fn next_view(&self) -> View {
        let views = self.views();
        let at = views.iter().position(|&v| v == self.view).unwrap_or(0);
        views[(at + 1) % views.len()]
    }

    fn running(&self) -> bool {
        matches!(self.health, Health::Alive(_) | Health::Waiting)
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let confirming = std::mem::take(&mut self.confirming);
        self.message = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => return Action::Back,
            KeyCode::Tab | KeyCode::BackTab => self.view = self.next_view(),
            // The parameters panel's scroll: clamped to what there is when
            // it is drawn, where the panel's height is known.
            KeyCode::Up => self.parameters_scroll = self.parameters_scroll.saturating_sub(1),
            KeyCode::Down => self.parameters_scroll += 1,
            KeyCode::PageUp => self.parameters_scroll = self.parameters_scroll.saturating_sub(10),
            KeyCode::PageDown => self.parameters_scroll += 10,
            KeyCode::Home => self.parameters_scroll = 0,
            KeyCode::End => self.parameters_scroll = usize::MAX / 2,
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

    pub fn draw(&mut self, frame: &mut Frame) {
        let panel = self.gpu_panel();
        let gpu_height = match &panel {
            GpuPanel::Hidden => 0,
            GpuPanel::Note(_) => 3,
            GpuPanel::Gpus(rows) => 3 * rows.len() as u16,
        };
        let [top, gpu, epoch, whole] = Layout::vertical([
            Constraint::Min(8),
            Constraint::Length(gpu_height),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        let [left, loss] =
            Layout::horizontal([Constraint::Length(46), Constraint::Min(20)]).areas(top);
        // The commands as tall as they are, leaving the parameters at least
        // a few rows; the parameters the rest.
        let commands_lines = self.commands_lines();
        let wanted = commands_lines
            .iter()
            .map(|l| {
                l.width()
                    .max(1)
                    .div_ceil(left.width.saturating_sub(2).max(1) as usize)
            })
            .sum::<usize>() as u16
            + 2;
        let [commands, parameters] = Layout::vertical([
            Constraint::Length(wanted.min(left.height.saturating_sub(5))),
            Constraint::Min(0),
        ])
        .areas(left);
        frame.render_widget(
            Paragraph::new(commands_lines)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" Commands ")),
            commands,
        );
        self.draw_parameters(frame, parameters);
        self.draw_loss(frame, loss);
        draw_gpu(frame, gpu, &panel);
        self.draw_bars(frame, epoch, whole);
    }

    /// What to show of the GPUs: the ones the job holds memory on, a word
    /// where it holds none, and nothing where there is no driver to ask or
    /// the job is over.
    fn gpu_panel(&self) -> GpuPanel {
        if !self.running() {
            return GpuPanel::Hidden;
        }
        let snapshot = self.gpu.snapshot();
        if !snapshot.available {
            return GpuPanel::Hidden;
        }
        let Some(pid) = self.status.as_ref().and_then(|status| status.pid) else {
            return GpuPanel::Hidden;
        };
        let found = snapshot.of_process(pid);
        if found.is_empty() {
            return GpuPanel::Note(
                "this job holds no memory on any GPU: it runs on the CPU, or the driver does \
                 not list it as a compute process"
                    .to_string(),
            );
        }
        GpuPanel::Gpus(
            found
                .into_iter()
                .map(|(gpu, job)| GpuRow::of(gpu, job))
                .collect(),
        )
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

    /// What the job was started with, one option a row, read only; scrolled
    /// with ↑ ↓, a bar at the side where they do not all fit.
    fn draw_parameters(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Parameters · ↑↓ scroll ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some(parameters) = &self.parameters else {
            frame.render_widget(
                Paragraph::new("Not recorded: the job was started before they were.")
                    .dim()
                    .wrap(Wrap { trim: true }),
                inner,
            );
            return;
        };
        let height = inner.height as usize;
        let overflow = parameters.len().saturating_sub(height);
        self.parameters_scroll = self.parameters_scroll.min(overflow);
        let name_width = parameters
            .iter()
            .map(|p| p.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(inner.width as usize / 2);
        let lines: Vec<Line> = parameters
            .iter()
            .skip(self.parameters_scroll)
            .take(height)
            .map(|p| {
                let (value, style) = match p.source {
                    Source::CommandLine => (p.value.clone(), Style::new().fg(CHANGED_COLOR)),
                    Source::Default => (p.value.clone(), Style::new().fg(DEFAULT_COLOR)),
                    Source::Unset => ("—".to_string(), Style::new().dim()),
                };
                Line::from(vec![
                    Span::raw(format!("{:<name_width$} ", p.name)).bold(),
                    Span::styled(value, style),
                ])
            })
            .collect();
        // Room for the bar where it is wanted.
        let text = if overflow > 0 {
            Rect {
                width: inner.width.saturating_sub(1),
                ..inner
            }
        } else {
            inner
        };
        frame.render_widget(Paragraph::new(lines), text);
        if overflow > 0 {
            let mut state = ScrollbarState::new(overflow + 1).position(self.parameters_scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                inner,
                &mut state,
            );
        }
    }

    fn commands_lines(&self) -> Vec<Line<'static>> {
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
            let last = |points: &[LossPoint], validation: bool| {
                points
                    .iter()
                    .rev()
                    .find(|p| p.validation == validation)
                    .map_or("—".to_string(), |p| format!("{:.2}%", p.loss))
            };
            for view in [
                View::Accuracy,
                View::LineIou,
                View::PointF1,
                View::ScaleError,
            ] {
                let points = self.points_of(view);
                if !points.is_empty() {
                    lines.push(Line::from(format!(
                        "{}: train {} · valid {}",
                        view.short(),
                        last(points, false),
                        last(points, true)
                    )));
                }
            }
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
        lines.push(key("Tab", format!("show the {}", self.next_view().short())));
        lines.push(key(
            "Esc",
            "back — the job goes on without this screen".into(),
        ));
        lines.push(key("↑↓", "scroll the parameters".into()));
        if let Some(message) = &self.message {
            lines.push(Line::styled(message.clone(), Style::new().fg(Color::Red)));
        }
        lines
    }

    /// The training loss as at most `buckets` averaged points, the
    /// validation loss as a point per epoch, and where the epochs end.
    fn series(&self, points: &[LossPoint], buckets: usize) -> Series {
        let train: Vec<&LossPoint> = points.iter().filter(|p| !p.validation).collect();
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
        let epochs = points.iter().map(|p| p.epoch).max().unwrap_or(0);
        for epoch in 1..=epochs {
            let of_epoch: Vec<&LossPoint> = points
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
        let (name, points) = (self.view.name(), self.points_of(self.view));
        let block = Block::bordered().title(format!(" {name} · Tab switches "));
        let inner = block.inner(area);
        let series = self.series(points, inner.width.saturating_sub(8) as usize * 2);
        if series.train.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(
                    "No {} yet: the first steps are still being taken.",
                    self.view.short()
                ))
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

/// What the strip of GPU bars holds.
enum GpuPanel {
    Hidden,
    Note(String),
    Gpus(Vec<GpuRow>),
}

/// One GPU's bar: its title says how busy it is, the bar how full.
struct GpuRow {
    title: String,
    fraction: f64,
    label: String,
}

impl GpuRow {
    fn of(gpu: &Gpu, job: Option<u64>) -> GpuRow {
        let mut title = format!(
            " GPU {} · {}",
            gpu.index,
            gpu.name.trim_start_matches("NVIDIA ")
        );
        if let Some(load) = gpu.utilization {
            title.push_str(&format!(" · load {load}%"));
        }
        if let Some(degrees) = gpu.temperature {
            title.push_str(&format!(" · {degrees} °C"));
        }
        match (gpu.power, gpu.power_limit) {
            (Some(power), Some(limit)) => title.push_str(&format!(" · {power:.0}/{limit:.0} W")),
            (Some(power), None) => title.push_str(&format!(" · {power:.0} W")),
            _ => {}
        }
        if let Some(clock) = gpu.clock {
            title.push_str(&format!(" · {clock} MHz"));
        }
        title.push(' ');

        let (used, total) = (gpu.memory_used.unwrap_or(0), gpu.memory_total.unwrap_or(0));
        let fraction = if total == 0 {
            0.0
        } else {
            used as f64 / total as f64
        };
        let mut label = format!("{used} / {total} MiB ({:.0}%)", fraction * 100.0);
        if let Some(job) = job {
            label.push_str(&format!(" · this job {job} MiB"));
        }
        GpuRow {
            title,
            fraction,
            label,
        }
    }
}

/// Draws the GPU strip into `area`, three lines to each GPU.
fn draw_gpu(frame: &mut Frame, area: Rect, panel: &GpuPanel) {
    match panel {
        GpuPanel::Hidden => {}
        GpuPanel::Note(note) => frame.render_widget(
            Paragraph::new(Line::from(note.clone()).dim())
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" GPU ")),
            area,
        ),
        GpuPanel::Gpus(rows) => {
            let slots = Layout::vertical(rows.iter().map(|_| Constraint::Length(3))).split(area);
            for (row, slot) in rows.iter().zip(slots.iter()) {
                // Green while there is room, then the colours of a warning:
                // a GPU out of memory ends a run without a word of why.
                let color = match row.fraction {
                    f if f < 0.7 => Color::Green,
                    f if f < 0.9 => Color::Yellow,
                    _ => Color::Red,
                };
                let mut gauge = bar(
                    row.fraction,
                    Block::bordered().title(row.title.clone()),
                    color,
                );
                gauge = gauge.label(row.label.clone());
                frame.render_widget(gauge, *slot);
            }
        }
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
/// Reads what `path` gained since `offset` into `points`, whole lines only
/// (the job may be midway through writing one).
fn read_points(
    path: &std::path::Path,
    offset: &mut u64,
    partial: &mut String,
    points: &mut Vec<LossPoint>,
) {
    let Ok(mut file) = File::open(path) else {
        return;
    };
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return;
    }
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return;
    }
    *offset += text.len() as u64;
    partial.push_str(&text);
    if let Some(end) = partial.rfind('\n') {
        let whole: String = partial.drain(..=end).collect();
        points.extend(whole.lines().filter_map(LossPoint::parse));
    }
}

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
        // Nobody is asked what the GPUs are doing: a test is the same on a
        // machine with one and a machine without.
        watch.gpu = Probe::fixed(Default::default());
        watch.points = points.iter().filter_map(|l| LossPoint::parse(l)).collect();
        watch
    }

    /// Every row `watch` draws on a terminal `width` by `height`.
    fn rows(watch: &mut Watch, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| watch.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    #[test]
    fn the_parameters_are_listed_and_scroll_where_they_do_not_fit() {
        let dir = tempfile::tempdir().unwrap();
        let args: Vec<String> = ["net", "train", "d", "--batch-size", "16"]
            .map(str::to_string)
            .to_vec();
        let parameters = crate::defaults::parameters(&args).unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        watch.gpu = Probe::fixed(Default::default());
        watch.parameters = Some(parameters.clone());

        let shown = rows(&mut watch, 110, 30).join("\n");
        assert!(shown.contains("Parameters"), "{shown}");
        assert!(shown.contains("<dataset>"), "{shown}");
        assert!(shown.contains("--batch-size"), "{shown}");
        // Thirty options do not fit in thirty rows less the rest: scrolled to
        // the end, the last is in sight and the first is not.
        let last = &parameters.last().unwrap().name;
        assert!(!shown.contains(last.as_str()), "{last} is out of sight");
        watch.handle(KeyEvent::from(KeyCode::End));
        let shown = rows(&mut watch, 110, 30).join("\n");
        assert!(shown.contains(last.as_str()), "{shown}");
        assert!(!shown.contains("<dataset>"), "{shown}");
        assert!(watch.parameters_scroll < parameters.len(), "clamped");
        watch.handle(KeyEvent::from(KeyCode::Home));
        assert_eq!(watch.parameters_scroll, 0);

        // A job started before they were recorded says so.
        watch.parameters = None;
        let shown = rows(&mut watch, 110, 30).join("\n");
        assert!(shown.contains("Not recorded"), "{shown}");
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
        watch.gpu = Probe::fixed(Default::default());
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

    /// A running job's screen, drawn against `snapshot`, as text.
    fn shown_with(pid: Option<u32>, snapshot: crate::tui::gpu::Snapshot) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = tempfile::tempdir().unwrap();
        let status = Status {
            state: State::Running,
            epoch: 1,
            epochs: 2,
            pid,
            ..Status::default()
        };
        std::fs::write(dir.path().join("status"), status.to_text()).unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        watch.gpu = Probe::fixed(snapshot);
        let mut terminal = Terminal::new(TestBackend::new(120, 34)).unwrap();
        terminal.draw(|frame| watch.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|c| c.symbol()).collect()
    }

    fn two_gpus() -> crate::tui::gpu::Snapshot {
        use crate::tui::gpu::{parse_apps, parse_gpus, Snapshot};
        Snapshot {
            available: true,
            gpus: parse_gpus(
                "0, GPU-a, NVIDIA GeForce GTX 1650, 4, 4096, 0, 36, 6.38, 85.00, 300\n\
                 1, GPU-b, NVIDIA GeForce RTX 4060 Ti, 8190, 16380, 87, 54, 120.5, 165.00, 2310\n",
            ),
            apps: parse_apps("GPU-b, 4242, 7900\n"),
        }
    }

    #[test]
    fn the_gpu_a_job_holds_is_drawn_with_its_memory_and_the_other_is_not() {
        let shown = shown_with(Some(4242), two_gpus());
        for wanted in [
            "GPU 1",
            "RTX 4060 Ti",
            "load 87%",
            "54 °C",
            "120/165 W",
            "2310 MHz",
            "8190 / 16380 MiB",
            "this job 7900 MiB",
        ] {
            assert!(shown.contains(wanted), "{wanted} missing from:\n{shown}");
        }
        assert!(!shown.contains("GTX 1650"), "{shown}");
    }

    #[test]
    fn a_job_the_driver_does_not_list_says_so_and_no_driver_says_nothing() {
        let shown = shown_with(Some(7), two_gpus());
        assert!(shown.contains("holds no memory on any GPU"), "{shown}");
        assert!(!shown.contains("RTX 4060 Ti"), "{shown}");

        let shown = shown_with(Some(4242), Default::default());
        assert!(!shown.contains("GPU"), "{shown}");
        // A job which never said what its process is has nothing to look up.
        let shown = shown_with(None, two_gpus());
        assert!(!shown.contains("MiB"), "{shown}");
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
        let series = watch.series(&watch.points, 100);
        assert_eq!(series.train.len(), 5);
        assert_eq!(series.valid, [(2.0, 0.95), (4.0, 0.75)]);
        assert_eq!(series.epoch_ends, [3.0, 5.0]);
    }

    #[test]
    fn a_long_run_is_averaged_into_the_buckets() {
        let lines: Vec<String> = (1..=1000).map(|i| format!("t 1 {i} 1.0")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let watch = watch_with(&refs);
        assert!(watch.series(&watch.points, 100).train.len() <= 100);
    }

    /// Tab goes through the charts the run has numbers for and no others: a
    /// run without a lines layer never offers the line IoU.
    #[test]
    fn tab_goes_through_what_the_run_has_numbers_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        watch.gpu = Probe::fixed(Default::default());
        assert_eq!(watch.views(), [View::Loss]);
        watch.handle(KeyEvent::from(KeyCode::Tab));
        assert_eq!(watch.view, View::Loss, "nothing else to show");

        std::fs::write(dir.path().join(ACCURACY_FILE), "t 1 1 50.0\n").unwrap();
        std::fs::write(dir.path().join(LINE_IOU_FILE), "t 1 1 2.5\nv 1 1 3.5\n").unwrap();
        watch.tick();
        assert_eq!(watch.views(), [View::Loss, View::Accuracy, View::LineIou]);

        for expected in [View::Accuracy, View::LineIou, View::Loss] {
            watch.handle(KeyEvent::from(KeyCode::Tab));
            assert_eq!(watch.view, expected);
        }

        // A run with a scale head has its error to chart as well.
        std::fs::write(
            dir.path().join(SCALE_ERROR_FILE),
            "t 1 1 12.0
",
        )
        .unwrap();
        watch.tick();
        assert_eq!(
            watch.views(),
            [View::Loss, View::Accuracy, View::LineIou, View::ScaleError]
        );
    }

    /// The latest line IoU of the run is written in the panel beside the loss.
    #[test]
    fn the_panel_says_how_much_of_the_lines_the_run_has_found() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LINE_IOU_FILE), "t 1 1 2.5\nv 1 1 3.5\n").unwrap();
        let status = Status {
            state: State::Running,
            epoch: 1,
            epochs: 4,
            ..Status::default()
        };
        std::fs::write(dir.path().join("status"), status.to_text()).unwrap();
        let mut watch = Watch::new(job_in(dir.path()));
        watch.gpu = Probe::fixed(Default::default());
        watch.tick();

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| watch.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("line IoU: train 2.50% · valid 3.50%"),
            "{text}"
        );
    }
}
