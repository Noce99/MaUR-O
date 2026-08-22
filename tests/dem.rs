//! That shading a model of the ground shows the shape of it.

use maur_o::dem::{hillshade, slope_shade, Hillshade};

/// A slope of a given steepness running east, as a grid of heights.
fn ramp(width: usize, height: usize, rise_per_cell: f32) -> Vec<f32> {
    (0..width * height)
        .map(|i| (i % width) as f32 * rise_per_cell)
        .collect()
}

const CELL_M: f64 = 10.0;

#[test]
fn flat_ground_is_shaded_evenly() {
    let heights = vec![100f32; 40 * 30];
    let shaded = hillshade(&heights, 40, 30, CELL_M, &Hillshade::default());

    assert_eq!(shaded.valid_count, 40 * 30);
    assert_eq!(shaded.rgba.len(), 40 * 30 * 4);
    let first = &shaded.rgba[..4];
    for pixel in shaded.rgba.chunks_exact(4) {
        assert_eq!(pixel, first, "flat ground should be one shade");
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn a_slope_facing_the_sun_is_lighter_than_one_facing_away() {
    // A ridge: up to the middle, then down again.
    let (w, h) = (41usize, 5usize);
    let mut heights = vec![0f32; w * h];
    for r in 0..h {
        for c in 0..w {
            let up = c.min(w - 1 - c) as f32;
            heights[r * w + c] = up * 2.0;
        }
    }
    let shaded = hillshade(&heights, w, h, CELL_M, &Hillshade::default());
    let gray = |c: usize| shaded.rgba[(2 * w + c) * 4];

    // With the sun in the north-west, the west-facing side is the lit one.
    assert!(
        gray(5) > gray(w - 6),
        "west {} should be lighter than east {}",
        gray(5),
        gray(w - 6)
    );
}

#[test]
fn nothing_is_shown_where_the_model_knows_nothing() {
    let mut heights = ramp(20, 20, 1.0);
    for h in heights.iter_mut().take(40) {
        *h = f32::NAN;
    }
    let shaded = hillshade(&heights, 20, 20, CELL_M, &Hillshade::default());

    assert_eq!(shaded.valid_count, 20 * 20 - 40);
    for i in 0..40 {
        assert_eq!(shaded.rgba[i * 4 + 3], 0, "a hole should be transparent");
    }
    for i in 40..20 * 20 {
        assert_eq!(shaded.rgba[i * 4 + 3], 255);
    }
}

#[test]
fn a_model_with_nothing_in_it_says_so_rather_than_dividing_by_it() {
    let heights = vec![f32::NAN; 10 * 10];
    let shaded = hillshade(&heights, 10, 10, CELL_M, &Hillshade::default());
    assert_eq!(shaded.valid_count, 0);
    assert!(shaded.rgba.chunks_exact(4).all(|p| p[3] == 0));
}

#[test]
fn the_steeper_the_ground_the_stronger_the_slope_colour() {
    let gentle = slope_shade(&ramp(20, 20, 0.2), 20, 20, CELL_M);
    let steep = slope_shade(&ramp(20, 20, 8.0), 20, 20, CELL_M);

    // The middle of each, away from the edges where the gradient is one-sided.
    let middle = (10 * 20 + 10) * 4;
    assert!(
        steep.rgba[middle + 3] > gentle.rgba[middle + 3],
        "steep {} should show more strongly than gentle {}",
        steep.rgba[middle + 3],
        gentle.rgba[middle + 3]
    );
    // And it should have gone from green towards blue.
    assert!(steep.rgba[middle + 2] > gentle.rgba[middle + 2]);
    assert!(steep.rgba[middle + 1] < gentle.rgba[middle + 1]);
}

#[test]
fn how_far_apart_the_heights_are_is_what_makes_a_slope() {
    // The same numbers, read as a metre apart and as a hundred metres apart,
    // are a cliff and a plain.
    let heights = ramp(20, 20, 5.0);
    let close = slope_shade(&heights, 20, 20, 1.0);
    let far = slope_shade(&heights, 20, 20, 100.0);
    let middle = (10 * 20 + 10) * 4;
    assert!(close.rgba[middle + 3] > far.rgba[middle + 3]);
}

#[test]
fn moving_the_sun_moves_the_shadow() {
    // A ridge, which has a side facing each way. A uniform slope would not
    // do: the contrast stretch spreads whatever shading it finds over the
    // whole range of grey, so a hillside of one aspect comes out the same
    // however it is lit.
    let (w, h) = (41usize, 5usize);
    let mut heights = vec![0f32; w * h];
    for r in 0..h {
        for c in 0..w {
            heights[r * w + c] = c.min(w - 1 - c) as f32 * 2.0;
        }
    }
    let lit_from = |azimuth_deg: f64| {
        let shaded = hillshade(
            &heights,
            w,
            h,
            CELL_M,
            &Hillshade {
                azimuth_deg,
                ..Hillshade::default()
            },
        );
        let gray = |c: usize| i32::from(shaded.rgba[(2 * w + c) * 4]);
        gray(5) - gray(w - 6)
    };

    // West side brighter with the sun in the west, and the other way round
    // with it in the east.
    assert!(lit_from(270.0) > 0, "{}", lit_from(270.0));
    assert!(lit_from(90.0) < 0, "{}", lit_from(90.0));
}
