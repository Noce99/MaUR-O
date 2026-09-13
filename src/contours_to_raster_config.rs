//! The `contours_to_raster` config file: the twenty-one parameters
//! `Contours-to-Raster.md` names, read from a small hand-rolled `key =
//! value` format (no `serde`/`toml` dependency exists anywhere else in this
//! crate, and one file with twenty-one numbers does not need one).

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
    /// resampled to, in ground meters (Appendix 1) -- also the Growing
    /// Process's own step size and window scale (Appendix 5).
    pub contours_step: f64,
    /// Pixel size of the Contour Raster, in ground meters (Step 1).
    pub rasterization_px_size: f64,
    /// Buffer width, in ground meters, used to turn a Jump's or a Heavy
    /// Object's own `ls` into a polygon (Appendix 2's `width`) -- for a
    /// Jump, the area Step 2 scans to find which contours it touches; for a
    /// Heavy Object, the area Step 1 searches for an intersecting contour,
    /// instead of only the pixels directly under its digitized line.
    pub heavy_object_width: f64,
    /// Extra padding, in ground meters, buffered on top of
    /// `heavy_object_width` (Appendix 2's `extra_growing`).
    pub heavy_object_growing: f64,
    /// How many `Coord`s on each side of a Heavy Object/contour
    /// intersection are used to fit a circle (Step 1).
    pub circumference_fitting_points_number: usize,
    /// How far, in ground meters, around a Slope Line's own position Step 1
    /// searches for the nearest Contour Raster pixel to attribute its
    /// gravity reading to, since that position is not always pixel-exact on
    /// top of its contour (Step 1).
    pub slope_lines_contours_search_radius: f64,
    /// Distance a rain drop advances per simulation step, in ground meters
    /// (Rain Drop Production Definition).
    pub rain_drop_step: f64,
    /// How many sources are placed per contour segment (Rain Drop
    /// Production Definition).
    pub sources_per_contour_segment: usize,
    /// How many `rain_drop_step`-sized steps a Cold rain drop is exempt
    /// from evaporating on crossing its own starting contour, or on
    /// re-crossing an undefined contour it has already voted for
    /// (Rain Drop Production Definition, Cold-only).
    pub rain_drop_starting_voting_hysteresis: u64,
    /// How close left and right vote counts must be, as a ratio in (0, 1),
    /// before being flagged as ambiguous (Step 3, Cold-only).
    pub undefined_gravity_vote_threshold: f64,
    /// How many extra rounds of 8-connected dilation are run, after the
    /// out-of-bound flood-fill reaches its own fixed point, growing the
    /// out-of-bound area inward over *any* pixel value rather than only
    /// undefined ones (Step 1).
    pub out_of_bound_extra_dilation: usize,
    /// How many steps a Flying End spends seeking the out-of-bound area on
    /// its own -- no matching against another Flying End at all, only the
    /// raster's own out-of-bound/high-density/other-contour pixels (Step
    /// 1's Growing Process phase 1) -- before giving up and switching to
    /// the full process (phase 2: matching restored, the out-of-bound
    /// attraction term dropped) for its remaining steps. Keeps two
    /// contours that run close and parallel near the border from matching
    /// each other just because they happen to sit in each other's window,
    /// when what both should actually do is each reach the border on their
    /// own.
    pub growing_oob_seeking_max_steps: u64,
    /// The Growing Process's own square scan window for contour-pixel
    /// repulsion, in pixels (Appendix 5) -- any real contour pixel or
    /// `TEMPORARY_CONTOUR` tail is only seen (and only repels) within
    /// `2*half+1` pixels of the Flying End, where `half = max(1,
    /// growing_window_size_px_contours / 2)`.
    pub growing_window_size_px_contours: u64,
    /// The Growing Process's own square scan window for attraction, in
    /// pixels (Appendix 5): an out-of-bound (`1`) or high-density (`3`)
    /// pixel, and another pending Flying End to match against (case (a)),
    /// are only seen within `2*half+1` pixels of the Flying End, where
    /// `half = max(1, growing_window_size_px_attractions / 2)`. Kept
    /// separate from `growing_window_size_px_contours` so how far the
    /// Growing Process reaches for something to move toward can be tuned
    /// independently of how far it reaches for something to repel it.
    pub growing_window_size_px_attractions: u64,
    /// The Growing Process's own case-(c) step length, as a fraction of
    /// `contours_step` (Appendix 5) -- `0.5` means each case-(c) step moves
    /// `contours_step / 2`, not the full `contours_step`.
    pub growing_step_length: f64,
    /// How strongly the Growing Process's next step favors continuing in
    /// the direction the contour was already heading, relative to the pull
    /// of nearby out-of-bound/high-density/other-contour pixels
    /// (Appendix 5).
    pub growing_previous_distance_direction_weight: f64,
    /// How strongly an out-of-bound pixel attracts the Growing Process
    /// toward it (Appendix 5). Positive.
    pub growing_out_of_bound_direction_weight: f64,
    /// How strongly a high-density pixel attracts the Growing Process
    /// toward it (Appendix 5). Positive.
    pub growing_density_direction_weight: f64,
    /// How strongly any contour pixel repels the Growing Process away from
    /// it (Appendix 5). Negative, unlike the other two
    /// `growing_*_direction_weight` parameters.
    pub growing_other_contours_direction_weight: f64,
    /// Scales the four push/pull vectors `--create_svg` draws in
    /// `01_<map_name>_step1_growing.svg` for every case-(c) growing step
    /// (Appendix 5) -- one per `growing_*_direction_weight` term, tail on
    /// the Flying End's own pre-step position, length proportional to that
    /// term's own (unscaled) magnitude. Purely a visualization knob: it
    /// never affects the Growing Process itself, only how long those arrows
    /// are drawn.
    pub growing_visualization_push_pull_vectors_scale: f64,
}

/// The keys `Config::load` requires, in the order they are checked, paired
/// with the field they fill.
const KEYS: &[&str] = &[
    "bezier_linearization_step",
    "contours_step",
    "rasterization_px_size",
    "heavy_object_width",
    "heavy_object_growing",
    "circumference_fitting_points_number",
    "slope_lines_contours_search_radius",
    "rain_drop_step",
    "sources_per_contour_segment",
    "rain_drop_starting_voting_hysteresis",
    "undefined_gravity_vote_threshold",
    "out_of_bound_extra_dilation",
    "growing_oob_seeking_max_steps",
    "growing_window_size_px_contours",
    "growing_window_size_px_attractions",
    "growing_step_length",
    "growing_previous_distance_direction_weight",
    "growing_out_of_bound_direction_weight",
    "growing_density_direction_weight",
    "growing_other_contours_direction_weight",
    "growing_visualization_push_pull_vectors_scale",
];

impl Config {
    /// Parses a config file: one `key = value` per line, blank lines and
    /// lines starting with `#` ignored. All twenty-one keys are required -- a
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
            out_of_bound_extra_dilation: values[&"out_of_bound_extra_dilation"] as usize,
            growing_oob_seeking_max_steps: values[&"growing_oob_seeking_max_steps"] as u64,
            growing_window_size_px_contours: values[&"growing_window_size_px_contours"] as u64,
            growing_window_size_px_attractions: values[&"growing_window_size_px_attractions"]
                as u64,
            growing_step_length: values[&"growing_step_length"],
            growing_previous_distance_direction_weight: values
                [&"growing_previous_distance_direction_weight"],
            growing_out_of_bound_direction_weight: values[&"growing_out_of_bound_direction_weight"],
            growing_density_direction_weight: values[&"growing_density_direction_weight"],
            growing_other_contours_direction_weight: values
                [&"growing_other_contours_direction_weight"],
            growing_visualization_push_pull_vectors_scale: values
                [&"growing_visualization_push_pull_vectors_scale"],
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
heavy_object_width = 3.0
heavy_object_growing = 0.5
circumference_fitting_points_number = 4
slope_lines_contours_search_radius = 3.0
rain_drop_step = 1.0
sources_per_contour_segment = 3
rain_drop_starting_voting_hysteresis = 5
undefined_gravity_vote_threshold = 0.8
out_of_bound_extra_dilation = 2
growing_oob_seeking_max_steps = 10
growing_window_size_px_contours = 10
growing_window_size_px_attractions = 10
growing_step_length = 1.0
growing_previous_distance_direction_weight = 1.0
growing_out_of_bound_direction_weight = 1.0
growing_density_direction_weight = 1.0
growing_other_contours_direction_weight = -1.0
growing_visualization_push_pull_vectors_scale = 1.0
";

    #[test]
    fn parses_a_full_config() {
        let config = Config::parse(FULL, "test").unwrap();
        assert_eq!(config.contours_step, 5.0);
        assert_eq!(config.circumference_fitting_points_number, 4);
        assert_eq!(config.sources_per_contour_segment, 3);
        assert_eq!(config.out_of_bound_extra_dilation, 2);
        assert_eq!(config.growing_oob_seeking_max_steps, 10);
        assert_eq!(config.growing_window_size_px_contours, 10);
        assert_eq!(config.growing_window_size_px_attractions, 10);
        assert_eq!(config.growing_step_length, 1.0);
        assert_eq!(config.growing_other_contours_direction_weight, -1.0);
        assert_eq!(config.growing_visualization_push_pull_vectors_scale, 1.0);
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
