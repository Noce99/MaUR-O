//! The first screen: every tool, one line each, to pick from.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::form::Tool;
use super::screens::{
    benchmark_create::BenchmarkCreate, benchmark_run::BenchmarkRun, dem::Dem, dem_eval::DemEval,
    ortho::Ortho, osm::Osm, render::Render, tiles_build::TilesBuild, tiles_query::TilesQuery,
    tiles_serve::TilesServe,
};
use super::Action;

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
const TOOLS: [Entry; 10] = [
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
        command: "tiles build",
        about: "Put maps into a MOT archive, the tiled world map",
        open: || Box::new(TilesBuild::new()),
    },
    Entry {
        command: "tiles query",
        about: "List what a MOT archive holds around a place",
        open: || Box::new(TilesQuery::new()),
    },
    Entry {
        command: "tiles serve",
        about: "Look at MOT archives as a map in the browser",
        open: || Box::new(TilesServe::new()),
    },
];

/// The Home screen.
pub struct Home {
    state: ListState,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            state: ListState::default().with_selected(Some(0)),
        }
    }
}

impl Home {
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        let last = TOOLS.len() - 1;
        let at = self.state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.state.select(Some(at.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => self.state.select(Some((at + 1).min(last))),
            KeyCode::Home => self.state.select(Some(0)),
            KeyCode::End => self.state.select(Some(last)),
            KeyCode::Enter | KeyCode::Right => return Action::Open((TOOLS[at].open)()),
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
        let list = List::new(items)
            .block(Block::bordered().title(" Tools "))
            .highlight_style(Style::new().reversed())
            .highlight_symbol("▶");
        frame.render_stateful_widget(list, body, &mut self.state);

        let footer = Line::from("↑↓ choose · Enter open · q quit").dim();
        frame.render_widget(Paragraph::new(footer).block(Block::bordered()), foot);
    }
}
