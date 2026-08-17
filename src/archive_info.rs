//! Reading the archive's own record of what it was made with back out of
//! `info.txt`.
//!
//! `create_benchmark` writes `info.txt` into every archive: what it is, how
//! it was made, and, on their own line each, the resolution and the frame
//! the reference images were drawn at. `benchmark` reads those two back with
//! [`parse`], so that a run measures at what they actually were rather than
//! at whatever was typed in by hand and may no longer agree.

use std::path::Path;

/// The two settings a reference image is only ground truth under: drawn at
/// another resolution, or with another frame, it is an image of another size,
/// and every pixel of a comparison against it is then wrong.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArchiveInfo {
    /// Pixels per meter on the ground the reference images were drawn at.
    pub resolution: f64,
    /// Width of the white frame around each one, in meters on the ground.
    pub frame: f64,
}

/// Parses `info.txt`'s content back into the two settings `benchmark` needs.
/// `None` for a value missing or not a number, which an archive made before
/// this file existed, or edited by hand, may well be.
///
/// Only the header — everything up to the first blank line — is looked at.
/// The rest is prose for whoever opens the archive, wrapped to a width that
/// does not know or care that "resolution" and "frame" mean something to a
/// program when they start a line; keeping the search to the header is what
/// stops such a line being misread as the setting.
pub fn parse(text: &str) -> Option<ArchiveInfo> {
    let header = text.split("\n\n").next().unwrap_or("");
    let value_of = |key: &str| {
        header.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(key)?;
            rest.split_whitespace().next()?.parse::<f64>().ok()
        })
    };
    Some(ArchiveInfo {
        resolution: value_of("resolution")?,
        frame: value_of("frame")?,
    })
}

/// Reads `info.txt` out of an archive at `root` (with a trailing slash, or
/// empty at the top of the zip) and parses it. `None` for an archive with no
/// `info.txt`, which one made before this file existed will not have.
pub fn read(archive: &Path, root: &str) -> Result<Option<ArchiveInfo>, String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("cannot read {}: {e}", archive.display()))?;
    let mut text = String::new();
    match zip.by_name(&format!("{root}info.txt")) {
        Ok(mut entry) => {
            std::io::Read::read_to_string(&mut entry, &mut text).map_err(|e| e.to_string())?
        }
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    Ok(parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_and_frame_survive_a_round_trip() {
        let written = "Benchmark archive info: suite\n  created    2026-08-15\n  resolution 12.5\n  frame      0.3\n\nMore about the archive.\n";
        assert_eq!(
            parse(written),
            Some(ArchiveInfo {
                resolution: 12.5,
                frame: 0.3
            })
        );
    }

    #[test]
    fn a_file_without_the_settings_parses_to_nothing() {
        assert_eq!(
            parse("Benchmark archive info: suite\n  created 2026-08-15\n"),
            None
        );
    }

    #[test]
    fn a_prose_line_which_happens_to_start_with_the_word_is_not_mistaken_for_the_setting() {
        let text = "Benchmark archive info: suite\n  resolution 12.5\n  frame      0.3\n\n\
                     frame around the described extent is added on every side.\n";
        assert_eq!(
            parse(text),
            Some(ArchiveInfo {
                resolution: 12.5,
                frame: 0.3
            })
        );
    }
}
