//! The `contours_to_raster` config file: the thirteen parameters
//! `Contours-to-Raster.md` names, read from a small hand-rolled `key =
//! value` format (no `serde`/`toml` dependency exists anywhere else in this
//! crate, and one file with thirteen numbers does not need one).

use std::path::Path;

/// Default location of the config file, relative to the current directory.
pub const DEFAULT_CONFIG_PATH: &str = "config/contours_to_raster.conf";

/// The algorithm's tunable parameters. See the doc comment on each field,
/// and `Contours-to-Raster.md`'s own "## Parameters" section, for what each
/// one means.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Maximum chord length, in ground meters, used to flatten a cubic
    /// Bezier segment into straight lines (Appendix 1).
    pub bezier_linearization_step: f64,
    /// The equally-spaced node distance every contour's final `ls` is
    /// resampled to, in ground meters (Appendix 1).
    pub contours_step: f64,
    /// Pixel size of the Contour Raster, in ground meters (Step 0).
    pub rasterization_px_size: f64,
    /// How finely a contour's `ls` is re-densified before writing it to the
    /// Contour Raster, as a multiple of `rasterization_px_size` (Appendix 3).
    pub rasterization_step_factor: f64,
    /// Buffer width, in ground meters, used to turn a Jump's or a Heavy
    /// Object's own `ls` into a polygon (Appendix 2's `width`) -- for a
    /// Jump, the area Step 1 scans to find which contours it touches; for a
    /// Heavy Object, the area Step 0 searches for an intersecting contour,
    /// instead of only the pixels directly under its digitized line.
    pub heavy_object_width: f64,
    /// Extra padding, in ground meters, buffered on top of
    /// `heavy_object_width` (Appendix 2's `extra_growing`).
    pub heavy_object_growing: f64,
    /// How many `Coord`s on each side of a Heavy Object/contour
    /// intersection are used to fit a circle (Step 0).
    pub circumference_fitting_points_number: usize,
    /// How far, in ground meters, around a Slope Line's own position Step 0
    /// searches for the nearest Contour Raster pixel to attribute its
    /// gravity reading to, since that position is not always pixel-exact on
    /// top of its contour (Step 0).
    pub slope_lines_contours_search_radius: f64,
    /// Distance a rain drop advances per simulation step, in ground meters
    /// (Step 2).
    pub rain_drop_step: f64,
    /// How many sources are placed per contour segment (Step 2).
    pub sources_per_contour_segment: usize,
    /// How many `rain_drop_step`-sized steps a rain drop is exempt from
    /// evaporating on crossing its own starting contour, or on re-crossing
    /// an undefined contour it has already voted for (Step 2).
    pub rain_drop_starting_voting_hysteresis: u64,
    /// How close left and right vote counts must be, as a ratio in (0, 1),
    /// before being flagged as ambiguous (Step 2).
    pub undefined_gravity_vote_threshold: f64,
    /// How close, in ground meters, a Contour Raster pixel conflict must be
    /// to *both* contours' own start or end node for Step 0 to unify them
    /// into one contour instead of crashing (Step 0) -- real contour
    /// digitizing sometimes splits one physical line into two objects whose
    /// endpoints are close but not exactly coincident.
    pub contour_gap_merge_radius: f64,
}

/// The keys `Config::load` requires, in the order they are checked, paired
/// with the field they fill.
const KEYS: &[&str] = &[
    "bezier_linearization_step",
    "contours_step",
    "rasterization_px_size",
    "rasterization_step_factor",
    "heavy_object_width",
    "heavy_object_growing",
    "circumference_fitting_points_number",
    "slope_lines_contours_search_radius",
    "rain_drop_step",
    "sources_per_contour_segment",
    "rain_drop_starting_voting_hysteresis",
    "undefined_gravity_vote_threshold",
    "contour_gap_merge_radius",
];

impl Config {
    /// Parses a config file: one `key = value` per line, blank lines and
    /// lines starting with `#` ignored. All thirteen keys are required -- a
    /// config file missing one is far more likely a mistake than an
    /// intentional partial override -- and an unknown key or an unparseable
    /// value is an error naming the offending line.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config file {}: {e}", path.display()))?;
        Config::parse(&text, &path.display().to_string())
    }

    fn parse(text: &str, source_name: &str) -> Result<Config, String> {
        let mut values: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
        for (line_no, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!(
                    "{source_name}:{}: expected `key = value`, got {raw_line:?}",
                    line_no + 1
                ));
            };
            let key = key.trim();
            let value = value.trim();
            if !KEYS.contains(&key) {
                return Err(format!(
                    "{source_name}:{}: unknown key {key:?}",
                    line_no + 1
                ));
            }
            let parsed: f64 = value.parse().map_err(|_| {
                format!(
                    "{source_name}:{}: cannot parse {value:?} as a number for {key:?}",
                    line_no + 1
                )
            })?;
            values.insert(key, parsed);
        }

        for key in KEYS {
            if !values.contains_key(key) {
                return Err(format!("{source_name}: missing required key {key:?}"));
            }
        }

        Ok(Config {
            bezier_linearization_step: values[&"bezier_linearization_step"],
            contours_step: values[&"contours_step"],
            rasterization_px_size: values[&"rasterization_px_size"],
            rasterization_step_factor: values[&"rasterization_step_factor"],
            heavy_object_width: values[&"heavy_object_width"],
            heavy_object_growing: values[&"heavy_object_growing"],
            circumference_fitting_points_number: values[&"circumference_fitting_points_number"]
                as usize,
            slope_lines_contours_search_radius: values[&"slope_lines_contours_search_radius"],
            rain_drop_step: values[&"rain_drop_step"],
            sources_per_contour_segment: values[&"sources_per_contour_segment"] as usize,
            rain_drop_starting_voting_hysteresis: values[&"rain_drop_starting_voting_hysteresis"]
                as u64,
            undefined_gravity_vote_threshold: values[&"undefined_gravity_vote_threshold"],
            contour_gap_merge_radius: values[&"contour_gap_merge_radius"],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "
# a comment
bezier_linearization_step = 0.5
contours_step = 5.0
rasterization_px_size = 1.0
rasterization_step_factor = 0.5
heavy_object_width = 3.0
heavy_object_growing = 0.5
circumference_fitting_points_number = 4
slope_lines_contours_search_radius = 3.0
rain_drop_step = 1.0
sources_per_contour_segment = 3
rain_drop_starting_voting_hysteresis = 5
undefined_gravity_vote_threshold = 0.8
contour_gap_merge_radius = 2.0
";

    #[test]
    fn parses_a_full_config() {
        let config = Config::parse(FULL, "test").unwrap();
        assert_eq!(config.contours_step, 5.0);
        assert_eq!(config.circumference_fitting_points_number, 4);
        assert_eq!(config.sources_per_contour_segment, 3);
    }

    #[test]
    fn missing_key_is_an_error() {
        let text = FULL.replace("contours_step = 5.0\n", "");
        let err = Config::parse(&text, "test").unwrap_err();
        assert!(err.contains("contours_step"), "{err}");
    }

    #[test]
    fn unknown_key_is_an_error() {
        let text = format!("{FULL}\nnot_a_real_key = 1.0\n");
        let err = Config::parse(&text, "test").unwrap_err();
        assert!(err.contains("not_a_real_key"), "{err}");
    }

    #[test]
    fn bad_value_is_an_error_naming_the_line() {
        let text = FULL.replace("contours_step = 5.0", "contours_step = not_a_number");
        let err = Config::parse(&text, "test").unwrap_err();
        assert!(
            err.contains("contours_step") && err.contains("not_a_number"),
            "{err}"
        );
    }
}
