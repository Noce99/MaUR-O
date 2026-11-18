//! `mauro net describe`'s screen: write the networks' architectures into the versioned folder.

use crate::tui::form::{Field, Form, Tool};
use crate::tui::widgets::Pick;

const MODEL: usize = 0;
const NETWORKS_DIR: usize = 1;
const CHECK: usize = 2;
const BUMP_MAJOR: usize = 3;

pub struct NetDescribe {
    form: Form,
}

impl NetDescribe {
    pub fn new() -> Self {
        let form = Form::new(
            &["net", "describe"],
            "Describe",
            vec![
                Field::choice("Network", &["all", "unet", "tree"]).arg("model"),
                Field::path("Networks folder", Pick::Folder).arg("networks_dir"),
                Field::toggle("Check only").arg("check"),
                Field::toggle("Bump major").arg("bump_major"),
            ],
        );
        Self { form }
    }
}

impl Tool for NetDescribe {
    fn form(&self) -> &Form {
        &self.form
    }

    fn form_mut(&mut self) -> &mut Form {
        &mut self.form
    }

    fn explain(&self) -> fn(i32) -> Option<&'static str> {
        crate::net_describe::exit_meaning
    }

    fn args(&self) -> Result<Vec<String>, String> {
        let form = &self.form;
        let mut args = vec!["net".to_string(), "describe".to_string()];
        if form.choice(MODEL) != "all" {
            args.push(form.choice(MODEL).to_string());
        }
        form.push_option(&mut args, NETWORKS_DIR, "--networks-dir");
        form.push_flag(&mut args, CHECK, "--check");
        form.push_flag(&mut args, BUMP_MAJOR, "--bump-major");
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::form::command;
    use crate::tui::form::tests::{key, type_into};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn it_needs_nothing_and_builds_the_plain_command() {
        let screen = NetDescribe::new();
        assert_eq!(command(&screen).unwrap(), ["net", "describe"]);
    }

    #[test]
    fn the_options_are_passed_on() {
        let mut screen = NetDescribe::new();
        type_into(&mut screen.form, NETWORKS_DIR, "elsewhere");
        screen.form.handle(key(KeyCode::Right));
        let command = command(&screen).unwrap();
        assert!(
            command.contains(&"--networks-dir".to_string()),
            "{command:?}"
        );
    }
}
