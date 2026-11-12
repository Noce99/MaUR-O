//! The `mauro render` screen: a map picked from the disk, the image's
//! options as a form, and what the map is and how large the image will be
//! beside it, before anything is drawn.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use maur_o::geometry::Rect as MapRect;
use maur_o::render::{DEFAULT_FRAME, DEFAULT_RESOLUTION};

use super::job::{command_line, JobSpec};
use super::widgets::{relative, visible, FilePicker, Picked, TextInput};
use super::Action;

/// The map formats the picker shows.
const MAP_EXTENSIONS: &[&str] = &["omap", "xmap", "ocd"];

/// The output formats, by suffix, in the order ← → cycles through them.
const FORMATS: [&str; 5] = ["png", "jpg", "tif", "bmp", "svg"];

/// An image this large (in megapixels) is pointed out before it is drawn.
const LARGE_MEGAPIXELS: f64 = 200.0;

/// The rows of the form.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Map,
    Format,
    Output,
    Area,
    Resolution,
    Frame,
    Window,
    Run,
}

/// What is known of the picked map.
#[derive(Clone, Debug)]
struct MapInfo {
    scale: i32,
    objects: usize,
    symbols: usize,
    /// The EPSG code of the CRS it is placed in, where it is placed at all.
    epsg: Option<i32>,
    warnings: usize,
    /// What the map's objects cover as drawn, in mm on the paper.
    extent: MapRect,
}

/// Where reading the picked map is.
enum Info {
    None,
    Loading(Receiver<Result<MapInfo, String>>),
    Ready(MapInfo),
    Failed(String),
}

/// The `mauro render` screen.
pub struct RenderForm {
    map: Option<PathBuf>,
    format: usize,
    output: TextInput,
    /// The whole map with a frame round it, or a window of it.
    whole_map: bool,
    resolution: TextInput,
    frame: TextInput,
    window: TextInput,
    focus: Field,
    picker: Option<FilePicker>,
    info: Info,
    /// Why the last attempt to run did not, until the next key.
    message: Option<String>,
}

impl RenderForm {
    /// The form with every option at its default; the map picker starts in
    /// `start`.
    pub fn new(start: &Path) -> Self {
        Self {
            map: None,
            format: 0,
            output: TextInput::default(),
            whole_map: true,
            resolution: TextInput::new(DEFAULT_RESOLUTION.to_string()),
            frame: TextInput::new(DEFAULT_FRAME.to_string()),
            window: TextInput::default(),
            focus: Field::Map,
            picker: Some(FilePicker::new(start, MAP_EXTENSIONS)),
            info: Info::None,
            message: None,
        }
    }

    fn svg(&self) -> bool {
        FORMATS[self.format] == "svg"
    }

    /// The rows shown, top to bottom: an SVG has no pixels and no window.
    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Map, Field::Format, Field::Output];
        if self.svg() {
            fields.push(Field::Frame);
        } else {
            fields.push(Field::Area);
            fields.push(Field::Resolution);
            fields.push(if self.whole_map {
                Field::Frame
            } else {
                Field::Window
            });
        }
        fields.push(Field::Run);
        fields
    }

    fn input(&mut self, field: Field) -> Option<&mut TextInput> {
        match field {
            Field::Output => Some(&mut self.output),
            Field::Resolution => Some(&mut self.resolution),
            Field::Frame => Some(&mut self.frame),
            Field::Window => Some(&mut self.window),
            _ => None,
        }
    }

    fn move_focus(&mut self, by: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let to = (at as isize + by).clamp(0, fields.len() as isize - 1);
        self.focus = fields[to as usize];
    }

    /// The map to draw is `map`: the output follows it, and what it is
    /// starts being read on a thread of its own, a large map taking a while.
    fn pick(&mut self, map: PathBuf) {
        self.output.set(
            map.with_extension(FORMATS[self.format])
                .display()
                .to_string(),
        );
        let (sender, receiver) = channel();
        let path = map.clone();
        std::thread::spawn(move || {
            sender.send(read_info(&path)).ok();
        });
        self.info = Info::Loading(receiver);
        self.map = Some(map);
    }

    /// The output keeps its name and takes the new format's suffix.
    fn cycle_format(&mut self, by: isize) {
        let count = FORMATS.len() as isize;
        self.format = ((self.format as isize + by).rem_euclid(count)) as usize;
        if !self.output.text().is_empty() {
            let renamed = Path::new(self.output.text()).with_extension(FORMATS[self.format]);
            self.output.set(renamed.display().to_string());
        }
        if !self.fields().contains(&self.focus) {
            self.focus = Field::Format;
        }
    }

    /// Picks up the map's information, once it has been read.
    pub fn tick(&mut self) {
        if let Info::Loading(receiver) = &self.info {
            if let Ok(result) = receiver.try_recv() {
                self.info = match result {
                    Ok(info) => Info::Ready(info),
                    Err(e) => Info::Failed(e),
                };
            }
        }
    }

    /// The command line the form stands for, after `mauro`, once it is one
    /// `mauro` accepts; why not, where it is not.
    pub fn args(&self) -> Result<Vec<String>, String> {
        let map = self
            .map
            .as_ref()
            .ok_or("Pick a map first (Enter on Map).")?;
        let mut args = vec!["render".to_string(), map.display().to_string()];
        if !self.output.text().trim().is_empty() {
            args.push(self.output.text().trim().to_string());
        }
        if !self.svg() {
            let resolution = number(&self.resolution, "Resolution")?;
            if resolution <= 0.0 {
                return Err("The resolution must be greater than zero.".to_string());
            }
            args.extend(["-r".to_string(), self.resolution.text().trim().to_string()]);
        }
        if self.svg() || self.whole_map {
            if number(&self.frame, "Frame")? < 0.0 {
                return Err("The frame cannot be negative.".to_string());
            }
            args.extend(["-f".to_string(), self.frame.text().trim().to_string()]);
        } else {
            crate::render::parse_window(self.window.text())?;
            args.extend(["-w".to_string(), self.window.text().trim().to_string()]);
        }
        super::check(&args)?;
        Ok(args)
    }

    fn run(&mut self) -> Action {
        match self.args() {
            Ok(args) => Action::Run(JobSpec {
                args,
                explain: crate::render::exit_meaning,
            }),
            Err(message) => {
                self.message = Some(message);
                Action::None
            }
        }
    }

    pub fn handle(&mut self, key: KeyEvent) -> Action {
        self.message = None;
        if let Some(picker) = &mut self.picker {
            match picker.handle(key) {
                Picked::Not => {}
                Picked::Cancelled => self.picker = None,
                Picked::File(path) => {
                    self.picker = None;
                    self.pick(relative(&path));
                    self.focus = Field::Run;
                }
            }
            return Action::None;
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.run();
        }
        match (key.code, self.focus) {
            (KeyCode::Esc, _) => return Action::Back,
            (KeyCode::Tab | KeyCode::Down, _) => self.move_focus(1),
            (KeyCode::BackTab | KeyCode::Up, _) => self.move_focus(-1),
            (KeyCode::Enter, Field::Map) => {
                let start = self.map.clone().unwrap_or_else(|| PathBuf::from("."));
                self.picker = Some(FilePicker::new(&start, MAP_EXTENSIONS));
            }
            (KeyCode::Enter, Field::Run) => return self.run(),
            (KeyCode::Enter, _) => self.move_focus(1),
            (KeyCode::Left, Field::Format) => self.cycle_format(-1),
            (KeyCode::Right | KeyCode::Char(' '), Field::Format) => self.cycle_format(1),
            (KeyCode::Left | KeyCode::Right | KeyCode::Char(' '), Field::Area) => {
                self.whole_map = !self.whole_map;
            }
            (_, field) => {
                if let Some(input) = self.input(field) {
                    input.handle(key);
                }
            }
        }
        Action::None
    }

    /// How large the image will be, in pixels and on the ground, once the
    /// map and the options say.
    fn size(&self) -> Option<String> {
        let Info::Ready(info) = &self.info else {
            return None;
        };
        let mm_per_meter = 1000.0 / info.scale as f64;
        let extent = if self.svg() || self.whole_map {
            let frame = self.frame.text().trim().parse::<f64>().ok()?;
            let frame = frame * mm_per_meter;
            info.extent.adjusted(-frame, -frame, frame, frame)
        } else {
            crate::render::parse_window(self.window.text()).ok()?
        };
        let ground = format!(
            "{:.0} × {:.0} m",
            extent.width() / mm_per_meter,
            extent.height() / mm_per_meter
        );
        if self.svg() {
            return Some(format!("vector, {ground}"));
        }
        let resolution = self.resolution.text().trim().parse::<f64>().ok()?;
        let pixel_per_mm = resolution * info.scale as f64 / 1000.0;
        let (width, height) = (
            (extent.width() * pixel_per_mm).round(),
            (extent.height() * pixel_per_mm).round(),
        );
        let megapixels = width * height / 1e6;
        let mut text = format!("{width:.0} × {height:.0} px ({megapixels:.1} MP), {ground}");
        if megapixels > LARGE_MEGAPIXELS {
            text.push_str(" — large!");
        }
        Some(text)
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let [body, command, foot] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        let [form_area, info_area] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);

        self.draw_form(frame, form_area);
        self.draw_info(frame, info_area);

        let command_text = match self.args() {
            Ok(args) => Line::from(command_line(&args)),
            Err(why) => Line::from(why).dim(),
        };
        frame.render_widget(
            Paragraph::new(command_text)
                .block(Block::bordered().title(" Command "))
                .wrap(Wrap { trim: false }),
            command,
        );

        let footer = match &self.message {
            Some(message) => Line::from(message.as_str()).red(),
            None => Line::from("↑↓ field · ←→ change · Enter pick/next · Ctrl+R render · Esc back")
                .dim(),
        };
        frame.render_widget(Paragraph::new(footer).block(Block::bordered()), foot);

        if let Some(picker) = &mut self.picker {
            picker.draw(frame, frame.area(), "Pick a map (.omap, .xmap, .ocd)");
        }
    }

    fn draw_form(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(Line::from(" mauro render ").bold());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        const LABEL: u16 = 13;
        let mut y = inner.y;
        for field in self.fields() {
            if y >= inner.bottom() {
                break;
            }
            let row = Rect::new(inner.x, y, inner.width, 1);
            y += 2;
            let focused = field == self.focus && self.picker.is_none();
            let label_style = if focused {
                Style::new().reversed().bold()
            } else {
                Style::new().bold()
            };
            if field == Field::Run {
                let button = Span::styled("[ Render ]", label_style);
                frame.render_widget(Paragraph::new(Line::from(button)), row);
                continue;
            }
            let [label_area, value_area] =
                Layout::horizontal([Constraint::Length(LABEL), Constraint::Min(0)]).areas(row);
            let width = value_area.width as usize;
            // A text field: what of it fits, round the cursor where it has it.
            let text = |input: &TextInput, unit: &'static str| {
                let cursor = focused.then_some(input.cursor());
                let (shown, x) = visible(input.text(), cursor, width.saturating_sub(unit.len()));
                (
                    Line::from(vec![Span::raw(shown), Span::raw(unit).dim()]),
                    Some(x),
                )
            };
            let (label, (value, cursor)): (&str, (Line, Option<usize>)) = match field {
                Field::Map => (
                    "Map",
                    match &self.map {
                        Some(map) => {
                            let (shown, _) = visible(&map.display().to_string(), None, width);
                            (Line::from(shown), None)
                        }
                        None => (Line::from("(Enter to pick)").dim(), None),
                    },
                ),
                Field::Format => ("Format", (choice(&FORMATS, self.format), None)),
                Field::Area => (
                    "Area",
                    (
                        choice(&["whole map", "window"], usize::from(!self.whole_map)),
                        None,
                    ),
                ),
                Field::Output => ("Output", text(&self.output, "")),
                Field::Resolution => (
                    "Resolution",
                    text(&self.resolution, "  px per m on the ground"),
                ),
                Field::Frame => ("Frame", text(&self.frame, "  m on the ground, each side")),
                Field::Window if self.window.text().is_empty() && !focused => (
                    "Window",
                    (
                        Line::from("left,top,right,bottom in mm on the paper").dim(),
                        None,
                    ),
                ),
                Field::Window => ("Window", text(&self.window, "")),
                Field::Run => unreachable!("drawn above"),
            };
            frame.render_widget(
                Paragraph::new(Span::styled(format!("{label:>11} "), label_style)),
                label_area,
            );
            frame.render_widget(Paragraph::new(value), value_area);
            if let (true, Some(x)) = (focused, cursor) {
                let x = value_area.x + (x as u16).min(value_area.width.saturating_sub(1));
                frame.set_cursor_position(Position::new(x, value_area.y));
            }
        }
    }

    fn draw_info(&self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" The map ");
        let lines: Vec<Line> = match &self.info {
            Info::None => vec![Line::from("No map picked yet.").dim()],
            Info::Loading(_) => vec![Line::from("Reading the map…").italic()],
            Info::Failed(e) => vec![Line::from(e.as_str()).red()],
            Info::Ready(info) => {
                let e = &info.extent;
                let mut lines = vec![
                    pair("Scale", format!("1:{}", info.scale)),
                    pair("Objects", info.objects.to_string()),
                    pair("Symbols", format!("{} in use", info.symbols)),
                    pair(
                        "Placed",
                        match info.epsg {
                            Some(epsg) => format!("EPSG:{epsg}"),
                            None => "not georeferenced".to_string(),
                        },
                    ),
                    pair(
                        "Extent",
                        format!(
                            "{:.0},{:.0},{:.0},{:.0} mm",
                            e.left(),
                            e.top(),
                            e.right(),
                            e.bottom()
                        ),
                    ),
                ];
                if info.warnings > 0 {
                    lines.push(pair("Warnings", info.warnings.to_string()).yellow());
                }
                lines.push(Line::default());
                lines.push(Line::from("The image").bold());
                lines.push(match self.size() {
                    Some(size) => Line::from(size),
                    None => Line::from("(the options are not numbers yet)").dim(),
                });
                lines
            }
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

/// The number in `input`, or what is wrong with it.
fn number(input: &TextInput, name: &str) -> Result<f64, String> {
    input
        .text()
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{name} is not a number: {:?}", input.text()))
}

/// The choices of a ← → field, the one chosen stressed.
fn choice(choices: &[&str], chosen: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("◀ ").dim()];
    for (i, c) in choices.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(if i == chosen {
            Span::raw(c.to_string()).bold().underlined()
        } else {
            Span::raw(c.to_string()).dim()
        });
    }
    spans.push(Span::raw(" ▶").dim());
    Line::from(spans)
}

/// A `label  value` line of the map panel.
fn pair(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("{label:<9}")).bold(),
        Span::raw(value),
    ])
}

/// Reads what the panel says about the map at `path`.
fn read_info(path: &Path) -> Result<MapInfo, String> {
    let (map, warnings) = maur_o::render::read_map_file(path)?;
    let stats = maur_o::stats::stats(&map);
    Ok(MapInfo {
        scale: map.scale_denominator,
        objects: stats.object_count,
        symbols: stats.symbol_count,
        epsg: map
            .georeferencing
            .as_ref()
            .map(|g| g.epsg)
            .filter(|epsg| *epsg != 0),
        warnings: warnings.len(),
        extent: maur_o::render::drawn_extent(&map),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// A form with `tests/data/shapes.xmap` picked, its information read.
    fn with_map() -> RenderForm {
        let mut form = RenderForm::new(Path::new("."));
        form.picker = None;
        form.pick(PathBuf::from("tests/data/shapes.xmap"));
        let until = Instant::now() + Duration::from_secs(10);
        while matches!(form.info, Info::Loading(_)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
            form.tick();
        }
        form
    }

    fn type_into(form: &mut RenderForm, field: Field, text: &str) {
        form.focus = field;
        form.input(field).unwrap().set("");
        for c in text.chars() {
            form.handle(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn no_map_is_no_command() {
        let mut form = RenderForm::new(Path::new("."));
        form.picker = None;
        assert!(form.args().unwrap_err().contains("Pick a map"));
        assert!(matches!(form.run(), Action::None));
        assert!(form.message.is_some());
    }

    #[test]
    fn the_defaults_make_a_command_mauro_accepts() {
        let form = with_map();
        assert_eq!(
            form.args().unwrap(),
            [
                "render",
                "tests/data/shapes.xmap",
                "tests/data/shapes.png",
                "-r",
                "3",
                "-f",
                "50"
            ]
        );
        assert!(matches!(form.info, Info::Ready(_)));
        assert!(form.size().unwrap().contains(" px "));
    }

    #[test]
    fn the_format_renames_the_output_and_svg_drops_the_pixels() {
        let mut form = with_map();
        form.focus = Field::Format;
        form.handle(key(KeyCode::Left));
        assert_eq!(FORMATS[form.format], "svg");
        assert_eq!(
            form.args().unwrap(),
            [
                "render",
                "tests/data/shapes.xmap",
                "tests/data/shapes.svg",
                "-f",
                "50"
            ]
        );
        assert!(!form.fields().contains(&Field::Resolution));
        assert!(form.size().unwrap().starts_with("vector"));
    }

    #[test]
    fn a_window_replaces_the_frame() {
        let mut form = with_map();
        form.focus = Field::Area;
        form.handle(key(KeyCode::Right));
        assert!(form.fields().contains(&Field::Window));
        assert!(!form.fields().contains(&Field::Frame));
        assert!(form.args().unwrap_err().contains("--window"));
        type_into(&mut form, Field::Window, "0,0,40,20");
        let args = form.args().unwrap();
        assert_eq!(args[args.len() - 2..], ["-w", "0,0,40,20"]);
        // 40 mm at 1:scale, 3 px per m.
        let Info::Ready(info) = &form.info else {
            unreachable!()
        };
        let width = (40.0 * 3.0 * info.scale as f64 / 1000.0).round();
        assert!(form.size().unwrap().starts_with(&format!("{width:.0} × ")));
    }

    #[test]
    fn bad_numbers_are_caught_before_running() {
        let mut form = with_map();
        type_into(&mut form, Field::Resolution, "0");
        assert!(form.args().unwrap_err().contains("greater than zero"));
        type_into(&mut form, Field::Resolution, "abc");
        assert!(form.args().unwrap_err().contains("not a number"));
        type_into(&mut form, Field::Resolution, "2.5");
        type_into(&mut form, Field::Frame, "-1");
        assert!(form.args().unwrap_err().contains("negative"));
    }

    #[test]
    fn enter_on_render_runs_it() {
        let mut form = with_map();
        form.focus = Field::Run;
        let Action::Run(spec) = form.handle(key(KeyCode::Enter)) else {
            panic!("no run")
        };
        assert_eq!(spec.args[0], "render");
        assert_eq!((spec.explain)(2), Some("the map could not be read"));
    }

    #[test]
    fn an_unreadable_map_says_so() {
        let mut form = RenderForm::new(Path::new("."));
        form.pick(PathBuf::from("tests/data/no_such_map.omap"));
        let until = Instant::now() + Duration::from_secs(10);
        while matches!(form.info, Info::Loading(_)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
            form.tick();
        }
        assert!(matches!(form.info, Info::Failed(_)));
    }
}
