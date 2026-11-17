//! The terminal user interface `mauro` opens when it is given nothing to do.
//!
//! Every tool has a screen of its own, a form shaped to it, but no screen
//! runs anything itself: each builds the command line it stands for, checks
//! it with the very parser the shell's `mauro` uses ([`check`]), and hands
//! it to a [`job::Job`], which runs `mauro` with it as a child process. So
//! the interface can never do something the command line cannot, and a
//! screen whose command line `mauro` no longer accepts fails its tests
//! rather than its user.
//!
//! The screens: [`home::Home`] lists the tools; [`screens`] has one per
//! tool, each a [`form::Tool`] on the shared [`form::Form`]; and the job
//! screen shows a running command's output as it comes.

mod form;
mod home;
mod job;
mod screens;
mod widgets;

use std::io;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{DefaultTerminal, Frame};

use form::Tool;
use home::Home;
use job::{Job, JobSpec};

/// What a key on a screen asks of the interface as a whole.
pub enum Action {
    None,
    Quit,
    /// Back to where this screen was opened from.
    Back,
    /// Show this tool's screen.
    Open(Box<dyn Tool>),
    /// Run `mauro` with these arguments, on the job screen.
    Run(JobSpec),
}

/// The screen being shown.
enum Screen {
    Home(Home),
    Tool(Box<dyn Tool>),
    /// A job, and the screen to go back to when it is done.
    Job {
        job: Box<Job>,
        back: Box<Screen>,
    },
}

/// The interface: one screen at a time.
struct App {
    screen: Screen,
    quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            screen: Screen::Home(Home::default()),
            quit: false,
        }
    }

    fn handle(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        let action = match &mut self.screen {
            Screen::Home(home) => home.handle(key),
            Screen::Tool(tool) => form::handle(tool.as_mut(), key),
            Screen::Job { job, .. } => job.handle(key),
        };
        match action {
            Action::None => {}
            Action::Quit => self.quit = true,
            Action::Back => {
                let left = std::mem::replace(&mut self.screen, Screen::Home(Home::default()));
                if let Screen::Job { back, .. } = left {
                    self.screen = *back;
                }
            }
            Action::Open(tool) => self.screen = Screen::Tool(tool),
            Action::Run(spec) => {
                let back = std::mem::replace(&mut self.screen, Screen::Home(Home::default()));
                self.screen = Screen::Job {
                    job: Box::new(Job::start(spec)),
                    back: Box::new(back),
                };
            }
        }
    }

    /// What has to happen between keys: output to show, a map read.
    fn tick(&mut self) {
        match &mut self.screen {
            Screen::Home(_) => {}
            Screen::Tool(tool) => tool.tick(),
            Screen::Job { job, .. } => job.tick(),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        match &mut self.screen {
            Screen::Home(home) => home.draw(frame),
            Screen::Tool(tool) => form::draw(tool.as_mut(), frame),
            Screen::Job { job, .. } => job.draw(frame),
        }
    }
}

/// Whether `mauro` takes `args` (the arguments after `mauro`); clap's own
/// complaint where it does not.
pub fn check(args: &[String]) -> Result<(), String> {
    crate::Cli::try_parse_from(std::iter::once("mauro").chain(args.iter().map(String::as_str)))
        .map(|_| ())
        .map_err(|e| {
            let text = e.to_string();
            let first = text.lines().next().unwrap_or_default();
            first.strip_prefix("error: ").unwrap_or(first).to_string()
        })
}

/// Runs the interface until it is quit.
pub fn run() -> ExitCode {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(e) => {
            eprintln!("Error: cannot start the terminal interface: {e}");
            return ExitCode::from(1);
        }
    };
    let result = event_loop(&mut terminal);
    ratatui::restore();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

fn event_loop(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::new();
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Windows reports releases too; a key counts once.
                if key.kind == KeyEventKind::Press {
                    app.handle(key);
                }
            }
        }
        app.tick();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// Everything `app` shows, as text, on a terminal `width` by `height`.
    fn screen_sized(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn screen(app: &mut App) -> String {
        screen_sized(app, 100, 30)
    }

    #[test]
    fn every_tool_opens_and_draws_its_screen() {
        let titles = [
            "mauro render",
            "mauro osm",
            "mauro ortho",
            "mauro dem",
            "mauro dem-eval",
            "mauro benchmark create",
            "mauro benchmark run",
            "mauro net dataset",
            "mauro net grid-to-map",
            "mauro net train",
            "mauro net image-to-map",
            "mauro net show-batches",
        ];
        for (at, title) in titles.iter().enumerate() {
            let mut app = App::new();
            let shown = screen(&mut app);
            let command = title.trim_start_matches("mauro ");
            assert!(shown.contains(command), "{command} missing from:\n{shown}");
            for _ in 0..at {
                app.handle(key(KeyCode::Down));
            }
            app.handle(key(KeyCode::Enter));
            assert!(matches!(app.screen, Screen::Tool(_)), "{title}");
            // Past the picker, where the screen opened on one.
            if screen(&mut app).contains("Pick: ") {
                app.handle(key(KeyCode::Esc));
            }
            let shown = screen(&mut app);
            assert!(shown.contains(title), "{title} missing from:\n{shown}");
            // Every field, and the button, can be walked through and drawn,
            // on a cramped terminal too.
            for _ in 0..40 {
                app.handle(key(KeyCode::Down));
                screen_sized(&mut app, 50, 14);
            }
            assert!(screen(&mut app).contains("[ "), "{title}: no run button");
        }
    }

    #[test]
    fn esc_goes_back_and_q_quits() {
        let mut app = App::new();
        app.handle(key(KeyCode::Enter));
        assert!(screen(&mut app).contains("Pick: Map"));
        app.handle(key(KeyCode::Esc)); // closes the picker
        assert!(matches!(app.screen, Screen::Tool(_)));
        assert!(screen(&mut app).contains("[ Render ]"));
        app.handle(key(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::Home(_)));
        app.handle(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn check_uses_clap_and_keeps_its_complaint_short() {
        assert!(check(&["render".into(), "map.omap".into()]).is_ok());
        let complaint = check(&["render".into(), "--no-such-flag".into()]).unwrap_err();
        assert!(complaint.contains("--no-such-flag"), "{complaint}");
        assert!(!complaint.contains('\n'));
    }
}
