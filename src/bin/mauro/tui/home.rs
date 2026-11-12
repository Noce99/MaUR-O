//! The first screen: every tool, one line each, to pick from.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::Action;

/// A tool on the list.
struct Tool {
    /// Its subcommand, as typed after `mauro`.
    command: &'static str,
    /// What it does, in one line.
    about: &'static str,
    /// What picking it does; `None` for a tool with no screen yet.
    open: Option<Action>,
}

/// The tools in the order the list shows them.
fn tools() -> [Tool; 7] {
    [
        Tool {
            command: "render",
            about: "Render a map to an image (PNG, JPEG, TIFF, BMP) or to SVG",
            open: Some(Action::OpenRender),
        },
        Tool {
            command: "osm",
            about: "Make a map from OpenStreetMap data",
            open: None,
        },
        Tool {
            command: "ortho",
            about: "Add a map's aerial orthophoto to it as a template",
            open: None,
        },
        Tool {
            command: "dem",
            about: "Extract an elevation GeoTIFF from a map's contours",
            open: None,
        },
        Tool {
            command: "dem-eval",
            about: "Score a predicted DEM GeoTIFF against a ground-truth one",
            open: None,
        },
        Tool {
            command: "benchmark create",
            about: "Build a benchmark archive with a ground-truth renderer",
            open: None,
        },
        Tool {
            command: "benchmark run",
            about: "Render a benchmark archive and report the differences",
            open: None,
        },
    ]
}

/// The Home screen.
pub struct Home {
    state: ListState,
    /// Said in the footer until the next key.
    message: Option<String>,
}

impl Default for Home {
    fn default() -> Self {
        Self {
            state: ListState::default().with_selected(Some(0)),
            message: None,
        }
    }
}

impl Home {
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        self.message = None;
        let count = tools().len();
        let at = self.state.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.state.select(Some(at.saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') => self.state.select(Some((at + 1).min(count - 1))),
            KeyCode::Home => self.state.select(Some(0)),
            KeyCode::End => self.state.select(Some(count - 1)),
            KeyCode::Enter | KeyCode::Right => {
                let tool = tools()
                    .into_iter()
                    .nth(at)
                    .expect("selection is in the list");
                match tool.open {
                    Some(action) => return action,
                    None => {
                        self.message = Some(format!(
                            "No screen for {} yet: run `mauro {} --help` from the shell.",
                            tool.command, tool.command
                        ))
                    }
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

        let width = tools().iter().map(|t| t.command.len()).max().unwrap_or(0);
        let items: Vec<ListItem> = tools()
            .iter()
            .map(|tool| {
                let line = Line::from(vec![
                    Span::raw(format!(" {:width$}  ", tool.command)).bold(),
                    Span::raw(tool.about),
                ]);
                if tool.open.is_some() {
                    ListItem::new(line)
                } else {
                    ListItem::new(line).dim()
                }
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title(" Tools "))
            .highlight_style(Style::new().reversed())
            .highlight_symbol("▶");
        frame.render_stateful_widget(list, body, &mut self.state);

        let footer = match &self.message {
            Some(message) => Line::from(message.as_str()).yellow(),
            None => Line::from("↑↓ choose · Enter open · q quit").dim(),
        };
        frame.render_widget(Paragraph::new(footer).block(Block::bordered()), foot);
    }
}
