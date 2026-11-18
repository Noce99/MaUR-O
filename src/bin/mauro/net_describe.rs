//! Writes down what the networks are, as versions in a folder.
//!
//! ```text
//! mauro net describe [OPTIONS] [unet|tree|all]
//! ```
//!
//! A network's architecture is declared once, beside its `forward`
//! ([`maur_o::net::describe`]), and this is what turns that declaration into a
//! history. The descriptors live in a folder, `networks/` by default, one file
//! per version (`networks/image_to_omap/tree/0.2.json`) and an `index.json`
//! over them ([`maur_o::net::registry`]). A viewer reads the folder; nothing
//! else does.
//!
//! Run it after changing a network. For each network asked about (all by
//! default) it builds the descriptor from the code and compares the
//! architecture's hash with the latest version's. **If they are the same it
//! writes nothing**; if they differ it writes the next version — `0.1`, then
//! `0.2`, … — and the old ones are left as they were. `--bump-major` makes it
//! `1.0`, `2.0`, … instead, for a rewrite.
//!
//! The networks are described as `mauro net train` builds them by default, for
//! the symbol set of the sample datasets
//! ([`maur_o::net::describe::REFERENCE_CLASSES`]); that is what makes a
//! version a statement about the code and not about one run's options.
//!
//! `--check` writes nothing and says whether the folder is up to date: that
//! every network's latest version is what the code builds now, and that no
//! version already written was altered. It is what CI and the git hook run.
//!
//! `--stdout` prints one network's descriptor and touches nothing.
//!
//! Exit codes: 0 success (with `--check`: everything is up to date), 1 usage
//! error, 2 the folder could not be read or written, 3 `--check` found it out
//! of date or altered.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::{Parser, ValueEnum};

use maur_o::net::describe::{blessed, Meta, NetDescriptor, BLESSED};
use maur_o::net::registry::{Registered, Registry, Status, DEFAULT_DIR};

/// Which network to describe.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Which {
    /// Every network the registry follows.
    All,
    /// The U-Net.
    Unet,
    /// The tree of curves.
    Tree,
}

#[derive(Parser)]
#[command(
    name = "describe",
    version,
    about = "Writes the architecture of the networks into a folder of versioned descriptors.\n\n\
             The descriptor is built from the code, as `net train` builds the network by \
             default; if it differs from the latest version of that network the next version is \
             written, and if not nothing is. --check says whether the folder is up to date \
             without writing anything, and fails if it is not."
)]
pub struct Args {
    /// Which network to describe.
    #[arg(value_enum, default_value_t = Which::All)]
    model: Which,

    /// The folder the versions are kept in. Made if it is not there.
    #[arg(long, default_value = DEFAULT_DIR, value_name = "FOLDER")]
    networks_dir: PathBuf,

    /// Writes nothing: says whether every network's latest version is what
    /// the code builds now and no version was altered, and exits with 3 if
    /// not.
    #[arg(long, conflicts_with_all = ["stdout", "bump_major"])]
    check: bool,

    /// Makes a changed network the next major version (1.0, 2.0, …) instead of
    /// the next minor one.
    #[arg(long)]
    bump_major: bool,

    /// Prints the descriptor of one network and touches nothing.
    #[arg(long, conflicts_with = "bump_major")]
    stdout: bool,
}

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "the folder could not be read or written",
        3 => "the descriptors are out of date, or one was altered",
        _ => return None,
    })
}

/// Runs `mauro net describe`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::from(0),
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}

/// The descriptors asked for.
fn chosen(which: Which) -> Vec<NetDescriptor> {
    let names: Vec<&str> = match which {
        Which::All => BLESSED.to_vec(),
        Which::Unet => vec!["unet"],
        Which::Tree => vec!["tree"],
    };
    names
        .into_iter()
        .map(|name| blessed(name).expect("every blessed name has a descriptor"))
        .collect()
}

/// When and from what commit the descriptors are written, as far as git
/// says; a commit with uncommitted changes on top is `abc1234-dirty`.
fn provenance() -> Meta {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let git_commit = git(&["rev-parse", "--short", "HEAD"]).map(|commit| {
        match git(&["status", "--porcelain", "--untracked-files=no"]) {
            Some(changes) if !changes.is_empty() => format!("{commit}-dirty"),
            _ => commit,
        }
    });
    Meta {
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        git_commit,
    }
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
    let failed = |message: String| (ExitCode::from(2), format!("Error: {message}"));

    if args.stdout {
        if args.model == Which::All {
            return Err((
                ExitCode::from(1),
                "Error: --stdout prints one network: name it (unet or tree).".to_string(),
            ));
        }
        for descriptor in chosen(args.model) {
            let text =
                serde_json::to_string_pretty(&descriptor).map_err(|e| failed(e.to_string()))?;
            println!("{text}");
        }
        return Ok(());
    }

    let registry = Registry::new(&args.networks_dir);

    if args.check {
        let mut stale = false;
        for descriptor in chosen(args.model) {
            let name = format!("{}/{}", descriptor.scope, descriptor.name);
            match registry.status(&descriptor).map_err(failed)? {
                Status::UpToDate(version) => println!("{name}: up to date ({version})"),
                Status::Stale(version) => {
                    stale = true;
                    println!(
                        "{name}: the code no longer builds version {version}. Run \
                         `mauro net describe {}` and commit {}.",
                        descriptor.name,
                        args.networks_dir.display()
                    );
                }
                Status::Missing => {
                    stale = true;
                    println!(
                        "{name}: no version yet. Run `mauro net describe {}` and commit {}.",
                        descriptor.name,
                        args.networks_dir.display()
                    );
                }
            }
        }
        for problem in registry.verify().map_err(failed)? {
            stale = true;
            println!("{problem}");
        }
        return if stale {
            Err((ExitCode::from(3), String::new()))
        } else {
            Ok(())
        };
    }

    let meta = provenance();
    for descriptor in chosen(args.model) {
        let name = format!("{}/{}", descriptor.scope, descriptor.name);
        let params = descriptor.total_params();
        let hash = descriptor.canonical_hash();
        match registry
            .register(descriptor, args.bump_major, meta.clone())
            .map_err(failed)?
        {
            Registered::Created(version) => println!(
                "{name}: wrote version {version} ({params} values, hash {hash}) in {}",
                args.networks_dir.display()
            ),
            Registered::UpToDate(version) => println!("{name}: up to date ({version})"),
        }
    }
    // Keeps the index whole even when nothing was added, so a folder made by
    // hand or by an older run is brought level.
    registry.write_index().map_err(failed)?;
    Ok(())
}
