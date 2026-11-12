//! `mauro benchmark create`'s screen: the ground-truth renderer — another
//! program, or `mauro` itself — the maps, and how the references are drawn.

use super::{number, MAP_EXTENSIONS};
use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const PROGRAM: &str = "another program";
const ITSELF: &str = "mauro render";

const RENDERER_IS: usize = 0;
const RENDERER: usize = 1;
const RENDERER_ARGS: usize = 2;
const SOURCE: usize = 3;
const OUTPUT: usize = 4;
const FORCE: usize = 5;
const RESOLUTION: usize = 6;
const FRAME: usize = 7;
const FILTER: usize = 8;

pub struct BenchmarkCreate {
    form: Form,
}

impl BenchmarkCreate {
    pub fn new() -> Self {
        let mut screen = Self {
            form: Form::new(
                &["benchmark", "create"],
                "Create",
                vec![
                    Field::choice("Renderer is", &[PROGRAM, ITSELF]).help(
                        "Who draws the reference images: the ground-truth renderer, an \
                         external program, or this very mauro, as `mauro render` — a handy way \
                         to try the tools out, though an archive made so only measures MaUR-O \
                         against itself.",
                    ),
                    Field::path("Renderer", Pick::AnyFile).arg("renderer"),
                    Field::text("Its arguments").arg("renderer_args").help(
                        "Arguments put before the renderer's own, separated by spaces: each \
                         becomes a --renderer-arg.",
                    ),
                    Field::path("Maps", Pick::FileOrFolder(MAP_EXTENSIONS)).arg("source"),
                    Field::text("Archive")
                        .arg("output")
                        .hint("benchmarks/benchmark_<maps>_<resolution>_px_m.zip"),
                    Field::toggle("Replace").arg("force"),
                    Field::text("Resolution")
                        .arg("resolution")
                        .unit("px per m on the ground"),
                    Field::text("Frame")
                        .arg("frame")
                        .unit("m on the ground, each side"),
                    Field::text("Filter").arg("filter").hint("every map"),
                ],
            ),
        };
        screen.update();
        screen
    }

    fn itself(&self) -> bool {
        self.form.choice(RENDERER_IS) == ITSELF
    }

    fn update(&mut self) {
        let program = !self.itself();
        self.form.show(RENDERER, program);
        self.form.show(RENDERER_ARGS, program);
    }
}

impl Tool for BenchmarkCreate {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::benchmark_create::exit_meaning
    }

    fn changed(&mut self, field: usize) {
        if field == RENDERER_IS {
            self.update();
        }
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let (renderer, renderer_args) = if self.itself() {
            let exe = std::env::current_exe().map_err(|e| format!("Where is mauro? {e}"))?;
            (exe.display().to_string(), vec!["render".to_string()])
        } else {
            let renderer = form
                .given(RENDERER)
                .ok_or("Pick the renderer (Enter on Renderer).")?;
            let extra = form
                .text(RENDERER_ARGS)
                .split_whitespace()
                .map(String::from)
                .collect();
            (renderer.to_string(), extra)
        };
        let source = form
            .given(SOURCE)
            .ok_or("Pick the maps: a folder or one map.")?;
        let mut args = vec!["benchmark".into(), "create".into()];
        for arg in renderer_args {
            // Joined to its flag, so that one starting with a dash is not
            // taken for a flag of its own.
            args.push(format!("--renderer-arg={arg}"));
        }
        args.extend([renderer, source.to_string()]);
        form.push_option(&mut args, OUTPUT, "-o");
        form.push_flag(&mut args, FORCE, "--force");
        if number(form.text(RESOLUTION), "Resolution")? <= 0.0 {
            return Err("The resolution must be greater than zero.".to_string());
        }
        form.push_option(&mut args, RESOLUTION, "-r");
        if number(form.text(FRAME), "Frame")? < 0.0 {
            return Err("The frame cannot be negative.".to_string());
        }
        form.push_option(&mut args, FRAME, "-f");
        form.push_option(&mut args, FILTER, "--filter");
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{press, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn an_external_renderer_and_its_arguments() {
        let mut screen = BenchmarkCreate::new();
        assert!(command(&screen).unwrap_err().contains("renderer"));
        type_into(&mut screen.form, RENDERER, "/opt/mapper/render");
        type_into(&mut screen.form, RENDERER_ARGS, "--quiet  draw");
        type_into(&mut screen.form, SOURCE, "maps");
        type_into(&mut screen.form, RESOLUTION, "6");
        assert_eq!(
            command(&screen).unwrap(),
            [
                "benchmark",
                "create",
                "--renderer-arg=--quiet",
                "--renderer-arg=draw",
                "/opt/mapper/render",
                "maps",
                "-r",
                "6"
            ]
        );
    }

    #[test]
    fn mauro_itself_is_the_renderer_as_mauro_render() {
        let mut screen = BenchmarkCreate::new();
        press(&mut screen.form, RENDERER_IS, KeyCode::Right);
        screen.changed(RENDERER_IS);
        assert!(!screen.form.shown(RENDERER));
        type_into(&mut screen.form, SOURCE, "maps/ISOM_10k.omap");
        let args = command(&screen).unwrap();
        assert_eq!(args[2], "--renderer-arg=render");
        assert_eq!(
            args[3],
            std::env::current_exe().unwrap().display().to_string()
        );
        assert_eq!(args[4], "maps/ISOM_10k.omap");
    }
}
