//! `mauro tiles serve`'s screen: the archives to show as layers, and where
//! to listen. The server runs as a job until it is cancelled.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const ARCHIVES: usize = 0;
const HOST: usize = 1;
const PORT: usize = 2;
const WORKERS: usize = 3;

pub struct TilesServe {
    form: Form,
}

impl TilesServe {
    pub fn new() -> Self {
        let mut form = Form::new(
            &["tiles", "serve"],
            "Serve",
            vec![
                Field::paths("Archives", Pick::Files(&["pmtiles"])).arg("archives"),
                Field::text("Host").arg("host"),
                Field::text("Port").arg("port"),
                Field::text("Workers").arg("workers").hint("one per core"),
            ],
        );
        form.open_picker(ARCHIVES);
        Self { form }
    }
}

impl Tool for TilesServe {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::tiles_serve::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let archives: Vec<&str> = form
            .text(ARCHIVES)
            .split(';')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .collect();
        if archives.is_empty() {
            return Err("Pick at least one archive, the highest priority first.".into());
        }
        let mut args = vec!["tiles".into(), "serve".into()];
        form.push_option(&mut args, HOST, "--host");
        form.push_option(&mut args, PORT, "--port");
        form.push_option(&mut args, WORKERS, "--workers");
        args.extend(archives.into_iter().map(String::from));
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::type_into;

    #[test]
    fn archives_make_a_command() {
        let mut screen = TilesServe::new();
        assert!(command(&screen).unwrap_err().contains("at least one"));
        type_into(&mut screen.form, ARCHIVES, "a.pmtiles;b.pmtiles");
        type_into(&mut screen.form, PORT, "9000");
        assert_eq!(
            command(&screen).unwrap(),
            ["tiles", "serve", "--port", "9000", "a.pmtiles", "b.pmtiles"]
        );
    }
}
