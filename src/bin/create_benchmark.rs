//! Builds a benchmark archive out of maps and a ground truth renderer.
//!
//! ```text
//! create_benchmark [options] <renderer> <folder or map file>
//! ```
//!
//! The renderer is the C++/Qt `map_to_image` this project was ported from —
//! the reference images of a benchmark archive are what it draws, which is
//! the whole point of the archive: `benchmark` then measures this project
//! against them.
//!
//! What the second argument is decides where the maps come from:
//!
//! * a **folder** is searched, subfolders included, for every `.omap` and
//!   `.xmap` in it;
//! * a **map file** is taken apart into one map per symbol by
//!   [`mti::all_symbols`], this project's port of the C++
//!   `create_map_with_all_symbols`. `--symbols-tool` runs that C++ tool
//!   instead, which is how the two are checked against each other.
//!
//! Either way the maps are renamed to the rules in [`mti::naming`] — a zero
//! padded ordinal, `__`, and a name free of spaces — rendered one by one, and
//! written into
//!
//! ```text
//! benchmarks/<archive>.zip
//!     <archive>/README.txt      what the archive is and how it was made
//!     <archive>/maps/           the maps, under their corrected names
//!     <archive>/expected/       one reference image per map
//!     <archive>/index/          what is on each generated map, when there is one
//! ```
//!
//! which is what `benchmark` expects, so the archive it produces runs without
//! being corrected first.
//!
//! A map the renderer cannot draw is left out of the archive rather than put
//! in it without a reference image: the ordinals are handed out after the
//! rendering, so what is left has no hole in it. The README says which maps
//! went missing and why.
//!
//! Exit codes: 0 success, 1 usage or setup error, 2 a map failed to render.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use chrono::{Datelike, Timelike};
use clap::Parser;

use mti::naming::{self, Plan};
use mti::progress::{stage, Progress};
use mti::render::{DEFAULT_FRAME, DEFAULT_RESOLUTION};
use mti::report;

/// How many stages a run has, for the headings.
const STAGES: usize = 3;

/// Where the archives go when nowhere else was asked for. A folder of its
/// own, made when it is first needed: an archive is a few hundred megabytes
/// of maps which are usually not ours to distribute, and one gitignored
/// folder keeps them out of the working tree without a rule per archive.
const ARCHIVES: &str = "benchmarks";

/// The map file suffixes this tool collects, and `benchmark` reads.
const MAP_SUFFIXES: [&str; 2] = [".omap", ".xmap"];

/// What the rendering stage leaves behind: the maps which now have a
/// reference image, and, for each of the others, the reason it has none.
type Rendered = (Vec<Entry>, Vec<(String, String)>);

/// Where an archive's maps came from, for the note it carries about itself.
enum Origin {
    /// Every map found in a folder.
    Folder,
    /// One map per symbol of a map file, made here, or by the outside tool
    /// named.
    Symbols(Option<PathBuf>),
}

#[derive(Parser)]
#[command(
    name = "create_benchmark",
    version,
    about = "Builds a benchmark archive: a zip of maps and of the reference images a \
             ground truth renderer draws from them.\n\n\
             A folder is searched, subfolders included, for every map in it. A single \
             map file is taken apart into one map per symbol, each on a grid of test \
             objects. The names follow the rules benchmark checks, so the archive runs \
             without being corrected first."
)]
struct Args {
    /// The ground truth renderer: the C++ map_to_image executable.
    renderer: PathBuf,

    /// The maps: a folder, searched recursively, or a single map file, whose
    /// symbols each become a map of their own.
    source: PathBuf,

    /// The archive to write. Defaults to benchmarks/benchmark_<source
    /// name>.zip. Whichever folder it names is made if it is not there.
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Replace the archive if it is already there.
    #[arg(long)]
    force: bool,

    /// Resolution of the reference images, in pixels per meter on the ground.
    #[arg(short = 'r', long, default_value_t = DEFAULT_RESOLUTION)]
    resolution: f64,

    /// Width of the white frame added on each side, in meters on the ground.
    #[arg(short = 'f', long, default_value_t = DEFAULT_FRAME)]
    frame: f64,

    /// Make the per-symbol maps by running this outside tool, the C++
    /// create_map_with_all_symbols, rather than with the built-in one.
    #[arg(long, value_name = "PATH")]
    symbols_tool: Option<PathBuf>,

    /// Only use maps whose name contains this.
    #[arg(long, default_value = "", value_name = "TEXT")]
    filter: String,
}

/// One map on its way into the archive.
struct Entry {
    /// The map file itself: somewhere in the source folder, or in the
    /// temporary folder the symbol maps were generated into.
    map: PathBuf,
    /// What is on the map, where the map was generated with a description of
    /// its grid. `None` for a map which came out of the source folder.
    index: Option<PathBuf>,
    /// The name the map is known by until the naming rules give it its final
    /// one: its own file name without the suffix, made unique across the
    /// whole source folder, so that the map, its reference image and its
    /// description can be named after one another.
    name: String,
    /// The map file's suffix, kept as it was so that a `.xmap` stays one.
    suffix: String,
}

impl Entry {
    /// The file name the naming rules are applied to.
    fn file_name(&self) -> String {
        format!("{}{}", self.name, self.suffix)
    }

    /// The name of its reference image, before and after those rules.
    fn image_name(&self) -> String {
        format!("{}.png", self.name)
    }
}

/// Whether `path` is a map, and which suffix it wears. Compared without
/// regard to case, since a map exported on Windows may well be `.OMAP`.
fn map_suffix(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    MAP_SUFFIXES
        .iter()
        .find(|suffix| name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix))
        .map(|suffix| name[name.len() - suffix.len()..].to_string())
}

/// Collects every map under `folder`, subfolders included.
///
/// The order is a folder's own maps by name and then its subfolders by name,
/// which is only a starting point — the naming rules reorder what they are
/// given by the ordinals it already has — but it is a repeatable one, so the
/// same folder twice gives the same archive.
fn walk(folder: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let listing = std::fs::read_dir(folder).map_err(|e| format!("cannot read {}: {e}", folder.display()))?;
    for entry in listing {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", folder.display()))?;
        let path = entry.path();
        // Hidden files are the file manager's and the editor's business, and
        // a name which is not text is nothing this tool can put in an archive.
        if path.file_name().and_then(|n| n.to_str()).unwrap_or(".").starts_with('.') {
            continue;
        }
        // The file type of the entry itself, not of what it points at: a
        // symlink back up the tree would otherwise walk for ever.
        let kind = entry.file_type().map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if kind.is_dir() {
            folders.push(path);
        } else if map_suffix(&path).is_some() {
            files.push(path);
        }
    }
    files.sort();
    folders.sort();
    found.extend(files);
    for folder in &folders {
        walk(folder, found)?;
    }
    Ok(())
}

/// Turns collected map paths into entries, giving each one a name no other
/// map shares.
///
/// Two folders of a suite can each hold a `Contour.omap`, and both end up in
/// one flat `maps/` folder. Where that happens the name grows a folder to the
/// left of it, as many as it takes, since a folder is what told them apart in
/// the source and so is what a reader will recognise them by.
fn entries_from(paths: &[PathBuf], root: &Path) -> Vec<Entry> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut entries = Vec::new();
    for path in paths {
        let Some(suffix) = map_suffix(path) else { continue };
        let relative = path.strip_prefix(root).unwrap_or(path);
        let mut parents: Vec<String> = relative
            .parent()
            .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        let file = relative.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut name = file[..file.len() - suffix.len()].to_string();
        while taken.contains(&name) {
            match parents.pop() {
                Some(parent) => name = format!("{parent}_{name}"),
                None => {
                    // Nothing left to tell them apart with: the same map, in
                    // the same place, twice. A counter is all that is left.
                    let mut copy = 2;
                    while taken.contains(&format!("{name}_{copy}")) {
                        copy += 1;
                    }
                    name = format!("{name}_{copy}");
                }
            }
        }
        taken.insert(name.clone());
        let index = path.with_extension("txt");
        entries.push(Entry {
            map: path.clone(),
            index: index.is_file().then_some(index),
            name,
            suffix,
        });
    }
    entries
}

/// What an executable says its version is, which is also the cheapest way to
/// find out whether it can be run at all.
fn tool_version(tool: &Path) -> Result<String, String> {
    let output = Command::new(tool)
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", tool.display()))?;
    if !output.status.success() {
        return Err(format!("{} --version failed: {}", tool.display(), output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Every line the tool complained about, cleaned up for a report.
fn complaints(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Makes one map per symbol of `source`, into `into`, with this project's own
/// [`all_symbols`](mti::all_symbols).
fn generate_symbol_maps(source: &Path, into: &Path) -> Result<Vec<PathBuf>, String> {
    let mut progress: Option<Progress> = None;
    let summary = mti::all_symbols::create_maps(source, into, |_, total| {
        progress.get_or_insert_with(|| Progress::new("Symbols", total)).tick();
    })?;
    if let Some(bar) = progress {
        bar.finish();
    }
    for warning in &summary.warnings {
        println!("  warning: {warning}");
    }
    if !summary.skipped.is_empty() {
        println!(
            "  {} symbol{} could not be turned into objects",
            summary.skipped.len(),
            if summary.skipped.len() == 1 { "" } else { "s" }
        );
    }
    println!("  {} symbols, map scale 1:{}", summary.written, summary.scale_denominator);

    let mut paths = Vec::new();
    walk(into, &mut paths)?;
    if paths.is_empty() {
        return Err(format!("no maps could be made out of {}", source.display()));
    }
    Ok(paths)
}

/// The same, by running an outside tool instead: the C++
/// `create_map_with_all_symbols`, which is where the Rust one was ported
/// from, and which `--symbols-tool` exists to be able to check it against.
fn generate_symbol_maps_with(tool: &Path, source: &Path, into: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new(tool)
        .arg(source)
        .arg(into)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", tool.display()))?;
    if !output.status.success() {
        let mut reason = complaints(&output.stderr).join("; ");
        if reason.is_empty() {
            reason = output.status.to_string();
        }
        return Err(format!("{} failed on {}: {reason}", tool.display(), source.display()));
    }

    // The tool says which symbols it could make nothing of, one line each,
    // and finishes with a summary. Both are worth passing on: a suite which
    // is short of a dozen symbols should say so before it is measured.
    let notes = String::from_utf8_lossy(&output.stdout);
    let skipped = notes.lines().filter(|line| line.starts_with("Note: Skipped symbol")).count();
    if let Some(summary) = notes.lines().rev().find(|line| !line.trim().is_empty()) {
        if let Some((_, tail)) = summary.split_once(": ") {
            println!("  {tail}");
        }
    }
    if skipped > 0 {
        println!("  {skipped} symbol{} could not be turned into objects", if skipped == 1 { "" } else { "s" });
    }

    let mut paths = Vec::new();
    walk(into, &mut paths)?;
    if paths.is_empty() {
        return Err(format!("{} made no maps out of {}", tool.display(), source.display()));
    }
    Ok(paths)
}

/// Draws the reference image of every entry with the ground truth renderer.
///
/// Returns the entries it worked for, and the reason it did not work for the
/// others. A map with no reference image has no place in the archive: the
/// archive would break its own rules, and `benchmark` would report the map as
/// missing on every run for ever after.
fn render_all(
    renderer: &Path,
    entries: Vec<Entry>,
    images: &Path,
    args: &Args,
) -> Result<Rendered, String> {
    std::fs::create_dir_all(images).map_err(|e| format!("cannot make {}: {e}", images.display()))?;

    let mut rendered = Vec::new();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let total = entries.len();
    let mut progress = Progress::new("Rendering", total);
    for entry in entries {
        let image = images.join(entry.image_name());
        match render_one(renderer, &entry.map, &image, args) {
            Ok(said) => {
                for warning in said {
                    warnings.push(format!("{}: {warning}", entry.name));
                }
                rendered.push(entry);
            }
            Err(reason) => failures.push((entry.name.clone(), reason)),
        }
        progress.tick();
    }
    progress.finish();

    for warning in &warnings {
        println!("  warning: {warning}");
    }
    for (name, reason) in &failures {
        println!("  FAILED to render {name}: {reason}");
    }
    println!("  {} of {total} maps rendered", rendered.len());
    Ok((rendered, failures))
}

/// Renders one map, and says what the renderer had to say about it.
fn render_one(renderer: &Path, map: &Path, image: &Path, args: &Args) -> Result<Vec<String>, String> {
    let output = Command::new(renderer)
        .arg(map)
        .arg(image)
        .arg("-r")
        .arg(args.resolution.to_string())
        .arg("-f")
        .arg(args.frame.to_string())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", renderer.display()))?;
    let said = complaints(&output.stderr);
    if !output.status.success() {
        let reason = if said.is_empty() { output.status.to_string() } else { said.join("; ") };
        return Err(reason);
    }
    // A renderer which reports success without writing anything would put a
    // map into the archive whose reference image is not there.
    if !image.is_file() {
        return Err(format!("no image was written to {}", image.display()));
    }
    Ok(said)
}

/// Adds one file of the working folders to the archive.
fn add_file<W: std::io::Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    name: &str,
    path: &Path,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    writer.start_file(name, options).map_err(|e| format!("cannot write {name}: {e}"))?;
    let mut file = std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    std::io::copy(&mut file, writer).map_err(|e| format!("cannot write {name}: {e}"))?;
    Ok(())
}

/// Writes the archive: the maps, their reference images, whatever describes
/// them, and the note saying where all of it came from.
fn write_archive(
    archive: &Path,
    root: &str,
    entries: &BTreeMap<String, Entry>,
    plan: &Plan,
    images: &Path,
    about: &str,
) -> Result<(), String> {
    let file = std::fs::File::create(archive).map_err(|e| format!("cannot write {}: {e}", archive.display()))?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));

    // The entries are stamped with now rather than with the working folder's
    // idea of it, which is a temporary folder's and means nothing.
    let now = chrono::Local::now();
    let stamp = zip::DateTime::from_date_and_time(
        now.year().clamp(1980, 2107) as u16,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
    .unwrap_or_default();
    let options = zip::write::SimpleFileOptions::default().last_modified_time(stamp);

    let has_index = entries.values().any(|entry| entry.index.is_some());
    for folder in ["", "maps/", "expected/"].iter().chain(has_index.then_some(&"index/")) {
        writer
            .add_directory(format!("{root}/{folder}"), options)
            .map_err(|e| format!("cannot write {root}/{folder}: {e}"))?;
    }

    writer
        .start_file(format!("{root}/README.txt"), options)
        .map_err(|e| format!("cannot write {root}/README.txt: {e}"))?;
    std::io::Write::write_all(&mut writer, about.as_bytes()).map_err(|e| e.to_string())?;

    // Maps first and then images, so that the archive lists in the order it
    // is read in rather than in pairs.
    let described: Vec<&Entry> = entries.values().filter(|entry| entry.index.is_some()).collect();
    let total = plan.maps.len() * 2 + described.len();
    let mut progress = Progress::new("Writing", total);

    for renaming in &plan.maps {
        let entry = entries
            .get(&renaming.original)
            .ok_or_else(|| format!("{} is not one of the maps", renaming.original))?;
        add_file(&mut writer, &format!("{root}/maps/{}", renaming.corrected), &entry.map, options)?;
        progress.tick();
    }
    for renaming in &plan.maps {
        let entry = entries.get(&renaming.original).expect("checked above");
        let image = plan
            .corrected_expected(&entry.image_name())
            .ok_or_else(|| format!("{} has no reference image", renaming.corrected))?;
        add_file(&mut writer, &format!("{root}/expected/{image}"), &images.join(entry.image_name()), options)?;
        progress.tick();
    }
    for renaming in &plan.maps {
        let entry = entries.get(&renaming.original).expect("checked above");
        let (Some(index), Some(image)) = (&entry.index, plan.corrected_expected(&entry.image_name())) else {
            continue;
        };
        let described = format!("{}.txt", image.strip_suffix(".png").unwrap_or(image));
        add_file(&mut writer, &format!("{root}/index/{described}"), index, options)?;
        progress.tick();
    }
    progress.finish();

    writer.finish().map_err(|e| format!("cannot write {}: {e}", archive.display()))?;
    Ok(())
}

/// The note the archive carries about itself.
///
/// The reference images are only ground truth for the settings they were
/// drawn at: a benchmark run at another resolution or another frame compares
/// them against images of another size, and every pixel is then wrong. The
/// archive is passed around and run months later, so it says what it was made
/// with rather than leaving it to be remembered.
fn about_text(
    args: &Args,
    root: &str,
    source: &Path,
    renderer_version: &str,
    origin: &Origin,
    maps: usize,
    failures: &[(String, String)],
) -> String {
    let mut text = format!("Benchmark archive: {root}\n\n");
    text.push_str(&format!("  maps        {maps}\n"));
    text.push_str(&format!("  source      {}\n", source.display()));
    match origin {
        Origin::Folder => text.push_str("  mode        every map found in the source folder\n"),
        Origin::Symbols(None) => text.push_str(&format!(
            "  mode        one map per symbol of the source map, made by create_benchmark {}\n",
            env!("CARGO_PKG_VERSION")
        )),
        Origin::Symbols(Some(tool)) => text.push_str(&format!(
            "  mode        one map per symbol of the source map, made by {}\n",
            tool.display()
        )),
    }
    text.push_str(&format!("  renderer    {} ({renderer_version})\n", args.renderer.display()));
    text.push_str(&format!("  resolution  {} pixels per meter on the ground\n", args.resolution));
    text.push_str(&format!("  frame       {} meters on the ground\n", args.frame));
    if !args.filter.is_empty() {
        text.push_str(&format!("  filter      only maps whose name contains {:?}\n", args.filter));
    }
    text.push_str(&format!("  created     {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z")));
    text.push_str(&format!("  tool        create_benchmark {}\n", env!("CARGO_PKG_VERSION")));

    let paragraphs = [
        "maps/ holds the maps and expected/ holds one reference image per map, under the \
         same name with a .png suffix. Every name starts with a zero padded ordinal, then \
         __, then a name free of spaces; the ordinals run from zero upwards with no hole \
         and no repeat, all padded to the same width, so that the suite sorts the same way \
         as text as it does as numbers.",
        "The reference images are what the renderer above drew, at the resolution and the \
         frame above. They are ground truth for those two settings and for no others: a run \
         at another resolution, or with another frame, makes images of another size, and \
         then every pixel differs. Whatever is measured against this archive has to be run \
         with the same -r and the same -f.",
    ];
    text.push('\n');
    for paragraph in paragraphs {
        text.push_str(&report::wrap(paragraph, report::WIDTH, "  "));
        text.push_str("\n\n");
    }
    if matches!(origin, Origin::Symbols(_)) {
        text.push_str(&report::wrap(
            "index/ holds, for each map, what is on it: the symbol it was made for, and what \
             each row and column of its grid of objects is. Nothing reads it; it is there for \
             whoever is looking at a difference and wants to know what they are looking at.",
            report::WIDTH,
            "  ",
        ));
        text.push_str("\n\n");
    }
    if !failures.is_empty() {
        text.push_str(&report::wrap(
            "These maps are not in the archive: the renderer could not draw them, and a map \
             whose reference image is missing would break the rules above on every run.",
            report::WIDTH,
            "  ",
        ));
        text.push_str("\n\n");
        for (name, reason) in failures {
            text.push_str(&format!("  {name}: {reason}\n"));
        }
    }
    text.trim_end().to_string() + "\n"
}

/// `bytes` in whichever unit makes it a number a person can read.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "kB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The archive to write when none was asked for: named after the source, in
/// [`ARCHIVES`].
fn default_archive(source: &Path) -> PathBuf {
    let name = if source.is_dir() { source.file_name() } else { source.file_stem() };
    let name = name.map_or_else(|| "maps".to_string(), |n| n.to_string_lossy().into_owned());
    let name: String = name.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
    Path::new(ARCHIVES).join(format!("benchmark_{name}.zip"))
}

fn run() -> Result<ExitCode, String> {
    let args = Args::parse();

    if !args.resolution.is_finite() || args.resolution <= 0.0 {
        return Err(format!("the resolution must be greater than zero, not {}", args.resolution));
    }
    if !args.frame.is_finite() || args.frame < 0.0 {
        return Err(format!("the frame cannot be {} meters", args.frame));
    }

    // Absolute from here on: the README says where the maps came from, and a
    // relative path in it means nothing to whoever opens the archive next.
    let source = args
        .source
        .canonicalize()
        .map_err(|e| format!("cannot use {}: {e}", args.source.display()))?;

    let archive = args.output.clone().unwrap_or_else(|| default_archive(&source));
    if archive.exists() && !args.force {
        return Err(format!("{} is already there; --force replaces it", archive.display()));
    }
    if archive.is_dir() {
        return Err(format!("{} is a folder", archive.display()));
    }
    // The folder it goes in is made now rather than at the end, so that one
    // which cannot be made is found out about before a few hundred maps have
    // been rendered for it.
    let folder = archive.parent().unwrap_or_else(|| Path::new(""));
    if !folder.as_os_str().is_empty() {
        std::fs::create_dir_all(folder).map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    let root = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{} is not a file name", archive.display()))?;

    // Run before anything is made: an unusable renderer is the one failure
    // worth finding out about before a few hundred maps have been prepared.
    let renderer_version = tool_version(&args.renderer)?;

    println!("I will build a benchmark archive from {}", source.display());
    println!("and I will write it to {}", archive.display());
    println!();

    let working = tempfile::Builder::new()
        .prefix("create_benchmark_")
        .tempdir()
        .map_err(|e| format!("cannot make a temporary folder: {e}"))?;

    stage(1, STAGES, "Collect the maps");
    let mut origin = Origin::Folder;
    let (root_of_maps, paths) = if source.is_dir() {
        let mut paths = Vec::new();
        walk(&source, &mut paths)?;
        println!("  {} maps under {}", paths.len(), source.display());
        (source.clone(), paths)
    } else {
        let generated = working.path().join("symbols");
        let paths = match &args.symbols_tool {
            None => {
                println!("  one map per symbol of {}", source.display());
                origin = Origin::Symbols(None);
                generate_symbol_maps(&source, &generated)?
            }
            Some(tool) => {
                let version = tool_version(tool)?;
                println!("  {} ({version})", tool.display());
                origin = Origin::Symbols(Some(tool.clone()));
                generate_symbol_maps_with(tool, &source, &generated)?
            }
        };
        println!("  {} maps, one per symbol of {}", paths.len(), source.display());
        (generated, paths)
    };

    let mut entries = entries_from(&paths, &root_of_maps);
    if !args.filter.is_empty() {
        let before = entries.len();
        entries.retain(|entry| entry.name.contains(&args.filter));
        println!("  {} of {before} maps match {:?}", entries.len(), args.filter);
    }
    if entries.is_empty() {
        return Err(format!("there are no maps to put in {}", archive.display()));
    }
    println!();

    stage(2, STAGES, "Render the reference images");
    let images = working.path().join("expected");
    let (rendered, failures) = render_all(&args.renderer, entries, &images, &args)?;
    if rendered.is_empty() {
        return Err("not one map could be rendered; the archive would be empty".to_string());
    }
    println!();

    stage(3, STAGES, "Write the archive");
    // The names are handed out now rather than before the rendering, so that
    // the maps which could not be drawn leave no hole in the ordinals.
    let entries: BTreeMap<String, Entry> =
        rendered.into_iter().map(|entry| (entry.file_name(), entry)).collect();
    let map_names: Vec<String> = entries.keys().cloned().collect();
    let image_names: Vec<String> = entries.values().map(Entry::image_name).collect();
    let plan = naming::plan(&map_names, &image_names);
    let renamed = plan.maps.iter().filter(|renaming| renaming.changed()).count();
    println!("  {} maps, {renamed} renamed to the naming rules", plan.maps.len());

    let about = about_text(
        &args,
        &root,
        &source,
        &renderer_version,
        &origin,
        plan.maps.len(),
        &failures,
    );
    write_archive(&archive, &root, &entries, &plan, &images, &about)?;

    let written = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    println!("  {}: {} maps, {}", archive.display(), plan.maps.len(), size(written));
    println!();
    println!("Archive in {}", archive.display());
    println!("Run it with: benchmark -r {} -f {} {}", args.resolution, args.frame, archive.display());
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
