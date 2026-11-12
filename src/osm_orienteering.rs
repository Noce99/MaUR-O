//! What OpenStreetMap features look like on an orienteering map: the symbol
//! set of a mapping standard — ISSprOM 2019 for sprint, ISOM 2017-2 for the
//! forest — and the tag rules deciding which feature gets which symbol.
//!
//! # The symbol set is the standard's own
//!
//! Nothing here defines a symbol. Each standard's symbol set is a reference
//! map shipped in `maps/` (`ISSprOM_4k.omap`, `ISOM_10k.omap`, Mapper's own
//! sets), and its colours and symbols go into the written map as that file
//! spells them ([`crate::xml_reader::read_fragments_str`]): every symbol of
//! the standard, the ones no feature was drawn with too, as a map Mapper
//! starts from one of its sets has them. The map is at the set's own scale,
//! 1:4000 and 1:10000, the only scale its symbols are drawn for.
//!
//! # The rules
//!
//! A feature gets a symbol by its *code*, the standard's number for it
//! (`"521"` is a building in both), so a rule reads like the standard
//! does. They start from the cross-reference tables Mapper ships for
//! OpenStreetMap (`OSM-ISSprOM 2019.crt`, `OSM-ISOM 2017-2.crt`), and depart
//! from them where a sprint map wants more: paved squares and parkings,
//! roads by how much traffic they carry, stairways, canopies.
//!
//! OpenStreetMap does not say how fast one runs through a wood or over a
//! fence, which is what an orienteering map is mostly about. A wood is
//! runnable forest unless tagged dense or young; a wall or a fence is
//! impassable unless a `height` under 1.5 m says otherwise, as Mapper's
//! tables have them. The map is a base for the mapper's field work, not
//! a finished map.
//!
//! Land settled or closed to the public — residential, commercial,
//! industrial land, allotments, cemeteries, farmyards, private gardens —
//! is an area that shall not be entered, as in Mapper's tables. What lies
//! on it — a park, a square, a pond — is cut out of it (see
//! [`crate::osm_convert::convert`]): every area symbol here but the
//! buildings, the canopies and the marshes is
//! [ground](crate::osm_convert::OsmRules::ground).
//!
//! Buildings wall to wall are joined into one block, with no outline
//! between them, and so are those mapped less than half a meter apart.
//!
//! Nothing underground is drawn: tunnels, culverts, and whatever is on a
//! negative `layer`. Of the trees, only those tagged a landmark or a natural
//! monument: a city maps every tree in its streets and parks. There are no
//! names: an orienteering map has none.
//!
//! On a sprint map a paved street is a paved area as wide as the street,
//! pavements included, laid around its centre line: its `width` or
//! `lanes` where it has them, what is usual for its kind where not (see
//! [`street_width`]); the streets of the map joined into one area. A path
//! is a footprint of fixed width. On a forest map roads are ISOM's lines.
//!
//! A park is open land with scattered trees, and its lawns, where mapped
//! on their own, open land. A portico — a walkway `covered=arcade` or `colonnade`,
//! as Bologna maps its own — is a canopy 4 m wide, cut out of the building
//! it runs under.

use std::collections::{HashMap, HashSet};

use crate::osm::Tags;
use crate::osm_convert::{OsmRules, Strip};
use crate::osm_style::{surface, Surface};
use crate::xml_reader::{read_fragments_str, Fragments};

/// The ISSprOM 2019 symbol set, at 1:4000.
const ISSPROM: &str = include_str!("../maps/ISSprOM_4k.omap");
/// The ISOM 2017-2 symbol set, at 1:10000.
const ISOM: &str = include_str!("../maps/ISOM_10k.omap");

/// A mapping standard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standard {
    /// ISSprOM 2019, for sprint orienteering, at 1:4000.
    Issprom,
    /// ISOM 2017-2, for forest orienteering, at 1:10000.
    Isom,
}

impl Standard {
    /// The standard's name, as Mapper's symbol sets give it.
    pub fn name(self) -> &'static str {
        match self {
            Standard::Issprom => "ISSprOM 2019",
            Standard::Isom => "ISOM 2017-2",
        }
    }

    /// The reference map holding the standard's symbol set.
    fn reference(self) -> &'static str {
        match self {
            Standard::Issprom => ISSPROM,
            Standard::Isom => ISOM,
        }
    }
}

/// A standard's symbol set, and the rules for drawing OpenStreetMap with it.
pub struct OrienteeringStyle {
    /// The standard.
    pub standard: Standard,
    /// Its colours and symbols, as the reference map spells them.
    pub fragments: Fragments,
    scale: i32,
    /// The id of each symbol, by code.
    ids: HashMap<String, i32>,
    /// The symbols which are land cover.
    ground: HashSet<i32>,
    /// The building symbol.
    building: i32,
}

/// The value of attribute `name` in the first tag of `element`.
fn attribute<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let tag = &element[..element.find('>')?];
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// The codes of the area symbols which are not land cover: drawn over it,
/// rather than cutting it.
const OVER_GROUND: &[&str] = &[
    "308", "308.1", "310", "521", "521.2", "521.3", "522", "522.1", "522.3",
];

impl OrienteeringStyle {
    /// The symbol set of `standard`.
    pub fn new(standard: Standard) -> OrienteeringStyle {
        let text = standard.reference();
        let fragments = read_fragments_str(text).expect("the shipped symbol sets are well-formed");
        let scale = text
            .find("<georeferencing ")
            .and_then(|at| attribute(&text[at..], "scale"))
            .and_then(|s| s.parse().ok())
            .expect("the shipped symbol sets have a scale");
        let mut ids = HashMap::new();
        let mut ground = HashSet::new();
        for fragment in &fragments.symbols {
            let Some(code) = attribute(&fragment.text, "code") else {
                continue;
            };
            // The first symbol with a code is the standard's; a later one
            // with the same code is a helper.
            ids.entry(code.to_string()).or_insert(fragment.id);
            let area = matches!(attribute(&fragment.text, "type"), Some("4" | "16"));
            if area && !OVER_GROUND.contains(&code) {
                ground.insert(fragment.id);
            }
        }
        let building = ids["521"];
        OrienteeringStyle {
            standard,
            fragments,
            scale,
            ids,
            ground,
            building,
        }
    }

    /// The id of the symbol with this code.
    ///
    /// Panics where the standard has none: every code is one of the rules'
    /// own, and the tests try them all.
    pub fn id(&self, code: &str) -> i32 {
        *self
            .ids
            .get(code)
            .unwrap_or_else(|| panic!("no symbol {code} in {}", self.standard.name()))
    }

    /// The code of the symbol with this id.
    pub fn code(&self, id: i32) -> Option<&str> {
        self.ids
            .iter()
            .find(|(_, &i)| i == id)
            .map(|(code, _)| code.as_str())
    }

    /// `sprint` for ISSprOM, `forest` for ISOM.
    fn pick(&self, sprint: &'static str, forest: &'static str) -> &'static str {
        match self.standard {
            Standard::Issprom => sprint,
            Standard::Isom => forest,
        }
    }

    fn ids(&self, codes: Vec<&'static str>) -> Vec<i32> {
        codes.into_iter().map(|code| self.id(code)).collect()
    }

    /// The codes of the area symbols a closed way or a multipolygon is
    /// drawn with. See [`OsmRules::area_symbols`].
    pub fn area_codes(&self, tags: &Tags, multipolygon: bool) -> Vec<&'static str> {
        if underground(tags) {
            return Vec::new();
        }
        let get = |key: &str| tags.get(key).unwrap_or("");
        let private = matches!(get("access"), "private" | "no");

        // Buildings and roofs.
        match get("building") {
            "" | "no" => {}
            "roof" | "carport" => return vec!["522"],
            "ruins" => return Vec::new(),
            _ => return vec!["521"],
        }
        if tags.is("man_made", "canopy") {
            return vec!["522"];
        }

        // Water.
        let water = tags.is("natural", "water")
            || tags.is("waterway", "riverbank")
            || matches!(get("landuse"), "reservoir" | "basin")
            || tags.is("leisure", "swimming_pool");
        if water {
            return if tags.is("intermittent", "yes") {
                vec!["302"]
            } else {
                vec!["301"]
            };
        }
        if tags.is("natural", "wetland") {
            return vec!["308"];
        }

        // Paved ground: squares, pedestrian areas, parkings, platforms.
        let paved = self.pick("501.5", "501.1");
        let highway_area = tags.get("highway").is_some()
            && (multipolygon || tags.is("area", "yes"))
            || tags.get("area:highway").is_some();
        let parking = tags.is("amenity", "parking")
            && !matches!(get("parking"), "underground" | "multi-storey" | "rooftop");
        let platform = tags.is("railway", "platform") || tags.is("public_transport", "platform");
        if highway_area || parking || platform || tags.is("place", "square") {
            return vec![paved];
        }
        if matches!(get("leisure"), "pitch" | "track") {
            return if surface(tags) == Surface::Paved {
                vec![paved]
            } else {
                vec!["401"]
            };
        }

        // Land closed to runners.
        let forbidden = matches!(
            get("landuse"),
            "residential"
                | "commercial"
                | "retail"
                | "industrial"
                | "garages"
                | "allotments"
                | "cemetery"
                | "farmyard"
                | "military"
                | "railway"
                | "construction"
                | "landfill"
        ) || tags.is("amenity", "prison")
            || tags.is("amenity", "grave_yard")
            || (tags.is("leisure", "garden") && private);
        if forbidden {
            return vec!["520"];
        }

        // Vegetation.
        let wood = matches!(get("landuse"), "forest") || tags.is("natural", "wood");
        if wood {
            let young = matches!(get("wood:age"), "young" | "very_young");
            return match get("wood:density") {
                "sparse" => vec!["404"],
                "dense" => vec!["408"],
                _ if young => vec!["408"],
                _ => vec!["405"],
            };
        }
        if tags.is("natural", "scrub") {
            return vec!["408"];
        }
        if matches!(get("natural"), "heath" | "grassland") {
            return vec!["403"];
        }
        // A park is lawns and trees: open land with scattered trees, its
        // lawns, where mapped on their own, open land lying on it.
        if tags.is("leisure", "park") {
            return vec!["402"];
        }
        let open = matches!(
            get("landuse"),
            "grass" | "meadow" | "village_green" | "recreation_ground"
        ) || matches!(
            get("leisure"),
            "garden" | "playground" | "golf_course" | "common" | "dog_park"
        );
        if open {
            return vec!["401"];
        }
        match get("landuse") {
            "farmland" => return vec!["412"],
            "orchard" => return vec!["413"],
            "vineyard" => return vec!["414"],
            _ => {}
        }

        // Bare ground.
        match get("natural") {
            "sand" | "beach" | "dune" => vec!["213"],
            "bare_rock" => vec!["214"],
            "scree" | "shingle" => vec!["210"],
            _ => Vec::new(),
        }
    }

    /// The codes of the line symbols a way is drawn with. See
    /// [`OsmRules::line_symbols`].
    pub fn line_codes(&self, tags: &Tags) -> Vec<&'static str> {
        if underground(tags) {
            return Vec::new();
        }
        let get = |key: &str| tags.get(key).unwrap_or("");
        let code = if let Some(highway) = tags.get("highway") {
            if tags.is("area", "yes") || self.strip_code(tags).is_some() {
                return Vec::new();
            }
            self.road(highway, tags)
        } else if let Some(railway) = tags.get("railway") {
            match railway {
                "rail" | "light_rail" | "narrow_gauge" | "subway" | "preserved" | "monorail"
                | "funicular" => Some(self.pick("509.1", "509")),
                "tram" => match self.standard {
                    Standard::Issprom => Some("509.2"),
                    // A tram runs in the street: at forest scales the
                    // street is all there is to draw.
                    Standard::Isom => None,
                },
                _ => None,
            }
        } else if let Some(waterway) = tags.get("waterway") {
            let intermittent = tags.is("intermittent", "yes");
            match waterway {
                "river" | "canal" if !intermittent => Some(self.pick("305", "304")),
                "stream" if !intermittent => Some("305"),
                "river" | "canal" | "stream" | "ditch" | "drain" => Some("306"),
                _ => None,
            }
        } else if let Some(barrier) = tags.get("barrier") {
            let low = height(tags).is_some_and(|h| h < 1.5);
            match barrier {
                "wall" | "city_wall" if low => Some(self.pick("513.1", "513")),
                "wall" | "city_wall" => Some("515"),
                "retaining_wall" => Some(self.pick("513.2", "513")),
                "fence" | "wire_fence" | "railing" if low => Some("516"),
                "fence" | "wire_fence" => Some("518"),
                "railing" | "guard_rail" => Some("516"),
                "hedge" if tags.is("area", "yes") => None,
                "hedge" => Some(self.pick("410.1", "410.4")),
                "ditch" => Some("306"),
                _ => None,
            }
        } else if let Some(power) = tags.get("power") {
            match power {
                "line" => Some(self.pick("511", "511.1")),
                "minor_line" => Some("510"),
                _ => None,
            }
        } else if tags.get("aerialway").is_some() {
            match get("aerialway") {
                "no" | "station" | "pylon" | "goods" => None,
                _ => Some("510"),
            }
        } else if tags.is("natural", "cliff") {
            Some("201")
        } else {
            match (get("man_made"), get("location")) {
                ("pipeline", "overhead") => Some("528"),
                ("pipeline", "overground") => Some("529"),
                ("cutline", _) => Some("508"),
                _ => None,
            }
        };
        code.into_iter().collect()
    }

    /// The code and width of the strip a way is drawn as (see
    /// [`OsmRules::strip`]), and whether it cuts what it runs under.
    ///
    /// A portico (a way `covered=arcade` or `colonnade`, as Bologna's are
    /// mapped: the walkway under the building) is a canopy 4 m wide, cut out
    /// of the building. On a sprint map a paved street is a paved area as wide
    /// as the street, pavements included ([`street_width`]), rather than a
    /// footprint of fixed width, so that the ground between it and the
    /// houses is not left to whatever area the street runs through — most
    /// often land not to be entered.
    pub fn strip_code(&self, tags: &Tags) -> Option<(&'static str, f64, bool)> {
        let highway = tags.get("highway")?;
        if underground(tags) || tags.is("area", "yes") {
            return None;
        }
        if matches!(tags.get("covered"), Some("arcade" | "colonnade")) && highway != "steps" {
            return Some(("522", PORTICO_WIDTH, true));
        }
        if self.standard != Standard::Issprom || surface(tags) == Surface::Unpaved {
            return None;
        }
        let heavy = matches!(
            highway,
            "motorway"
                | "trunk"
                | "primary"
                | "secondary"
                | "motorway_link"
                | "trunk_link"
                | "primary_link"
                | "secondary_link"
        );
        let width = street_width(highway, tags)?;
        Some((if heavy { "501.11" } else { "501.5" }, width, false))
    }

    /// The code of a road, a path or a stairway.
    fn road(&self, highway: &str, tags: &Tags) -> Option<&'static str> {
        let get = |key: &str| tags.get(key).unwrap_or("");
        let paved = surface(tags);
        let sprint = self.standard == Standard::Issprom;
        let code = match highway {
            "motorway" | "trunk" | "primary" | "secondary" => self.pick("501.19", "502"),
            "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" => {
                self.pick("501.18", "502")
            }
            "tertiary" => self.pick("501.9", "503"),
            "tertiary_link" => self.pick("501.8", "503"),
            "unclassified" | "residential" | "living_street" | "road" | "pedestrian" => match paved
            {
                Surface::Unpaved => self.pick("505.2", "504"),
                _ => self.pick("501.8", "503"),
            },
            "service" => match (paved, get("service")) {
                (Surface::Unpaved, _) => self.pick("505.2", "504"),
                (_, "parking_aisle" | "driveway" | "alley") => self.pick("501.7", "503"),
                _ => self.pick("501.8", "503"),
            },
            "track" => match (get("tracktype"), paved) {
                ("grade1", _) | (_, Surface::Paved) => self.pick("501.7", "503"),
                ("grade4" | "grade5", _) => self.pick("505.1", "505"),
                _ => self.pick("505.2", "504"),
            },
            // A crossing is drawn by the street it crosses.
            "footway" if tags.is("footway", "crossing") => return None,
            "footway" | "cycleway" => match paved {
                Surface::Unpaved => self.pick("505.1", "505"),
                _ => self.pick("501.6", "505"),
            },
            "bridleway" => self.pick("506", "505"),
            "path" => {
                let faint = matches!(
                    get("trail_visibility"),
                    "bad" | "horrible" | "no" | "intermediate"
                ) || matches!(get("smoothness"), "horrible" | "very_horrible");
                match paved {
                    _ if faint => "507",
                    Surface::Paved if sprint => "501.6",
                    Surface::Paved => "505",
                    _ => "506",
                }
            }
            "steps" => self.pick("532.6", "532"),
            _ => return None,
        };
        Some(code)
    }

    /// The codes of the point symbols a tagged node is drawn with. See
    /// [`OsmRules::point_symbols`].
    pub fn point_codes(&self, tags: &Tags) -> Vec<&'static str> {
        if underground(tags) {
            return Vec::new();
        }
        let get = |key: &str| tags.get(key).unwrap_or("");
        let code = match (get("natural"), get("man_made"), get("amenity")) {
            ("spring", _, _) => Some("312"),
            // A city maps every tree in its streets and parks; the map
            // shows the prominent ones only.
            ("tree", _, _) => match get("denotation") {
                "landmark" | "natural_monument" => Some("417"),
                _ => None,
            },
            ("stone", _, _) => Some("204"),
            ("cave_entrance", _, _) => Some(self.pick("203.1", "203.2")),
            ("knoll", _, _) => Some("109"),
            ("hollow" | "depression", _, _) => Some("111"),
            (_, "water_well", _) | (_, _, "fountain" | "drinking_water") => Some("311"),
            (
                _,
                "tower"
                | "mast"
                | "chimney"
                | "lighthouse"
                | "water_tower"
                | "communications_tower",
                _,
            ) => Some("524"),
            (_, _, "hunting_stand") => Some("525"),
            (_, "cairn" | "survey_point", _) => Some("526"),
            (_, _, "feeding_place") => Some("527"),
            _ => None,
        };
        let code = code.or_else(|| {
            let plaque = matches!(get("memorial"), "plaque" | "blue_plaque" | "stolperstein");
            match get("historic") {
                "memorial" if plaque => return None,
                "memorial" | "boundary_stone" | "wayside_cross" | "wayside_shrine" => {
                    return Some("526")
                }
                _ => {}
            }
            if tags.is("highway", "milestone") {
                return Some("526");
            }
            if tags.is("tourism", "information")
                && matches!(get("information"), "guidepost" | "map" | "board")
            {
                return Some("531");
            }
            match get("building") {
                "" | "no" => None,
                _ => Some("521.1"),
            }
        });
        code.into_iter().collect()
    }
}

/// The widest gap between two buildings that is taken for a mapping
/// inaccuracy and closed, in meters: no passage is this narrow.
const BUILDING_GAP: f64 = 0.5;

/// How wide a portico is, in meters: Bologna's are 3 to 5.
const PORTICO_WIDTH: f64 = 4.0;

/// How wide a pavement is, in meters.
const PAVEMENT_WIDTH: f64 = 1.5;

/// How wide a street is on the ground, in meters, pavements included: its
/// `width` where it has one, else its `lanes` at 3.25 m, else what is usual
/// for its kind; plus a pavement on each side its `sidewalk` tags say, or,
/// where they say nothing, on both sides of a street in town. `None` for a
/// way which is no street.
pub fn street_width(highway: &str, tags: &Tags) -> Option<f64> {
    let get = |key: &str| tags.get(key).unwrap_or("");
    let (usual, in_town) = match highway {
        "motorway" | "trunk" => (7.5, false),
        "primary" => (7.5, true),
        "secondary" => (7.0, true),
        "tertiary" => (6.5, true),
        "motorway_link" | "trunk_link" | "primary_link" | "secondary_link" | "tertiary_link" => {
            (5.0, false)
        }
        "residential" => (6.0, true),
        "unclassified" | "road" => (6.0, false),
        "living_street" => (5.0, false),
        "pedestrian" => (6.0, false),
        "service" => match get("service") {
            "parking_aisle" => (5.0, false),
            "driveway" | "alley" => (3.5, false),
            _ => (4.5, false),
        },
        _ => return None,
    };
    let carriageway = tags
        .get("width")
        .and_then(|w| {
            let w = w.trim();
            w.strip_suffix('m').unwrap_or(w).trim().parse::<f64>().ok()
        })
        .filter(|w| *w > 0.0)
        .or_else(|| {
            tags.get("lanes")
                .and_then(|l| l.parse::<f64>().ok())
                .map(|l| l * 3.25)
        })
        .unwrap_or(usual);
    let side = |value: &str| match value {
        "both" => Some(2.0),
        "left" | "right" | "yes" => Some(1.0),
        "no" | "none" | "separate" => Some(0.0),
        _ => None,
    };
    let either = |key: &str| match get(key) {
        "yes" => Some(1.0),
        "no" | "separate" => Some(0.0),
        _ => None,
    };
    let pavements = side(get("sidewalk"))
        .or_else(|| side(get("sidewalk:both")))
        .or_else(
            || match (either("sidewalk:left"), either("sidewalk:right")) {
                (None, None) => None,
                (l, r) => Some(l.unwrap_or(0.0) + r.unwrap_or(0.0)),
            },
        )
        .unwrap_or(if in_town { 2.0 } else { 0.0 });
    Some(carriageway + pavements * PAVEMENT_WIDTH)
}

/// Whether a feature lies under the ground: a tunnel, a culvert, an
/// underground car park, anything on a negative layer.
fn underground(tags: &Tags) -> bool {
    matches!(tags.get("tunnel"), Some("yes" | "culvert"))
        || tags.is("location", "underground")
        || tags
            .get("layer")
            .and_then(|l| l.trim().parse::<i32>().ok())
            .is_some_and(|l| l < 0)
}

/// A wall's or a fence's `height`, in metres.
fn height(tags: &Tags) -> Option<f64> {
    let text = tags.get("height")?.trim();
    let text = text.strip_suffix('m').unwrap_or(text).trim();
    text.parse().ok()
}

impl OsmRules for OrienteeringStyle {
    fn scale(&self) -> i32 {
        self.scale
    }

    /// None: the paper's own white, which on an orienteering map is
    /// runnable forest.
    fn background(&self) -> Option<i32> {
        None
    }

    fn area_symbols(&self, tags: &Tags, multipolygon: bool) -> Vec<i32> {
        self.ids(self.area_codes(tags, multipolygon))
    }

    fn line_symbols(&self, tags: &Tags) -> Vec<i32> {
        self.ids(self.line_codes(tags))
    }

    fn point_symbols(&self, tags: &Tags) -> Vec<i32> {
        self.ids(self.point_codes(tags))
    }

    fn ground(&self, symbol: i32) -> bool {
        self.ground.contains(&symbol)
    }

    /// Buildings: houses wall to wall are one block, as on a sprint map,
    /// and so are those mapped a hair apart.
    fn merge(&self, symbol: i32) -> Option<f64> {
        (symbol == self.building).then_some(BUILDING_GAP)
    }

    fn strip(&self, tags: &Tags) -> Option<Strip> {
        let (code, width, cuts) = self.strip_code(tags)?;
        Some(Strip {
            symbol: self.id(code),
            width,
            cuts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        Tags(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    /// One feature of every kind the rules know, so that every code they
    /// can give is looked up in both standards.
    fn every_feature() -> Vec<Tags> {
        let mut all = Vec::new();
        let mut add = |key: &str, values: &[&str], extra: &[(&str, &str)]| {
            for v in values {
                let mut pairs = vec![(key, *v)];
                pairs.extend_from_slice(extra);
                all.push(tags(&pairs));
            }
        };
        add("building", &["yes", "roof", "ruins"], &[]);
        add(
            "man_made",
            &["canopy", "water_well", "tower", "cairn", "cutline"],
            &[],
        );
        add("man_made", &["pipeline"], &[("location", "overhead")]);
        add("man_made", &["pipeline"], &[("location", "overground")]);
        add(
            "natural",
            &[
                "water",
                "wetland",
                "wood",
                "scrub",
                "heath",
                "sand",
                "bare_rock",
                "scree",
                "cliff",
            ],
            &[],
        );
        add(
            "natural",
            &[
                "spring",
                "tree",
                "stone",
                "cave_entrance",
                "knoll",
                "hollow",
            ],
            &[],
        );
        add("natural", &["tree"], &[("denotation", "landmark")]);
        add("natural", &["water"], &[("intermittent", "yes")]);
        add("natural", &["wood"], &[("wood:density", "sparse")]);
        add("natural", &["wood"], &[("wood:density", "dense")]);
        add(
            "landuse",
            &[
                "residential",
                "forest",
                "grass",
                "farmland",
                "orchard",
                "vineyard",
            ],
            &[],
        );
        add("leisure", &["park", "pitch"], &[]);
        add("leisure", &["pitch"], &[("surface", "asphalt")]);
        add(
            "amenity",
            &["parking", "fountain", "hunting_stand", "feeding_place"],
            &[],
        );
        add("place", &["square"], &[]);
        add("highway", &["pedestrian"], &[("area", "yes")]);
        add(
            "highway",
            &[
                "motorway",
                "primary_link",
                "tertiary",
                "tertiary_link",
                "residential",
                "service",
                "track",
                "footway",
                "cycleway",
                "bridleway",
                "path",
                "steps",
                "milestone",
            ],
            &[],
        );
        add(
            "highway",
            &["residential", "service", "path"],
            &[("surface", "gravel")],
        );
        add("highway", &["service"], &[("service", "driveway")]);
        add("highway", &["footway"], &[("covered", "arcade")]);
        add(
            "highway",
            &["secondary", "pedestrian", "living_street"],
            &[],
        );
        add("highway", &["track"], &[("tracktype", "grade1")]);
        add("highway", &["track"], &[("tracktype", "grade5")]);
        add("highway", &["path"], &[("surface", "asphalt")]);
        add("highway", &["path"], &[("trail_visibility", "bad")]);
        add("railway", &["rail", "tram"], &[]);
        add("waterway", &["river", "stream", "ditch"], &[]);
        add("waterway", &["stream"], &[("intermittent", "yes")]);
        add(
            "barrier",
            &[
                "wall",
                "retaining_wall",
                "fence",
                "railing",
                "hedge",
                "ditch",
            ],
            &[],
        );
        add("barrier", &["wall", "fence"], &[("height", "1 m")]);
        add("power", &["line", "minor_line"], &[]);
        add("aerialway", &["chair_lift"], &[]);
        add("historic", &["memorial"], &[]);
        add("tourism", &["information"], &[("information", "board")]);
        all
    }

    #[test]
    fn every_code_the_rules_give_is_in_both_standards() {
        for standard in [Standard::Issprom, Standard::Isom] {
            let style = OrienteeringStyle::new(standard);
            for tags in every_feature() {
                // `id` panics on a code the standard does not have.
                style.area_symbols(&tags, false);
                style.area_symbols(&tags, true);
                style.line_symbols(&tags);
                style.point_symbols(&tags);
                style.strip(&tags);
            }
        }
    }

    #[test]
    fn the_symbol_sets_are_at_their_own_scales() {
        assert_eq!(OrienteeringStyle::new(Standard::Issprom).scale(), 4000);
        assert_eq!(OrienteeringStyle::new(Standard::Isom).scale(), 10000);
    }

    #[test]
    fn roads_are_paved_footprints_in_sprint_and_lines_in_the_forest() {
        let sprint = OrienteeringStyle::new(Standard::Issprom);
        let forest = OrienteeringStyle::new(Standard::Isom);
        let primary = tags(&[("highway", "primary")]);
        assert_eq!(sprint.strip_code(&primary), Some(("501.11", 10.5, false)));
        assert_eq!(forest.line_codes(&primary), ["502"]);
        let residential = tags(&[("highway", "residential")]);
        assert!(sprint.line_codes(&residential).is_empty());
        assert_eq!(sprint.strip_code(&residential), Some(("501.5", 9.0, false)));
        assert_eq!(forest.line_codes(&residential), ["503"]);
        assert_eq!(forest.strip_code(&residential), None);
        // An unpaved street stays a footprint.
        let gravel = tags(&[("highway", "residential"), ("surface", "gravel")]);
        assert_eq!(sprint.line_codes(&gravel), ["505.2"]);
        // A square is paved ground, not a loop of road.
        let square = tags(&[("highway", "pedestrian"), ("area", "yes")]);
        assert_eq!(sprint.area_codes(&square, false), ["501.5"]);
        assert!(sprint.line_codes(&square).is_empty());
    }

    #[test]
    fn a_street_is_as_wide_as_its_tags_say() {
        let width = |pairs: &[(&str, &str)]| street_width("residential", &tags(pairs)).unwrap();
        assert_eq!(width(&[]), 9.0);
        assert_eq!(width(&[("sidewalk", "separate")]), 6.0);
        assert_eq!(width(&[("sidewalk", "right")]), 7.5);
        assert_eq!(width(&[("width", "4 m"), ("sidewalk", "no")]), 4.0);
        assert_eq!(width(&[("lanes", "2"), ("sidewalk:left", "yes")]), 8.0);
        assert_eq!(street_width("footway", &tags(&[])), None);
    }

    #[test]
    fn a_portico_is_a_canopy_cut_out_of_its_building() {
        for standard in [Standard::Issprom, Standard::Isom] {
            let style = OrienteeringStyle::new(standard);
            let portico = tags(&[("highway", "footway"), ("covered", "arcade")]);
            assert_eq!(style.strip_code(&portico), Some(("522", 4.0, true)));
            assert!(style.line_codes(&portico).is_empty());
        }
    }

    #[test]
    fn a_park_has_scattered_trees_and_its_lawn_is_open_land() {
        let sprint = OrienteeringStyle::new(Standard::Issprom);
        assert_eq!(
            sprint.area_codes(&tags(&[("leisure", "park")]), false),
            ["402"]
        );
        assert_eq!(
            sprint.area_codes(&tags(&[("landuse", "grass")]), false),
            ["401"]
        );
    }

    #[test]
    fn a_wall_is_impassable_unless_it_is_low() {
        let sprint = OrienteeringStyle::new(Standard::Issprom);
        assert_eq!(sprint.line_codes(&tags(&[("barrier", "wall")])), ["515"]);
        let low = tags(&[("barrier", "wall"), ("height", "0.8")]);
        assert_eq!(sprint.line_codes(&low), ["513.1"]);
    }

    #[test]
    fn nothing_underground_is_drawn() {
        let sprint = OrienteeringStyle::new(Standard::Issprom);
        let tunnel = tags(&[("highway", "residential"), ("tunnel", "yes")]);
        assert!(sprint.line_codes(&tunnel).is_empty());
        let parking = tags(&[("building", "yes"), ("layer", "-1")]);
        assert!(sprint.area_codes(&parking, false).is_empty());
    }

    #[test]
    fn land_cover_is_ground_and_buildings_are_not() {
        let sprint = OrienteeringStyle::new(Standard::Issprom);
        assert!(sprint.ground(sprint.id("520")));
        assert!(sprint.ground(sprint.id("401")));
        assert!(sprint.ground(sprint.id("501.5")));
        assert!(!sprint.ground(sprint.id("521")));
        assert!(!sprint.ground(sprint.id("522")));
        // A line is no land cover.
        assert!(!sprint.ground(sprint.id("501.8")));
    }
}
