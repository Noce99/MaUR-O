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
//! The screens: [`home::Home`] lists the tools, [`render_form::RenderForm`]
//! is `mauro render`'s, and the job screen shows a running command's output
//! as it comes.

mod home;
mod job;
mod render_form;
mod widgets;

use std::io;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{DefaultTerminal, Frame};

use home::Home;
use job::{Job, JobSpec};
use render_form::RenderForm;

/// What a key on a screen asks of the interface as a whole.
pub enum Action {
    None,
    Quit,
    /// Back to where this screen was opened from.
    Back,
    OpenRender,
    /// Run `mauro` with these arguments, on the job screen.
    Run(JobSpec),
}

/// The screen being shown.
enum Screen {
    Home(Home),
    Render(Box<RenderForm>),
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
            Screen::Render(form) => form.handle(key),
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
            Action::OpenRender => {
                let here = std::env::current_dir().unwrap_or_else(|_| ".".into());
                self.screen = Screen::Render(Box::new(RenderForm::new(&here)));
            }
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
            Screen::Render(form) => form.tick(),
            Screen::Job { job, .. } => job.tick(),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        match &mut self.screen {
            Screen::Home(home) => home.draw(frame),
            Screen::Render(form) => form.draw(frame),
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

    /// Everything `app` shows, as text.
    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn home_lists_every_tool_and_opens_render() {
        let mut app = App::new();
        let shown = screen(&mut app);
        for tool in [
            "render",
            "osm",
            "ortho",
            "dem-eval",
            "benchmark create",
            "benchmark run",
        ] {
            assert!(shown.contains(tool), "{tool} missing from:\n{shown}");
        }
        app.handle(key(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::Render(_)));
        assert!(screen(&mut app).contains("Pick a map"));
    }

    #[test]
    fn a_tool_without_a_screen_says_so() {
        let mut app = App::new();
        app.handle(key(KeyCode::Down));
        app.handle(key(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::Home(_)));
        assert!(screen(&mut app).contains("mauro osm --help"));
    }

    #[test]
    fn esc_goes_back_and_q_quits() {
        let mut app = App::new();
        app.handle(key(KeyCode::Enter));
        app.handle(key(KeyCode::Esc)); // closes the picker
        assert!(matches!(app.screen, Screen::Render(_)));
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
