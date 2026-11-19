//! The form every tool's screen is built on, and the screen around it.
//!
//! A [`Form`] is a column of [`Field`]s — paths with a picker behind them,
//! text, choices, toggles — and a run button. A field can name the clap
//! argument it gives (`Field::arg`): its help text, its default, whether it
//! must be given and which values it takes then come from `mauro`'s own
//! parser ([`crate::defaults::command`]), not from a second copy here that
//! would drift from it.
//!
//! Every value is drawn in the colour of where it stands: green where it is
//! its option's default, orange where it was changed from it, red where it is
//! wrong — a value its option does not take, or one the tool says cannot
//! work ([`Form::set_problem`]). Ctrl+D makes what a field holds its option's
//! default, in `config/defaults.toml`. A field the command cannot do without
//! has a `*` before it.
//!
//! The focused path field's picker is on the right, under the help: Enter or
//! → (at the end of the text) goes into it, ← or Esc back to the fields.
//!
//! A screen is a [`Tool`]: a form, the command line it stands for, and
//! whatever it does when a field changes. [`handle`] and [`draw`] are the
//! same for all of them.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use super::job::{command_line, JobSpec};
use super::widgets::{relative, visible, FilePicker, Pick, Picked, TextInput};
use super::Action;

/// What a value is drawn in where it is its option's default.
pub const DEFAULT_COLOR: Color = Color::Green;
/// … where it was changed from the default.
pub const CHANGED_COLOR: Color = Color::Indexed(208);
/// … where it is wrong.
pub const ERROR_COLOR: Color = Color::Red;

/// What separates a choice's value from what is said about it, in a choice
/// field: `1 · NVIDIA GeForce RTX 4060 Ti` gives `1`.
pub const CHOICE_NOTE: &str = " · ";

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

/// Where a field's value stands, which is the colour it is drawn in.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Nothing to say: no default to be or not be.
    Plain,
    /// Its option's default.
    Default,
    /// Changed from the default.
    Changed,
    /// Wrong, and why.
    Error(String),
}

impl State {
    fn color(&self) -> Option<Color> {
        match self {
            State::Plain => None,
            State::Default => Some(DEFAULT_COLOR),
            State::Changed => Some(CHANGED_COLOR),
            State::Error(_) => Some(ERROR_COLOR),
        }
    }
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
    /// That argument, to check a value by.
    clap: Option<clap::Arg>,
    help: String,
    default: Option<String>,
    /// Whether the command cannot do without it.
    required: bool,
    /// What the tool says is wrong with it.
    problem: Option<String>,
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
            clap: None,
            help: String::new(),
            default: None,
            required: false,
            problem: None,
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

    /// The command cannot do without this field, though `mauro`'s parser
    /// does not say so (an argument it requires is marked anyway).
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    fn focusable(&self) -> bool {
        self.shown && !matches!(self.kind, Kind::Heading)
    }

    fn is_path(&self) -> bool {
        matches!(self.kind, Kind::Path(_) | Kind::Paths(_))
    }

    /// The value of the choice it is on: what comes before [`CHOICE_NOTE`].
    fn choice_value(&self) -> &str {
        match &self.kind {
            Kind::Choice(choices) => choices.get(self.choice).map_or("", |c| {
                c.split_once(CHOICE_NOTE)
                    .map_or(c.as_str(), |(value, _)| value)
            }),
            _ => "",
        }
    }
}

/// What a key did to a [`Form`].
#[derive(Debug, PartialEq)]
pub enum Event {
    None,
    /// The field at this index changed.
    Changed(usize),
    /// Make what the field at this index holds its default (Ctrl+D).
    SetDefault(usize),
    Run,
    Back,
}

/// A column of fields, and a button to run what they say.
pub struct Form {
    title: String,
    /// The subcommand: `["net", "train"]`.
    command: Vec<&'static str>,
    run_label: &'static str,
    fields: Vec<Field>,
    /// The focused field's index; `fields.len()` is the run button.
    focus: usize,
    /// The focused path field's picker, and what the field held when the
    /// picker was last put where it says.
    picker: Option<(usize, FilePicker, String)>,
    /// Whether the keys go to the picker rather than to the fields.
    in_picker: bool,
    /// Why the last attempt to run did not, until the next key.
    pub message: Option<String>,
    /// What the last key did, where it is worth saying, until the next one.
    pub notice: Option<String>,
}

impl Form {
    /// A form for `mauro <command…>`: the fields' clap arguments are looked
    /// up in that subcommand, for their help and their defaults.
    pub fn new(command: &[&'static str], run_label: &'static str, mut fields: Vec<Field>) -> Self {
        let mut clap = crate::defaults::command();
        clap.build();
        for name in command {
            clap = clap
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("mauro has no subcommand {name}"))
                .clone();
        }
        for field in &mut fields {
            if let Some(id) = field.arg {
                let arg = clap
                    .get_arguments()
                    .find(|a| a.get_id() == id)
                    .unwrap_or_else(|| panic!("mauro {} has no argument {id}", command.join(" ")));
                if field.help.is_empty() {
                    if let Some(help) = arg.get_long_help().or(arg.get_help()) {
                        field.help = help.to_string();
                    }
                }
                field.required |= arg.is_required_set();
                field.default = arg
                    .get_default_values()
                    .first()
                    .map(|v| v.to_string_lossy().into_owned());
                field.clap = Some(arg.clone());
            }
            match &field.kind {
                Kind::Choice(choices) => match &field.default {
                    Some(default) => {
                        if let Some(at) = choices.iter().position(|c| {
                            c == default
                                || c.split_once(CHOICE_NOTE).is_some_and(|(v, _)| v == default)
                        }) {
                            field.choice = at;
                        }
                    }
                    // A choice which gives no argument is the first one
                    // until it is changed.
                    None => field.default = Some(field.choice_value().to_string()),
                },
                Kind::Text | Kind::Path(_) if field.input.text().is_empty() => {
                    if let Some(default) = &field.default {
                        field.input.set(default.clone());
                    }
                }
                _ => {}
            }
        }
        let mut form = Self {
            title: format!("mauro {}", command.join(" ")),
            command: command.to_vec(),
            run_label,
            fields,
            focus: 0,
            picker: None,
            in_picker: false,
            message: None,
            notice: None,
        };
        form.focus = (0..form.fields.len())
            .find(|&i| form.fields[i].focusable())
            .unwrap_or(form.fields.len());
        form.sync_picker();
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

    /// Field `i`'s default, where it has one.
    pub fn default(&self, i: usize) -> Option<&str> {
        self.fields[i].default.as_deref()
    }

    /// Gives field `i` a default of the tool's, where its option has none,
    /// and fills it with it where it is empty.
    pub fn set_default(&mut self, i: usize, default: &str) {
        let field = &mut self.fields[i];
        if field.input.text().trim().is_empty() {
            field.input.set(default);
        }
        field.default = Some(default.to_string());
    }

    /// The choice field `i` is on, as shown.
    pub fn choice(&self, i: usize) -> &str {
        match &self.fields[i].kind {
            Kind::Choice(choices) => choices
                .get(self.fields[i].choice)
                .map_or("", String::as_str),
            _ => panic!("field {i} is not a choice"),
        }
    }

    /// The value of the choice field `i` is on: the choice, or the part of
    /// it before [`CHOICE_NOTE`].
    pub fn choice_value(&self, i: usize) -> &str {
        self.fields[i].choice_value()
    }

    /// Gives choice field `i` new choices, keeping the one it is on where
    /// it is still among them — by value, so that a note changing keeps it.
    pub fn set_choices(&mut self, i: usize, choices: Vec<String>) {
        let current = self.choice_value(i).to_string();
        let field = &mut self.fields[i];
        let value = |c: &String| {
            c.split_once(CHOICE_NOTE)
                .map_or(c.clone(), |(v, _)| v.to_string())
        };
        field.choice = choices
            .iter()
            .position(|c| value(c) == current)
            .or_else(|| {
                let default = field.default.as_deref()?;
                choices.iter().position(|c| value(c) == default)
            })
            .unwrap_or(0);
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
        self.sync_picker();
    }

    pub fn shown(&self, i: usize) -> bool {
        self.fields[i].shown
    }

    /// Says what is wrong with field `i` (it is drawn red, and the help says
    /// why), or that nothing is.
    pub fn set_problem(&mut self, i: usize, problem: Option<String>) {
        self.fields[i].problem = problem;
    }

    /// Focuses field `i` and goes into its picker, where it is a path field.
    pub fn open_picker(&mut self, i: usize) {
        if !self.fields[i].is_path() {
            return;
        }
        self.focus = i;
        self.sync_picker();
        self.in_picker = true;
    }

    /// Whether the keys are going to the picker.
    #[cfg(test)]
    pub fn in_picker(&self) -> bool {
        self.in_picker
    }

    /// Where the picker for field `i` starts: what it holds (the last of
    /// several), where that is on the disk, else the working folder.
    fn picker_start(&self, i: usize) -> std::path::PathBuf {
        let last = self
            .text(i)
            .rsplit(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if last.is_empty() || !std::path::Path::new(&last).exists() {
            std::path::PathBuf::from(".")
        } else {
            std::path::PathBuf::from(last)
        }
    }

    /// Keeps the picker the focused field's: one for a path field, put
    /// where the field says when what it says was typed; none for any other.
    fn sync_picker(&mut self) {
        let i = self.focus;
        let Some(Kind::Path(pick) | Kind::Paths(pick)) = self.fields.get(i).map(|f| f.kind.clone())
        else {
            self.picker = None;
            self.in_picker = false;
            return;
        };
        let text = self.text(i).to_string();
        let stale = match &self.picker {
            Some((at, _, seen)) => *at != i || (*seen != text && self.picker_start(i) != *"."),
            None => true,
        };
        if stale {
            self.picker = Some((i, FilePicker::new(&self.picker_start(i), pick), text));
        }
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

    /// What field `i` holds, as its option would be given it.
    fn value(&self, i: usize) -> String {
        let field = &self.fields[i];
        match field.kind {
            Kind::Toggle => field.on.to_string(),
            Kind::Choice(_) => field.choice_value().to_string(),
            _ => self.text(i).to_string(),
        }
    }

    /// Where field `i`'s value stands.
    pub fn state(&self, i: usize) -> State {
        let field = &self.fields[i];
        if let Some(problem) = &field.problem {
            return State::Error(problem.clone());
        }
        let text = self.text(i);
        match &field.kind {
            Kind::Heading => return State::Plain,
            Kind::Toggle => {
                return if field.on {
                    State::Changed
                } else {
                    State::Default
                }
            }
            Kind::Text | Kind::Path(_) if !text.is_empty() => {
                if let Some(arg) = &field.clap {
                    if let Err(why) = crate::defaults::accepts(arg, text) {
                        return State::Error(why);
                    }
                }
            }
            _ => {}
        }
        match &field.default {
            None => State::Plain,
            Some(default) if matches!(field.kind, Kind::Choice(_)) => {
                if field.choice_value() == default {
                    State::Default
                } else {
                    State::Changed
                }
            }
            Some(default) if text.is_empty() || text == default => State::Default,
            Some(_) => State::Changed,
        }
    }

    /// Makes what field `i` holds its option's default, in the defaults
    /// file: what was done, or why it was not.
    pub fn save_default(&mut self, i: usize) -> Result<String, String> {
        let field = &self.fields[i];
        let label = field.label;
        match field.kind {
            Kind::Toggle => {
                return Err(format!(
                    "{label} is a switch, off unless it is turned on: it has no default to set."
                ))
            }
            Kind::Paths(_) => return Err(format!("{label} has no default to set.")),
            _ => {}
        }
        let id = field.arg.ok_or_else(|| {
            format!("{label} is not an option of mauro's: it has no default to set.")
        })?;
        let value = self.value(i);
        if value.is_empty() {
            return Err(format!(
                "{label} is empty: there is nothing to make its default."
            ));
        }
        if let State::Error(why) = self.state(i) {
            return Err(format!("{label} cannot default to that: {why}"));
        }
        let path = crate::defaults::set(&self.command, id, &value)?;
        self.fields[i].default = Some(value.clone());
        Ok(format!(
            "{label} defaults to {value} now (in {}).",
            relative(&path).display()
        ))
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
        self.notice = None;
        let event = self.handle_key(key);
        self.sync_picker();
        event
    }

    fn handle_key(&mut self, key: KeyEvent) -> Event {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('r') {
            return Event::Run;
        }
        if ctrl && key.code == KeyCode::Char('d') && self.focus < self.fields.len() {
            return Event::SetDefault(self.focus);
        }
        if self.in_picker {
            return self.handle_picker(key);
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
        let at_end = self.fields[i].input.cursor() == self.fields[i].input.text().chars().count();
        match (key.code, &self.fields[i].kind) {
            (KeyCode::Tab | KeyCode::Down, _) => self.move_focus(true),
            (KeyCode::BackTab | KeyCode::Up, _) => self.move_focus(false),
            (KeyCode::Enter, Kind::Path(_) | Kind::Paths(_)) => self.in_picker = true,
            (KeyCode::Right, Kind::Path(_) | Kind::Paths(_)) if at_end => self.in_picker = true,
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

    /// A key while the picker has them.
    fn handle_picker(&mut self, key: KeyEvent) -> Event {
        if matches!(
            key.code,
            KeyCode::Left | KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab
        ) {
            self.in_picker = false;
            return Event::None;
        }
        let Some((i, picker, seen)) = &mut self.picker else {
            self.in_picker = false;
            return Event::None;
        };
        let i = *i;
        let pick = match self.fields[i].kind {
            Kind::Path(pick) | Kind::Paths(pick) => pick,
            _ => return Event::None,
        };
        let several = matches!(self.fields[i].kind, Kind::Paths(_));
        let path = match picker.handle(key) {
            Picked::Not => return Event::None,
            Picked::File(path) => {
                self.in_picker = false;
                path
            }
            // A folder field is the folder the picker is in: walking is
            // picking, and ← leaves it there.
            Picked::Entered(dir) if pick.folders() && !several => dir,
            Picked::Entered(_) => return Event::None,
        };
        let path = relative(&path).display().to_string();
        let field = &mut self.fields[i];
        if several && !field.input.text().trim().is_empty() {
            let joined = format!("{}; {path}", field.input.text().trim());
            field.input.set(joined);
        } else {
            field.input.set(path);
        }
        *seen = field.input.text().trim().to_string();
        Event::Changed(i)
    }

    /// The help of the focused field, and what is wrong with it.
    fn focused_help(&self) -> Vec<Line<'static>> {
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
                let mut lines = Vec::new();
                if let State::Error(why) = self.state(self.focus) {
                    lines.push(Line::from(why).fg(ERROR_COLOR));
                }
                lines.push(Line::from(help));
                lines
            }
            None => vec![Line::from(
                "Runs the command line below: Enter here, or Ctrl+R anywhere.",
            )],
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
        // Room for the `* ` of a required field, before the label.
        let label_width = self
            .fields
            .iter()
            .filter(|f| !matches!(f.kind, Kind::Heading))
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0) as u16
            + 4;

        for (row, entry) in rows.iter().enumerate().skip(top).take(height) {
            let Some(i) = *entry else { continue };
            let area = Rect::new(inner.x, inner.y + (row - top) as u16, inner.width, 1);
            let focused = i == self.focus && !self.in_picker;
            let label_style = if focused {
                Style::new().reversed().bold()
            } else if i == self.focus {
                // The field the picker is filling.
                Style::new().bold().underlined()
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
            let star = if field.required { "*" } else { " " };
            let label = format!("{:>width$} ", field.label, width = label_width as usize - 3);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(format!("{star} ")).fg(ERROR_COLOR).bold(),
                    Span::styled(label, label_style),
                ])),
                label_area,
            );

            // A path looks like text, so say a picker is behind it: a tag at
            // the right edge, which the typed text keeps clear of.
            let tag = match (&field.kind, focused) {
                (Kind::Path(_) | Kind::Paths(_), true) => " [Enter/→: browse]",
                (Kind::Path(_) | Kind::Paths(_), false) => " [...]",
                _ => "",
            };
            let tag_width = tag.chars().count() as u16;
            let (value_area, tag_area) = if tag_width > 0 && value_area.width > tag_width * 2 {
                let [v, t] =
                    Layout::horizontal([Constraint::Min(0), Constraint::Length(tag_width)])
                        .areas(value_area);
                (v, Some(t))
            } else {
                (value_area, None)
            };
            if let Some(t) = tag_area {
                frame.render_widget(Paragraph::new(Span::raw(tag).dim()), t);
            }
            let color = self.state(i).color();
            let colored = |span: Span<'static>| match color {
                Some(c) => span.fg(c),
                None => span,
            };
            let width = value_area.width as usize;
            let (value, cursor) = match &field.kind {
                Kind::Heading => unreachable!("drawn above"),
                Kind::Toggle => (
                    Line::from(colored(Span::raw(if field.on { "[x]" } else { "[ ]" }))),
                    None,
                ),
                Kind::Choice(choices) => (choice_line(choices, field.choice, width, color), None),
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
                            Line::from(vec![colored(Span::raw(shown)), Span::raw(unit).dim()]),
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

    /// Whether there is a picker to draw.
    pub fn has_picker(&self) -> bool {
        self.picker.is_some()
    }

    /// Draws the focused path field's picker, if it is one, in `area`.
    pub fn draw_picker(&mut self, frame: &mut Frame, area: Rect) {
        if let Some((i, picker, _)) = &mut self.picker {
            let title = format!("Browse: {}", self.fields[*i].label);
            picker.draw(frame, area, &title, self.in_picker);
        }
    }
}

/// The choices of a ← → field, the one chosen stressed (in `color`, where it
/// has one); just that one, with where it is among them, where they would not
/// fit.
fn choice_line(
    choices: &[String],
    chosen: usize,
    width: usize,
    color: Option<Color>,
) -> Line<'static> {
    let stressed = |text: String| {
        let span = Span::raw(text).bold();
        match color {
            Some(c) => span.fg(c),
            None => span,
        }
    };
    let all: usize = choices.iter().map(|c| c.chars().count() + 1).sum::<usize>() + 4;
    if all > width {
        let current = choices.get(chosen).cloned().unwrap_or_default();
        return Line::from(vec![
            Span::raw("◀ ").dim(),
            stressed(current),
            Span::raw(format!(" ▶  ({} of {})", chosen + 1, choices.len())).dim(),
        ]);
    }
    let mut spans = vec![Span::raw("◀ ").dim()];
    for (i, c) in choices.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(if i == chosen {
            stressed(c.clone()).underlined()
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
    /// Makes what field `field` holds its default (Ctrl+D): what was done, or
    /// why not. A tool whose fields hang together sets them together.
    fn save_default(&mut self, field: usize) -> Result<String, String> {
        self.form_mut().save_default(field)
    }
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
        Event::SetDefault(i) => {
            match tool.save_default(i) {
                Ok(done) => tool.form_mut().notice = Some(done),
                Err(why) => tool.form_mut().message = Some(why),
            }
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

/// How many rows `lines` take wrapped to `width`, roughly: by characters.
fn wrapped_height(lines: &[Line], width: u16) -> u16 {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

/// Draws `tool`'s screen: the form; beside it the tool's panel, the focused
/// field's help and its picker; under it the command line and the keys.
pub fn draw(tool: &mut dyn Tool, frame: &mut Frame) {
    let [body, command_area, foot] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let [form_area, side] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body);

    tool.form().draw(frame, form_area);

    let mut side = side;
    if let Some((title, lines)) = tool.panel() {
        let wanted = (lines.len() as u16 + 2).min(side.height * 2 / 5);
        let [panel_area, rest] =
            Layout::vertical([Constraint::Length(wanted), Constraint::Min(0)]).areas(side);
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::bordered().title(format!(" {title} ")))
                .wrap(Wrap { trim: false }),
            panel_area,
        );
        side = rest;
    }
    let help_lines = tool.form().focused_help();
    let help_area = if tool.form().has_picker() {
        // The help as tall as it is, up to a third; the picker the rest.
        let wanted = (wrapped_height(&help_lines, side.width.saturating_sub(2)) + 2)
            .min(side.height / 3)
            .max(3);
        let [help_area, picker_area] =
            Layout::vertical([Constraint::Length(wanted), Constraint::Min(0)]).areas(side);
        tool.form_mut().draw_picker(frame, picker_area);
        help_area
    } else {
        side
    };
    frame.render_widget(
        Paragraph::new(help_lines)
            .block(Block::bordered().title(" Help "))
            .wrap(Wrap { trim: true }),
        help_area,
    );

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

    let form = tool.form();
    let footer = match (&form.message, &form.notice) {
        (Some(message), _) => Line::from(message.clone()).fg(ERROR_COLOR),
        (None, Some(notice)) => Line::from(notice.clone()).fg(DEFAULT_COLOR),
        (None, None) => Line::from(vec![
            Span::raw("↑↓ field · ←→ change · Enter/→ browse · Ctrl+D make default · Ctrl+R run · Esc back   ").dim(),
            Span::raw("default").fg(DEFAULT_COLOR),
            Span::raw(" ").dim(),
            Span::raw("changed").fg(CHANGED_COLOR),
            Span::raw(" ").dim(),
            Span::raw("wrong").fg(ERROR_COLOR),
            Span::raw("  * needed").dim(),
        ]),
    };
    frame.render_widget(
        Paragraph::new(footer)
            .block(Block::bordered())
            .wrap(Wrap { trim: true }),
        foot,
    );
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
        form.in_picker = false;
        form.focus = i;
        form.fields[i].input.set("");
        for c in text.chars() {
            form.handle(key(KeyCode::Char(c)));
        }
    }

    /// Focuses field `i` and presses `code` on it.
    pub fn press(form: &mut Form, i: usize, code: KeyCode) -> Event {
        form.in_picker = false;
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
    fn a_value_is_green_at_its_default_orange_changed_and_red_wrong() {
        let mut form = sample();
        assert_eq!(form.state(2), State::Default);
        type_into(&mut form, 2, "6");
        assert_eq!(form.state(2), State::Changed);
        type_into(&mut form, 2, "six");
        assert!(matches!(form.state(2), State::Error(_)), "not an f64");
        type_into(&mut form, 2, "");
        assert_eq!(form.state(2), State::Default, "empty is the default");
        // A choice without an option defaults to its first; a toggle to off.
        assert_eq!(form.state(3), State::Default);
        press(&mut form, 3, KeyCode::Right);
        assert_eq!(form.state(3), State::Changed);
        assert_eq!(form.state(4), State::Default);
        press(&mut form, 4, KeyCode::Char(' '));
        assert_eq!(form.state(4), State::Changed);
        // No default, nothing to be changed from; a tool's word is final.
        assert_eq!(form.state(1), State::Plain);
        form.set_problem(1, Some("no such map".into()));
        assert_eq!(form.state(1), State::Error("no such map".into()));
    }

    #[test]
    fn what_mauro_cannot_do_without_is_marked() {
        let form = sample();
        assert!(form.fields[1].required, "the map is render's positional");
        assert!(!form.fields[2].required, "the resolution has a default");
    }

    #[test]
    fn only_an_options_value_can_be_made_its_default() {
        let mut form = sample();
        assert!(form.save_default(3).unwrap_err().contains("not an option"));
        assert!(form.save_default(4).unwrap_err().contains("switch"));
        type_into(&mut form, 2, "six");
        assert!(form.save_default(2).unwrap_err().contains("cannot default"));
        type_into(&mut form, 2, "");
        assert!(form.save_default(2).unwrap_err().contains("empty"));
        form.focus = 2;
        assert_eq!(
            form.handle(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Event::SetDefault(2)
        );
    }

    #[test]
    fn the_picker_is_beside_the_fields_and_arrows_go_in_and_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.omap"), "").unwrap();
        let mut form = sample();
        form.set_text(1, dir.path().display().to_string());
        form.focus = 2;
        form.sync_picker();
        assert!(!form.has_picker(), "a text field has none");
        form.handle(key(KeyCode::Up));
        assert!(form.has_picker() && !form.in_picker());
        // → at the end of the text goes in, ← comes back out.
        form.handle(key(KeyCode::Right));
        assert!(form.in_picker());
        assert_eq!(form.handle(key(KeyCode::Left)), Event::None);
        assert!(!form.in_picker());
        // Esc in the picker is back to the fields, not off the screen.
        form.handle(key(KeyCode::Enter));
        assert!(form.in_picker());
        assert_eq!(form.handle(key(KeyCode::Esc)), Event::None);
        assert_eq!(form.handle(key(KeyCode::Esc)), Event::Back);
        // A file picked fills the field and leaves the picker.
        form.handle(key(KeyCode::Enter));
        form.handle(key(KeyCode::End));
        assert_eq!(form.handle(key(KeyCode::Enter)), Event::Changed(1));
        assert!(form.text(1).ends_with("a.omap"), "{}", form.text(1));
        assert!(!form.in_picker());
    }

    #[test]
    fn a_folder_field_is_the_folder_the_picker_is_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        let mut form = Form::new(
            &["net", "train"],
            "Train",
            vec![Field::path("Dataset", Pick::Folder).arg("dataset")],
        );
        form.set_text(0, dir.path().display().to_string());
        form.open_picker(0);
        form.handle(key(KeyCode::Down)); // past ../ to out/
        assert_eq!(form.handle(key(KeyCode::Enter)), Event::Changed(0));
        assert!(form.text(0).ends_with("out"), "{}", form.text(0));
        assert!(form.in_picker(), "still walking");
        assert_eq!(form.handle(key(KeyCode::Backspace)), Event::Changed(0));
        assert_eq!(
            std::path::Path::new(form.text(0)),
            std::path::absolute(dir.path()).unwrap()
        );
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
