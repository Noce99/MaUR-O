//! What the integration tests share.

use assert_cmd::cargo::CargoError;
use assert_cmd::Command;

/// The built `mauro`, run on the tests' own defaults
/// (`tests/data/defaults.toml`) rather than `config/defaults.toml`, which
/// anybody may have changed — with the terminal interface's Ctrl+D, say —
/// since the tests were written. Its children inherit them: a benchmark's
/// `mauro render` is run on the same.
pub fn mauro() -> Result<Command, CargoError> {
    Command::cargo_bin("mauro").map(|mut command| {
        command.env(
            "MAURO_DEFAULTS",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/defaults.toml"),
        );
        command
    })
}
