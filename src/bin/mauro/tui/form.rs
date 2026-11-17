//! The form every tool's screen is built on, and the screen around it.
//!
//! A [`Form`] is a column of [`Field`]s — paths with a picker behind them,
//! text, choices, toggles — and a run button. A field can name the clap
//! argument it gives (`Field::arg`): its help text and its default then
//! come from `mauro`'s own parser, not from a second copy here that would
//! drift from it.
//!
//! A screen is a [`Tool`]: a form, the command line it stands for, and
//! whatever it does when a field changes. [`handle`] and [`draw`] are the
//! same for all of them.

use clap::CommandFactory;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use super::job::{command_line, JobSpec};
use super::widgets::{relative, visible, FilePicker, Pick, Picked, TextInput};
use super::Action;

/// What a field holds.
#[derive(Clone, Debug)]
pub enum Kind {
    /// Not a field: a title over the ones after it.
    Heading,
    /// A path, typed, or picked with Enter.
    Path(Pick),
    /// Several paths, separated by `;`: Enter picks one more.
    Paths(Pick),
    /// Text: a number, a name, a list.
    Text,
    /// One of these, changed with ← →.
    Choice(Vec<String>),
    /// On or off, changed with Space or Enter.
    Toggle,
}

/// One row of a [`Form`].
pub struct Field {
    label: &'static str,
    kind: Kind,
    input: TextInput,
    choice: usize,
    on: bool,
    /// Said after the value, dimmed: the unit it is in.
    unit: &'static str,
    /// Shown, dimmed, in place of an empty value.
    hint: &'static str,
    /// The clap argument this field gives (its id), for help and default.
    arg: Option<&'static str>,
    help: String,
    default: Option<String>,
    shown: bool,
}

impl Field {
    fn new(label: &'static str, kind: Kind) -> Self {
        Self {
            label,
            kind,
            input: TextInput::default(),
            choice: 0,
            on: false,
            unit: "",
            hint: "",
            arg: None,
            help: String::new(),
            default: None,
            shown: true,
        }
    }

    pub fn heading(label: &'static str) -> Self {
        Self::new(label, Kind::Heading)
    }

    pub fn path(label: &'static str, pick: Pick) -> Self {
        Self::new(label, Kind::Path(pick))
    }

    pub fn paths(label: &'static str, pick: Pick) -> Self {
        Self::new(label, Kind::Paths(pick))
    }

    pub fn text(label: &'static str) -> Self {
        Self::new(label, Kind::Text)
    }

    pub fn choice(label: &'static str, choices: &[&str]) -> Self {
        Self::new(
            label,
            Kind::Choice(choices.iter().map(|c| c.to_string()).collect()),
        )
    }

    pub fn toggle(label: &'static str) -> Self {
        Self::new(label, Kind::Toggle)
    }

    /// The field gives the clap argument `id`: its help and default are
    /// that argument's.
    pub fn arg(mut self, id: &'static str) -> Self {
        self.arg = Some(id);
        self
    }

    pub fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    pub fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }

    /// Help of the field's own, where it gives no argument (or more than
    /// its argument's help says).
    pub fn help(mut self, help: &str) -> Self {
        self.help = help.to_string();
        self
    }

    fn focusable(&self) -> bool {
        self.shown && !matches!(self.kind, Kind::Heading)
    }
}

/// What a key did to a [`Form`].
#[derive(Debug, PartialEq)]
pub enum Event {
    None,
    /// The field at this index changed.
    Changed(usize),
    Run,
    Back,
}

/// A column of fields, and a button to run what they say.
pub struct Form {
    title: String,
    run_label: &'static str,
    fields: Vec<Field>,
    /// The focused field's index; `fields.len()` is the run button.
    focus: usize,
    picker: Option<(usize, FilePicker)>,
    /// Why the last attempt to run did not, until the next key.
    pub message: Option<String>,
}

impl Form {
    /// A form for `mauro <command…>`: the fields' clap arguments are looked
    /// up in that subcommand, for their help and their defaults.
    pub fn new(command: &[&str], run_label: &'static str, mut fields: Vec<Field>) -> Self {
        let mut clap = crate::Cli::command();
        for name in command {
            clap = clap
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("mauro has no subcommand {name}"))
                .clone();
        }
        for field in &mut fields {
            let Some(id) = field.arg else { continue };
            let arg = clap
                .get_arguments()
                .find(|a| a.get_id() == id)
                .unwrap_or_else(|| panic!("mauro {} has no argument {id}", command.join(" ")));
            if field.help.is_empty() {
                if let Some(help) = arg.get_long_help().or(arg.get_help()) {
                    field.help = help.to_string();
                }
            }
            field.default = arg
                .get_default_values()
                .first()
                .map(|v| v.to_string_lossy().into_owned());
            if let Some(default) = &field.default {
                match &field.kind {
                    Kind::Choice(choices) => {
                        if let Some(at) = choices.iter().position(|c| c == default) {
                            field.choice = at;
                        }
                    }
                    Kind::Text | Kind::Path(_) if field.input.text().is_empty() => {
                        field.input.set(default.clone());
                    }
                    _ => {}
                }
            }
        }
        let mut form = Self {
            title: format!("mauro {}", command.join(" ")),
            run_label,
            fields,
            focus: 0,
            picker: None,
            message: None,
        };
        form.focus = (0..form.fields.len())
            .find(|&i| form.fields[i].focusable())
            .unwrap_or(form.fields.len());
        form
    }

    /// What field `i` holds, trimmed.
    pub fn text(&self, i: usize) -> &str {
        self.fields[i].input.text().trim()
    }

    pub fn set_text(&mut self, i: usize, text: impl Into<String>) {
        self.fields[i].input.set(text);
    }

    /// Whether field `i` holds its argument's default (or, without one,
    /// nothing): not worth writing on the command line.
    pub fn is_default(&self, i: usize) -> bool {
        let text = self.text(i);
        text.is_empty() || self.fields[i].default.as_deref() == Some(text)
    }

    /// The choice field `i` is on.
    pub fn choice(&self, i: usize) -> &str {
        match &self.fields[i].kind {
            Kind::Choice(choices) => choices
                .get(self.fields[i].choice)
                .map_or("", String::as_str),
            _ => panic!("field {i} is not a choice"),
        }
    }

    /// Gives choice field `i` new choices, keeping the one it is on where
    /// it is still among them.
    pub fn set_choices(&mut self, i: usize, choices: Vec<String>) {
        let current = self.choice(i).to_string();
        let field = &mut self.fields[i];
        field.choice = choices.iter().position(|c| *c == current).unwrap_or(0);
        field.kind = Kind::Choice(choices);
    }

    /// Whether toggle `i` is on.
    pub fn on(&self, i: usize) -> bool {
        self.fields[i].on
    }

    /// Shows or hides field `i`; a hidden field is skipped, and gives
    /// nothing to the command line.
    pub fn show(&mut self, i: usize, shown: bool) {
        self.fields[i].shown = shown;
        if !self.is_focusable(self.focus) {
            self.move_focus(false);
            if !self.is_focusable(self.focus) {
                self.move_focus(true);
            }
        }
    }

    pub fn shown(&self, i: usize) -> bool {
        self.fields[i].shown
    }

    /// Opens field `i`'s picker, where it is a path field.
    pub fn open_picker(&mut self, i: usize) {
        let pick = match self.fields[i].kind {
            Kind::Path(pick) | Kind::Paths(pick) => pick,
            _ => return,
        };
        let last = self
            .text(i)
            .rsplit(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let start = if last.is_empty() || !std::path::Path::new(&last).exists() {
            std::path::PathBuf::from(".")
        } else {
            std::path::PathBuf::from(last)
        };
        self.picker = Some((i, FilePicker::new(&start, pick)));
    }

    /// Adds `flag value` to `args` where field `i` is shown and holds
    /// something other than its default.
    pub fn push_option(&self, args: &mut Vec<String>, i: usize, flag: &str) {
        if self.shown(i) && !self.is_default(i) {
            args.extend([flag.to_string(), self.text(i).to_string()]);
        }
    }

    /// Adds `flag` to `args` where toggle `i` is shown and on.
    pub fn push_flag(&self, args: &mut Vec<String>, i: usize, flag: &str) {
        if self.shown(i) && self.on(i) {
            args.push(flag.to_string());
        }
    }

    /// Field `i`'s value, or `None` where it is hidden or empty.
    pub fn given(&self, i: usize) -> Option<&str> {
        (self.shown(i) && !self.text(i).is_empty()).then(|| self.text(i))
    }

    fn is_focusable(&self, i: usize) -> bool {
        i == self.fields.len() || self.fields.get(i).is_some_and(Field::focusable)
    }

    fn move_focus(&mut self, forward: bool) {
        let count = self.fields.len() + 1;
        let mut at = self.focus;
        for _ in 0..count {
            at = if forward {
                (at + 1).min(count - 1)
            } else {
                at.saturating_sub(1)
            };
            if self.is_focusable(at) {
                self.focus = at;
                return;
            }
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Event {
        self.message = None;
        if let Some((i, picker)) = &mut self.picker {
            let i = *i;
            match picker.handle(key) {
                Picked::Not => {}
                Picked::Cancelled => self.picker = None,
                Picked::File(path) => {
                    self.picker = None;
                    let path = relative(&path).display().to_string();
                    let field = &mut self.fields[i];
                    if matches!(field.kind, Kind::Paths(_)) && !field.input.text().trim().is_empty()
                    {
                        let joined = format!("{}; {path}", field.input.text().trim());
                        field.input.set(joined);
                    } else {
                        field.input.set(path);
                    }
                    return Event::Changed(i);
                }
            }
            return Event::None;
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Event::Run;
        }
        if key.code == KeyCode::Esc {
            return Event::Back;
        }
        if self.focus == self.fields.len() {
            return match key.code {
                KeyCode::Enter => Event::Run,
                KeyCode::Up | KeyCode::BackTab => {
                    self.move_focus(false);
                    Event::None
                }
                _ => Event::None,
            };
        }
        let i = self.focus;
        match (key.code, &self.fields[i].kind) {
            (KeyCode::Tab | KeyCode::Down, _) => self.move_focus(true),
            (KeyCode::BackTab | KeyCode::Up, _) => self.move_focus(false),
            (KeyCode::Enter, Kind::Path(_) | Kind::Paths(_)) => self.open_picker(i),
            (
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right,
                Kind::Toggle,
            ) => {
                self.fields[i].on = !self.fields[i].on;
                return Event::Changed(i);
            }
            (KeyCode::Left | KeyCode::Right | KeyCode::Char(' '), Kind::Choice(choices)) => {
                let count = choices.len().max(1);
                let field = &mut self.fields[i];
                field.choice = if key.code == KeyCode::Left {
                    (field.choice + count - 1) % count
                } else {
                    (field.choice + 1) % count
                };
                return Event::Changed(i);
            }
            (KeyCode::Enter, _) => self.move_focus(true),
            (_, Kind::Text | Kind::Path(_) | Kind::Paths(_)) => {
                let before = self.fields[i].input.text().to_string();
                self.fields[i].input.handle(key);
                if self.fields[i].input.text() != before {
                    return Event::Changed(i);
                }
            }
            _ => {}
        }
        Event::None
    }

    /// The help of the focused field.
    fn focused_help(&self) -> String {
        match self.fields.get(self.focus) {
            Some(field) => {
                let mut help = field.help.clone();
                let shows_default = !matches!(field.kind, Kind::Toggle | Kind::Heading);
                if let Some(default) = field
                    .default
                    .as_deref()
                    .filter(|d| shows_default && !d.is_empty())
                {
                    help.push_str(&format!(" [default: {default}]"));
                }
                help
            }
            None => "Runs the command line below: Enter here, or Ctrl+R anywhere.".to_string(),
        }
    }

    /// Draws the fields and the run button in `area`, scrolled to keep the
    /// focused one in sight.
    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(Line::from(format!(" {} ", self.title)).bold());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The rows: a field each, a blank line before a heading (but the
        // first), the button last.
        let mut rows: Vec<Option<usize>> = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            if !field.shown {
                continue;
            }
            if matches!(field.kind, Kind::Heading) && !rows.is_empty() {
                rows.push(None);
            }
            rows.push(Some(i));
        }
        rows.push(None);
        rows.push(Some(self.fields.len()));

        let height = inner.height as usize;
        let focused_row = rows
            .iter()
            .position(|r| *r == Some(self.focus))
            .unwrap_or(0);
        let top = (focused_row + 1).saturating_sub(height);
        let label_width = self
            .fields
            .iter()
            .filter(|f| !matches!(f.kind, Kind::Heading))
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0) as u16
            + 2;

        for (row, entry) in rows.iter().enumerate().skip(top).take(height) {
            let Some(i) = *entry else { continue };
            let area = Rect::new(inner.x, inner.y + (row - top) as u16, inner.width, 1);
            let focused = i == self.focus && self.picker.is_none();
            let label_style = if focused {
                Style::new().reversed().bold()
            } else {
                Style::new().bold()
            };
            if i == self.fields.len() {
                let button = format!("[ {} ]", self.run_label);
                frame.render_widget(Paragraph::new(Span::styled(button, label_style)), area);
                continue;
            }
            let field = &self.fields[i];
            if matches!(field.kind, Kind::Heading) {
                frame.render_widget(
                    Paragraph::new(Line::from(field.label).bold().underlined()),
                    area,
                );
                continue;
            }
            let [label_area, value_area] =
                Layout::horizontal([Constraint::Length(label_width), Constraint::Min(0)])
                    .areas(area);
            let label = format!("{:>width$} ", field.label, width = label_width as usize - 1);
            frame.render_widget(Paragraph::new(Span::styled(label, label_style)), label_area);

            let width = value_area.width as usize;
            let (value, cursor) = match &field.kind {
                Kind::Heading => unreachable!("drawn above"),
                Kind::Toggle => (Line::from(if field.on { "[x]" } else { "[ ]" }), None),
                Kind::Choice(choices) => (choice_line(choices, field.choice, width), None),
                Kind::Text | Kind::Path(_) | Kind::Paths(_) => {
                    if field.input.text().is_empty() && !focused {
                        (Line::from(field.hint).dim(), None)
                    } else {
                        let unit = if field.unit.is_empty() {
                            String::new()
                        } else {
                            format!("  {}", field.unit)
                        };
                        let cursor = focused.then_some(field.input.cursor());
                        let (shown, x) = visible(
                            field.input.text(),
                            cursor,
                            width.saturating_sub(unit.chars().count()),
                        );
                        (
                            Line::from(vec![Span::raw(shown), Span::raw(unit).dim()]),
                            cursor.map(|_| x),
                        )
                    }
                }
            };
            frame.render_widget(Paragraph::new(value), value_area);
            if let Some(x) = cursor {
                let x = value_area.x + (x as u16).min(value_area.width.saturating_sub(1));
                frame.set_cursor_position(Position::new(x, value_area.y));
            }
        }
    }

    /// Draws the open picker, if one is, over the whole of `area`.
    pub fn draw_picker(&mut self, frame: &mut Frame, area: Rect) {
        if let Some((i, picker)) = &mut self.picker {
            let title = format!("Pick: {}", self.fields[*i].label);
            picker.draw(frame, area, &title);
        }
    }
}

/// The choices of a ← → field, the one chosen stressed; just that one, with
/// where it is among them, where they would not fit.
fn choice_line(choices: &[String], chosen: usize, width: usize) -> Line<'static> {
    let all: usize = choices.iter().map(|c| c.chars().count() + 1).sum::<usize>() + 4;
    if all > width {
        let current = choices.get(chosen).cloned().unwrap_or_default();
        return Line::from(vec![
            Span::raw("◀ ").dim(),
            Span::raw(current).bold(),
            Span::raw(format!(" ▶  ({} of {})", chosen + 1, choices.len())).dim(),
        ]);
    }
    let mut spans = vec![Span::raw("◀ ").dim()];
    for (i, c) in choices.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(if i == chosen {
            Span::raw(c.clone()).bold().underlined()
        } else {
            Span::raw(c.clone()).dim()
        });
    }
    spans.push(Span::raw(" ▶").dim());
    Line::from(spans)
}

/// A screen of a tool: its form, and what the form stands for.
pub trait Tool {
    fn form(&self) -> &Form;
    fn form_mut(&mut self) -> &mut Form;
    /// The arguments after `mauro` the form stands for; why not, where it
    /// stands for none yet.
    fn args(&self) -> Result<Vec<String>, String>;
    /// What an exit code of the tool means.
    fn explain(&self) -> fn(i32) -> Option<&'static str>;
    /// Field `field` changed: what follows from it (fields shown or hidden,
    /// a file read).
    fn changed(&mut self, _field: usize) {}
    /// Between keys: whatever is being read in the background.
    fn tick(&mut self) {}
    /// Whether this tool's job is one that outlives the interface — started
    /// detached and watched from the home screen — and which kind of job it
    /// is. The rest run on the job screen and end with it.
    fn background(&self) -> Option<&'static str> {
        None
    }
    /// A panel beside the form: its title and lines.
    fn panel(&self) -> Option<(&'static str, Vec<Line<'static>>)> {
        None
    }
}

/// The command line `tool` stands for, checked by `mauro`'s own parser.
pub fn command(tool: &dyn Tool) -> Result<Vec<String>, String> {
    let args = tool.args()?;
    super::check(&args)?;
    Ok(args)
}

/// A key on `tool`'s screen.
pub fn handle(tool: &mut dyn Tool, key: KeyEvent) -> Action {
    match tool.form_mut().handle(key) {
        Event::None => Action::None,
        Event::Back => Action::Back,
        Event::Changed(i) => {
            tool.changed(i);
            Action::None
        }
        Event::Run => match command(tool) {
            Ok(args) if tool.background().is_some() => {
                Action::Launch(tool.background().unwrap_or_default(), args)
            }
            Ok(args) => Action::Run(JobSpec {
                args,
                explain: tool.explain(),
            }),
            Err(why) => {
                tool.form_mut().message = Some(why);
                Action::None
            }
        },
    }
}

/// Draws `tool`'s screen: the form, beside it the tool's panel and the
/// focused field's help, under it the command line and the keys.
pub fn draw(tool: &mut dyn Tool, frame: &mut Frame) {
    let [body, command_area, foot] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let [form_area, side] =
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body);

    tool.form().draw(frame, form_area);

    let help = Paragraph::new(tool.form().focused_help())
        .block(Block::bordered().title(" Help "))
        .wrap(Wrap { trim: true });
    match tool.panel() {
        Some((title, lines)) => {
            let wanted = (lines.len() as u16 + 2).min(side.height * 3 / 5);
            let [panel_area, help_area] =
                Layout::vertical([Constraint::Length(wanted), Constraint::Min(0)]).areas(side);
            frame.render_widget(
                Paragraph::new(lines)
                    .block(Block::bordered().title(format!(" {title} ")))
                    .wrap(Wrap { trim: false }),
                panel_area,
            );
            frame.render_widget(help, help_area);
        }
        None => frame.render_widget(help, side),
    }

    let command_text = match command(tool) {
        Ok(args) => Line::from(command_line(&args)),
        Err(why) => Line::from(why).dim(),
    };
    frame.render_widget(
        Paragraph::new(command_text)
            .block(Block::bordered().title(" Command "))
            .wrap(Wrap { trim: false }),
        command_area,
    );

    let footer = match &tool.form().message {
        Some(message) => Line::from(message.clone()).red(),
        None => Line::from("↑↓ field · ←→ change · Enter pick/toggle/next · Ctrl+R run · Esc back")
            .dim(),
    };
    frame.render_widget(Paragraph::new(footer).block(Block::bordered()), foot);

    tool.form_mut().draw_picker(frame, frame.area());
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    pub fn ctrl_r() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
    }

    /// Types `text` into field `i` of `form`, over what it held.
    pub fn type_into(form: &mut Form, i: usize, text: &str) {
        form.picker = None;
        form.focus = i;
        form.fields[i].input.set("");
        for c in text.chars() {
            form.handle(key(KeyCode::Char(c)));
        }
    }

    /// Focuses field `i` and presses `code` on it.
    pub fn press(form: &mut Form, i: usize, code: KeyCode) -> Event {
        form.picker = None;
        form.focus = i;
        form.handle(key(code))
    }

    fn sample() -> Form {
        Form::new(
            &["render"],
            "Render",
            vec![
                Field::heading("The map"),
                Field::path("Map", Pick::Files(&["omap"])).arg("map_file"),
                Field::text("Resolution").arg("resolution"),
                Field::choice("Format", &["png", "svg"]),
                Field::toggle("Window"),
                Field::text("Window").arg("window"),
            ],
        )
    }

    #[test]
    fn help_and_defaults_come_from_clap() {
        let form = sample();
        assert_eq!(form.text(2), "3");
        assert!(form.is_default(2));
        assert!(form.fields[2].help.contains("pixels per meter"));
        assert!(form.fields[5].help.contains("left,top,right,bottom"));
        // The first field to focus is past the heading.
        assert_eq!(form.focus, 1);
    }

    #[test]
    fn focus_skips_headings_and_hidden_fields_and_reaches_the_button() {
        let mut form = sample();
        form.show(4, false);
        let mut seen = vec![form.focus];
        for _ in 0..6 {
            form.handle(key(KeyCode::Down));
            seen.push(form.focus);
        }
        assert_eq!(seen, [1, 2, 3, 5, 6, 6, 6]);
        assert_eq!(form.handle(key(KeyCode::Enter)), Event::Run);
        // Hiding the focused field moves the focus off it.
        form.focus = 5;
        form.show(5, false);
        assert_eq!(form.focus, 3);
    }

    #[test]
    fn choices_cycle_and_toggles_flip() {
        let mut form = sample();
        assert_eq!(press(&mut form, 3, KeyCode::Left), Event::Changed(3));
        assert_eq!(form.choice(3), "svg");
        press(&mut form, 3, KeyCode::Right);
        assert_eq!(form.choice(3), "png");
        press(&mut form, 4, KeyCode::Char(' '));
        assert!(form.on(4));
        form.set_choices(3, vec!["svg".into(), "png".into()]);
        assert_eq!(form.choice(3), "png", "kept where still a choice");
    }

    #[test]
    fn options_go_on_the_command_line_only_when_not_their_default() {
        let mut form = sample();
        let mut args = Vec::new();
        form.push_option(&mut args, 2, "-r");
        assert!(args.is_empty());
        type_into(&mut form, 2, "6");
        form.push_option(&mut args, 2, "-r");
        assert_eq!(args, ["-r", "6"]);
        form.show(2, false);
        let mut hidden = Vec::new();
        form.push_option(&mut hidden, 2, "-r");
        assert!(hidden.is_empty(), "a hidden field gives nothing");
    }
}
