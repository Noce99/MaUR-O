//! What the GPUs are doing, for the screen of a job to show beside it.
//!
//! The numbers come from `nvidia-smi`, asked once a second from a thread of
//! its own, because it takes a moment to answer and a screen which waited
//! for it would stutter. Nothing here needs a library or a build feature: a
//! machine without an NVIDIA driver has no `nvidia-smi`, the probe notices
//! at its first question, and the screen simply has no GPU panel.
//!
//! Which GPU is the job's? The job writes its own process id into its status
//! (see [`maur_o::net::monitor::Status::pid`]), the driver lists the compute
//! processes on every GPU with the memory each holds, and the two are joined
//! on the id. A job which does not appear — it runs on the CPU, or on a
//! backend the driver counts as graphics rather than compute — gets no
//! figures rather than somebody else's.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the driver is asked.
const EVERY: Duration = Duration::from_secs(1);

const GPU_QUERY: &str = "index,uuid,name,memory.used,memory.total,utilization.gpu,\
                         temperature.gpu,power.draw,power.limit,clocks.sm";
const APPS_QUERY: &str = "gpu_uuid,pid,used_memory";

/// One GPU, as of one moment. Every figure is `None` where the driver says
/// it does not have it (`[N/A]`, which is what a laptop's GPU says of its
/// power limit).
#[derive(Clone, Debug, PartialEq)]
pub struct Gpu {
    pub index: u32,
    pub uuid: String,
    pub name: String,
    /// Memory in use and in all, in MiB.
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    /// Share of the last sample period the GPU was busy, in percent.
    pub utilization: Option<u32>,
    /// In degrees Celsius.
    pub temperature: Option<u32>,
    /// What it draws and what it may, in watts.
    pub power: Option<f32>,
    pub power_limit: Option<f32>,
    /// The shader clock, in MHz.
    pub clock: Option<u32>,
}

/// One compute process on one GPU.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub uuid: String,
    pub pid: u32,
    /// The memory it holds, in MiB.
    pub memory: Option<u64>,
}

/// What the probe last saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Whether there is a driver to ask and it has answered at least once.
    pub available: bool,
    pub gpus: Vec<Gpu>,
    pub apps: Vec<App>,
}

impl Snapshot {
    /// The GPUs process `pid` has memory on, each with how much of it.
    pub fn of_process(&self, pid: u32) -> Vec<(&Gpu, Option<u64>)> {
        self.gpus
            .iter()
            .filter_map(|gpu| {
                self.apps
                    .iter()
                    .find(|app| app.pid == pid && app.uuid == gpu.uuid)
                    .map(|app| (gpu, app.memory))
            })
            .collect()
    }
}

/// A figure of the driver's csv, which is a number or a bracketed word for
/// why there is none.
fn number<T: std::str::FromStr>(field: &str) -> Option<T> {
    field.trim().parse().ok()
}

/// Reads what `nvidia-smi --query-gpu=<GPU_QUERY> --format=csv,noheader,nounits`
/// prints.
pub fn parse_gpus(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 10 {
                return None;
            }
            Some(Gpu {
                index: number(f[0])?,
                uuid: f[1].to_string(),
                name: f[2].to_string(),
                memory_used: number(f[3]),
                memory_total: number(f[4]),
                utilization: number(f[5]),
                temperature: number(f[6]),
                power: number(f[7]),
                power_limit: number(f[8]),
                clock: number(f[9]),
            })
        })
        .collect()
}

/// Reads what `nvidia-smi --query-compute-apps=<APPS_QUERY>
/// --format=csv,noheader,nounits` prints.
pub fn parse_apps(text: &str) -> Vec<App> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 3 {
                return None;
            }
            Some(App {
                uuid: f[0].to_string(),
                pid: number(f[1])?,
                memory: number(f[2]),
            })
        })
        .collect()
}

/// What `nvidia-smi` printed for `query`, or why it did not: `Err(true)` for
/// no such program, `Err(false)` for any other failure.
fn ask(kind: &str, query: &str) -> Result<String, bool> {
    let output = Command::new("nvidia-smi")
        .arg(format!("--query-{kind}={query}"))
        .arg("--format=csv,noheader,nounits")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.kind() == std::io::ErrorKind::NotFound)?;
    if !output.status.success() {
        return Err(false);
    }
    String::from_utf8(output.stdout).map_err(|_| false)
}

/// Asks the driver, on a thread of its own, for as long as it is held.
pub struct Probe {
    shared: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl Probe {
    /// Starts asking. Ends of its own accord at the first question if there
    /// is no `nvidia-smi` to ask.
    pub fn start() -> Probe {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (out, flag) = (shared.clone(), stop.clone());
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match ask("gpu", GPU_QUERY) {
                    Err(true) => return,
                    Err(false) => {}
                    Ok(gpus) => {
                        // A process list which fails to come is an empty
                        // one: the GPU's own figures are still worth having.
                        let apps = ask("compute-apps", APPS_QUERY).unwrap_or_default();
                        *out.lock().unwrap() = Snapshot {
                            available: true,
                            gpus: parse_gpus(&gpus),
                            apps: parse_apps(&apps),
                        };
                    }
                }
                // In small steps, so that a screen closed is a thread ended
                // within a quarter second rather than a whole one.
                for _ in 0..(EVERY.as_millis() / 250) {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        });
        Probe { shared, stop }
    }

    /// A probe which always says `snapshot`, and asks nobody.
    #[cfg(test)]
    pub fn fixed(snapshot: Snapshot) -> Probe {
        Probe {
            shared: Arc::new(Mutex::new(snapshot)),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }

    /// What it last saw.
    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().unwrap().clone()
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GPUS: &str = "0, GPU-aaa, NVIDIA GeForce GTX 1650, 4, 4096, 0, 36, 6.38, 85.00, 300\n\
                        1, GPU-bbb, NVIDIA GeForce RTX 4060 Ti, 513, 16380, 87, 54, [N/A], [N/A], 2310\n";
    const APPS: &str = "GPU-bbb, 4242, 420\nGPU-bbb, 99, 90\n";

    #[test]
    fn the_drivers_lines_are_read_and_its_missing_figures_are_none() {
        let gpus = parse_gpus(GPUS);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[1].name, "NVIDIA GeForce RTX 4060 Ti");
        assert_eq!(gpus[1].memory_used, Some(513));
        assert_eq!(gpus[1].utilization, Some(87));
        assert_eq!(gpus[1].power, None);
        assert_eq!(gpus[0].power_limit, Some(85.0));
        assert_eq!(parse_gpus("garbage\n\n"), []);

        let apps = parse_apps(APPS);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].memory, Some(420));
        assert_eq!(parse_apps("No devices were found\n"), []);
    }

    #[test]
    fn a_process_is_found_on_the_gpu_it_holds_memory_on() {
        let snapshot = Snapshot {
            available: true,
            gpus: parse_gpus(GPUS),
            apps: parse_apps(APPS),
        };
        let found = snapshot.of_process(4242);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.index, 1);
        assert_eq!(found[0].1, Some(420));
        assert!(snapshot.of_process(7).is_empty());
    }
}
