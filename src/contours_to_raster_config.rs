//! The `contours_to_raster` config file: the thirty-five parameters
//! `Contours-to-Raster.md` names, read from a small hand-rolled `key =
//! value` format (no `serde`/`toml` dependency exists anywhere else in this
//! crate, and one file with thirty-five numbers does not need one).

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
    /// Minimum combined confidence weight (Heavy Object and Jump readings
    /// on a contour, each weighted by how perpendicular it is to the
    /// contour at its own contact point -- see
    /// `gravity_model::tangent_alignment_confidence`) a contour needs before
    /// Step 2's vote will resolve it at all. Below this, there simply isn't
    /// enough evidence to trust either side, and the contour is left
    /// undefined for Step 3 to resolve instead (Step 2).
    pub step2_vote_min_total_weight: f64,
    /// Minimum margin, in the same confidence-weight units as
    /// `step2_vote_min_total_weight`, the winning side of Step 2's vote must
    /// lead the losing side by. Below this the vote is too close to call,
    /// and the contour is left undefined for Step 3 to resolve instead
    /// (Step 2).
    pub step2_vote_min_margin: f64,
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
    /// Minimum combined confidence weight -- every Hot (Anti) Rain Drop that
    /// hits a given contour during one Elevation/Anti Elevation
    /// Proliferation call, each weighted by how far from perpendicular its
    /// own direction is to the hit contour's own gravity there (`1.0`
    /// exactly aligned/anti-aligned, `0.0` exactly perpendicular) -- before
    /// Step 4's own vote will decide that contour's `elevation_height` at
    /// all in this call. Below this, there simply isn't enough evidence to
    /// trust either accordance or discordance yet, and the contour is left
    /// undecided for a later call (a different, better-placed contour) to
    /// resolve instead (Step 4).
    pub elevation_vote_min_total_weight: f64,
    /// Minimum margin, in the same confidence-weight units as
    /// `elevation_vote_min_total_weight`, the winning side (accordance or
    /// discordance) of Step 4's own vote must lead the losing side by.
    /// Below this the vote is too close to call cleanly; it is still
    /// decided (accordance wins an exact tie), but flagged with a warning
    /// naming both weights (Step 4).
    pub elevation_vote_min_margin: f64,
    /// Master switch for Step 1's whole Growing Process (Close Search,
    /// Seeking and Matching alike): `0.0` skips all three passes entirely --
    /// every contour is left exactly as extracted, dangling Flying Ends and
    /// all, and none of `01_<map_name>_step1_close_search.svg`/
    /// `02_<map_name>_step1_growing_seeking.svg`/
    /// `03_<map_name>_step1_growing_matching.svg` are written, since none of
    /// the three passes ran for them to show. Any nonzero value runs the
    /// Growing Process normally. Unlike every other `0.0`-disables knob
    /// below, this does not merely skip one sub-pass -- zeroing out the
    /// force constants instead would leave Seeking/Matching's own
    /// round-robin loop spinning forever, since nothing would ever move or
    /// merge for it to terminate on.
    pub growing_enabled: f64,
    /// The Growing Process's own preliminary Close Search pass's own
    /// "obvious match" distance, in ground meters: any two Flying Ends
    /// closer than this to each other are recorded as a match candidate
    /// outright, before Close Search's own cone-based searches ever run and
    /// regardless of either one's own forward direction -- close enough
    /// that which way either one happens to be pointing doesn't matter (Step
    /// 1's Growing Process). Still subject to the same crossing check every
    /// other Flying-End candidate is (a third contour physically between two
    /// close Flying Ends still blocks the match). Should stay small -- a
    /// handful of meters at most -- since it deliberately ignores direction
    /// entirely; **searching_fov**/**searching_distance** below are the
    /// direction-aware searches. `0.0` turns this particular check off
    /// outright, the same convention as **growing_oob_seeking_max_steps**'
    /// own `0`.
    pub obvious_to_close_contour_distance: f64,
    /// The Growing Process's own preliminary Close Search pass's own cone's
    /// total angular width, in degrees, centered on a Flying End's own
    /// forward direction (continuing past its tip, along its last segment):
    /// half this angle to each side of that direction (Step 1's Growing
    /// Process). Together with **searching_distance**, bounds the area Close
    /// Search scans, first for another Flying End, then for an out-of-bound
    /// pixel, then for a high-density pixel, before Seeking ever takes a
    /// single integration step.
    pub searching_fov: f64,
    /// The Growing Process's own preliminary Close Search pass's own max
    /// search radius, in ground meters, for all three of its cone-based
    /// searches -- another Flying End, an out-of-bound pixel, or a
    /// high-density pixel (Step 1's Growing Process). `0.0` turns those
    /// three off outright, the same convention as
    /// **growing_oob_seeking_max_steps**' own `0` (**obvious_to_close_contour_distance**'s
    /// own check is independent, and stays on unless it is itself `0.0`).
    /// Should stay small: this pass exists to catch Flying Ends that are
    /// already essentially where they need to be, not to replace
    /// Seeking/Matching's own, farther-reaching physics.
    pub searching_distance: f64,
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
    /// The Growing Process's own scan radius, in ground meters, for
    /// contour-pixel force (Appendix 5): a real contour pixel or
    /// `TEMPORARY_CONTOUR` tail only contributes its own contour force
    /// (`contour_force_max_repulsion`/`_equilibrium`/`_max_attraction`/
    /// `_second_equilibrium`) if its Euclidean distance from the Flying End
    /// is within this radius -- the Flying End's own pixel and its 8
    /// immediate neighbors are always excluded regardless, since the Flying
    /// End always sits right on top of its own just-written body there.
    pub contour_force_window: f64,
    /// The Growing Process's own scan radius, in ground meters, for
    /// out-of-bound/high-density/other-Flying-End force (Appendix 5): an
    /// out-of-bound pixel, a high-density pixel, or another pending Flying
    /// End only contributes its own constant force
    /// (`out_of_bound_force`/`density_region_force`/`flying_end_force`) if
    /// its Euclidean distance from the Flying End is within this radius.
    /// Kept separate from `contour_force_window` so how far the Growing
    /// Process reaches for something to move toward can be tuned
    /// independently of how far it reaches for something to repel it.
    pub attraction_force_window: f64,
    /// The contour-pixel force curve's own repulsion magnitude at zero
    /// distance, in Newtons (Appendix 5) -- `cfmr` in the doc's own
    /// notation. Positive: a contour pixel closer than
    /// `contour_force_equilibrium` pushes the Flying End away.
    pub contour_force_max_repulsion: f64,
    /// The contour-pixel force curve's own equilibrium distance, in ground
    /// meters (Appendix 5) -- `cfe`. The distance at which a contour
    /// pixel's own force is exactly zero: closer than this repels
    /// (`contour_force_max_repulsion` at distance zero, fading to zero
    /// here), farther than this attracts (up to
    /// `contour_force_second_equilibrium`).
    pub contour_force_equilibrium: f64,
    /// The contour-pixel force curve's own attraction magnitude at
    /// `contour_force_second_equilibrium` and beyond, in Newtons
    /// (Appendix 5) -- `cfma`. Negative: a contour pixel farther than
    /// `contour_force_equilibrium` gently pulls the Flying End back toward
    /// it, up to this magnitude, so a Flying End doesn't drift arbitrarily
    /// far from a contour it's meant to stay loosely leashed near.
    pub contour_force_max_attraction: f64,
    /// The contour-pixel force curve's own second equilibrium distance, in
    /// ground meters (Appendix 5) -- `cfse`, always greater than
    /// `contour_force_equilibrium`. The distance beyond which a contour
    /// pixel's own attraction saturates at `contour_force_max_attraction`
    /// rather than continuing to grow.
    pub contour_force_second_equilibrium: f64,
    /// Constant-magnitude attraction, in Newtons, contributed by every
    /// out-of-bound pixel found within `attraction_force_window` (Appendix
    /// 5). Positive; summed over every such pixel found, not just the
    /// nearest one.
    pub out_of_bound_force: f64,
    /// Constant-magnitude attraction, in Newtons, contributed by every
    /// high-density pixel found within `attraction_force_window` (Appendix
    /// 5). Positive; summed over every such pixel found, not just the
    /// nearest one.
    pub density_region_force: f64,
    /// Constant-magnitude attraction, in Newtons, contributed by every
    /// other pending Flying End found within `attraction_force_window`
    /// (Appendix 5). Positive; summed over every such end found, not just
    /// the nearest one. Only applied during the Matching phase (Step 1's
    /// Growing Process) -- dropped entirely during Seeking, same as
    /// matching itself.
    pub flying_end_force: f64,
    /// Euclidean distance, in ground meters, below which two pending Flying
    /// Ends merge -- either closing one contour into a ring (its own two
    /// ends) or splicing two contours together (Step 1's Growing Process,
    /// Matching phase only).
    pub flying_end_merge_distance: f64,
    /// The smallest net force magnitude, in Newtons, a Matching-phase
    /// integration step is allowed: a nonzero net force smaller than this
    /// (`density_region_force`/`flying_end_force` combined -- the only two
    /// terms Matching ever sees) is scaled up to this magnitude, direction
    /// preserved, before becoming a displacement. Without it, two Flying
    /// Ends near the far edge of each other's `attraction_force_window`
    /// (see `flying_end_force`'s own falloff), or forces that partly cancel
    /// against a third nearby end, can end up crawling toward a merge over
    /// an impractically large number of integration steps. Seeking is
    /// unaffected -- its own `growing_oob_seeking_max_steps` budget already
    /// bounds it, and a weak contour-pixel pull there genuinely means
    /// little is nearby to react to, not something to force along faster.
    /// `0.0` turns this off outright.
    pub matching_min_force: f64,
    /// How many seconds of simulated time each Growing Process integration
    /// step advances by (Appendix 5): a Flying End's displacement each step
    /// is its net force (Newtons, used directly as meters/second -- no
    /// mass, no inertia) times this. `1.0` unless there's a specific reason
    /// to change it.
    pub grow_time_step: f64,
    /// Scales the four push/pull vectors `--create_svg` draws in
    /// `02_<map_name>_step1_growing_seeking.svg`/
    /// `03_<map_name>_step1_growing_matching.svg` for every integration step
    /// (Appendix 5) -- one per force term (contour, out-of-bound, density,
    /// flying-end), tail on the Flying End's own pre-step position, length
    /// proportional to that term's own (unscaled, now genuinely
    /// Newton-valued) magnitude. Purely a visualization knob: it never
    /// affects the Growing Process itself, only how long those arrows are
    /// drawn.
    pub growing_visualization_push_pull_vectors_scale: f64,
    /// The window size, in pixels, of the Gaussian kernel Step 5's own
    /// Gravity Direction sub-step ([`crate::step5_gravity_raster`]) smooths
    /// its per-pixel direction field with, once gap-filling has run: a
    /// `kernel_size x kernel_size` window (radius `(kernel_size - 1) / 2`)
    /// centered on each pixel, standard deviation `radius / 3.0` (so the
    /// window's own edge sits at roughly three standard deviations -- the
    /// usual rule of thumb for how large a Gaussian window needs to be to
    /// capture essentially all of the kernel's own mass). `1` (or `0`) turns
    /// smoothing off outright.
    pub gravity_gaussian_kernel_size: usize,
    /// The same, for Step 5's own final per-pixel elevation
    /// ([`crate::step5_elevation_raster`]) instead of its gravity direction:
    /// the window size, in pixels, of the Gaussian kernel the E2V is
    /// smoothed with once gap-filling has run. Independent of
    /// `gravity_gaussian_kernel_size` -- the two fields have different
    /// units and no reason to share a single knob. `1` (or `0`) turns
    /// smoothing off outright.
    pub elevation_gaussian_kernel_size: usize,
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
    "step2_vote_min_total_weight",
    "step2_vote_min_margin",
    "rain_drop_step",
    "sources_per_contour_segment",
    "rain_drop_starting_voting_hysteresis",
    "undefined_gravity_vote_threshold",
    "elevation_vote_min_total_weight",
    "elevation_vote_min_margin",
    "growing_enabled",
    "obvious_to_close_contour_distance",
    "searching_fov",
    "searching_distance",
    "growing_oob_seeking_max_steps",
    "contour_force_window",
    "attraction_force_window",
    "contour_force_max_repulsion",
    "contour_force_equilibrium",
    "contour_force_max_attraction",
    "contour_force_second_equilibrium",
    "out_of_bound_force",
    "density_region_force",
    "flying_end_force",
    "flying_end_merge_distance",
    "matching_min_force",
    "grow_time_step",
    "growing_visualization_push_pull_vectors_scale",
    "gravity_gaussian_kernel_size",
    "elevation_gaussian_kernel_size",
];

impl Config {
    /// Parses a config file: one `key = value` per line, blank lines and
    /// lines starting with `#` ignored. All thirty-five keys are required --
    /// a config file missing one is far more likely a mistake than an
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
            step2_vote_min_total_weight: values[&"step2_vote_min_total_weight"],
            step2_vote_min_margin: values[&"step2_vote_min_margin"],
            rain_drop_step: values[&"rain_drop_step"],
            sources_per_contour_segment: values[&"sources_per_contour_segment"] as usize,
            rain_drop_starting_voting_hysteresis: values[&"rain_drop_starting_voting_hysteresis"]
                as u64,
            undefined_gravity_vote_threshold: values[&"undefined_gravity_vote_threshold"],
            elevation_vote_min_total_weight: values[&"elevation_vote_min_total_weight"],
            elevation_vote_min_margin: values[&"elevation_vote_min_margin"],
            growing_enabled: values[&"growing_enabled"],
            obvious_to_close_contour_distance: values[&"obvious_to_close_contour_distance"],
            searching_fov: values[&"searching_fov"],
            searching_distance: values[&"searching_distance"],
            growing_oob_seeking_max_steps: values[&"growing_oob_seeking_max_steps"] as u64,
            contour_force_window: values[&"contour_force_window"],
            attraction_force_window: values[&"attraction_force_window"],
            contour_force_max_repulsion: values[&"contour_force_max_repulsion"],
            contour_force_equilibrium: values[&"contour_force_equilibrium"],
            contour_force_max_attraction: values[&"contour_force_max_attraction"],
            contour_force_second_equilibrium: values[&"contour_force_second_equilibrium"],
            out_of_bound_force: values[&"out_of_bound_force"],
            density_region_force: values[&"density_region_force"],
            flying_end_force: values[&"flying_end_force"],
            flying_end_merge_distance: values[&"flying_end_merge_distance"],
            matching_min_force: values[&"matching_min_force"],
            grow_time_step: values[&"grow_time_step"],
            growing_visualization_push_pull_vectors_scale: values
                [&"growing_visualization_push_pull_vectors_scale"],
            gravity_gaussian_kernel_size: values[&"gravity_gaussian_kernel_size"] as usize,
            elevation_gaussian_kernel_size: values[&"elevation_gaussian_kernel_size"] as usize,
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
step2_vote_min_total_weight = 0.5
step2_vote_min_margin = 0.2
rain_drop_step = 1.0
sources_per_contour_segment = 3
rain_drop_starting_voting_hysteresis = 5
undefined_gravity_vote_threshold = 0.8
elevation_vote_min_total_weight = 0.3
elevation_vote_min_margin = 0.15
growing_enabled = 1.0
obvious_to_close_contour_distance = 2.0
searching_fov = 90.0
searching_distance = 3.0
growing_oob_seeking_max_steps = 10
contour_force_window = 15.0
attraction_force_window = 20.0
contour_force_max_repulsion = 2.0
contour_force_equilibrium = 2.5
contour_force_max_attraction = -0.5
contour_force_second_equilibrium = 7.5
out_of_bound_force = 0.5
density_region_force = 1.0
flying_end_force = 1.0
flying_end_merge_distance = 1.0
matching_min_force = 0.1
grow_time_step = 1.0
growing_visualization_push_pull_vectors_scale = 1.0
gravity_gaussian_kernel_size = 5
elevation_gaussian_kernel_size = 5
";

    #[test]
    fn parses_a_full_config() {
        let config = Config::parse(FULL, "test").unwrap();
        assert_eq!(config.contours_step, 5.0);
        assert_eq!(config.circumference_fitting_points_number, 4);
        assert_eq!(config.step2_vote_min_total_weight, 0.5);
        assert_eq!(config.step2_vote_min_margin, 0.2);
        assert_eq!(config.elevation_vote_min_total_weight, 0.3);
        assert_eq!(config.elevation_vote_min_margin, 0.15);
        assert_eq!(config.growing_enabled, 1.0);
        assert_eq!(config.sources_per_contour_segment, 3);
        assert_eq!(config.obvious_to_close_contour_distance, 2.0);
        assert_eq!(config.searching_fov, 90.0);
        assert_eq!(config.searching_distance, 3.0);
        assert_eq!(config.growing_oob_seeking_max_steps, 10);
        assert_eq!(config.contour_force_window, 15.0);
        assert_eq!(config.attraction_force_window, 20.0);
        assert_eq!(config.contour_force_max_repulsion, 2.0);
        assert_eq!(config.contour_force_equilibrium, 2.5);
        assert_eq!(config.contour_force_max_attraction, -0.5);
        assert_eq!(config.contour_force_second_equilibrium, 7.5);
        assert_eq!(config.out_of_bound_force, 0.5);
        assert_eq!(config.density_region_force, 1.0);
        assert_eq!(config.flying_end_force, 1.0);
        assert_eq!(config.flying_end_merge_distance, 1.0);
        assert_eq!(config.matching_min_force, 0.1);
        assert_eq!(config.grow_time_step, 1.0);
        assert_eq!(config.growing_visualization_push_pull_vectors_scale, 1.0);
        assert_eq!(config.gravity_gaussian_kernel_size, 5);
        assert_eq!(config.elevation_gaussian_kernel_size, 5);
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
