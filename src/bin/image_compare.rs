//! Compares a rendered image against a reference image. Ported from
//! `tests/image_compare.cpp`.
//!
//! Usage: `image_compare [--tolerance=N] [--threshold=F] <actual> <expected>`
//!
//! Exit codes: 0 the images match within the threshold, 1 usage error,
//! 2 an image could not be read, 3 the images differ in size, 4 too many
//! pixels differ.

use std::process::ExitCode;

/// Difference per color channel, summed over red, green and blue, above
/// which a pixel counts as significantly different.
///
/// Rasterizing the same shape twice may place an antialiased edge pixel
/// slightly differently. Such pixels differ by a small amount on a single
/// edge, while an actual rendering error changes whole areas by much more.
const DEFAULT_TOLERANCE: i32 = 60;

/// Fraction of significantly different pixels accepted by default.
const DEFAULT_THRESHOLD: f64 = 0.01;

#[derive(Default)]
struct Difference {
    /// Number of pixels which differ at all.
    any: u64,
    /// Number of pixels which differ beyond the tolerance.
    significant: u64,
    total: u64,
    /// Largest per-pixel difference found.
    peak: i32,
}

impl Difference {
    fn any_ratio(&self) -> f64 {
        if self.total > 0 { self.any as f64 / self.total as f64 } else { 0.0 }
    }
    fn significant_ratio(&self) -> f64 {
        if self.total > 0 { self.significant as f64 / self.total as f64 } else { 0.0 }
    }
}

fn load(path: &str) -> Result<image::RgbaImage, String> {
    let img = image::open(path).map_err(|_| format!("cannot read '{path}'"))?;
    Ok(img.to_rgba8())
}

fn compare(actual: &image::RgbaImage, expected: &image::RgbaImage, tolerance: i32) -> Difference {
    let mut result = Difference {
        total: (actual.width() as u64) * (actual.height() as u64),
        ..Default::default()
    };
    for (a, b) in actual.pixels().zip(expected.pixels()) {
        if a == b { continue; }
        result.any += 1;
        let delta = (a[0] as i32 - b[0] as i32).abs()
            + (a[1] as i32 - b[1] as i32).abs()
            + (a[2] as i32 - b[2] as i32).abs();
        if delta > tolerance {
            result.significant += 1;
        }
        if delta > result.peak {
            result.peak = delta;
        }
    }
    result
}

/// Reads the value of a `--name=value` option, or leaves `value` untouched.
fn take_option(argument: &str, name: &str, value: &mut Option<String>) -> bool {
    let prefix = format!("--{name}=");
    if let Some(rest) = argument.strip_prefix(&prefix) {
        *value = Some(rest.to_string());
        true
    } else {
        false
    }
}

fn run() -> Result<bool, (ExitCode, String)> {
    let mut tolerance = DEFAULT_TOLERANCE;
    let mut threshold = DEFAULT_THRESHOLD;
    let mut files = Vec::new();

    for argument in std::env::args().skip(1) {
        let mut value = None;
        if take_option(&argument, "tolerance", &mut value) {
            tolerance = value.unwrap().parse().map_err(|_| {
                (ExitCode::from(1), format!("image_compare: invalid argument '{argument}'"))
            })?;
        } else if take_option(&argument, "threshold", &mut value) {
            threshold = value.unwrap().parse().map_err(|_| {
                (ExitCode::from(1), format!("image_compare: invalid argument '{argument}'"))
            })?;
        } else if argument.starts_with("--") {
            return Err((ExitCode::from(1), format!("image_compare: invalid argument '{argument}'")));
        } else {
            files.push(argument);
        }
    }

    if files.len() != 2 {
        return Err((ExitCode::from(1), "Usage: image_compare [--tolerance=N] [--threshold=F] <actual> <expected>".to_string()));
    }

    let actual = load(&files[0]).map_err(|e| (ExitCode::from(2), format!("image_compare: {e}")))?;
    let expected = load(&files[1]).map_err(|e| (ExitCode::from(2), format!("image_compare: {e}")))?;

    if actual.dimensions() != expected.dimensions() {
        return Err((ExitCode::from(3), format!(
            "image_compare: size mismatch, {}x{} instead of {}x{}",
            actual.width(), actual.height(), expected.width(), expected.height()
        )));
    }

    let difference = compare(&actual, &expected, tolerance);
    let failed = difference.significant_ratio() > threshold;

    // The report goes to stdout in full, so that a passing run states the
    // settings it passed under -- a benchmark which silently tolerates a
    // generous threshold is worth noticing.
    println!("{}x{} pixels, {} in total", actual.width(), actual.height(), difference.total);
    println!("  differing pixels   {} ({:.4}%)", difference.any, 100.0 * difference.any_ratio());
    println!("  beyond tolerance   {} ({:.4}%), largest difference {}", difference.significant, 100.0 * difference.significant_ratio(), difference.peak);
    println!("  tolerance          {} per pixel, summed over red, green and blue", tolerance);
    println!("  threshold          {:.4}% of the pixels", 100.0 * threshold);
    println!("  verdict            {}", if failed { "FAIL" } else { "PASS" });

    Ok(!failed)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::from(0),
        Ok(false) => ExitCode::from(4),
        Err((code, message)) => {
            eprintln!("{message}");
            code
        }
    }
}
