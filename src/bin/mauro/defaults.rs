//! The options' defaults: `config/defaults.toml`, applied to the parser.
//!
//! None of `mauro`'s clap definitions carries a default of its own. The file
//! holds them all, a table per subcommand (`[net.train]` for `mauro net
//! train`) and a key per argument id, and [`command`] is `mauro`'s parser
//! with them put in: every argument the file names gets its value as clap's
//! default, and is no longer required. Everything that parses a command line
//! — `mauro` itself, the terminal interface's checks and forms — goes through
//! [`command`], so the help, the parse and the forms all say the same.
//!
//! Which file: the one `$MAURO_DEFAULTS` names, else `config/defaults.toml`
//! in the source tree this binary was built from, where it is still there;
//! else the copy compiled into the binary. [`set`] writes a default back into
//! the file read, comments and all — the terminal interface's Ctrl+D — so a
//! change is the next run's default and a line in the file's git history.

use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Command, CommandFactory};
use toml_edit::{DocumentMut, Item, Table, Value};

/// The file as it was when the binary was built.
#[cfg(not(test))]
const EMBEDDED: &str = include_str!("../../../config/defaults.toml");

/// The unit tests' own defaults, which a default changed in the real file
/// cannot change: see `tests/data/defaults.toml`.
#[cfg(test)]
const EMBEDDED: &str = include_str!("../../../tests/data/defaults.toml");

/// The real file, for the tests which check it against the parser.
#[cfg(test)]
const REAL: &str = include_str!("../../../config/defaults.toml");

/// The environment variable naming a defaults file to use instead.
pub const ENV: &str = "MAURO_DEFAULTS";

/// The defaults in use, and the file they were read from (`None` for the
/// compiled-in copy). Read once, at the first question.
static LOADED: Mutex<Option<(DocumentMut, Option<PathBuf>)>> = Mutex::new(None);

/// The file to read and write, where there is one. None for the unit tests,
/// which run on their own copy and write nowhere.
#[cfg(test)]
fn file() -> Option<PathBuf> {
    None
}

/// The file to read and write, where there is one.
#[cfg(not(test))]
fn file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(ENV) {
        return Some(PathBuf::from(path));
    }
    let source = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/config/defaults.toml"));
    source.is_file().then_some(source)
}

/// Runs `f` on the defaults, reading them first if they have not been.
fn with<T>(f: impl FnOnce(&mut DocumentMut, &Option<PathBuf>) -> T) -> T {
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let (doc, path) = loaded.get_or_insert_with(|| {
        let path = file();
        let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
        let doc = text
            .as_deref()
            .and_then(|t| t.parse::<DocumentMut>().ok())
            .unwrap_or_else(|| {
                if let Some(p) = &path {
                    eprintln!(
                        "Warning: {} cannot be read as TOML; using the built-in defaults.",
                        p.display()
                    );
                }
                EMBEDDED.parse().expect("the built-in defaults are TOML")
            });
        (doc, path)
    });
    f(doc, path)
}

/// A TOML value as clap takes it: `0.0003`, not `3e-4`, and `5` for `5.0`,
/// which is how clap showed a float default when the code still had them.
fn text_of(value: &Value) -> Option<String> {
    Some(match value {
        Value::String(s) => s.value().clone(),
        Value::Integer(i) => i.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        _ => return None,
    })
}

/// A value typed in, as TOML: a number where it reads back as what was
/// typed, text otherwise.
fn value_of(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        if i.to_string() == text {
            return Value::from(i);
        }
    }
    if let Ok(f) = text.parse::<f64>() {
        if f.is_finite() && f.to_string() == text {
            return Value::from(f);
        }
    }
    Value::from(text)
}

/// Puts the defaults in `table` into `command`, and into its subcommands
/// those of the tables under it. `path` is where it is, for the warnings.
fn apply(mut command: Command, table: &Table, path: &str) -> Command {
    // clap numbers the positional arguments in the order they were added,
    // and `mut_arg` takes one out and adds it back last: pinned first, or a
    // default given to an earlier one would put it after the later ones.
    let positionals: Vec<String> = command
        .get_positionals()
        .map(|a| a.get_id().to_string())
        .collect();
    for (at, id) in positionals.iter().enumerate() {
        command = command.mut_arg(id, |a| a.index(at + 1));
    }
    for (key, item) in table.iter() {
        let here = if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        };
        if let Some(sub) = item.as_table() {
            if command.find_subcommand(key).is_some() {
                command = command.mut_subcommand(key, |c| apply(c, sub, &here));
            } else {
                eprintln!("Warning: defaults: mauro has no subcommand [{here}].");
            }
            continue;
        }
        let Some(value) = item.as_value().and_then(text_of) else {
            eprintln!("Warning: defaults: {here} is not a number, text or true/false.");
            continue;
        };
        if command.get_arguments().any(|a| a.get_id() == key) {
            command = command.mut_arg(key, |a| a.default_value(value).required(false));
        } else {
            eprintln!(
                "Warning: defaults: mauro {} has no argument {key}.",
                path.replace('.', " ")
            );
        }
    }
    command
}

/// Whether `arg` takes `value`, by its own value parser; clap's complaint,
/// shortened, where it does not.
pub fn accepts(arg: &clap::Arg, value: &str) -> Result<(), String> {
    Command::new("value")
        .no_binary_name(true)
        .arg(
            clap::Arg::new("value")
                .value_parser(arg.get_value_parser().clone())
                .allow_hyphen_values(true)
                .required(true),
        )
        .try_get_matches_from([value])
        .map(|_| ())
        .map_err(|e| {
            let text = e.to_string();
            let first = text.lines().next().unwrap_or_default();
            first
                .strip_prefix("error: ")
                .unwrap_or(first)
                .replace(" for '<value>'", "")
        })
}

/// `mauro`'s parser, with the defaults in.
pub fn command() -> Command {
    with(|doc, _| apply(crate::Cli::command(), doc.as_table(), ""))
}

/// Where a [`Parameter`]'s value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The command line.
    CommandLine,
    /// The defaults.
    Default,
    /// Nowhere: an option left out which has no default.
    Unset,
}

/// One argument of a command line, and the value it ran with.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// How the command line names it: `--batch-size`, or `<dataset>`.
    pub name: String,
    /// Its value, several joined with `,`; empty where it is unset.
    pub value: String,
    pub source: Source,
}

/// Every argument of the subcommand `args` (the arguments after `mauro`)
/// runs, and the value each runs with — given or defaulted — as of now: the
/// positionals in their order, then the options in the order `--help` lists
/// them. Or clap's complaint where `mauro` does not take `args`.
pub fn parameters(args: &[String]) -> Result<Vec<Parameter>, String> {
    let mut command = command();
    command.build();
    let mut matches = command
        .clone()
        .try_get_matches_from(std::iter::once("mauro").chain(args.iter().map(String::as_str)))
        .map_err(|e| e.to_string().lines().next().unwrap_or_default().to_string())?;
    // Down to the subcommand which runs.
    while let Some((name, sub)) = matches.subcommand() {
        let sub = sub.clone();
        command = command
            .find_subcommand(name)
            .ok_or_else(|| format!("mauro has no subcommand {name}"))?
            .clone();
        matches = sub;
    }
    let mut arguments: Vec<&clap::Arg> = command
        .get_arguments()
        .filter(|a| !matches!(a.get_id().as_str(), "help" | "version"))
        .collect();
    arguments.sort_by_key(|a| {
        (
            !a.is_positional(),
            a.get_index().unwrap_or(0),
            a.get_display_order(),
        )
    });
    Ok(arguments
        .into_iter()
        .map(|arg| {
            let id = arg.get_id().as_str();
            let name = match arg.get_long() {
                Some(long) => format!("--{long}"),
                None => format!("<{id}>"),
            };
            let value = matches
                .get_raw(id)
                .map(|values| {
                    values
                        .map(|v| v.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let source = match matches.value_source(id) {
                Some(clap::parser::ValueSource::CommandLine)
                | Some(clap::parser::ValueSource::EnvVariable) => Source::CommandLine,
                Some(_) => Source::Default,
                None => Source::Unset,
            };
            Parameter {
                name,
                value,
                source,
            }
        })
        .collect())
}

/// The default of argument `id` of `mauro <subcommand…>`, where there is one.
#[cfg(test)]
pub fn get(subcommand: &[&str], id: &str) -> Option<String> {
    with(|doc, _| get_in(doc, subcommand, id))
}

#[cfg(test)]
fn get_in(doc: &DocumentMut, subcommand: &[&str], id: &str) -> Option<String> {
    let mut table = doc.as_table();
    for name in subcommand {
        table = table.get(name)?.as_table()?;
    }
    table.get(id)?.as_value().and_then(text_of)
}

/// Makes `value` the default of argument `id` of `mauro <subcommand…>`, and
/// writes the file. Where it was written, or why it was not.
pub fn set(subcommand: &[&str], id: &str, value: &str) -> Result<PathBuf, String> {
    with(|doc, path| {
        let path = path.clone().ok_or_else(|| {
            format!(
                "There is no defaults file to write: this binary's source tree is gone. \
                 Name one with {ENV}."
            )
        })?;
        set_in(doc, subcommand, id, value)?;
        std::fs::write(&path, doc.to_string())
            .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        Ok(path)
    })
}

/// [`set`] on `doc`, which is not written anywhere.
fn set_in(doc: &mut DocumentMut, subcommand: &[&str], id: &str, value: &str) -> Result<(), String> {
    let mut table = doc.as_table_mut();
    for (at, name) in subcommand.iter().enumerate() {
        let entry = table.entry(name).or_insert_with(|| {
            let mut t = Table::new();
            // `[net.train]`, not an empty `[net]` over it.
            t.set_implicit(at + 1 < subcommand.len());
            Item::Table(t)
        });
        table = entry
            .as_table_mut()
            .ok_or_else(|| format!("the defaults hold [{name}] as something else"))?;
    }
    // Kept where it was in the file, with whatever comment it had.
    match table.get_mut(id).and_then(Item::as_value_mut) {
        Some(old) => {
            let decor = old.decor().clone();
            *old = value_of(value);
            *old.decor_mut() = decor;
        }
        None => {
            table.insert(id, Item::Value(value_of(value)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key of the built-in file is an argument, and its value one the
    /// argument takes: a default which clap would refuse fails here, not at
    /// somebody's first run.
    #[test]
    fn every_default_is_an_argument_and_parses() {
        for text in [REAL, EMBEDDED] {
            check_file(text);
        }
    }

    fn check_file(text: &str) {
        let doc: DocumentMut = text.parse().unwrap();
        fn walk(command: &Command, table: &Table, path: &str) {
            for (key, item) in table.iter() {
                if let Some(sub) = item.as_table() {
                    let sub_command = command
                        .find_subcommand(key)
                        .unwrap_or_else(|| panic!("no subcommand {path} {key}"));
                    walk(sub_command, sub, &format!("{path} {key}"));
                    continue;
                }
                let arg = command
                    .get_arguments()
                    .find(|a| a.get_id() == key)
                    .unwrap_or_else(|| panic!("mauro{path} has no argument {key}"));
                let value = text_of(item.as_value().unwrap()).unwrap();
                accepts(arg, &value).unwrap_or_else(|e| panic!("mauro{path} {key} = {value}: {e}"));
            }
        }
        let mut cli = crate::Cli::command();
        cli.build();
        walk(&cli, doc.as_table(), "");
    }

    #[test]
    fn the_defaults_reach_the_parser() {
        let command = command();
        let train = command
            .find_subcommand("net")
            .and_then(|c| c.find_subcommand("train"))
            .unwrap();
        let batch = train
            .get_arguments()
            .find(|a| a.get_id() == "batch_size")
            .unwrap();
        assert!(!batch.is_required_set());
        assert_eq!(
            batch.get_default_values()[0].to_string_lossy(),
            get(&["net", "train"], "batch_size").unwrap()
        );
    }

    #[test]
    fn a_value_is_checked_by_its_arguments_own_parser() {
        let command = command();
        let train = command
            .find_subcommand("net")
            .and_then(|c| c.find_subcommand("train"))
            .unwrap();
        let arg = |id: &str| train.get_arguments().find(|a| a.get_id() == id).unwrap();
        assert!(accepts(arg("batch_size"), "16").is_ok());
        assert!(accepts(arg("batch_size"), "-3").is_err());
        assert!(accepts(arg("learning_rate"), "-1e-4").is_ok());
        let complaint = accepts(arg("model"), "transformer").unwrap_err();
        assert!(complaint.contains("transformer"), "{complaint}");
        assert!(!complaint.contains('\n'));
    }

    /// However many of a subcommand's positional arguments the defaults
    /// give a value, they are taken in the order the code declares them.
    #[test]
    fn defaults_keep_the_positional_arguments_in_their_order() {
        fn order(command: &Command) -> Vec<String> {
            let mut c = command.clone();
            c.build();
            let mut all: Vec<_> = c
                .get_positionals()
                .map(|a| (a.get_index().unwrap(), a.get_id().to_string()))
                .collect();
            all.sort();
            all.into_iter().map(|(_, id)| id).collect()
        }
        fn walk(plain: &Command, table: &Table) {
            let mut with = apply(plain.clone(), table, "");
            with.build();
            assert_eq!(order(plain), order(&with), "mauro {}", plain.get_name());
            for sub in plain.get_subcommands() {
                if let Some(t) = table.get(sub.get_name()).and_then(Item::as_table) {
                    walk(sub, t);
                }
            }
        }
        // Every positional with a default, the later ones first in the file:
        // the order which once swapped `net dataset`'s two.
        let doc: DocumentMut = "[net.dataset]\nfolder = \"out\"\nsymbol_set = \"a.omap\"\n\
                                [net.train]\ntrainings = \"t\"\ndataset = \"d\"\n"
            .parse()
            .unwrap();
        let mut plain = crate::Cli::command();
        plain.build();
        walk(&plain, doc.as_table());
        for text in [REAL, EMBEDDED] {
            let file: DocumentMut = text.parse().unwrap();
            walk(&plain, file.as_table());
        }
    }

    #[test]
    fn a_command_lines_parameters_are_every_argument_given_or_not() {
        let args: Vec<String> = ["net", "train", "data", "--batch-size", "16"]
            .map(str::to_string)
            .to_vec();
        let all = parameters(&args).unwrap();
        let find = |name: &str| all.iter().find(|p| p.name == name).unwrap().clone();
        assert_eq!(all[0].name, "<dataset>", "the positionals first");
        assert_eq!(all[1].name, "<trainings>");
        assert_eq!(find("<dataset>").value, "data");
        let batch = find("--batch-size");
        assert_eq!(
            (batch.value.as_str(), batch.source),
            ("16", Source::CommandLine)
        );
        let epochs = find("--epochs");
        assert_eq!(epochs.source, Source::Default);
        assert_eq!(Some(epochs.value), get(&["net", "train"], "epochs"));
        assert_eq!(find("--overhang").source, Source::Unset);
        assert_eq!(find("--dashboard").value, "false");
        assert!(!all.iter().any(|p| p.name == "--help"));
        assert!(parameters(&["net".into(), "train".into(), "--no-such".into()]).is_err());
    }

    #[test]
    fn values_are_written_as_what_they_read_back_as() {
        assert!(matches!(value_of("8"), Value::Integer(_)));
        assert!(matches!(value_of("0.0003"), Value::Float(_)));
        assert!(
            matches!(value_of("3e-4"), Value::String(_)),
            "would read back as 0.0003"
        );
        assert!(matches!(value_of("08"), Value::String(_)));
        assert!(matches!(value_of("trainings"), Value::String(_)));
        for text in ["8", "0.0003", "3e-4", "dataset", "0.5"] {
            assert_eq!(text_of(&value_of(text)).unwrap(), text);
        }
    }

    #[test]
    fn a_default_set_is_written_in_place_and_read_back() {
        let mut doc: DocumentMut =
            "# head\n[net.train]\n# the batch\nbatch_size = 8 # crops\nseed = 0\n"
                .parse()
                .unwrap();
        set_in(&mut doc, &["net", "train"], "batch_size", "16").unwrap();
        set_in(&mut doc, &["render"], "resolution", "4.5").unwrap();
        set_in(&mut doc, &["net", "dataset"], "maps", "100").unwrap();
        let written = doc.to_string();
        assert!(
            written.contains("# the batch\nbatch_size = 16 # crops\n"),
            "{written}"
        );
        assert!(
            written.contains("[render]\nresolution = 4.5\n"),
            "{written}"
        );
        assert!(written.contains("[net.dataset]\nmaps = 100\n"), "{written}");
        assert!(!written.contains("[net]\n"), "{written}");
        assert_eq!(
            get_in(&doc, &["net", "train"], "batch_size").as_deref(),
            Some("16")
        );
    }
}
