//! Shading a digital elevation model, so that terrain can be seen.
//!
//! A grid of heights says everything about the shape of the ground and shows
//! nothing: printed as grey levels it is a fog. These are the two ways of
//! turning it into a picture that a map reader can use.
//!
//! [`hillshade`] lights the terrain from one corner of the sky and shades
//! each cell by how much of that light it catches, which is how a hill comes
//! to look like a hill. [`slope_shade`] colours the ground by how steep it
//! is, which is what a runner wants to know and a hillshade does not say --
//! a slope facing the sun looks gentle whatever its angle.
//!
//! Both come back as straight RGBA, transparent wherever the model has no
//! data, ready to lay over a map.
//!
//! Ported from pyorienteering's hillshading, which is matplotlib's
//! `LightSource`: the sun at the north-west, a contrast stretch between two
//! percentiles of the shading, and the result lifted clear of black so that
//! shading a map darkens its slopes without dimming its flat ground.

/// Where the sun is, and how the shading is stretched to use the grey it has.
#[derive(Clone, Copy, Debug)]
pub struct Hillshade {
    /// Which way the sun comes from, in degrees clockwise from north.
    pub azimuth_deg: f64,
    /// How high it is above the horizon, in degrees.
    pub altitude_deg: f64,
    /// The shading percentile taken as fully dark.
    pub low_percentile: f64,
    /// The shading percentile taken as fully light.
    pub high_percentile: f64,
    /// How dark the darkest slope is allowed to go, as a fraction of white.
    /// Above zero because this is drawn over a map, not instead of one.
    pub min_gray: f64,
}

impl Default for Hillshade {
    fn default() -> Hillshade {
        Hillshade {
            // The north-west, which is where a reader expects light on a map:
            // lit the other way, hills read as hollows.
            azimuth_deg: 315.0,
            altitude_deg: 45.0,
            low_percentile: 2.0,
            high_percentile: 98.0,
            min_gray: 0.35,
        }
    }
}

/// A shaded grid, and how much of it was real.
#[derive(Debug)]
pub struct Shaded {
    /// Straight RGBA, row by row; transparent where the model had no data.
    pub rgba: Vec<u8>,
    /// How many cells held a height at all. Zero means the model missed the
    /// map altogether, which is worth telling the user rather than showing
    /// them an empty picture.
    pub valid_count: usize,
}

/// A grid of heights with its holes filled in, and where they were.
struct Filled {
    heights: Vec<f32>,
    valid: Vec<bool>,
    valid_count: usize,
}

/// Fills the holes with the average height.
///
/// A gradient taken across a hole would be enormous, and the cells beside it
/// would come out as a cliff that is not there. The filled value is never
/// shown -- the holes are transparent in the result -- it is only there to
/// keep the slopes beside them honest.
fn fill_no_data(heights: &[f32]) -> Filled {
    let mut valid = vec![false; heights.len()];
    let mut sum = 0f64;
    let mut valid_count = 0usize;
    for (i, &h) in heights.iter().enumerate() {
        if !h.is_nan() {
            valid[i] = true;
            sum += f64::from(h);
            valid_count += 1;
        }
    }
    let mean = if valid_count > 0 {
        (sum / valid_count as f64) as f32
    } else {
        0.0
    };
    let filled = heights
        .iter()
        .zip(&valid)
        .map(|(&h, &ok)| if ok { h } else { mean })
        .collect();
    Filled {
        heights: filled,
        valid,
        valid_count,
    }
}

/// The slope at each cell, as the rise eastward and the rise northward.
///
/// Central differences, one-sided at the edges. Rows run south, so the
/// northward rise is the negative of the rise down the rows.
fn gradients(
    heights: &[f32],
    width: usize,
    height: usize,
    pixel_size_m: f64,
    mut each: impl FnMut(usize, f64, f64),
) {
    for r in 0..height {
        let r_up = r.saturating_sub(1);
        let r_dn = (r + 1).min(height - 1);
        for c in 0..width {
            let c_lf = c.saturating_sub(1);
            let c_rt = (c + 1).min(width - 1);
            let at = |r: usize, c: usize| f64::from(heights[r * width + c]);
            let dz_dx = (at(r, c_rt) - at(r, c_lf)) / ((c_rt - c_lf) as f64 * pixel_size_m);
            let dz_dn = -(at(r_dn, c) - at(r_up, c)) / ((r_dn - r_up) as f64 * pixel_size_m);
            each(r * width + c, dz_dx, dz_dn);
        }
    }
}

/// The value at a percentile of the cells that hold data.
fn percentile_range(intensity: &[f32], valid: &[bool], low: f64, high: f64) -> (f64, f64) {
    let mut values: Vec<f32> = intensity
        .iter()
        .zip(valid)
        .filter(|(_, &ok)| ok)
        .map(|(&v, _)| v)
        .collect();
    if values.is_empty() {
        return (0.0, 1.0);
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |p: f64| {
        let last = values.len() - 1;
        let i = ((p / 100.0) * last as f64).round().clamp(0.0, last as f64) as usize;
        f64::from(values[i])
    };
    (at(low), at(high))
}

/// Lights the terrain and shades it by how much light each cell catches.
///
/// `pixel_size_m` is how far apart the heights are on the ground, which is
/// what makes a slope a slope: the same grid at a different spacing is a
/// different hill.
pub fn hillshade(
    heights: &[f32],
    width: usize,
    height: usize,
    pixel_size_m: f64,
    options: &Hillshade,
) -> Shaded {
    let filled = fill_no_data(heights);

    // Where the light comes from, as a direction in the sky. The azimuth is
    // measured clockwise from north and the arithmetic wants it
    // counterclockwise from east.
    let az = (90.0 - options.azimuth_deg).to_radians();
    let alt = options.altitude_deg.to_radians();
    let dir_x = az.cos() * alt.cos();
    let dir_y = az.sin() * alt.cos();
    let dir_z = alt.sin();

    let mut intensity = vec![0f32; width * height];
    gradients(
        &filled.heights,
        width,
        height,
        pixel_size_m,
        |i, dz_dx, dz_dn| {
            // The surface normal is (-dz/dx, -dz/dnorth, 1); how brightly the
            // cell is lit is that against the direction of the light.
            let mag = (dz_dx * dz_dx + dz_dn * dz_dn + 1.0).sqrt();
            intensity[i] = ((-dz_dx * dir_x - dz_dn * dir_y + dir_z) / mag) as f32;
        },
    );

    // Most of a real landscape's shading sits in a narrow band, so it is
    // stretched to use the grey available rather than the grey it happens to
    // occupy.
    let (lo, hi) = percentile_range(
        &intensity,
        &filled.valid,
        options.low_percentile,
        options.high_percentile,
    );
    let span = if hi - lo > 1e-9 { hi - lo } else { 1.0 };

    let mut rgba = vec![0u8; width * height * 4];
    for (i, &v) in intensity.iter().enumerate() {
        let t = ((f64::from(v) - lo) / span).clamp(0.0, 1.0);
        let gray = ((options.min_gray + (1.0 - options.min_gray) * t) * 255.0).round() as u8;
        rgba[i * 4] = gray;
        rgba[i * 4 + 1] = gray;
        rgba[i * 4 + 2] = gray;
        rgba[i * 4 + 3] = if filled.valid[i] { 255 } else { 0 };
    }
    Shaded {
        rgba,
        valid_count: filled.valid_count,
    }
}

/// The steepest slope a colour is given to; anything steeper is that colour.
const STEEPEST_DEG: f64 = 35.0;

/// Colours the ground by how steep it is: green where a runner can move,
/// through yellow, to blue where they cannot.
///
/// Gentle ground is left nearly transparent, so that the colour appears only
/// where the slope is worth knowing about.
pub fn slope_shade(heights: &[f32], width: usize, height: usize, pixel_size_m: f64) -> Shaded {
    let filled = fill_no_data(heights);
    let mut rgba = vec![0u8; width * height * 4];

    gradients(
        &filled.heights,
        width,
        height,
        pixel_size_m,
        |i, dz_dx, dz_dn| {
            if !filled.valid[i] {
                return;
            }
            let degrees = dz_dx.hypot(dz_dn).atan().to_degrees();
            let t = (degrees / STEEPEST_DEG).clamp(0.0, 1.0);
            rgba[i * 4] = (255.0 - 75.0 * (t - 0.65).max(0.0)).round() as u8;
            rgba[i * 4 + 1] = (236.0 * (1.0 - t) + 40.0 * t).round() as u8;
            rgba[i * 4 + 2] = (80.0 * (1.0 - t) + 160.0 * t).round() as u8;
            rgba[i * 4 + 3] = (30.0 + 225.0 * t).round() as u8;
        },
    );

    Shaded {
        rgba,
        valid_count: filled.valid_count,
    }
}
