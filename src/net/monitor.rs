//! Watching, pausing and stopping a run from outside it.
//!
//! A training can take hours, so `mauro`'s terminal interface starts it as a
//! detached process and looks in on it from time to time — possibly from
//! another session of the interface than the one which started it. The two
//! cannot share memory, so they share a folder, the **job folder**, which the
//! run is told about by the [`JOB_DIR_ENV`] environment variable:
//!
//! ```text
//! status      what the run is doing now, `key=value` a line, replaced as a
//!             whole (written to a sibling and renamed) twice a second
//! losses      one line per step: `t|v <epoch> <iteration> <loss>`,
//!             `t` for a training step and `v` for a validation one
//! accuracy    the same, one line per step, for the share of pixels right (in %)
//! line_iou    the same, for how much of the lines the run found (in %); only
//!             a run with a lines layer writes it
//! point_f1    the same, for how well the run found the point symbols (in %);
//!             only a run with a points layer writes it
//! scale_error the same, for how far out the network was about how dense its
//!             crops were (in %); only a run with a scale head writes it
//! control     what the watcher asks for: `run`, `pause` or `stop`
//! ```
//!
//! The status carries the run's process id, which is how the watcher finds the
//! GPU it is using.
//!
//! Nothing here needs a signal or a pid, which is what keeps it the same on
//! every platform: a run that is alive rewrites `status` twice a second
//! whatever it is doing, so a `status` that has not changed for a while is a
//! run that is gone ([`Status::alive`]).
//!
//! `stop` is burn's own interrupter: the epoch under way is abandoned, and
//! what the run has so far is wrapped up as a finished run is. `pause` holds
//! the data loaders where they are — see [`Gate`] — so the run idles after the
//! few batches already read ahead.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use burn::data::dataset::{Dataset, DatasetError};
use burn::train::logger::{EvaluationProgressLogger, TrainingProgressLogger};
use burn::train::metric::{MetricDefinition, MetricId, NumericEntry};
use burn::train::renderer::{
    EvaluationName, MetricState, MetricsRenderer, MetricsRendererEvaluation,
    MetricsRendererTraining,
};
use burn::train::Interrupter;

/// The environment variable naming the job folder a run reports to. Without
/// it a run reports to nobody.
pub const JOB_DIR_ENV: &str = "MAURO_JOB_DIR";

/// The file the run describes itself in.
pub const STATUS_FILE: &str = "status";
/// The file the run's losses are appended to.
pub const LOSSES_FILE: &str = "losses";
/// The file the run's pixel accuracies (in %) are appended to, in the
/// losses file's own line format.
pub const ACCURACY_FILE: &str = "accuracy";
/// The file the run's line IoUs (in %) are appended to, in the losses file's
/// own line format. Only a run whose network has a lines layer makes it.
pub const LINE_IOU_FILE: &str = "line_iou";
/// The file the run's point F1 scores (in %) are appended to, in the losses
/// file's own line format. Only a run whose network has a points layer makes
/// it.
pub const POINT_F1_FILE: &str = "point_f1";
/// The file the run's scale errors (in %) are appended to, in the losses
/// file's own line format. Only a run whose network has a scale head makes
/// it.
pub const SCALE_ERROR_FILE: &str = "scale_error";
/// The file the watcher asks things of the run in.
pub const CONTROL_FILE: &str = "control";

/// How often the run rewrites its status, and the control file is read.
const BEAT: Duration = Duration::from_millis(500);

/// A status older than this many beats is a run that is gone.
const MISSED_BEATS: u64 = 8;

/// What a watcher asks of a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    /// Carry on (or go on).
    Run,
    /// Hold still until told to [`Control::Run`].
    Pause,
    /// End the run here, keeping what it has.
    Stop,
}

impl Control {
    fn word(self) -> &'static str {
        match self {
            Control::Run => "run",
            Control::Pause => "pause",
            Control::Stop => "stop",
        }
    }

    /// Asks the run in `job` for this.
    pub fn send(self, job: &Path) -> std::io::Result<()> {
        write_whole(&job.join(CONTROL_FILE), self.word())
    }

    /// What `job` is being asked for now.
    pub fn read(job: &Path) -> Control {
        match fs::read_to_string(job.join(CONTROL_FILE))
            .unwrap_or_default()
            .trim()
        {
            "pause" => Control::Pause,
            "stop" => Control::Stop,
            _ => Control::Run,
        }
    }
}

/// Where a run is in its life.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// Reading the dataset and building the network.
    Starting,
    /// Taking steps.
    Running,
    /// Held still on request.
    Paused,
    /// Asked to stop, and wrapping up.
    Stopping,
    /// Got to the end of its epochs.
    Finished,
    /// Ended on request, with what it had.
    Stopped,
    /// Ended on an error.
    Failed,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Paused => "paused",
            State::Stopping => "stopping",
            State::Finished => "finished",
            State::Stopped => "stopped",
            State::Failed => "failed",
        }
    }

    fn from_word(word: &str) -> Option<State> {
        Some(match word {
            "starting" => State::Starting,
            "running" => State::Running,
            "paused" => State::Paused,
            "stopping" => State::Stopping,
            "finished" => State::Finished,
            "stopped" => State::Stopped,
            "failed" => State::Failed,
            _ => return None,
        })
    }

    /// Whether the run is over, one way or another.
    pub fn ended(self) -> bool {
        matches!(self, State::Finished | State::Stopped | State::Failed)
    }
}

/// What a run says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// Where the run is in its life.
    pub state: State,
    /// The epoch under way, from 1; nought before the first.
    pub epoch: usize,
    /// How many epochs the run was set up for.
    pub epochs: usize,
    /// How many items of this epoch's pass (training or validation) are done.
    pub done: usize,
    /// How many items that pass has.
    pub of: usize,
    /// Whether the pass under way is the validation one.
    pub validating: bool,
    /// Steps taken so far, over the whole run.
    pub iteration: usize,
    /// The last training loss seen.
    pub train_loss: Option<f64>,
    /// The last validation loss seen.
    pub valid_loss: Option<f64>,
    /// The run's own folder, once it has made it.
    pub run: Option<PathBuf>,
    /// What went wrong, for [`State::Failed`].
    pub message: Option<String>,
    /// The process id of the run, which is how a watcher finds the GPU it is
    /// using: the driver lists the processes on each one.
    pub pid: Option<u32>,
    /// When this was written, in seconds since the epoch.
    pub updated: u64,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            state: State::Starting,
            epoch: 0,
            epochs: 0,
            done: 0,
            of: 0,
            validating: false,
            iteration: 0,
            train_loss: None,
            valid_loss: None,
            run: None,
            message: None,
            pid: None,
            updated: now(),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Writes `text` to `path` so that nobody reading it meets half of it.
fn write_whole(path: &Path, text: &str) -> std::io::Result<()> {
    let side = path.with_extension("new");
    fs::write(&side, text)?;
    fs::rename(&side, path)
}

impl Status {
    /// The text the status is kept as in its file.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "state={}\nepoch={}\nepochs={}\ndone={}\nof={}\nvalidating={}\niteration={}\nupdated={}\n",
            self.state.word(),
            self.epoch,
            self.epochs,
            self.done,
            self.of,
            self.validating,
            self.iteration,
            self.updated,
        );
        if let Some(loss) = self.train_loss {
            text.push_str(&format!("train_loss={loss}\n"));
        }
        if let Some(loss) = self.valid_loss {
            text.push_str(&format!("valid_loss={loss}\n"));
        }
        if let Some(run) = &self.run {
            text.push_str(&format!("run={}\n", run.display()));
        }
        if let Some(message) = &self.message {
            text.push_str(&format!("message={}\n", message.replace('\n', " ")));
        }
        if let Some(pid) = self.pid {
            text.push_str(&format!("pid={pid}\n"));
        }
        text
    }

    /// Parses what [`Status::to_text`] wrote; `None` for what it did not.
    pub fn parse(text: &str) -> Option<Status> {
        let mut status = Status::default();
        let mut seen = false;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "state" => {
                    status.state = State::from_word(value)?;
                    seen = true;
                }
                "epoch" => status.epoch = value.parse().ok()?,
                "epochs" => status.epochs = value.parse().ok()?,
                "done" => status.done = value.parse().ok()?,
                "of" => status.of = value.parse().ok()?,
                "validating" => status.validating = value == "true",
                "iteration" => status.iteration = value.parse().ok()?,
                "updated" => status.updated = value.parse().ok()?,
                "train_loss" => status.train_loss = value.parse().ok(),
                "valid_loss" => status.valid_loss = value.parse().ok(),
                "run" => status.run = Some(PathBuf::from(value)),
                "message" => status.message = Some(value.to_string()),
                "pid" => status.pid = value.parse().ok(),
                _ => {}
            }
        }
        seen.then_some(status)
    }

    /// The status `job` last wrote, if it has.
    pub fn read(job: &Path) -> Option<Status> {
        Status::parse(&fs::read_to_string(job.join(STATUS_FILE)).ok()?)
    }

    /// Whether the run behind this status can still be there: it has not
    /// ended, and has been heard from lately.
    pub fn alive(&self) -> bool {
        !self.state.ended()
            && now().saturating_sub(self.updated) <= MISSED_BEATS * BEAT.as_secs().max(1)
    }

    /// How far through the epoch under way, 0 to 1.
    pub fn epoch_fraction(&self) -> f64 {
        if self.of == 0 {
            0.0
        } else {
            (self.done as f64 / self.of as f64).clamp(0.0, 1.0)
        }
    }

    /// How far through the whole run, 0 to 1. A validation pass counts as
    /// the end of its epoch.
    pub fn overall_fraction(&self) -> f64 {
        if self.state == State::Finished {
            return 1.0;
        }
        if self.epochs == 0 || self.epoch == 0 {
            return 0.0;
        }
        let within = if self.validating {
            1.0
        } else {
            self.epoch_fraction()
        };
        (((self.epoch - 1) as f64 + within) / self.epochs as f64).clamp(0.0, 1.0)
    }
}

/// One line of the losses file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LossPoint {
    /// Whether it is a validation step rather than a training one.
    pub validation: bool,
    /// The epoch it was taken in.
    pub epoch: usize,
    /// The step it was taken at, over the whole run.
    pub iteration: usize,
    /// The batch's loss.
    pub loss: f64,
}

impl LossPoint {
    /// Reads one line of the losses file.
    pub fn parse(line: &str) -> Option<LossPoint> {
        let mut parts = line.split(' ');
        let validation = match parts.next()? {
            "t" => false,
            "v" => true,
            _ => return None,
        };
        Some(LossPoint {
            validation,
            epoch: parts.next()?.parse().ok()?,
            iteration: parts.next()?.parse().ok()?,
            loss: parts.next()?.parse().ok()?,
        })
    }
}

/// The run's side of the job folder.
#[derive(Clone)]
pub struct Monitor {
    dir: PathBuf,
    status: Arc<Mutex<Status>>,
    gate: Gate,
}

impl Monitor {
    /// The monitor of the job folder named in the environment, if one is.
    pub fn from_env() -> Option<Monitor> {
        let dir = PathBuf::from(std::env::var_os(JOB_DIR_ENV)?);
        Some(Monitor::new(dir))
    }

    /// A monitor reporting into `dir`, which is made if need be.
    pub fn new(dir: PathBuf) -> Monitor {
        fs::create_dir_all(&dir).ok();
        let monitor = Monitor {
            dir,
            status: Arc::new(Mutex::new(Status {
                pid: Some(std::process::id()),
                ..Status::default()
            })),
            gate: Gate::default(),
        };
        monitor.write();
        monitor
    }

    /// Starts the thread which keeps the status fresh and the control file
    /// read: asks `interrupter` to stop the run on `stop`, and closes or
    /// opens the gate on `pause`. Ends with the run, once it has ended.
    pub fn watch(&self, interrupter: Interrupter) {
        self.watch_with(move || interrupter.stop(Some("asked to stop from the job folder")));
    }

    /// [`Monitor::watch`] for a loop which is not burn's learner: `on_stop`
    /// is called, every beat, for as long as a `stop` is asked for.
    pub fn watch_with(&self, on_stop: impl Fn() + Send + 'static) {
        let monitor = self.clone();
        std::thread::spawn(move || loop {
            let control = Control::read(&monitor.dir);
            let ended = {
                let mut status = monitor.status.lock().unwrap();
                match control {
                    Control::Stop => {
                        on_stop();
                        monitor.gate.pause(false);
                        if !status.state.ended() {
                            status.state = State::Stopping;
                        }
                    }
                    Control::Pause => {
                        monitor.gate.pause(true);
                        if matches!(status.state, State::Running | State::Starting) {
                            status.state = State::Paused;
                        }
                    }
                    Control::Run => {
                        monitor.gate.pause(false);
                        if status.state == State::Paused {
                            status.state = State::Running;
                        }
                    }
                }
                status.state.ended()
            };
            monitor.write();
            if ended {
                return;
            }
            std::thread::sleep(BEAT);
        });
    }

    /// Says where a hand-written loop is: epoch `epoch` of `epochs`, `done`
    /// of its `of` items, `iteration` steps in all. The first call takes the
    /// run out of `Starting`.
    pub fn report(&self, epoch: usize, epochs: usize, done: usize, of: usize, iteration: usize) {
        let mut status = self.status.lock().unwrap();
        status.epoch = epoch;
        status.epochs = epochs;
        status.done = done;
        status.of = of;
        status.validating = false;
        status.iteration = iteration;
        if status.state == State::Starting {
            status.state = State::Running;
        }
    }

    /// Writes down a loss, a training step's or a validation pass's, for the
    /// watcher's chart, and keeps it as the latest of its kind.
    pub fn record_loss(&self, validation: bool, epoch: usize, iteration: usize, loss: f64) {
        if !loss.is_finite() {
            return;
        }
        {
            let mut status = self.status.lock().unwrap();
            if validation {
                status.valid_loss = Some(loss);
            } else {
                status.train_loss = Some(loss);
            }
        }
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(LOSSES_FILE))
        {
            let kind = if validation { 'v' } else { 't' };
            writeln!(file, "{kind} {epoch} {iteration} {loss}").ok();
        }
    }

    /// Writes down an accuracy in %, a training step's or a validation
    /// pass's, for the watcher's chart (the line format of the losses).
    pub fn record_accuracy(&self, validation: bool, epoch: usize, iteration: usize, percent: f64) {
        if !percent.is_finite() {
            return;
        }
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(ACCURACY_FILE))
        {
            let kind = if validation { 'v' } else { 't' };
            writeln!(file, "{kind} {epoch} {iteration} {percent}").ok();
        }
    }

    /// Writes the status as it stands, stamped with the time.
    fn write(&self) {
        // Written under the lock, so that a stale copy cannot land after a
        // newer one — the last word of a run, above all.
        let mut status = self.status.lock().unwrap();
        status.updated = now();
        write_whole(&self.dir.join(STATUS_FILE), &status.to_text()).ok();
    }

    /// The valve the data loaders pass through: closed by a `pause`.
    pub fn gate(&self) -> Gate {
        self.gate.clone()
    }

    /// Says where the run's own folder is.
    pub fn set_run(&self, run: &Path) {
        self.status.lock().unwrap().run = Some(run.to_path_buf());
        self.write();
    }

    /// The renderer which feeds this monitor.
    pub fn renderer(&self) -> MonitorRenderer {
        MonitorRenderer {
            status: self.status.clone(),
            losses: fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(LOSSES_FILE))
                .ok(),
            accuracies: fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(ACCURACY_FILE))
                .ok(),
            line_ious: None,
            point_f1s: None,
            scale_errors: None,
            dir: self.dir.clone(),
            validating: false,
            steps: 0,
            loss_id: None,
            accuracy_id: None,
            line_iou_id: None,
            point_f1_id: None,
            scale_error_id: None,
        }
    }

    /// Records how the run ended, which the watcher reads as the last word:
    /// an error is a failure, a stop request a stop, and anything else is
    /// the end of the epochs.
    pub fn end(&self, result: &Result<(), String>) {
        let stopped = Control::read(&self.dir) == Control::Stop;
        {
            let mut status = self.status.lock().unwrap();
            match result {
                Err(message) => {
                    status.state = State::Failed;
                    status.message = Some(message.clone());
                }
                Ok(()) if stopped => status.state = State::Stopped,
                Ok(()) => status.state = State::Finished,
            }
        }
        self.write();
    }
}

/// A valve: data loaders go through it a crop at a time, and while it is
/// closed they wait, so that a run which cannot read more crops cannot take
/// more steps.
#[derive(Clone, Default)]
pub struct Gate(Arc<AtomicBool>);

impl Gate {
    fn pause(&self, paused: bool) {
        self.0.store(paused, Ordering::SeqCst);
    }

    /// Returns at once while the valve is open, and as it opens otherwise.
    pub fn wait(&self) {
        while self.0.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A dataset whose crops are handed out only while its [`Gate`] is open.
pub struct Gated<D> {
    inner: D,
    gate: Gate,
}

impl<D> Gated<D> {
    /// `inner`, behind `gate`.
    pub fn new(inner: D, gate: Gate) -> Gated<D> {
        Gated { inner, gate }
    }
}

impl<I, D: Dataset<I>> Dataset<I> for Gated<D> {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn get(&self, index: usize) -> Result<I, DatasetError> {
        self.gate.wait();
        self.inner.get(index)
    }
}

/// What burn's renderer is told, written down for the watcher.
pub struct MonitorRenderer {
    status: Arc<Mutex<Status>>,
    losses: Option<fs::File>,
    accuracies: Option<fs::File>,
    /// Opened when the metric is registered, which a run without a lines
    /// layer never does.
    line_ious: Option<fs::File>,
    /// The same, for a run with a points layer.
    point_f1s: Option<fs::File>,
    /// The same, for a run with a scale head.
    scale_errors: Option<fs::File>,
    /// Where the job folder is, for opening those files.
    dir: PathBuf,
    validating: bool,
    /// Training steps seen. burn's own iteration counts from one again in
    /// every epoch, which is no use on a chart of the whole run.
    steps: usize,
    /// Which metric is the loss, learned as the learner registers them.
    loss_id: Option<MetricId>,
    /// Which metric is the pixel accuracy.
    accuracy_id: Option<MetricId>,
    /// Which metric is the line IoU, where there is one.
    line_iou_id: Option<MetricId>,
    /// Which metric is the point F1, where there is one.
    point_f1_id: Option<MetricId>,
    /// Which metric is the scale error, where there is one.
    scale_error_id: Option<MetricId>,
}

impl MonitorRenderer {
    fn loss(&mut self, state: MetricState) {
        let MetricState::Numeric(entry, Some(value)) = state else {
            return;
        };
        // The epoch's own mean comes through as a final entry; the chart has
        // a point per step and that one would be a second at the same place.
        let value = match value {
            NumericEntry::Final(_) => return,
            value => value.current(),
        };
        if !value.is_finite() {
            return;
        }
        if self.accuracy_id.as_ref() == Some(&entry.metric_id) {
            let kind = if self.validating { 'v' } else { 't' };
            let status = self.status.lock().unwrap();
            if let Some(file) = &mut self.accuracies {
                writeln!(file, "{kind} {} {} {value}", status.epoch, status.iteration).ok();
            }
            return;
        }
        if self.line_iou_id.as_ref() == Some(&entry.metric_id) {
            let kind = if self.validating { 'v' } else { 't' };
            let status = self.status.lock().unwrap();
            if let Some(file) = &mut self.line_ious {
                writeln!(file, "{kind} {} {} {value}", status.epoch, status.iteration).ok();
            }
            return;
        }
        if self.point_f1_id.as_ref() == Some(&entry.metric_id) {
            let kind = if self.validating { 'v' } else { 't' };
            let status = self.status.lock().unwrap();
            if let Some(file) = &mut self.point_f1s {
                writeln!(file, "{kind} {} {} {value}", status.epoch, status.iteration).ok();
            }
            return;
        }
        if self.scale_error_id.as_ref() == Some(&entry.metric_id) {
            let kind = if self.validating { 'v' } else { 't' };
            let status = self.status.lock().unwrap();
            if let Some(file) = &mut self.scale_errors {
                writeln!(file, "{kind} {} {} {value}", status.epoch, status.iteration).ok();
            }
            return;
        }
        if self.loss_id.as_ref() != Some(&entry.metric_id) {
            return;
        }
        let mut status = self.status.lock().unwrap();
        let kind = if self.validating {
            status.valid_loss = Some(value);
            'v'
        } else {
            status.train_loss = Some(value);
            't'
        };
        if let Some(file) = &mut self.losses {
            writeln!(file, "{kind} {} {} {value}", status.epoch, status.iteration).ok();
        }
    }
}

impl MetricsRendererTraining for MonitorRenderer {
    fn update_train(&mut self, state: MetricState) {
        self.validating = false;
        self.loss(state);
    }

    fn update_valid(&mut self, state: MetricState) {
        self.validating = true;
        self.loss(state);
    }
}

impl MetricsRendererEvaluation for MonitorRenderer {
    fn update_test(&mut self, _name: EvaluationName, _state: MetricState) {}
}

impl TrainingProgressLogger for MonitorRenderer {
    fn start(
        &mut self,
        total_epochs: usize,
        starting_epoch: usize,
        _total_items: Option<usize>,
        _label: Option<&str>,
    ) {
        let mut status = self.status.lock().unwrap();
        status.epochs = total_epochs;
        // Epochs count from one, and the first split of each bumps this.
        status.epoch = starting_epoch.saturating_sub(1);
    }

    fn update_epoch(&mut self, _epoch: usize) {}

    fn start_split(&mut self, split: &str, total_items: usize) {
        self.validating = split == "valid";
        let mut status = self.status.lock().unwrap();
        if !self.validating {
            status.epoch += 1;
        }
        status.done = 0;
        status.of = total_items;
        status.validating = self.validating;
        if status.state == State::Starting {
            status.state = State::Running;
        }
    }

    fn update_split(&mut self, items_processed: usize) {
        let mut status = self.status.lock().unwrap();
        status.done = items_processed;
        if !self.validating {
            self.steps += 1;
            status.iteration = self.steps;
        }
    }

    fn end_split(&mut self) {}

    fn end(&mut self) {}

    fn log_event_training(&mut self, _event: String) {}
}

impl EvaluationProgressLogger for MonitorRenderer {
    fn start_global_progress(&mut self, _total_tests: usize) {}
    fn start_test(&mut self, _name: &str, _total_items: usize) {}
    fn update_test_progress(&mut self, _items_processed: usize) {}
    fn end_test(&mut self) {}
    fn end_global_progress(&mut self) {}
    fn log_event_evaluation(&mut self, _event: String) {}
}

impl MetricsRenderer for MonitorRenderer {
    fn manual_close(&mut self) {}

    fn register_metric(&mut self, definition: MetricDefinition) {
        if definition.name.as_str() == "Loss" {
            self.loss_id = Some(definition.metric_id);
        } else if definition.name.as_str() == "Pixel accuracy" {
            self.accuracy_id = Some(definition.metric_id);
        } else if definition.name.as_str() == "Line IoU" {
            self.line_iou_id = Some(definition.metric_id);
            self.line_ious = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(LINE_IOU_FILE))
                .ok();
        } else if definition.name.as_str() == "Point F1" {
            self.point_f1_id = Some(definition.metric_id);
            self.point_f1s = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(POINT_F1_FILE))
                .ok();
        } else if definition.name.as_str() == "Scale error" {
            self.scale_error_id = Some(definition.metric_id);
            self.scale_errors = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(SCALE_ERROR_FILE))
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_survives_its_own_text() {
        let status = Status {
            state: State::Paused,
            epoch: 3,
            epochs: 10,
            done: 40,
            of: 100,
            validating: true,
            iteration: 77,
            train_loss: Some(0.5),
            valid_loss: None,
            run: Some(PathBuf::from("trainings/UNet_x")),
            message: None,
            pid: Some(4242),
            updated: 12,
        };
        assert_eq!(Status::parse(&status.to_text()), Some(status));
        assert_eq!(Status::parse("nothing here"), None);
    }

    #[test]
    fn progress_is_counted_over_the_whole_run() {
        let mut status = Status {
            state: State::Running,
            epoch: 2,
            epochs: 4,
            done: 50,
            of: 100,
            ..Status::default()
        };
        assert_eq!(status.epoch_fraction(), 0.5);
        assert_eq!(status.overall_fraction(), 0.375);
        status.validating = true;
        assert_eq!(status.overall_fraction(), 0.5);
        status.state = State::Finished;
        assert_eq!(status.overall_fraction(), 1.0);
    }

    #[test]
    fn losses_parse_and_control_round_trips() {
        let point = LossPoint::parse("v 2 40 0.25").unwrap();
        assert!(point.validation && point.epoch == 2 && point.iteration == 40);
        assert_eq!(LossPoint::parse("x 1 1 1"), None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Control::read(dir.path()), Control::Run);
        Control::Pause.send(dir.path()).unwrap();
        assert_eq!(Control::read(dir.path()), Control::Pause);
    }

    #[test]
    fn a_closed_gate_holds_a_crop_until_it_opens() {
        let gate = Gate::default();
        gate.pause(true);
        let opener = gate.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            opener.pause(false);
        });
        let started = std::time::Instant::now();
        gate.wait();
        assert!(started.elapsed() >= Duration::from_millis(100));
        handle.join().unwrap();
    }
}
