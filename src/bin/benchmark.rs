//! Runs a whole benchmark suite out of a zip archive.
//!
//! ```text
//! benchmark [options] <archive.zip>
//! ```
//!
//! The archive holds a `maps/` folder of `.omap` files and an `expected/`
//! folder with a reference image for each of them, named by the rules in
//! [`mti::naming`]. An archive which breaks the rules is not rejected: the
//! names it should have had are printed, and a corrected copy of the archive
//! is written next to it as `<name>_corrected.zip`, before the run carries
//! on with the corrected names.
//!
//! The archive is unpacked into a temporary folder, every map is rendered,
//! and the results go into
//!
//! ```text
//! Results/<archive name>_YYYY_MM_DD__hh_mm_ss/
//!     naming.txt      what was wrong with the names, when something was
//!     results.txt     a row per map: how much differs, and by how much
//!     predictions/    the rendered maps
//!     differences/    where they disagree with expected/, see mti::differences
//! ```
//!
//! The three stages announce themselves and then get on with it: the long
//! two draw a progress bar, and everything they would otherwise have to say
//! per map goes into one of the two text files, which is where it is still
//! readable after a couple of hundred maps.
//!
//! Exit codes: 0 success, 1 usage or archive error, 2 a map failed to render.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use mti::archive_info;
use mti::differences::{self, Options};
use mti::naming::{self, Plan};
use mti::progress::{stage, Progress};
use mti::report;
use mti::render::{render_map, save_pixmap, DEFAULT_FRAME, DEFAULT_RESOLUTION};

/// How many stages a run has, for the headings.
const STAGES: usize = 3;

/// One setting in `info.txt`: its name, its value in words, and what it does.
type Setting = (&'static str, String, String);

/// A group of settings under a heading.
type Section = (&'static str, Vec<Setting>);

#[derive(Parser)]
#[command(
    name = "benchmark",
    version,
    about = "Renders a benchmark suite out of a zip archive and reports where the \
             renderings differ from the reference images.\n\n\
             The archive holds a maps/ folder of .omap files and an expected/ folder \
             with a reference image for each of them. Names which break the naming \
             rules are reported and written out as a corrected copy of the archive."
)]
struct Args {
    /// The benchmark archive: a .zip holding maps/ and expected/.
    archive: PathBuf,

    /// Where the run folder is created.
    #[arg(long, default_value = "Results")]
    results: PathBuf,

    /// Check and correct the names only; do not render anything.
    #[arg(long)]
    names_only: bool,

    /// Per pixel difference, summed over red, green and blue, which does not
    /// count as a difference.
    #[arg(long, default_value_t = differences::DEFAULT_TOLERANCE, value_name = "N")]
    tolerance: i32,

    /// Count differences antialiasing explains as real ones, i.e. report
    /// every differing pixel rather than only the ones worth looking at.
    #[arg(long)]
    keep_antialiasing: bool,

    /// How many regions to crop out, 0 for as many as it takes to cover
    /// every difference.
    #[arg(long, default_value_t = 0, value_name = "N")]
    crops: usize,

    /// The side of a cropped region, in pixels.
    #[arg(long, default_value_t = 128, value_name = "N")]
    crop_size: u32,

    /// The width a cropped region is enlarged to.
    #[arg(long, default_value_t = 512, value_name = "N")]
    zoom: u32,

    /// The width the whole images are scaled down to in side_by_side.png, 0
    /// for their full size.
    #[arg(long, default_value_t = 2000, value_name = "N")]
    overview: u32,

    /// Only use maps whose name contains this.
    #[arg(long, default_value = "", value_name = "TEXT")]
    filter: String,
}

/// Where in an archive the two folders are, and what is in them.
struct Contents {
    /// What comes before `maps/` and `expected/` inside the archive, with a
    /// trailing slash, empty when they are at the top of it.
    root: String,
    /// The file names in `maps/`, without the folder.
    maps: Vec<String>,
    /// The file names in `expected/`, without the folder.
    expected: Vec<String>,
}

/// Splits an archive entry into the folder it is directly in and its file name.
fn parent_and_name(entry: &str) -> Option<(&str, &str)> {
    let entry = entry.trim_end_matches('/');
    let cut = entry.rfind('/')?;
    Some((&entry[..cut], &entry[cut + 1..]))
}

/// Reads what an archive holds, without unpacking it.
fn read_contents(archive: &Path) -> Result<Contents, String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("cannot read {}: {e}", archive.display()))?;

    let mut roots = std::collections::BTreeSet::new();
    let mut maps = Vec::new();
    let mut expected = Vec::new();
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        // Archives made on macOS carry a shadow copy of every file.
        if name.starts_with("__MACOSX/") || name.contains("/.") || name.starts_with('.') {
            continue;
        }
        let Some((parent, file_name)) = parent_and_name(&name) else { continue };
        let (folder, root) = match parent.rfind('/') {
            Some(cut) => (&parent[cut + 1..], &parent[..cut + 1]),
            None => (parent, ""),
        };
        let lower = file_name.to_ascii_lowercase();
        if folder.eq_ignore_ascii_case("maps") && (lower.ends_with(".omap") || lower.ends_with(".xmap")) {
            roots.insert(root.to_string());
            maps.push(file_name.to_string());
        } else if folder.eq_ignore_ascii_case("expected") && lower.ends_with(".png") {
            roots.insert(root.to_string());
            expected.push(file_name.to_string());
        }
    }

    if maps.is_empty() {
        return Err(format!("{} holds no maps/*.omap", archive.display()));
    }
    if roots.len() > 1 {
        let list: Vec<String> = roots.iter().map(|r| format!("{r:?}")).collect();
        return Err(format!(
            "{} holds more than one maps/expected pair, under {}",
            archive.display(),
            list.join(" and ")
        ));
    }
    let root = roots.into_iter().next().unwrap_or_default();
    Ok(Contents { root, maps, expected })
}

/// Where a rendering setting came from, for `info.txt`'s own record of the run.
enum Source {
    /// The archive's own info.txt, made when it was created.
    Archive,
    /// The archive carries no info.txt, so the built-in default was used.
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::Archive => "from the archive",
            Source::Default => "default; the archive carries no info.txt",
        })
    }
}

/// Resolves one setting: what the archive says, if it says anything,
/// otherwise the built-in default.
fn resolve(archived: Option<f64>, default: f64) -> (f64, Source) {
    match archived {
        Some(value) => (value, Source::Archive),
        None => (default, Source::Default),
    }
}

/// Writes what the run was asked to do: every setting, with what it means.
///
/// A results table read a month later is worth what is known about how it was
/// produced — a rendering at half the resolution, or a comparison at a
/// tolerance of 60, is a different measurement, and nothing else in the run
/// folder records which one it was.
fn write_info(
    args: &Args,
    title: &str,
    run: &Path,
    started: chrono::DateTime<chrono::Local>,
    maps: usize,
    resolution: (f64, &Source),
    frame: (f64, &Source),
) -> Result<(), String> {
    let mut text = format!("Benchmark settings: {title}\n\n");
    text.push_str(&format!("  archive     {}\n", args.archive.display()));
    text.push_str(&format!("  results     {}\n", run.display()));
    text.push_str(&format!("  started     {}\n", started.format("%Y-%m-%d %H:%M:%S %z")));
    text.push_str(&format!("  tool        benchmark {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!("  maps        {maps} in the archive\n"));
    if args.names_only {
        text.push_str("  mode        --names-only: the names were checked, nothing was rendered\n");
    }

    // Each setting is its value in words and what the value does, so that the
    // file answers "what was this run" without a manual next to it.
    let sections: [Section; 3] = [
        (
            "Rendering",
            vec![
                (
                    "resolution",
                    format!("{} pixels per meter on the ground ({})", resolution.0, resolution.1),
                    "How many image pixels one meter of terrain becomes. It is a ground measure, \
                     not a paper one: the map's own scale (1:15000 and so on) converts it to the \
                     paper millimetres the map file is drawn in, so the same value gives images \
                     of the same detail whatever a map's scale. Doubling it doubles each side of \
                     every image, and quadruples the work the comparison then has to do."
                        .to_string(),
                ),
                (
                    "frame",
                    format!("{} meters on the ground ({})", frame.0, frame.1),
                    "The width of the white margin added on each side of the map's own extent. \
                     It sets the image size together with the resolution, so a rendering only \
                     lines up with a reference image made with the same frame: a different one \
                     shifts every pixel, and makes every pixel wrong."
                        .to_string(),
                ),
                (
                    "filter",
                    if args.filter.is_empty() {
                        "(none)".to_string()
                    } else {
                        format!("{:?}", args.filter)
                    },
                    "Only maps whose file name contains this text are rendered and compared. \
                     With no filter the whole suite runs."
                        .to_string(),
                ),
            ],
        ),
        (
            "Comparison",
            vec![
                (
                    "tolerance",
                    args.tolerance.to_string(),
                    format!(
                        "The error a pixel is allowed before it counts as wrong. The error of a \
                         pixel is its difference summed over red, green and blue, so it runs from \
                         0 to 765. The default of {} is one unit per channel: two renderers land \
                         a unit apart on a flat area of one colour often enough, purely from \
                         where each one's float-to-integer conversion of that colour falls, and \
                         forgiving it stops a whole area being reported over a rounding \
                         difference nobody can see. At 0 any difference at all is wrong.",
                        differences::DEFAULT_TOLERANCE
                    ),
                ),
                (
                    "antialiasing",
                    if args.keep_antialiasing {
                        "counted as real (--keep-antialiasing)".to_string()
                    } else {
                        "classified and set aside".to_string()
                    },
                    "A wrong pixel is put down to antialiasing when both renderings have a colour \
                     step in the 3x3 window around it, i.e. when both drew an edge there and can \
                     only be disagreeing about how much of the pixel it covers. Requiring it of \
                     both is what carries the weight: where only one has an edge, one of them drew \
                     something the other did not. Those pixels are counted separately in \
                     results.txt, drawn dim orange rather than red in diff.png, and not used to \
                     choose which regions get cropped out; a map whose every difference is one of \
                     them gets no folder in differences/ at all. Up to a pixel of positional \
                     disagreement is forgiven along with it, since no local test can tell a shape \
                     drawn a pixel out of place from a coverage disagreement. \
                     --keep-antialiasing turns the whole classification off."
                        .to_string(),
                ),
            ],
        ),
        (
            "Difference report",
            vec![
                (
                    "crops",
                    if args.crops == 0 {
                        "0 (as many as it takes)".to_string()
                    } else {
                        args.crops.to_string()
                    },
                    "How many regions of an image are cropped out and enlarged, worst region \
                     first. At 0 there are as many as it takes for every wrong pixel to be \
                     inside one, so that no difference goes unlooked at."
                        .to_string(),
                ),
                (
                    "crop size",
                    format!("{} pixels", args.crop_size),
                    "The side of one cropped region, in pixels of the original image. Small \
                     enough to see individual pixels, large enough to see what they are part of."
                        .to_string(),
                ),
                (
                    "zoom",
                    format!("{} pixels", args.zoom),
                    format!(
                        "The width a cropped region is enlarged to, by a whole-number nearest \
                         neighbour scale, so a {} pixel crop is drawn {} times life size. \
                         Nearest neighbour on purpose: these differences are one pixel wide, and \
                         any smoothing would blur away exactly what is being looked at.",
                        args.crop_size,
                        (args.zoom / args.crop_size.max(1)).max(1)
                    ),
                ),
                (
                    "overview",
                    if args.overview == 0 {
                        "0 (full size)".to_string()
                    } else {
                        format!("{} pixels", args.overview)
                    },
                    "The width the two whole images are scaled down to in side_by_side.png. That \
                     image is there to show where in the map to look, while the crops show the \
                     differences themselves; at 0 it is written at full size, which for a real \
                     map is a hundred megapixels of it."
                        .to_string(),
                ),
            ],
        ),
    ];

    // Wide enough for the longest name, so the values line up down the whole
    // file rather than down one section of it.
    let label = sections
        .iter()
        .flat_map(|(_, settings)| settings.iter().map(|(name, _, _)| name.len()))
        .max()
        .unwrap_or(0);

    // Every block below ends with a blank line of its own, so the sections
    // only need one to separate them from what came before.
    text.push('\n');
    for (heading, settings) in &sections {
        text.push_str(&format!("{heading}\n\n"));
        for (name, value, description) in settings {
            text.push_str(&format!("  {name:<label$} {value}\n"));
            text.push_str(&report::wrap(description, report::WIDTH, "      "));
            text.push_str("\n\n");
        }
    }

    let path = run.join("info.txt");
    std::fs::write(&path, text.trim_end().to_string() + "\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Writes the whole naming report, every problem and the fix for it, to a
/// file. The terminal only gets told how many there were and where to look.
fn write_naming_report(archive: &Path, plan: &Plan, path: &Path) -> Result<(), String> {
    let problems = plan.problems();
    let mut text = format!("Naming problems in {}\n\n", archive.display());
    text.push_str(&format!("{} problems, of {} kinds:\n", problems.len(), plan.summary().len()));
    for (kind, count) in plan.summary() {
        text.push_str(&format!("  {count:>5}  {kind}\n"));
    }
    text.push_str("\nEach one, with the name it was corrected to:\n\n");
    for problem in &problems {
        text.push_str(&format!("  {problem}\n"));
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Writes a copy of the archive under the corrected names.
fn write_corrected(archive: &Path, contents: &Contents, plan: &Plan) -> Result<PathBuf, String> {
    let stem = archive.file_stem().unwrap_or_default().to_string_lossy();
    let corrected = archive.with_file_name(format!("{stem}_corrected.zip"));

    let source = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(source)).map_err(|e| e.to_string())?;
    let target = std::fs::File::create(&corrected)
        .map_err(|e| format!("cannot write {}: {e}", corrected.display()))?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(target));

    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            // Kept, so that the corrected archive has the same shape as the
            // one it was made from.
            let name = entry.name().to_string();
            drop(entry);
            writer
                .add_directory(name, zip::write::SimpleFileOptions::default())
                .map_err(|e| e.to_string())?;
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let renamed = match parent_and_name(&name) {
            Some((parent, file_name)) if parent == format!("{}maps", contents.root) => plan
                .corrected_map(file_name)
                .map(|corrected| format!("{parent}/{corrected}")),
            Some((parent, file_name)) if parent == format!("{}expected", contents.root) => plan
                .corrected_expected(file_name)
                .map(|corrected| format!("{parent}/{corrected}")),
            _ => None,
        }
        .unwrap_or_else(|| name.clone());
        // Copied compressed as it is: the payload is unchanged, only its name
        // is, and these archives are hundreds of megabytes.
        writer.raw_copy_file_rename(entry, renamed).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(corrected)
}

/// Unpacks `maps/` and `expected/` into `into`, under their corrected names.
fn extract(archive: &Path, contents: &Contents, plan: &Plan, into: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(into.join("maps")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(into.join("expected")).map_err(|e| e.to_string())?;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let target = match parent_and_name(&name) {
            Some((parent, file_name)) if parent == format!("{}maps", contents.root) => {
                plan.corrected_map(file_name).map(|c| into.join("maps").join(c))
            }
            Some((parent, file_name)) if parent == format!("{}expected", contents.root) => {
                plan.corrected_expected(file_name).map(|c| into.join("expected").join(c))
            }
            _ => None,
        };
        let Some(target) = target else { continue };
        let mut out = std::fs::File::create(&target)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Renders every map of `maps` into `predictions`. Returns the maps which
/// could not be rendered, with the reason.
fn render_all(
    maps: &Path,
    predictions: &Path,
    resolution: f64,
    frame: f64,
    filter: &str,
) -> Result<Vec<(String, String)>, String> {
    std::fs::create_dir_all(predictions).map_err(|e| e.to_string())?;

    // Sorted by name, which the naming rules make the order of the suite.
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in std::fs::read_dir(maps).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.contains(filter) {
            continue;
        }
        files.insert(name.to_string(), path);
    }

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let total = files.len();
    let mut progress = Progress::new("Rendering", total);
    for (name, path) in &files {
        let stem = Path::new(name).file_stem().unwrap().to_string_lossy();
        match render_map(path, resolution, frame) {
            Ok(rendering) => {
                for warning in &rendering.warnings {
                    warnings.push(format!("{name}: {warning}"));
                }
                let image = predictions.join(format!("{stem}.png"));
                if let Err(e) = save_pixmap(&rendering.pixmap, &image) {
                    failures.push((name.clone(), e));
                }
            }
            Err(e) => failures.push((name.clone(), e.to_string())),
        }
        progress.tick();
    }
    progress.finish();

    // Anything gone wrong is worth interrupting the quiet for, and there is
    // never much of it: a suite where every map warns is a broken suite.
    for warning in &warnings {
        println!("  warning: {warning}");
    }
    for (name, reason) in &failures {
        println!("  FAILED to render {name}: {reason}");
    }
    println!("  {} of {total} maps rendered", total - failures.len());
    Ok(failures)
}

fn run() -> Result<ExitCode, String> {
    let args = Args::parse();

    // Read before anything is created, so that an archive which is not one
    // does not leave an empty run folder behind.
    let contents = read_contents(&args.archive)?;
    let archived = archive_info::read(&args.archive, &contents.root)?;
    let (resolution, resolution_source) = resolve(archived.map(|i| i.resolution), DEFAULT_RESOLUTION);
    let (frame, frame_source) = resolve(archived.map(|i| i.frame), DEFAULT_FRAME);

    let title = args.archive.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let started = chrono::Local::now();
    let run = args.results.join(format!("{title}_{}", started.format("%Y_%m_%d__%H_%M_%S")));
    std::fs::create_dir_all(&run).map_err(|e| format!("cannot make {}: {e}", run.display()))?;

    println!("I will run the benchmarks on {}", args.archive.display());
    println!("and I will save the results in {}", run.display());
    println!();

    // Written before anything else, so that a run which is interrupted still
    // says what it was asked to do.
    write_info(
        &args,
        &title,
        &run,
        started,
        contents.maps.len(),
        (resolution, &resolution_source),
        (frame, &frame_source),
    )?;

    stage(1, STAGES, "Check the benchmark archive naming rules");
    let plan = naming::plan(&contents.maps, &contents.expected);
    if plan.is_valid() {
        println!("  {} maps, names are as they should be", plan.maps.len());
    } else {
        let report = run.join("naming.txt");
        write_naming_report(&args.archive, &plan, &report)?;
        let corrected = write_corrected(&args.archive, &contents, &plan)?;
        println!("  {} problems found and corrected, listed in {}", plan.problems().len(), report.display());
        println!("  Corrected archive: {}", corrected.display());
    }
    if args.names_only {
        return Ok(ExitCode::SUCCESS);
    }
    println!();

    let unpacked = tempfile::Builder::new()
        .prefix("benchmark_")
        .tempdir()
        .map_err(|e| format!("cannot make a temporary folder: {e}"))?;
    extract(&args.archive, &contents, &plan, unpacked.path())?;

    stage(2, STAGES, "Map rendering");
    let predictions = run.join("predictions");
    let failures = render_all(&unpacked.path().join("maps"), &predictions, resolution, frame, &args.filter)?;
    println!();

    stage(3, STAGES, "Renders comparison with expected ones");
    let options = Options {
        tolerance: args.tolerance,
        crops: args.crops,
        crop_size: args.crop_size,
        zoom: args.zoom,
        overview: args.overview,
        filter: args.filter.clone(),
        keep_antialiasing: args.keep_antialiasing,
    };
    let report = differences::compare(
        &unpacked.path().join("expected"),
        &predictions,
        &run.join("differences"),
        &options,
    )?;
    let results = run.join("results.txt");
    differences::write_results(&report, &title, &options, &results)?;

    print!(
        "  {} image{}: {} identical, {} antialiasing only, {} differing",
        report.total,
        if report.total == 1 { "" } else { "s" },
        report.identical,
        report.antialiasing_only,
        report.differing
    );
    if report.missing.is_empty() {
        println!();
    } else {
        println!(", {} not rendered", report.missing.len());
    }
    if !report.no_overview.is_empty() {
        println!(
            "  {} map{} too large for a side_by_side.png; only the crops were written",
            report.no_overview.len(),
            if report.no_overview.len() == 1 { "" } else { "s" }
        );
    }
    println!("  Per map: {}", results.display());
    println!();
    println!("Results in {}", run.display());
    Ok(if failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(2) })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("Error: {message}");
            ExitCode::from(1)
        }
    }
}
