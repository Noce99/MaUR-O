//! Jobs that outlive the interface: started detached, found again by whoever
//! opens the interface next.
//!
//! A long job — for now only a training — is not run on the job screen like
//! the rest: it is `mauro` started as a process of its own, in a group of its
//! own and with its output going to a file, the way a shell's trailing `&`
//! would, so closing the interface (or the terminal) leaves it running. What
//! ties the interface to it afterwards is a folder per job under
//! [`root`]:
//!
//! ```text
//! job         what was started: `kind=`, `command=`, `started=`
//! output.log  what the process printed, both streams
//! status, losses, control
//!             the run's side of [`maur_o::net::monitor`]
//! ```
//!
//! The interface never needs the process itself, only those files, which is
//! why it cannot tell a run from another session's from its own.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use maur_o::net::monitor::{Control, State, Status, JOB_DIR_ENV};

use super::job::command_line;

/// What kind of job this is, which is the tab it is listed under.
pub const TRAINING: &str = "Training";

/// Every kind of job there is, in the order the tabs show them.
pub const KINDS: [&str; 1] = [TRAINING];

/// Where the job folders live: the platform's state directory, or the
/// temporary one where there is none.
pub fn root() -> PathBuf {
    let base = std::env::var_os("MAURO_JOBS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("mauro"))
            } else {
                std::env::var_os("XDG_STATE_HOME")
                    .map(|p| PathBuf::from(p).join("mauro"))
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|h| PathBuf::from(h).join(".local/state/mauro"))
                    })
            }
        })
        .unwrap_or_else(|| std::env::temp_dir().join("mauro"));
    if base.ends_with("jobs") {
        base
    } else {
        base.join("jobs")
    }
}

/// A job, as its folder describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// The folder's name: when it was started.
    pub id: String,
    /// Which tab it belongs under.
    pub kind: String,
    /// The command line it was started with.
    pub command: String,
    /// Its folder.
    pub dir: PathBuf,
}

/// Where a job is, as far as an outsider can tell.
#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    /// Heard from lately, in this state.
    Alive(State),
    /// Ended, in this state.
    Over(State),
    /// Not over and not heard from: the process died without a word, or the
    /// machine went down under it.
    Lost,
    /// Started, and has not reported yet.
    Waiting,
}

impl Job {
    /// What the job last said of itself.
    pub fn status(&self) -> Option<Status> {
        Status::read(&self.dir)
    }

    /// Whether it is going, done, or gone.
    pub fn health(&self) -> Health {
        match self.status() {
            None => {
                if age(&self.dir).is_some_and(|secs| secs > 20) {
                    Health::Lost
                } else {
                    Health::Waiting
                }
            }
            Some(status) if status.state.ended() => Health::Over(status.state),
            Some(status) if status.alive() => Health::Alive(status.state),
            Some(_) => Health::Lost,
        }
    }

    /// A name to list it by: the run's folder name where it has one.
    pub fn title(&self) -> String {
        self.status()
            .and_then(|s| s.run)
            .and_then(|run| run.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| self.id.clone())
    }

    /// Asks the job to pause, carry on or stop.
    pub fn ask(&self, control: Control) -> std::io::Result<()> {
        control.send(&self.dir)
    }

    /// The tail of what the process printed, for a failure to be explained by.
    pub fn output_tail(&self, lines: usize) -> Vec<String> {
        let text = fs::read_to_string(self.dir.join("output.log")).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all[all.len().saturating_sub(lines)..]
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    /// Forgets a job which is over: its folder goes, the run's own folder
    /// (the weights, the metrics) does not.
    pub fn forget(&self) -> std::io::Result<()> {
        if matches!(self.health(), Health::Alive(_) | Health::Waiting) {
            return Ok(());
        }
        fs::remove_dir_all(&self.dir)
    }
}

/// Seconds since `path` was last changed.
fn age(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|d| d.as_secs())
}

/// Every job there is, newest first.
pub fn list() -> Vec<Job> {
    let Ok(entries) = fs::read_dir(root()) else {
        return Vec::new();
    };
    let mut jobs: Vec<Job> = entries
        .flatten()
        .filter_map(|entry| read(&entry.path()))
        .collect();
    jobs.sort_by(|a, b| b.id.cmp(&a.id));
    jobs
}

/// The job in `dir`, if it holds one.
fn read(dir: &Path) -> Option<Job> {
    let text = fs::read_to_string(dir.join("job")).ok()?;
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    Some(Job {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        kind: field("kind")?,
        command: field("command")?,
        dir: dir.to_path_buf(),
    })
}

/// Starts `mauro` with `args` as a job of `kind`, detached.
pub fn start(kind: &str, args: &[String]) -> Result<Job, String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let id = format!("{secs:012}-{nanos:09}");
    let dir = root().join(&id);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let command = command_line(args);
    fs::write(
        dir.join("job"),
        format!("kind={kind}\ncommand={command}\nstarted={secs}\n"),
    )
    .map_err(|e| format!("cannot write the job's description: {e}"))?;

    let log = fs::File::create(dir.join("output.log"))
        .map_err(|e| format!("cannot write the job's log: {e}"))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("cannot write the job's log: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find mauro: {e}"))?;
    let mut child = Command::new(exe);
    child
        .args(args)
        .env(JOB_DIR_ENV, &dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    detach(&mut child);
    let spawned = child.spawn();
    match spawned {
        // Not waited for: the job is not this process's to reap, and the
        // handle going away does not touch the process.
        Ok(_) => read(&dir).ok_or_else(|| "the job's folder is unreadable".to_string()),
        Err(e) => {
            fs::remove_dir_all(&dir).ok();
            Err(format!("cannot start mauro: {e}"))
        }
    }
}

/// Puts the child out of the reach of this terminal: its own process group
/// on unix, so that a Ctrl-C or a hang-up aimed at the interface misses it,
/// and no console of its own on Windows.
fn detach(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_folder_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("000000000001-000000000");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("job"),
            "kind=Training\ncommand=mauro net train data\nstarted=1\n",
        )
        .unwrap();
        let job = read(&folder).unwrap();
        assert_eq!(job.kind, TRAINING);
        assert_eq!(job.command, "mauro net train data");
        assert_eq!(job.health(), Health::Waiting);
        assert!(read(dir.path()).is_none());
    }
}
