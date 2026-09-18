//! Step 4: Elevation Value Assignation (`Contours-to-Raster.md`). Grows a
//! tree **T** of contours, rooted at an arbitrary already-gravity-defined
//! contour at height `0`, by repeatedly running a Hot Rain Drop Production
//! and a Hot Anti Rain Drop Production (Elevation/Anti Elevation
//! Proliferation) from whichever leaf of **T** still has unexplored progeny
//! -- see [`resolve`] for the full loop and [`Tree`] for the data structure
//! backing it.
//!
//! Every "choose randomly" in the doc (the root contour, and a tie-break
//! among equal-depth leaves) is instead a deterministic smallest-index pick,
//! the same convention `step1_extract::merge_contours` already uses for the
//! doc's own "one of the two contour indices is discarded at random": any
//! consistent choice satisfies the doc, and this one needs no RNG dependency
//! and keeps every run and test reproducible.

use std::collections::HashMap;

use geo::{Coord, Euclidean, Length};

use crate::contour_geometry::{local_tangent, nearest_index};
use crate::contour_raster::ContourRaster;
use crate::contours_to_raster_config::Config;
use crate::gravity_model::{contour_gravity_side, lwg_gravity_side, vector_on_side, Contour, LineWithGravity};
use crate::step3_rain_drop::{hot_drop_evaporation_contour, placed_sources};

/// One node of [`Tree`]: its parent (`None` for the root), its own children,
/// and its depth (the root is `0`, a child is its parent's depth plus one).
struct TreeNode {
    parent: Option<usize>,
    children: Vec<usize>,
    depth: usize,
}

/// Step 4's own tree **T**: which contours (by index into the `Vec<Contour>`
/// Step 4 runs over) are members, and how they relate. Membership here and a
/// contour's own `elevation_height` being `Some` are kept as one invariant
/// throughout [`resolve`]: a contour is in `T` exactly when it has a height.
#[derive(Default)]
struct Tree {
    nodes: HashMap<usize, TreeNode>,
}

impl Tree {
    /// Adds `idx` as `T`'s own root, at depth `0` with no parent. Only ever
    /// called once, before the main loop in [`resolve`] starts.
    fn insert_root(&mut self, idx: usize) {
        self.nodes.insert(
            idx,
            TreeNode {
                parent: None,
                children: Vec::new(),
                depth: 0,
            },
        );
    }

    /// Adds `child_idx` as a new child of `parent_idx`, one depth level
    /// deeper. `parent_idx` must already be in `T`.
    fn insert_child(&mut self, parent_idx: usize, child_idx: usize) {
        let depth = self.nodes[&parent_idx].depth + 1;
        self.nodes.insert(
            child_idx,
            TreeNode {
                parent: Some(parent_idx),
                children: Vec::new(),
                depth,
            },
        );
        self.nodes.get_mut(&parent_idx).unwrap().children.push(child_idx);
    }

    /// Whether `node` is a descendant of `ancestor` -- walks `node`'s own
    /// parent chain looking for `ancestor`. Used to guard the one case the
    /// doc says should "ordinarily" not be possible: evicting `ancestor`'s
    /// subtree while re-parenting it under `node` would otherwise remove
    /// `node` itself out from under the very proliferation that is currently
    /// running on it. A hole Step 1's Growing Process left unfilled can still
    /// make this happen in practice, in which case [`elevation_proliferation`]
    /// voids the eviction and warns instead of crashing.
    fn is_descendant(&self, node: usize, ancestor: usize) -> bool {
        let mut current = self.nodes.get(&node).and_then(|n| n.parent);
        while let Some(p) = current {
            if p == ancestor {
                return true;
            }
            current = self.nodes.get(&p).and_then(|n| n.parent);
        }
        false
    }

    /// Removes `idx` and every descendant of it from `T` (detaching `idx`
    /// from its own parent's children list first), returning every removed
    /// index, `idx` included, in no particular order. The caller is
    /// responsible for resetting each removed contour's own
    /// `elevation_height`/`empty_progeny` and for re-inserting `idx` itself
    /// elsewhere if the doc calls for it (Step 4's own eviction, point 3):
    /// every *other* removed index is simply dropped out of `T` for good,
    /// unless some later proliferation happens to re-discover it.
    fn evict_subtree(&mut self, idx: usize) -> Vec<usize> {
        if let Some(parent) = self.nodes.get(&idx).and_then(|n| n.parent) {
            if let Some(p) = self.nodes.get_mut(&parent) {
                p.children.retain(|&c| c != idx);
            }
        }
        let mut removed = Vec::new();
        let mut stack = vec![idx];
        while let Some(i) = stack.pop() {
            if let Some(node) = self.nodes.remove(&i) {
                stack.extend(node.children);
                removed.push(i);
            }
        }
        removed
    }

    fn depth_of(&self, idx: usize) -> Option<usize> {
        self.nodes.get(&idx).map(|n| n.depth)
    }

    fn is_leaf(&self, idx: usize) -> bool {
        self.nodes.get(&idx).is_some_and(|n| n.children.is_empty())
    }

    /// Every `(parent, child)` edge of `T`, for `--create_svg`'s own
    /// `07_<map_name>_step4.svg`.
    fn edges(&self) -> Vec<(usize, usize)> {
        self.nodes
            .iter()
            .filter_map(|(&i, n)| n.parent.map(|p| (p, i)))
            .collect()
    }
}

/// **Next Proliferator Selection**: the leaf of `tree` with the smallest
/// depth whose own `empty_progeny` is still `false`, or `None` if no such
/// leaf remains. Ties are broken by the smallest contour index (see the
/// module's own doc comment on why this is deterministic rather than the
/// doc's literal "chooses randomly").
fn next_proliferator_selection(tree: &Tree, contours: &[Contour]) -> Option<usize> {
    tree.nodes
        .keys()
        .copied()
        .filter(|&i| tree.is_leaf(i) && !contours[i].empty_progeny)
        .min_by_key(|&i| (tree.depth_of(i).unwrap(), i))
}

/// The doc's own accordance/discordance check (Step 4): whether `dir` (the
/// drop's own direction of travel) agrees with `hit`'s own gravity direction
/// at the point `at` it was hit, plus how confidently so, as `|cos(theta)|`
/// between the two (both already unit vectors) -- `1.0` when `dir` is
/// exactly aligned or anti-aligned with `hit`'s own gravity there (a
/// confident read either way), fading to `0.0` exactly at the perpendicular
/// case, where the sign this same dot product's own accordance verdict
/// hinges on is most sensitive to noise. [`elevation_proliferation`] uses
/// this as a vote's own weight, precisely so a single near-perpendicular
/// drop can't flip a hit contour's own decision on its own (see
/// `Contours-to-Raster.md`'s own note on why Step 4 votes rather than
/// letting the first, or every, drop decide immediately). `None` only if
/// `hit` somehow has no gravity yet, which should not happen in Step 4
/// (every contour still in play is assumed to have one by this point).
fn accordance(hit: &LineWithGravity, at: Coord<f64>, dir: (f64, f64)) -> Option<(bool, f64)> {
    let side = lwg_gravity_side(hit)?;
    let idx = nearest_index(&hit.ls, at);
    let (tf, tt) = local_tangent(&hit.ls, idx);
    let (hgx, hgy) = vector_on_side(tf, tt, side);
    let dot = dir.0 * hgx + dir.1 * hgy;
    Some((dot >= 0.0, dot.abs()))
}

/// The height a hit contour should get, given its source's own `c_height`
/// and whether the drop reached it in `accordance` -- Elevation Proliferation
/// (`anti = false`, a Hot Rain Drop): accordance means one step further
/// downhill (`c_height - 1`), discordance means the same downhill band
/// reached from its other side (`c_height`). Anti Elevation Proliferation
/// (`anti = true`, a Hot Anti Rain Drop) flips both cases: since the drop
/// already travels *against* `C`'s own downhill, the *ordinary* case of
/// genuinely reaching higher ground is discordance with the hit's own
/// gravity (`c_height + 1`), and accordance is the "same band, other side"
/// case instead (`c_height`). See `Contours-to-Raster.md`'s own note on why
/// this flip is needed.
fn expected_elevation_height(c_height: f64, accordance: bool, anti: bool) -> f64 {
    match (anti, accordance) {
        (false, true) => c_height - 1.0,
        (false, false) => c_height,
        (true, true) => c_height,
        (true, false) => c_height + 1.0,
    }
}

/// One hit contour's own accumulated vote, across every source in a single
/// [`elevation_proliferation`] call that reached it: summed confidence
/// weight on each side (accordance/discordance -- see [`accordance`]), plus
/// each side's own single most-confident drop path, kept so a warning this
/// vote goes on to raise (a near tie, or a voided cycle-forming eviction --
/// see [`elevation_proliferation`]) can name a real, representative landing
/// point rather than just the two contours' own indices.
#[derive(Default)]
struct HitVote {
    accordance_weight: f64,
    discordance_weight: f64,
    accordance_best_weight: f64,
    discordance_best_weight: f64,
    accordance_path: Vec<Coord<f64>>,
    discordance_path: Vec<Coord<f64>>,
}

impl HitVote {
    fn add(&mut self, accordance: bool, weight: f64, path: Vec<Coord<f64>>) {
        if accordance {
            self.accordance_weight += weight;
            if weight > self.accordance_best_weight {
                self.accordance_best_weight = weight;
                self.accordance_path = path;
            }
        } else {
            self.discordance_weight += weight;
            if weight > self.discordance_best_weight {
                self.discordance_best_weight = weight;
                self.discordance_path = path;
            }
        }
    }

    /// The single most-confident drop path on `accordance`'s own winning
    /// side -- whichever of [`Self::accordance_path`]/[`Self::discordance_path`]
    /// that side accumulated.
    fn representative_path(&self, accordance: bool) -> &[Coord<f64>] {
        if accordance {
            &self.accordance_path
        } else {
            &self.discordance_path
        }
    }
}

/// The verdict [`elevation_proliferation`] draws from one hit contour's own
/// accumulated [`HitVote`], or `None` if its own total weight never cleared
/// `config.elevation_vote_min_total_weight` -- not enough evidence yet, left
/// for a later call (a different, better-placed contour) to decide instead.
/// `near_tie` is `true` when the margin between the two sides didn't clear
/// `config.elevation_vote_min_margin` even though a decision was still made
/// (accordance wins an exact tie, per `Contours-to-Raster.md`'s own note on
/// why).
struct VoteVerdict {
    accordance: bool,
    near_tie: bool,
}

fn decide_vote(vote: &HitVote, config: &Config) -> Option<VoteVerdict> {
    let total_weight = vote.accordance_weight + vote.discordance_weight;
    if total_weight < config.elevation_vote_min_total_weight {
        return None;
    }
    let accordance = vote.accordance_weight >= vote.discordance_weight;
    let margin = (vote.accordance_weight - vote.discordance_weight).abs();
    Some(VoteVerdict {
        accordance,
        near_tie: margin < config.elevation_vote_min_margin,
    })
}

/// **Elevation Proliferation** (`anti = false`, a Hot Rain Drop Production)
/// or **Anti Elevation Proliferation** (`anti = true`, a Hot Anti Rain Drop
/// Production) on `c_idx`, already a member of `tree` with a defined height:
/// runs one Hot (Anti) Rain Drop Production from `c_idx`'s own `ls`, but
/// unlike the doc's own literal point-2/3 wording, no single drop decides a
/// hit contour's own fate the moment it evaporates -- every drop that hits a
/// given contour during this call instead casts a confidence-weighted vote
/// (accordance or discordance, weighted by [`accordance`]'s own `|cos|`) for
/// it, and only once every source has evaporated does each hit contour's own
/// tally get turned into a decision (`config.elevation_vote_min_total_weight`/
/// `elevation_vote_min_margin` gating whether it is decided at all this call,
/// and how confidently -- see `Contours-to-Raster.md`'s own updated Step 4).
/// A single near-perpendicular drop -- exactly the case where the raw
/// accordance/discordance sign is most sensitive to noise -- can then no
/// longer flip a contour's own elevation on its own. Once decided, the doc's
/// own three cases (Step 4) apply exactly as before, except when winning an
/// eviction would require evicting `c_idx`'s own ancestor -- `c_idx` already
/// a descendant of the contour it just "won" against -- in which case the
/// eviction is voided (that contour is left exactly as it was) and a warning
/// is raised instead, rather than corrupting `tree`. Returns the number of
/// distinct contours newly added (or evicted and re-parented) as children of
/// `c_idx`. `warnings` collects one entry per contour whose own vote this
/// call decided despite a too-close margin, plus one per voided eviction.
fn elevation_proliferation(
    c_idx: usize,
    anti: bool,
    contours: &mut [Contour],
    raster: &mut ContourRaster,
    tree: &mut Tree,
    config: &Config,
    warnings: &mut Vec<String>,
) -> u64 {
    let pass_name = if anti {
        "Anti Elevation Proliferation"
    } else {
        "Elevation Proliferation"
    };

    let ls = contours[c_idx].lwg.ls.clone();
    let side = contour_gravity_side(&contours[c_idx]).unwrap_or_else(|| {
        panic!(
            "Step 4 ({pass_name} from contour {c_idx}) assumes every contour still in play has \
             a gravity direction, but contour {c_idx} itself does not"
        )
    });
    let c_height = contours[c_idx].elevation_height.unwrap_or_else(|| {
        panic!(
            "Step 4 ({pass_name} from contour {c_idx}): a contour Next Proliferator Selection \
             picks is always already in T with a defined elevation_height, but contour {c_idx} \
             has none"
        )
    });

    let mut votes: HashMap<usize, HitVote> = HashMap::new();
    for (source, (dx, dy)) in placed_sources(&ls, side, config.sources_per_contour_segment) {
        let dir = if anti { (-dx, -dy) } else { (dx, dy) };
        let Some((hit_idx, at, drop_path)) =
            hot_drop_evaporation_contour(raster, c_idx as u64, source, dir, config)
        else {
            continue; // high density or out of bound: a non-operation
        };
        let hit_idx = hit_idx as usize;

        let (acc, weight) = accordance(&contours[hit_idx].lwg, at, dir).unwrap_or_else(|| {
            panic!(
                "Step 4 ({pass_name} from contour {c_idx}) assumes every contour still in play \
                 has a gravity direction, but contour {hit_idx}, hit at ({:.2}, {:.2}), does not",
                at.x, at.y
            )
        });
        votes.entry(hit_idx).or_default().add(acc, weight, drop_path);
    }

    let mut children_added = 0u64;
    for (hit_idx, vote) in votes {
        let Some(verdict) = decide_vote(&vote, config) else {
            continue; // not enough evidence yet; a later call may still resolve it
        };
        let acc = verdict.accordance;
        let acc_word = if acc { "accordance" } else { "discordance" };
        if verdict.near_tie {
            warnings.push(format!(
                "contour {hit_idx} has a near-tied elevation vote from contour {c_idx}'s \
                 {pass_name} (accordance weight {:.2} vs discordance weight {:.2}); picked \
                 {acc_word}",
                vote.accordance_weight, vote.discordance_weight,
            ));
        }
        let expected = expected_elevation_height(c_height, acc, anti);
        let representative_path = vote.representative_path(acc);

        match contours[hit_idx].elevation_height {
            None => {
                contours[hit_idx].elevation_height = Some(expected);
                tree.insert_child(c_idx, hit_idx);
                children_added += 1;
            }
            Some(existing) if expected.abs() > existing.abs() => {
                if hit_idx == c_idx || tree.is_descendant(c_idx, hit_idx) {
                    // Either a self-hit -- a concave source contour's own
                    // drop curled back and evaporated on itself, so hit_idx
                    // and c_idx are literally the same node -- or a hole
                    // Step 1's Growing Process left unfilled let the drop
                    // reach back up to its own ancestor. Either way, evicting
                    // hit_idx's subtree would also remove c_idx, the very
                    // contour this call is running on. Void the eviction
                    // (hit_idx is left exactly as it was) and warn instead of
                    // corrupting the tree.
                    let at = representative_path.last().copied().unwrap_or(Coord { x: f64::NAN, y: f64::NAN });
                    let relation = if hit_idx == c_idx {
                        format!("{hit_idx} and {c_idx} are the same contour")
                    } else {
                        format!("{c_idx} is itself already a descendant of {hit_idx} in T")
                    };
                    warnings.push(format!(
                        "Step 4: {pass_name} from contour {c_idx} (elevation_height {c_height}) \
                         decided contour {hit_idx} (currently elevation_height {existing}) \
                         should be in {acc_word} with its own gravity direction (weighted vote: \
                         accordance {:.2} vs discordance {:.2}, most confident drop landing at \
                         ({:.2}, {:.2})), computing an expected elevation_height of {expected} \
                         for it. abs({expected}) > abs({existing}), so {hit_idx} would normally \
                         be evicted (along with its own subtree) and re-parented under {c_idx} \
                         at the new value -- but {relation}, so evicting {hit_idx}'s subtree \
                         would also remove {c_idx}, the very contour this {pass_name} call is \
                         running on. Voiding the eviction: contour {hit_idx} keeps its current \
                         elevation_height.",
                        vote.accordance_weight, vote.discordance_weight, at.x, at.y,
                    ));
                    continue;
                }
                for removed in tree.evict_subtree(hit_idx) {
                    contours[removed].elevation_height = None;
                    contours[removed].empty_progeny = false;
                }
                contours[hit_idx].elevation_height = Some(expected);
                tree.insert_child(c_idx, hit_idx);
                children_added += 1;
            }
            Some(_) => {} // abs(expected) <= abs(existing): no-op
        }
    }
    children_added
}

/// What Step 4 resolved: how many contours got an `elevation_height`, one
/// warning per contour that ended without one (either because Step 3 had
/// already dropped it, or because Step 4's own tree never reached it), plus
/// `T`'s own structure for `--create_svg`'s `07_<map_name>_step4.svg`.
pub struct Step4Result {
    /// How many contours have a defined `elevation_height` once Step 4
    /// finishes -- excludes contours Step 3 had already dropped.
    pub resolved: u64,
    /// One warning per contour that ended Step 4 without an
    /// `elevation_height`, naming its own length in meters, the same
    /// convention Step 3 uses for a contour it drops, plus one per near-tied
    /// vote and one per voided cycle-forming eviction (see
    /// [`elevation_proliferation`]).
    pub warnings: Vec<String>,
    /// Every `(parent_contour_idx, child_contour_idx)` edge of `T`, as far as
    /// it got built.
    pub tree_edges: Vec<(usize, usize)>,
    /// Every contour index whose own `empty_progeny` ended `true`: a leaf of
    /// `T` whose Elevation and Anti Elevation Proliferation both added zero
    /// children.
    pub dead_ends: Vec<usize>,
}

/// Runs Step 4: erases the raster footprint of any contour Step 3 already
/// dropped (still gravity-undefined -- see the doc's own note on why, added
/// alongside this rewrite), grows `T` from an arbitrary gravity-defined root
/// contour at height `0` by repeatedly proliferating whichever leaf
/// [`next_proliferator_selection`] returns, and reports whatever contour is
/// still without an `elevation_height` once no leaf is left to proliferate.
pub fn resolve(contours: &mut [Contour], raster: &mut ContourRaster, config: &Config) -> Step4Result {
    for (idx, c) in contours.iter().enumerate() {
        if c.lwg.gravity_dx.is_none() {
            raster.clear_contour(idx as u64);
        }
    }

    let mut tree = Tree::default();
    let mut warnings = Vec::new();
    let root_idx = contours.iter().position(|c| c.lwg.gravity_dx.is_some());
    let Some(root_idx) = root_idx else {
        return Step4Result {
            resolved: 0,
            warnings,
            tree_edges: Vec::new(),
            dead_ends: Vec::new(),
        };
    };
    tree.insert_root(root_idx);
    contours[root_idx].elevation_height = Some(0.0);

    while let Some(c_idx) = next_proliferator_selection(&tree, contours) {
        let rain_children =
            elevation_proliferation(c_idx, false, contours, raster, &mut tree, config, &mut warnings);
        let anti_children =
            elevation_proliferation(c_idx, true, contours, raster, &mut tree, config, &mut warnings);
        if rain_children == 0 && anti_children == 0 {
            contours[c_idx].empty_progeny = true;
        }
    }

    let mut resolved = 0u64;
    for (idx, c) in contours.iter().enumerate() {
        if c.lwg.gravity_dx.is_none() {
            continue; // already reported (and erased above) as a Step 3 drop
        }
        if c.elevation_height.is_some() {
            resolved += 1;
        } else {
            let length_m = Euclidean.length(&c.lwg.ls);
            warnings.push(format!(
                "contour {idx} ({length_m:.1}m long) still has no elevation height after Step 4; \
                 dropped"
            ));
        }
    }

    Step4Result {
        resolved,
        warnings,
        tree_edges: tree.edges(),
        dead_ends: contours
            .iter()
            .enumerate()
            .filter(|(_, c)| c.empty_progeny)
            .map(|(i, _)| i)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity_model::LineWithGravity;
    use geo::LineString;

    fn c(x: f64, y: f64) -> Coord<f64> {
        Coord { x, y }
    }

    fn default_config() -> Config {
        Config {
            bezier_linearization_step: 0.1,
            contours_step: 1.0,
            rasterization_px_size: 0.5,
            heavy_object_width: 1.0,
            heavy_object_growing: 0.2,
            circumference_fitting_points_number: 4,
            slope_lines_contours_search_radius: 3.0,
            step2_vote_min_total_weight: 0.3,
            step2_vote_min_margin: 0.15,
            rain_drop_step: 0.25,
            sources_per_contour_segment: 3,
            rain_drop_starting_voting_hysteresis: 3,
            undefined_gravity_vote_threshold: 0.8,
            elevation_vote_min_total_weight: 0.3,
            elevation_vote_min_margin: 0.15,
            growing_enabled: 1.0,
            obvious_to_close_contour_distance: 0.0,
            searching_fov: 0.0,
            searching_distance: 0.0,
            growing_oob_seeking_max_steps: 0,
            contour_force_window: 4.0,
            attraction_force_window: 4.0,
            contour_force_max_repulsion: 2.0,
            contour_force_equilibrium: 1.0,
            contour_force_max_attraction: -0.5,
            contour_force_second_equilibrium: 3.0,
            out_of_bound_force: 0.5,
            density_region_force: 1.0,
            flying_end_force: 1.0,
            flying_end_merge_distance: 0.5,
            matching_min_force: 0.0,
            grow_time_step: 1.0,
            growing_visualization_push_pull_vectors_scale: 1.0,
            gravity_gaussian_kernel_size: 5,
            elevation_gaussian_kernel_size: 5,
        }
    }

    fn straight_ls(y: f64) -> LineString<f64> {
        LineString::new(vec![c(0.0, y), c(20.0, y)])
    }

    fn contour_with_gravity(y: f64, gravity_dy: f64) -> Contour {
        let mut contour = Contour {
            lwg: LineWithGravity::new(straight_ls(y)),
            elevation_height: None,
            empty_progeny: false,
        };
        contour.lwg.gravity_dx = Some(0.0);
        contour.lwg.gravity_dy = Some(gravity_dy);
        contour
    }

    fn raster_for(contours: &[Contour]) -> ContourRaster {
        let mut r = ContourRaster::new(c(-5.0, -20.0), 0.5, 60, 100);
        for (i, contour) in contours.iter().enumerate() {
            r.write_contour(i as u64, &contour.lwg.ls);
        }
        r
    }

    #[test]
    fn accordance_weight_is_the_cosine_magnitude_between_drop_and_hit_gravity() {
        let hit = {
            let mut lwg = LineWithGravity::new(straight_ls(0.0));
            lwg.gravity_dx = Some(0.0);
            lwg.gravity_dy = Some(-1.0);
            lwg
        };
        // Straight down: exactly aligned with the hit's own downhill -- full
        // confidence, accordance.
        let (acc, weight) = accordance(&hit, c(5.0, 0.0), (0.0, -1.0)).unwrap();
        assert!(acc);
        assert!((weight - 1.0).abs() < 1e-9);

        // Straight up: exactly anti-aligned -- full confidence, discordance.
        let (acc, weight) = accordance(&hit, c(5.0, 0.0), (0.0, 1.0)).unwrap();
        assert!(!acc);
        assert!((weight - 1.0).abs() < 1e-9);

        // Sideways: exactly perpendicular -- zero confidence either way, the
        // exact case a single vote must not be allowed to decide alone.
        let (_, weight) = accordance(&hit, c(5.0, 0.0), (1.0, 0.0)).unwrap();
        assert!(weight.abs() < 1e-9);
    }

    #[test]
    fn hit_vote_accumulates_weight_and_keeps_the_most_confident_path_per_side() {
        let mut vote = HitVote::default();
        vote.add(true, 0.2, vec![c(0.0, 0.0), c(1.0, 0.0)]);
        vote.add(true, 0.9, vec![c(0.0, 0.0), c(2.0, 0.0)]); // stronger accordance vote
        vote.add(false, 0.4, vec![c(0.0, 0.0), c(3.0, 0.0)]);

        assert!((vote.accordance_weight - 1.1).abs() < 1e-9);
        assert!((vote.discordance_weight - 0.4).abs() < 1e-9);
        // The most confident accordance vote (weight 0.9) is the
        // representative one, not the first or the weakest.
        assert_eq!(vote.representative_path(true), &[c(0.0, 0.0), c(2.0, 0.0)]);
        assert_eq!(vote.representative_path(false), &[c(0.0, 0.0), c(3.0, 0.0)]);
    }

    #[test]
    fn decide_vote_skips_a_contour_below_the_minimum_total_weight() {
        let mut config = default_config();
        config.elevation_vote_min_total_weight = 1.0;
        let vote = HitVote {
            accordance_weight: 0.9,
            ..Default::default()
        };
        assert!(
            decide_vote(&vote, &config).is_none(),
            "0.9 total weight must not clear a 1.0 minimum"
        );
    }

    #[test]
    fn decide_vote_flags_a_near_tie_but_still_decides_it() {
        let mut config = default_config();
        config.elevation_vote_min_total_weight = 0.1;
        config.elevation_vote_min_margin = 0.5;
        let vote = HitVote {
            accordance_weight: 2.1,
            discordance_weight: 1.9,
            ..Default::default()
        };
        let verdict = decide_vote(&vote, &config).unwrap();
        assert!(verdict.accordance, "the larger side still wins");
        assert!(verdict.near_tie, "a 0.2 margin must not clear a 0.5 minimum");
    }

    #[test]
    fn decide_vote_favors_accordance_on_an_exact_tie() {
        let mut config = default_config();
        config.elevation_vote_min_total_weight = 0.1;
        config.elevation_vote_min_margin = 0.0;
        let vote = HitVote {
            accordance_weight: 1.5,
            discordance_weight: 1.5,
            ..Default::default()
        };
        let verdict = decide_vote(&vote, &config).unwrap();
        assert!(verdict.accordance);
        assert!(!verdict.near_tie, "an exact tie still clears a 0.0 margin");
    }

    #[test]
    fn decide_vote_does_not_flag_a_clear_win() {
        let config = default_config();
        let vote = HitVote {
            accordance_weight: 3.0,
            discordance_weight: 0.1,
            ..Default::default()
        };
        let verdict = decide_vote(&vote, &config).unwrap();
        assert!(verdict.accordance);
        assert!(!verdict.near_tie);
    }

    #[test]
    fn a_stack_of_parallel_contours_gets_one_lower_height_per_downhill_step() {
        // Three parallel contours, downhill = -y (toward y=0), 5m apart:
        // a Hot Rain Drop from the top one reaches the middle one directly
        // below, and from there the bottom one -- each one step lower.
        let mut contours = vec![
            contour_with_gravity(10.0, -1.0),
            contour_with_gravity(5.0, -1.0),
            contour_with_gravity(0.0, -1.0),
        ];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert_eq!(contours[0].elevation_height, Some(0.0), "the root");
        assert_eq!(contours[1].elevation_height, Some(-1.0));
        assert_eq!(contours[2].elevation_height, Some(-2.0));
        assert_eq!(result.resolved, 3);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn anti_elevation_proliferation_gives_higher_ground_a_higher_height() {
        // Same stack, but rooted at the *bottom* contour (index 2, the only
        // one whose gravity Step 2 would have found first here doesn't
        // matter -- resolve always roots at the lowest surviving index).
        // Anti Elevation Proliferation should climb the stack: y=5 above
        // y=0 gets +1, y=10 above that gets +2.
        let mut contours = vec![
            contour_with_gravity(0.0, -1.0),
            contour_with_gravity(5.0, -1.0),
            contour_with_gravity(10.0, -1.0),
        ];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert_eq!(contours[0].elevation_height, Some(0.0), "the root");
        assert_eq!(
            contours[1].elevation_height,
            Some(1.0),
            "one genuine step uphill from the root"
        );
        assert_eq!(
            contours[2].elevation_height,
            Some(2.0),
            "two genuine steps uphill from the root"
        );
        assert_eq!(result.resolved, 3);
    }

    #[test]
    fn a_contour_step3_dropped_is_skipped_and_its_raster_footprint_erased() {
        // The middle contour never got a gravity direction (as if Step 3 had
        // dropped it): its own pixels must be erased before Step 4 starts,
        // so a Hot Rain Drop from the top contour passes straight through to
        // the bottom one instead of evaporating on it.
        let mut top = contour_with_gravity(10.0, -1.0);
        top.lwg.gravity_dx = Some(0.0);
        let middle = Contour {
            lwg: LineWithGravity::new(straight_ls(5.0)),
            elevation_height: None,
            empty_progeny: false,
        }; // no gravity: as if Step 3 dropped it
        let bottom = contour_with_gravity(0.0, -1.0);
        let mut contours = vec![top, middle, bottom];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert_eq!(contours[0].elevation_height, Some(0.0));
        assert!(
            contours[1].elevation_height.is_none(),
            "never had gravity, so Step 4 must never touch it"
        );
        assert_eq!(
            contours[2].elevation_height,
            Some(-1.0),
            "reached straight through where the dropped contour used to be"
        );
        assert_eq!(result.resolved, 2, "the dropped contour is not counted");
        assert!(
            result.warnings.is_empty(),
            "a Step-3-dropped contour is not Step 4's own warning to raise"
        );
    }

    #[test]
    fn a_contour_step4_never_reaches_is_warned_about_and_left_undefined() {
        // A lone contour far away in x from the reachable stack: since every
        // drop from the stack travels straight in y (gravity_dx = 0), none
        // of them ever changes x, so nothing ever reaches a contour placed
        // far off to the side regardless of the raster's own size -- a
        // genuinely unreachable contour, not just a distant one.
        let mut contours = vec![
            contour_with_gravity(0.0, -1.0),
            contour_with_gravity(5.0, -1.0),
        ];
        let mut isolated = contour_with_gravity(0.0, -1.0);
        isolated.lwg.ls = LineString::new(vec![c(200.0, 0.0), c(220.0, 0.0)]);
        contours.push(isolated);
        let mut raster = ContourRaster::new(c(-5.0, -20.0), 0.5, 460, 60);
        for (i, contour) in contours.iter().enumerate() {
            raster.write_contour(i as u64, &contour.lwg.ls);
        }
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert!(contours[0].elevation_height.is_some());
        assert!(contours[1].elevation_height.is_some());
        assert!(contours[2].elevation_height.is_none());
        assert_eq!(result.resolved, 2);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("contour 2") && w.contains("20.0m")),
            "expected a warning naming the isolated contour's own length: {:?}",
            result.warnings
        );
    }

    #[test]
    fn a_leaf_with_no_new_children_is_marked_empty_progeny_and_never_reselected() {
        // A single contour with no neighbors at all: its own Elevation and
        // Anti Elevation Proliferation both add zero children, so it should
        // be marked empty_progeny and the loop should terminate immediately
        // rather than spin forever reselecting it.
        let mut contours = vec![contour_with_gravity(0.0, -1.0)];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert!(contours[0].empty_progeny);
        assert_eq!(result.dead_ends, vec![0]);
        assert_eq!(result.resolved, 1);
    }

    #[test]
    fn a_stronger_competing_path_evicts_and_reparents_a_weaker_one() {
        // AC (index 1) starts out already in T as its own root, at a weak
        // height (-1.0, abs 1), with its own child AC_child (index 2, height
        // -2.0) already hanging off it. C0 (index 0), sitting one segment
        // above AC and artificially set to a much lower height (-5.0, as if
        // deep in some other branch of the real tree), now proliferates and
        // its Hot Rain Drop reaches AC directly: accordance gives AC an
        // expected height of -6.0, abs 6 > abs 1, so AC (and everything under
        // it) must be evicted and AC re-parented under C0 with the new,
        // stronger height -- exactly Step 4's own point 3.
        let mut contours = vec![
            contour_with_gravity(10.0, -1.0), // C0
            contour_with_gravity(5.0, -1.0),  // AC
            contour_with_gravity(0.0, -1.0),  // AC_child (never on the drop's path)
        ];
        contours[0].elevation_height = Some(-5.0);
        contours[1].elevation_height = Some(-1.0);
        contours[2].elevation_height = Some(-2.0);

        let mut raster = raster_for(&contours[..2]);
        let config = default_config();

        let mut tree = Tree::default();
        tree.insert_root(0);
        tree.insert_root(1);
        tree.insert_child(1, 2);

        let mut warnings = Vec::new();
        let children_added = elevation_proliferation(
            0,
            false,
            &mut contours,
            &mut raster,
            &mut tree,
            &config,
            &mut warnings,
        );

        assert_eq!(children_added, 1);
        assert_eq!(
            contours[1].elevation_height,
            Some(-6.0),
            "AC should be re-parented under C0 with the new, stronger expected height"
        );
        assert_eq!(tree.depth_of(1), Some(1), "AC is now C0's own child");
        assert!(
            contours[2].elevation_height.is_none(),
            "AC's evicted former child must have its own height reset"
        );
        assert!(
            !contours[2].empty_progeny,
            "AC's evicted former child must have empty_progeny reset too"
        );
        assert!(
            tree.depth_of(2).is_none(),
            "AC's evicted former child is no longer a member of T at all"
        );
    }

    #[test]
    fn c_descendant_of_ac_voids_the_eviction_and_warns_instead_of_corrupting_the_tree() {
        // AC (index 0) is C's own ancestor in T (AC -> C). If C's own drop
        // now reaches back to AC with a stronger competing height, evicting
        // AC's subtree would also remove C -- the very contour currently
        // being proliferated -- which the doc says should ordinarily not be
        // possible; this must be voided (AC left untouched) and warned about
        // instead of corrupting T.
        let mut contours = vec![
            contour_with_gravity(5.0, -1.0), // AC
            contour_with_gravity(0.0, -1.0), // C, AC's own child
        ];
        contours[0].elevation_height = Some(-1.0);
        contours[1].elevation_height = Some(-100.0); // artificially huge, forces eviction

        let mut raster = raster_for(&contours);
        let config = default_config();

        let mut tree = Tree::default();
        tree.insert_root(0);
        tree.insert_child(0, 1);

        // Anti Elevation Proliferation: C's own drop travels uphill, toward
        // AC sitting above it.
        let mut warnings = Vec::new();
        let children_added = elevation_proliferation(
            1,
            true,
            &mut contours,
            &mut raster,
            &mut tree,
            &config,
            &mut warnings,
        );

        assert_eq!(children_added, 0, "the voided eviction adds no child");
        assert_eq!(
            contours[0].elevation_height,
            Some(-1.0),
            "AC must be left exactly as it was, not evicted or re-parented"
        );
        assert_eq!(tree.depth_of(0), Some(0), "AC is still T's own root");
        assert_eq!(tree.depth_of(1), Some(1), "C is still AC's own child");

        // The warning must be debuggable on its own: which pass, which two
        // contours, their elevation_height values, the accordance/discordance
        // verdict, and the expected value that triggered the eviction
        // attempt -- not just "1" and "0" somewhere in the text.
        assert_eq!(warnings.len(), 1, "expected exactly one warning: {warnings:?}");
        let warning = &warnings[0];
        assert!(
            warning.contains("Anti Elevation Proliferation from contour 1"),
            "expected the warning to name the pass and the proliferating contour: {warning}"
        );
        assert!(
            warning.contains("elevation_height -100"),
            "expected the warning to name C's own elevation_height: {warning}"
        );
        assert!(
            warning.contains("contour 0 (currently elevation_height -1)"),
            "expected the warning to name AC and its own current elevation_height: {warning}"
        );
        assert!(
            warning.contains("discordance"),
            "expected the warning to name the accordance/discordance verdict: {warning}"
        );
        assert!(
            warning.contains("expected elevation_height of -99"),
            "expected the warning to name the computed expected value that triggered the \
             eviction attempt: {warning}"
        );
    }

    #[test]
    fn tree_edges_report_every_parent_child_relationship() {
        let mut contours = vec![
            contour_with_gravity(10.0, -1.0),
            contour_with_gravity(5.0, -1.0),
            contour_with_gravity(0.0, -1.0),
        ];
        let mut raster = raster_for(&contours);
        let config = default_config();

        let result = resolve(&mut contours, &mut raster, &config);

        assert_eq!(result.tree_edges.len(), 2);
        assert!(result.tree_edges.contains(&(0, 1)));
        assert!(result.tree_edges.contains(&(1, 2)));
    }
}
