//! What `map_to_altitude` makes of ground whose height is known beforehand.
//!
//! Every other map is a guess about what its contours meant. These are drawn
//! from the answer instead: a cone of concentric circles five meters apart,
//! and the identical hollow, told apart only by the slope line on one of
//! them. What comes back can then be compared against the arithmetic rather
//! than against an impression of a picture.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use maur_o::altitude::{map_to_altitude, Settings};

/// The map scale the fixtures are drawn at, and what one meter of ground is
/// in the units a map file stores: 1/1000 mm on the paper, so a meter of
/// ground at 1:10000 is a tenth of a millimetre, which is a hundred of them.
const SCALE: i32 = 10000;
const UNITS_PER_METER: f64 = 100.0;

/// Where the circles are centred, in meters, so that nothing is negative.
const CENTRE: f64 = 300.0;

/// A closed circle of `radius` meters about the centre, as the coordinate
/// list a map file holds. The last point repeats the first and carries the
/// closing flag, which is how the format says a path comes back round.
fn circle(radius: f64) -> String {
    const SIDES: usize = 96;
    let mut coords = String::new();
    for i in 0..SIDES {
        let angle = i as f64 / SIDES as f64 * 2.0 * PI;
        let x = (CENTRE + radius * angle.cos()) * UNITS_PER_METER;
        let y = (CENTRE + radius * angle.sin()) * UNITS_PER_METER;
        coords.push_str(&format!("{} {};", x.round(), y.round()));
    }
    let x = (CENTRE + radius) * UNITS_PER_METER;
    let y = CENTRE * UNITS_PER_METER;
    coords.push_str(&format!("{} {} 2;", x.round(), y.round()));
    format!(
        "<object type=\"1\" symbol=\"0\"><coords count=\"{}\">{}</coords></object>",
        SIDES + 1,
        coords
    )
}

/// A slope line standing on the circle of `radius` meters, at the point due
/// east of the centre, with its tick reaching the way `turn` points it.
///
/// The symbol draws its tick towards the top of the page, and the renderer
/// turns a rotatable point symbol by the negated rotation of the object, so
/// a rotation of a quarter turn lays the tick towards the centre — which is
/// what says the ground falls inwards, and makes the cone a hollow.
fn slope_line(radius: f64, turn: f64) -> String {
    let x = (CENTRE + radius) * UNITS_PER_METER;
    let y = CENTRE * UNITS_PER_METER;
    format!(
        "<object type=\"0\" symbol=\"1\" rotation=\"{turn}\">\
         <coords count=\"1\">{} {};</coords></object>",
        x.round(),
        y.round()
    )
}

/// A map holding the given objects, drawn with a contour symbol and a slope
/// line symbol numbered the way every ISOM set numbers them.
fn map_of(dir: &Path, name: &str, objects: &str) -> PathBuf {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <map xmlns=\"http://openorienteering.org/apps/mapper/xml/v2\" version=\"9\">\
         <notes></notes>\
         <georeferencing scale=\"{SCALE}\"><projected_crs id=\"Local\"/></georeferencing>\
         <colors count=\"1\">\
         <color priority=\"0\" name=\"Brown\" c=\"0\" m=\"0.56\" y=\"1\" k=\"0.18\" opacity=\"1\">\
         <rgb method=\"cmyk\" r=\"0.82\" g=\"0.361\" b=\"0\"/></color>\
         </colors>\
         <symbols count=\"3\">\
         <symbol type=\"2\" id=\"0\" code=\"101\" name=\"Contour\">\
         <line_symbol color=\"0\" line_width=\"210\" join_style=\"2\" cap_style=\"1\"/></symbol>\
         <symbol type=\"1\" id=\"1\" code=\"104\" name=\"Slope line\">\
         <point_symbol rotatable=\"true\" inner_radius=\"0\" inner_color=\"-1\" \
         outer_width=\"0\" outer_color=\"-1\" elements=\"1\"><element>\
         <symbol type=\"2\" code=\"\"><line_symbol color=\"0\" line_width=\"210\"/></symbol>\
         <object type=\"1\"><coords count=\"2\">0 0;0 -750;</coords></object>\
         </element></point_symbol></symbol>\
         <symbol type=\"2\" id=\"2\" code=\"103\" name=\"Form line\">\
         <line_symbol color=\"0\" line_width=\"210\" dashed=\"true\"/></symbol>\
         </symbols>\
         <parts count=\"1\"><part name=\"main\"><objects count=\"1\">{objects}</objects></part></parts>\
         </map>\n"
    );
    let at = dir.join(name);
    std::fs::write(&at, xml).expect("the fixture map");
    at
}

/// The four circles of the cone, from the foot of it up.
fn cone_contours() -> String {
    [40.0, 30.0, 20.0, 10.0]
        .iter()
        .map(|&r| circle(r))
        .collect()
}

/// The settings the fixtures are read with: a meter to the pixel, and a
/// little ground around the circles so that the outermost one is not cut by
/// the edge of the raster.
fn settings() -> Settings {
    Settings {
        resolution: 1.0,
        equidistance: Some(5.0),
        frame: 10.0,
        ..Settings::default()
    }
}

/// The altitude at a point given in meters from the centre of the circles.
fn at(ground: &maur_o::altitude::AltitudeMap, east: f64, south: f64) -> f32 {
    let x = ((CENTRE + east - ground.origin.0) * ground.resolution).round() as usize;
    let y = ((CENTRE + south - ground.origin.1) * ground.resolution).round() as usize;
    ground.altitude[y * ground.width as usize + x]
}

#[test]
fn concentric_contours_come_back_as_the_cone_they_draw() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    let ground = map_to_altitude(&map, &settings()).expect("the ground under the cone");

    // Nothing was guessed at: the innermost circle is closed inside the map
    // and touches no edge of it, which says by itself that it is a summit.
    assert_eq!(
        ground.contours,
        (4, 0),
        "every contour should have been used"
    );
    assert!(
        ground.warnings.is_empty(),
        "a closed cone should raise nothing: {:?}",
        ground.warnings
    );

    // Four contours five meters apart is fifteen meters of relief: the ground
    // outside the widest circle is flat at its foot, and the ground inside the
    // narrowest is flat at its top.
    assert_eq!(ground.range.0, 0.0);
    assert!(
        (ground.range.1 - 15.0).abs() < 0.01,
        "expected 15 m of relief, got {}",
        ground.range.1
    );

    // On the circles themselves, where the answer is exact.
    for (radius, expected) in [(40.0, 0.0), (30.0, 5.0), (20.0, 10.0), (10.0, 15.0)] {
        let got = at(&ground, radius, 0.0);
        assert!(
            (got - expected as f32).abs() < 0.6,
            "the {radius} m circle should stand at {expected} m, got {got}"
        );
    }

    // The summit is a plateau at the top ring rather than a spike, and the
    // ground beyond the foot is flat rather than a pit.
    assert!((at(&ground, 0.0, 0.0) - 15.0).abs() < 0.01, "the summit");
    assert!(
        (at(&ground, 48.0, 0.0) - 0.0).abs() < 0.01,
        "beyond the foot"
    );

    // Halfway between two circles is halfway up the interval, which is what
    // says the interpolation is proportional and not merely monotonic.
    let halfway = at(&ground, 25.0, 0.0);
    assert!(
        (halfway - 7.5).abs() < 0.6,
        "halfway between the 30 m and 20 m circles should be 7.5 m, got {halfway}"
    );

    // The cone is round: the same radius is the same height whichever way it
    // is measured from the centre.
    let north = at(&ground, 0.0, -25.0);
    assert!(
        (north - halfway).abs() < 0.6,
        "the cone should be round: {north} north against {halfway} east"
    );
}

#[test]
fn a_slope_line_turns_the_same_cone_into_a_hollow() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    // The one difference from the cone: a tick on the outer circle, pointing
    // inwards, which is the mapper saying the ground falls that way.
    let objects = format!("{}{}", cone_contours(), slope_line(40.0, PI / 2.0));
    let map = map_of(dir.path(), "hollow.omap", &objects);

    let ground = map_to_altitude(&map, &settings()).expect("the ground under the hollow");

    assert!(
        ground.warnings.is_empty(),
        "a hollow with a slope line should raise nothing: {:?}",
        ground.warnings
    );
    assert!(
        (ground.range.1 - 15.0).abs() < 0.01,
        "expected 15 m of relief, got {}",
        ground.range.1
    );

    // Everything the cone said, upside down: the middle is now the bottom.
    assert!(
        (at(&ground, 0.0, 0.0) - 0.0).abs() < 0.01,
        "the floor of the hollow should be the lowest ground, got {}",
        at(&ground, 0.0, 0.0)
    );
    assert!(
        (at(&ground, 48.0, 0.0) - 15.0).abs() < 0.01,
        "the ground outside the hollow should be the highest, got {}",
        at(&ground, 48.0, 0.0)
    );
    for (radius, expected) in [(40.0, 15.0), (30.0, 10.0), (20.0, 5.0), (10.0, 0.0)] {
        let got = at(&ground, radius, 0.0);
        assert!(
            (got - expected as f32).abs() < 0.6,
            "the {radius} m circle should stand at {expected} m, got {got}"
        );
    }
}

#[test]
fn inverting_a_cone_is_the_hollow_it_would_have_been() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    let upright = map_to_altitude(&map, &settings()).expect("the cone");
    let over = map_to_altitude(
        &map,
        &Settings {
            invert: true,
            ..settings()
        },
    )
    .expect("the cone turned over");

    assert_eq!(upright.width, over.width);
    for (a, b) in upright.altitude.iter().zip(over.altitude.iter()) {
        // Both are measured up from their own lowest ground, so turning one
        // over is the other subtracted from the relief.
        assert!(
            (a + b - 15.0).abs() < 0.01,
            "turning the cone over should mirror it: {a} against {b}"
        );
    }
}

#[test]
fn the_contour_interval_is_read_from_the_map_notes() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());
    let noted = std::fs::read_to_string(&map)
        .expect("the fixture")
        .replace("<notes></notes>", "<notes>Equidistance 2.5 m</notes>");
    let with_notes = dir.path().join("noted.omap");
    std::fs::write(&with_notes, noted).expect("the noted fixture");

    let ground = map_to_altitude(
        &with_notes,
        &Settings {
            equidistance: None,
            ..settings()
        },
    )
    .expect("the ground");

    assert_eq!(ground.equidistance, 2.5);
    assert!(
        (ground.range.1 - 7.5).abs() < 0.01,
        "three intervals of 2.5 m is 7.5 m of relief, got {}",
        ground.range.1
    );
}

#[test]
fn a_map_which_says_no_interval_and_is_told_none_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    let complaint = map_to_altitude(
        &map,
        &Settings {
            equidistance: None,
            ..settings()
        },
    )
    .err()
    .expect("a map with no interval anywhere cannot be read");

    assert!(
        complaint.contains("--equidistance"),
        "the complaint should say what to do about it: {complaint}"
    );
}

#[test]
fn a_map_without_contours_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "bare.omap", "");

    let complaint = map_to_altitude(&map, &settings())
        .err()
        .expect("a map with no contours has no ground");
    assert!(
        complaint.contains("no contours"),
        "the complaint should say why: {complaint}"
    );
}

#[test]
fn the_tool_writes_a_raster_and_a_picture_of_it() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());
    let raster = dir.path().join("cone.tif");
    let preview = dir.path().join("cone.png");

    Command::cargo_bin("map_to_altitude")
        .unwrap()
        .args(["-e", "5", "-r", "1", "-f", "10"])
        .arg(&map)
        .arg(&raster)
        .arg("--preview")
        .arg(&preview)
        .assert()
        .success();

    // A single band of 32-bit floats, which is what anything reading a
    // terrain model expects to find -- and the summit still fifteen meters
    // up once it has been through the file.
    let file = std::io::BufReader::new(std::fs::File::open(&raster).expect("the raster"));
    let mut decoder = tiff::decoder::Decoder::new(file).expect("a TIFF");
    assert_eq!(
        decoder.colortype().expect("a colour type"),
        tiff::ColorType::Gray(32),
        "one band of 32 bits"
    );
    let (width, _) = decoder.dimensions().expect("the size");
    let tiff::decoder::DecodingResult::F32(pixels) = decoder.read_image().expect("the pixels")
    else {
        panic!("the raster should hold floats");
    };
    let middle = pixels[(width as usize / 2) * width as usize + width as usize / 2];
    assert!(
        (middle - 15.0).abs() < 0.01,
        "the summit should survive the round trip, got {middle}"
    );

    assert!(preview.is_file(), "the picture should be there too");
}

#[test]
fn a_bad_resolution_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    Command::cargo_bin("map_to_altitude")
        .unwrap()
        .args(["-e", "5", "-r", "0"])
        .arg(&map)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--resolution"));
}

/// A straight contour running west to east across the fixture at `north`
/// meters from the top of it, open at both ends.
fn ridge_line(north: f64) -> String {
    let y = ((CENTRE + north) * UNITS_PER_METER).round();
    let (west, east) = (
        ((CENTRE - 100.0) * UNITS_PER_METER).round(),
        ((CENTRE + 100.0) * UNITS_PER_METER).round(),
    );
    format!(
        "<object type=\"1\" symbol=\"0\"><coords count=\"2\">{west} {y};{east} {y};</coords>\
         </object>"
    )
}

/// Four parallel contours: a plain hillside, cut off by the edge of the map
/// at both ends, with nothing anywhere on it to say which way it falls.
fn hillside() -> String {
    [-30.0, -10.0, 10.0, 30.0]
        .iter()
        .map(|&n| ridge_line(n))
        .collect()
}

#[test]
fn a_hillside_of_open_contours_comes_back_as_an_even_slope() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "hillside.omap", &hillside());

    // No frame: a contour the map was cut through has to reach the edge of
    // the raster to go on dividing the ground it divides.
    let ground = map_to_altitude(
        &map,
        &Settings {
            frame: 0.0,
            ..settings()
        },
    )
    .expect("the ground under the hillside");

    // Nothing here says which way the ground runs -- no slope line, and not
    // one contour closed inside the map -- so the tool should say so rather
    // than pretend. The shape is still fully determined.
    assert!(
        ground.warnings.iter().any(|w| w.contains("guessed at")),
        "an unorientable hillside should be reported as one: {:?}",
        ground.warnings
    );

    // Three intervals between four contours, with the ground beyond the
    // outermost two flat.
    assert!(
        (ground.range.1 - 15.0).abs() < 0.01,
        "expected 15 m of relief, got {}",
        ground.range.1
    );

    // Down the middle of the map, the profile has to be a staircase which
    // only ever goes one way -- whichever way that turned out to be.
    // The contours span 60 m and the raster only adds its couple of pixels of
    // margin, so this is the whole of the profile there is.
    let profile: Vec<f32> = (-31..=31)
        .map(|north| at(&ground, 0.0, north as f64))
        .collect();
    let rising = profile[profile.len() - 1] > profile[0];
    for pair in profile.windows(2) {
        let step = pair[1] - pair[0];
        assert!(
            if rising { step >= -0.01 } else { step <= 0.01 },
            "the slope should run one way only, got {profile:?}"
        );
    }

    // Each contour is one whole interval from the last.
    for (north, step) in [(-30.0, 0.0), (-10.0, 1.0), (10.0, 2.0), (30.0, 3.0)] {
        let expected = if rising {
            step * 5.0
        } else {
            15.0 - step * 5.0
        };
        let got = at(&ground, 0.0, north);
        assert!(
            (got - expected).abs() < 0.6,
            "the contour {north} m along should stand at {expected} m, got {got}"
        );
    }
}

#[test]
fn the_same_map_twice_is_the_same_raster_twice() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    // The hillside rather than the cone: its direction has to be guessed at,
    // and a guess is exactly what an arbitrary ordering would make wobble.
    let map = map_of(dir.path(), "hillside.omap", &hillside());
    let settings = Settings {
        frame: 0.0,
        ..settings()
    };

    let once = map_to_altitude(&map, &settings).expect("the ground");
    let twice = map_to_altitude(&map, &settings).expect("the ground again");

    assert_eq!(once.width, twice.width);
    assert_eq!(once.height, twice.height);
    assert_eq!(
        once.altitude, twice.altitude,
        "the same map should give the same raster every time"
    );
}

/// The colours `walls_picture` paints with, which are what the tests below
/// are really checking: the map's own line, an end carried out to the edge of
/// the raster, and an end joined to a neighbouring contour.
const CONTOUR: [u8; 3] = [26, 26, 26];
const SEAL: [u8; 3] = [31, 111, 208];
const BRIDGE: [u8; 3] = [208, 48, 31];
const HEADING: [u8; 3] = [240, 190, 0];

fn holds(walls: &image::RgbImage, colour: [u8; 3]) -> bool {
    walls.pixels().any(|pixel| pixel.0 == colour)
}

#[test]
fn the_walls_picture_marks_the_ends_which_were_carried_to_the_edge() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "hillside.omap", &hillside());

    let ground = map_to_altitude(
        &map,
        &Settings {
            frame: 0.0,
            walls: true,
            ..settings()
        },
    )
    .expect("the ground");

    // Four contours, two ends each, every one of them running off the side of
    // the map and so carried out through the margin to the edge.
    assert_eq!(
        ground.mends,
        maur_o::altitude::Mends {
            sealed: 8,
            bridged: 0,
            unmatched: 0
        }
    );

    let walls = ground.walls.expect("the picture was asked for");
    assert_eq!(
        (walls.width(), walls.height()),
        (ground.width, ground.height)
    );
    assert!(holds(&walls, CONTOUR), "the map's own lines");
    assert!(holds(&walls, SEAL), "the ends carried out to the edge");
    assert!(!holds(&walls, BRIDGE), "nothing here needed bridging");
}

#[test]
fn a_map_of_closed_contours_needs_no_mending_at_all() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    let ground = map_to_altitude(
        &map,
        &Settings {
            walls: true,
            ..settings()
        },
    )
    .expect("the ground");

    // A closed contour has no ends, so there is nothing to close.
    assert_eq!(ground.mends, maur_o::altitude::Mends::default());
    let walls = ground.walls.expect("the picture");
    assert!(holds(&walls, CONTOUR));
    assert!(!holds(&walls, SEAL) && !holds(&walls, BRIDGE));
}

#[test]
fn the_walls_picture_is_only_painted_when_it_is_asked_for() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "cone.omap", &cone_contours());

    let ground = map_to_altitude(&map, &settings()).expect("the ground");
    assert!(ground.walls.is_none());
}

#[test]
fn the_tool_writes_the_walls_picture_where_it_is_told_to() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let map = map_of(dir.path(), "hillside.omap", &hillside());
    let walls = dir.path().join("walls.png");

    Command::cargo_bin("map_to_altitude")
        .unwrap()
        .args(["-e", "5", "-r", "1", "-f", "0"])
        .arg(&map)
        .arg(dir.path().join("out.tif"))
        .arg("--walls")
        .arg(&walls)
        .assert()
        .success()
        .stderr(predicates::str::contains("8 ends sealed"));

    let picture = image::open(&walls)
        .expect("the picture reads back")
        .to_rgb8();
    assert!(holds(&picture, SEAL), "the sealed ends should be marked");
}

/// A straight contour from one point to another, in meters from the centre,
/// open at both ends.
fn segment(from: (f64, f64), to: (f64, f64), symbol: &str) -> String {
    let unit = |p: (f64, f64)| {
        (
            ((CENTRE + p.0) * UNITS_PER_METER).round(),
            ((CENTRE + p.1) * UNITS_PER_METER).round(),
        )
    };
    let (ax, ay) = unit(from);
    let (bx, by) = unit(to);
    format!(
        "<object type=\"1\" symbol=\"{symbol}\"><coords count=\"2\">{ax} {ay};{bx} {by};\
         </coords></object>"
    )
}

#[test]
fn two_ends_facing_across_a_gap_are_joined_and_a_line_passing_by_is_not() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    // Three parallel contours, the middle one with a 10 m gap bitten out of
    // it. The gap is what should be closed. Its two ends are 20 m from the
    // contours either side of them and 10 m from each other, so joining an
    // end to the nearest *wall* would still find the right one here -- but
    // the test below has the case where it would not.
    let objects = format!(
        "{}{}{}{}",
        segment((-60.0, -20.0), (60.0, -20.0), "0"),
        segment((-60.0, 0.0), (-5.0, 0.0), "0"),
        segment((5.0, 0.0), (60.0, 0.0), "0"),
        segment((-60.0, 20.0), (60.0, 20.0), "0"),
    );
    let map = map_of(dir.path(), "gap.omap", &objects);

    let ground = map_to_altitude(
        &map,
        &Settings {
            frame: 0.0,
            walls: true,
            ..settings()
        },
    )
    .expect("the ground");

    // Eight ends. Six of them run off the side of the map and are sealed; the
    // two facing one another across the gap are joined to each other.
    assert_eq!(
        ground.mends,
        maur_o::altitude::Mends {
            sealed: 6,
            bridged: 1,
            unmatched: 0
        }
    );

    // All four pieces divide the ground. The two halves of the gapped contour
    // run between the same pair of bands and only one of them can carry the
    // step between those bands, but both are still lines at that height.
    assert_eq!(ground.contours, (4, 0));

    let walls = ground.walls.expect("the picture");
    assert!(holds(&walls, BRIDGE), "the gap should have been closed");
}

#[test]
fn an_end_which_faces_nothing_is_left_alone() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    // A contour running west to east, and a stub pointing straight at it and
    // stopping 14 m short. The nearest wall to the stub's end is that
    // contour, and the two are lines at different heights: joining them is
    // exactly what must not happen, and what joining to the nearest wall
    // would have done.
    let objects = format!(
        "{}{}",
        segment((-60.0, 0.0), (60.0, 0.0), "0"),
        segment((0.0, -40.0), (0.0, -14.0), "0"),
    );
    let map = map_of(dir.path(), "stub.omap", &objects);

    let ground = map_to_altitude(
        &map,
        &Settings {
            frame: 0.0,
            walls: true,
            ..settings()
        },
    )
    .expect("the ground");

    // The stub's lower end faces the long contour but nothing faces it back,
    // so it stays open and the stub divides nothing.
    assert_eq!(ground.mends.bridged, 0, "nothing should have been joined");
    assert!(ground.mends.unmatched > 0, "the stub's end stays open");
    assert_eq!(ground.contours.1, 1, "the stub should divide nothing");

    let walls = ground.walls.expect("the picture");
    assert!(!holds(&walls, BRIDGE), "no bridge should have been drawn");
    // The end that found nothing keeps its arrow, standing in the white where
    // there is no wall to hide it.
    assert!(
        holds(&walls, HEADING),
        "an unjoined end shows the way it headed"
    );
}

#[test]
fn form_lines_are_counted_and_left_out() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    // The cone, with a form line arcing through the middle of one of its
    // bands. A form line shows what the ground does between two contours and
    // is not one itself, so the answer should be the plain cone's.
    let with_form = format!(
        "{}{}",
        cone_contours(),
        segment((-25.0, 25.0), (25.0, 25.0), "2")
    );
    let map = map_of(dir.path(), "formline.omap", &with_form);

    let ground = map_to_altitude(&map, &settings()).expect("the ground");

    assert_eq!(ground.form_lines, 1, "the form line should be counted");
    assert_eq!(
        ground.contours,
        (4, 0),
        "and not counted among the contours"
    );
    assert_eq!(ground.mends, maur_o::altitude::Mends::default());
    assert!(
        ground.warnings.is_empty(),
        "a form line should raise nothing: {:?}",
        ground.warnings
    );

    // Bit for bit the cone it would have been without it.
    let plain = map_of(dir.path(), "cone.omap", &cone_contours());
    let bare = map_to_altitude(&plain, &settings()).expect("the plain cone");
    assert_eq!(ground.altitude, bare.altitude);
}
