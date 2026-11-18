//! The first screen: every tool, one line each, to pick from, and beside
//! them the jobs which run on their own.
//!
//! ← and → move between the two panels; in the jobs one, ← and → first move
//! between its tabs (one per kind of job) and leave it past the first, and ↑
//! and ↓ go down the jobs of the tab.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::Frame;

use super::form::Tool;
use super::jobs::{self, Health, Job, KINDS};
use super::screens::{
    benchmark_create::BenchmarkCreate, benchmark_run::BenchmarkRun, dem::Dem, dem_eval::DemEval,
    net_dataset::NetDataset, net_describe::NetDescribe, net_grid_to_map::NetGridToMap,
    net_image_to_map::NetImageToMap, net_show_batches::NetShowBatches, net_train::NetTrain,
    ortho::Ortho, osm::Osm, render::Render,
};
use super::Action;
use maur_o::net::monitor::State;

/// A tool on the list.
struct Entry {
    /// Its subcommand, as typed after `mauro`.
    command: &'static str,
    /// What it does, in one line.
    about: &'static str,
    /// Its screen, freshly made.
    open: fn() -> Box<dyn Tool>,
}

/// The tools in the order the list shows them.
const TOOLS: [Entry; 13] = [
    Entry {
        command: "render",
        about: "Render a map to an image (PNG, JPEG, TIFF, BMP) or to SVG",
        open: || Box::new(Render::new()),
    },
    Entry {
        command: "osm",
        about: "Make a map from OpenStreetMap data",
        open: || Box::new(Osm::new()),
    },
    Entry {
        command: "ortho",
        about: "Add a map's aerial orthophoto to it as a template",
        open: || Box::new(Ortho::new()),
    },
    Entry {
        command: "dem",
        about: "Extract an elevation GeoTIFF from a map's contours",
        open: || Box::new(Dem::new()),
    },
    Entry {
        command: "dem-eval",
        about: "Score a predicted DEM GeoTIFF against a ground-truth one",
        open: || Box::new(DemEval::new()),
    },
    Entry {
        command: "benchmark create",
        about: "Build a benchmark archive with a ground-truth renderer",
        open: || Box::new(BenchmarkCreate::new()),
    },
    Entry {
        command: "benchmark run",
        about: "Render a benchmark archive and report the differences",
        open: || Box::new(BenchmarkRun::new()),
    },
    Entry {
        command: "net dataset",
        about: "Generate a dataset of random maps with pixel labels",
        open: || Box::new(NetDataset::new()),
    },
    Entry {
        command: "net grid-to-map",
        about: "Turn a grid of labels back into a map",
        open: || Box::new(NetGridToMap::new()),
    },
    Entry {
        command: "net train",
        about: "Train a network to read a rendered map",
        open: || Box::new(NetTrain::new()),
    },
    Entry {
        command: "net image-to-map",
        about: "Read a picture of a map back into a map",
        open: || Box::new(NetImageToMap::new()),
    },
    Entry {
        command: "net show-batches",
        about: "Draw the batches a run is trained on",
        open: || Box::new(NetShowBatches::new()),
    },
    Entry {
        command: "net describe",
        about: "Write the networks' architectures into the versioned folder",
        open: || Box::new(NetDescribe::new()),
    },
];

/// Which panel the keys go to.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Tools,
    Jobs,
}

/// The Home screen.
pub struct Home {
    state: ListState,
    focus: Focus,
    /// The tab of the jobs panel, an index into [`KINDS`].
    tab: usize,
    /// Every job there is, read again every second or so.
    jobs: Vec<Job>,
    job_state: ListState,
    ticks: usize,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            state: ListState::default().with_selected(Some(0)),
            focus: Focus::Tools,
            tab: 0,
            jobs: jobs::list(),
            job_state: ListState::default(),
            ticks: 0,
        }
    }
}

impl Home {
    /// The home screen with the jobs panel in front, which is where a job
    /// screen goes back to.
    pub fn on_jobs() -> Self {
        Self {
            focus: Focus::Jobs,
            job_state: ListState::default().with_selected(Some(0)),
            ..Self::default()
        }
    }

    /// The jobs of the tab showing.
    fn shown(&self) -> Vec<&Job> {
        self.jobs
            .iter()
            .filter(|j| j.kind == KINDS[self.tab])
            .collect()
    }

    /// Reads the jobs again, and keeps the selection on the same row.
    pub fn tick(&mut self) {
        self.ticks += 1;
        if !self.ticks.is_multiple_of(10) {
            return;
        }
        self.jobs = jobs::list();
        let count = self.shown().len();
        match self.job_state.selected() {
            _ if count == 0 => self.job_state.select(None),
            Some(at) if at >= count => self.job_state.select(Some(count - 1)),
            None if self.focus == Focus::Jobs => self.job_state.select(Some(0)),
            _ => {}
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        match self.focus {
            Focus::Tools => self.handle_tools(key),
            Focus::Jobs => self.handle_jobs(key),
        }
    }

    fn handle_tools(&mut self, key: KeyEvent) -> Action {
        let last = TOOLS.len() - 1;
        let at = self.state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.state.select(Some(at.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => self.state.select(Some((at + 1).min(last))),
            KeyCode::Home => self.state.select(Some(0)),
            KeyCode::End => self.state.select(Some(last)),
            KeyCode::Enter => return Action::Open((TOOLS[at].open)()),
            KeyCode::Right | KeyCode::Tab => {
                self.focus = Focus::Jobs;
                if self.job_state.selected().is_none() && !self.shown().is_empty() {
                    self.job_state.select(Some(0));
                }
            }
            _ => {}
        }
        Action::None
    }

    fn handle_jobs(&mut self, key: KeyEvent) -> Action {
        let count = self.shown().len();
        let at = self.job_state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc | KeyCode::BackTab => self.focus = Focus::Tools,
            KeyCode::Left | KeyCode::Char('h') => {
                if self.tab == 0 {
                    self.focus = Focus::Tools;
                } else {
                    self.tab -= 1;
                    self.job_state
                        .select((!self.shown().is_empty()).then_some(0));
                }
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab if self.tab + 1 < KINDS.len() => {
                self.tab += 1;
                self.job_state
                    .select((!self.shown().is_empty()).then_some(0));
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.job_state.select(Some(at.saturating_sub(1)))
            }
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.job_state.select(Some((at + 1).min(count - 1)))
            }
            KeyCode::Enter => {
                if let Some(job) = self.shown().get(at) {
                    return Action::Watch((*job).clone());
                }
            }
            _ => {}
        }
        Action::None
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let [head, body, foot] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .areas(frame.area());

        let title = Line::from(vec![
            Span::raw("MaUR-O").bold(),
            Span::raw(" — Map Utils in Rust for Orienteering  "),
            Span::raw(format!("v{}", env!("CARGO_PKG_VERSION"))).dim(),
        ]);
        frame.render_widget(Paragraph::new(title).block(Block::bordered()), head);

        let [tools_area, jobs_area] =
            Layout::horizontal([Constraint::Min(40), Constraint::Percentage(34)]).areas(body);

        let width = TOOLS.iter().map(|t| t.command.len()).max().unwrap_or(0);
        let items: Vec<ListItem> = TOOLS
            .iter()
            .map(|tool| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!(" {:width$}  ", tool.command)).bold(),
                    Span::raw(tool.about),
                ]))
            })
            .collect();
        let focused = |on: bool| {
            if on {
                Style::new().fg(Color::Cyan)
            } else {
                Style::new()
            }
        };
        let list = List::new(items)
            .block(
                Block::bordered()
                    .title(" Tools ")
                    .border_style(focused(self.focus == Focus::Tools)),
            )
            .highlight_style(if self.focus == Focus::Tools {
                Style::new().reversed()
            } else {
                Style::new().bold()
            })
            .highlight_symbol("▶");
        frame.render_stateful_widget(list, tools_area, &mut self.state);

        self.draw_jobs(frame, jobs_area);

        let footer = Line::from(match self.focus {
            Focus::Tools => "↑↓ choose · Enter open · → jobs · q quit",
            Focus::Jobs => "↑↓ choose · Enter watch · ← tools · q quit",
        })
        .dim();
        frame.render_widget(Paragraph::new(footer).block(Block::bordered()), foot);
    }

    fn draw_jobs(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let on = self.focus == Focus::Jobs;
        let border = if on {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        };
        let block = Block::bordered().title(" Jobs ").border_style(border);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [tabs, list] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        frame.render_widget(
            Tabs::new(KINDS.iter().map(|k| format!("{k}s")).collect::<Vec<_>>())
                .select(self.tab)
                .highlight_style(Style::new().bold().underlined())
                .divider("│"),
            tabs,
        );

        let shown = self.shown();
        if shown.is_empty() {
            frame.render_widget(
                Paragraph::new("None yet. Start a training and it shows up here.").dim(),
                list,
            );
            return;
        }
        let items: Vec<ListItem> = shown.iter().map(|job| job_row(job)).collect();
        let list_widget = List::new(items)
            .highlight_style(if on {
                Style::new().reversed()
            } else {
                Style::new().bold()
            })
            .highlight_symbol("▶");
        frame.render_stateful_widget(list_widget, list, &mut self.job_state);
    }
}

/// A job as one row of the list: a mark for how it is, its name, how far it is.
fn job_row(job: &Job) -> ListItem<'static> {
    let status = job.status();
    let (mark, color, word) = match job.health() {
        Health::Waiting => ("◌", Color::Cyan, "starting"),
        Health::Lost => ("✗", Color::Red, "lost"),
        Health::Alive(State::Paused) => ("‖", Color::Yellow, "paused"),
        Health::Alive(State::Stopping) => ("■", Color::Yellow, "stopping"),
        Health::Alive(_) => ("●", Color::Green, "running"),
        Health::Over(State::Finished) => ("✓", Color::Green, "done"),
        Health::Over(State::Stopped) => ("■", Color::Yellow, "stopped"),
        Health::Over(_) => ("✗", Color::Red, "failed"),
    };
    let progress = status.map_or(String::new(), |s| {
        if s.epochs > 0 {
            format!(
                " {}/{} {:.0}%",
                s.epoch,
                s.epochs,
                s.overall_fraction() * 100.0
            )
        } else {
            String::new()
        }
    });
    ListItem::new(Line::from(vec![
        Span::styled(format!(" {mark} "), Style::new().fg(color)),
        Span::raw(job.title()),
        Span::styled(format!("  {word}{progress}"), Style::new().dim()),
    ]))
}
