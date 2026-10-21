//! Reads OpenStreetMap data, in its own XML format (`.osm`): nodes, ways,
//! relations and their tags.
//!
//! A different kind of input from everything else in this crate. A map file
//! says what to draw; OpenStreetMap only says what *is there* — a way tagged
//! `highway=residential`, a node tagged `natural=tree` — and leaves how it
//! looks to whoever draws it. So this module stops at the data:
//! [`crate::osm_style`] decides what each feature looks like, and
//! [`crate::osm_convert`] turns the two into a [`crate::map::Map`].
//!
//! What it does keep is the geometry OpenStreetMap only implies. A way is a
//! list of node ids, not of positions, and an area with a hole in it is a
//! multipolygon relation whose rings may each be split over several ways;
//! [`OsmData::way_coords`] and [`OsmData::multipolygon_rings`] put those
//! back together.
//!
//! The whole file is held in memory. A city extract of a few hundred MB —
//! around a million nodes — takes well under a GB, and the alternative, the
//! several passes over the file a streaming reader needs to resolve a
//! relation's ways and a way's nodes, would make the reading itself the slow
//! part.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// The key/value tags of a node, way or relation, in the order the file
/// gives them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags(pub Vec<(String, String)>);

impl Tags {
    /// The value of `key`, if the element has that tag.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the element is tagged `key=value`.
    pub fn is(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }

    /// Whether the element has no tags at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A rectangle in degrees, as OpenStreetMap gives one: `min_lon,min_lat,
/// max_lon,max_lat`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    /// The western edge, in degrees of longitude.
    pub min_lon: f64,
    /// The southern edge, in degrees of latitude.
    pub min_lat: f64,
    /// The eastern edge, in degrees of longitude.
    pub max_lon: f64,
    /// The northern edge, in degrees of latitude.
    pub max_lat: f64,
}

impl BBox {
    /// Parses `min_lon,min_lat,max_lon,max_lat`, the order OpenStreetMap's
    /// own export and API use.
    pub fn parse(text: &str) -> Result<BBox, String> {
        let values: Vec<f64> = text
            .split(',')
            .map(|v| v.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| {
                format!("not a bounding box: {text:?} (want min_lon,min_lat,max_lon,max_lat)")
            })?;
        let [min_lon, min_lat, max_lon, max_lat] = values[..] else {
            return Err(format!(
                "not a bounding box: {text:?} (want min_lon,min_lat,max_lon,max_lat)"
            ));
        };
        if !(min_lon < max_lon && min_lat < max_lat) {
            return Err(format!("empty bounding box: {text:?}"));
        }
        Ok(BBox {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        })
    }

    /// The centre, as `(lon, lat)`.
    pub fn center(&self) -> (f64, f64) {
        (
            (self.min_lon + self.max_lon) / 2.0,
            (self.min_lat + self.max_lat) / 2.0,
        )
    }

    /// Whether `(lon, lat)` lies inside or on the edge.
    pub fn contains(&self, (lon, lat): (f64, f64)) -> bool {
        lon >= self.min_lon && lon <= self.max_lon && lat >= self.min_lat && lat <= self.max_lat
    }

    /// The smallest box holding every point, or `None` for no points.
    pub fn around(points: impl IntoIterator<Item = (f64, f64)>) -> Option<BBox> {
        let mut points = points.into_iter();
        let (lon, lat) = points.next()?;
        let mut bbox = BBox {
            min_lon: lon,
            min_lat: lat,
            max_lon: lon,
            max_lat: lat,
        };
        for (lon, lat) in points {
            bbox.min_lon = bbox.min_lon.min(lon);
            bbox.min_lat = bbox.min_lat.min(lat);
            bbox.max_lon = bbox.max_lon.max(lon);
            bbox.max_lat = bbox.max_lat.max(lat);
        }
        Some(bbox)
    }
}

/// A node carrying tags of its own: a feature in its own right, a tree or a
/// bench, rather than just a vertex of some way.
#[derive(Clone, Debug)]
pub struct TaggedNode {
    /// The node id.
    pub id: i64,
    /// The position, as `(lon, lat)` in degrees.
    pub position: (f64, f64),
    /// What the node is.
    pub tags: Tags,
}

/// A way: an ordered list of nodes, closed when the last is the first.
#[derive(Clone, Debug)]
pub struct Way {
    /// The way id.
    pub id: i64,
    /// The ids of its nodes, in order.
    pub nodes: Vec<i64>,
    /// What the way is.
    pub tags: Tags,
}

impl Way {
    /// Whether the way comes back to where it started, and so can be an area.
    pub fn is_closed(&self) -> bool {
        self.nodes.len() >= 4 && self.nodes.first() == self.nodes.last()
    }
}

/// What kind of element a relation member is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    /// A node.
    Node,
    /// A way.
    Way,
    /// Another relation.
    Relation,
}

/// One member of a relation.
#[derive(Clone, Debug)]
pub struct Member {
    /// What kind of element it is.
    pub kind: MemberKind,
    /// Its id, among elements of that kind.
    pub id: i64,
    /// What part it plays, e.g. `outer` or `inner` in a multipolygon.
    pub role: String,
}

/// A relation: a group of elements which together make one feature.
#[derive(Clone, Debug)]
pub struct Relation {
    /// The relation id.
    pub id: i64,
    /// Its members, in order.
    pub members: Vec<Member>,
    /// What the relation is.
    pub tags: Tags,
}

/// The contents of an `.osm` file.
#[derive(Debug, Default)]
pub struct OsmData {
    /// The position of every node, as `(lon, lat)` in degrees, by id.
    pub nodes: HashMap<i64, (f64, f64)>,
    /// The nodes which carry tags, in file order.
    pub tagged_nodes: Vec<TaggedNode>,
    /// Every way, in file order.
    pub ways: Vec<Way>,
    /// Every relation, in file order.
    pub relations: Vec<Relation>,
    /// The `<bounds>` the file declares, where it declares one.
    pub bounds: Option<BBox>,
    way_index: HashMap<i64, usize>,
}

/// Reads an `.osm` file.
pub fn read_osm(path: &Path) -> Result<OsmData, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    parse(Reader::from_reader(std::io::BufReader::with_capacity(
        1 << 20,
        file,
    )))
    .map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads `.osm` data from a string.
pub fn read_osm_str(content: &str) -> Result<OsmData, String> {
    parse(Reader::from_reader(content.as_bytes()))
}

/// The element being read, while its children (tags, node references,
/// members) are still coming.
enum Open {
    None,
    Node(TaggedNode),
    Way(Way),
    Relation(Relation),
}

fn parse<R: BufRead>(mut reader: Reader<R>) -> Result<OsmData, String> {
    let mut data = OsmData::default();
    let mut open = Open::None;
    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| format!("XML error at byte {}: {e}", reader.buffer_position()))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    b"node" => {
                        let node = TaggedNode {
                            id: attr_i64(e, b"id")?,
                            position: (attr_f64(e, b"lon")?, attr_f64(e, b"lat")?),
                            tags: Tags::default(),
                        };
                        data.nodes.insert(node.id, node.position);
                        if !empty {
                            open = Open::Node(node);
                        }
                    }
                    b"way" => {
                        let way = Way {
                            id: attr_i64(e, b"id")?,
                            nodes: Vec::new(),
                            tags: Tags::default(),
                        };
                        if empty {
                            data.push_way(way);
                        } else {
                            open = Open::Way(way);
                        }
                    }
                    b"relation" => {
                        let relation = Relation {
                            id: attr_i64(e, b"id")?,
                            members: Vec::new(),
                            tags: Tags::default(),
                        };
                        if empty {
                            data.relations.push(relation);
                        } else {
                            open = Open::Relation(relation);
                        }
                    }
                    b"tag" => {
                        let tag = (attr_string(e, b"k")?, attr_string(e, b"v")?);
                        match &mut open {
                            Open::Node(n) => n.tags.0.push(tag),
                            Open::Way(w) => w.tags.0.push(tag),
                            Open::Relation(r) => r.tags.0.push(tag),
                            Open::None => {}
                        }
                    }
                    b"nd" => {
                        if let Open::Way(w) = &mut open {
                            w.nodes.push(attr_i64(e, b"ref")?);
                        }
                    }
                    b"member" => {
                        if let Open::Relation(r) = &mut open {
                            let kind = match attr_string(e, b"type")?.as_str() {
                                "node" => MemberKind::Node,
                                "way" => MemberKind::Way,
                                _ => MemberKind::Relation,
                            };
                            r.members.push(Member {
                                kind,
                                id: attr_i64(e, b"ref")?,
                                role: attr_string(e, b"role").unwrap_or_default(),
                            });
                        }
                    }
                    b"bounds" => {
                        data.bounds = Some(BBox {
                            min_lon: attr_f64(e, b"minlon")?,
                            min_lat: attr_f64(e, b"minlat")?,
                            max_lon: attr_f64(e, b"maxlon")?,
                            max_lat: attr_f64(e, b"maxlat")?,
                        });
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match (
                e.local_name().as_ref(),
                std::mem::replace(&mut open, Open::None),
            ) {
                (b"node", Open::Node(node)) => {
                    if !node.tags.is_empty() {
                        data.tagged_nodes.push(node);
                    }
                }
                (b"way", Open::Way(way)) => data.push_way(way),
                (b"relation", Open::Relation(relation)) => data.relations.push(relation),
                (_, still_open) => open = still_open,
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(data)
}

fn attr_string(e: &BytesStart, name: &[u8]) -> Result<String, String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == name {
            return attr
                .unescape_value()
                .map(|v| v.into_owned())
                .map_err(|err| err.to_string());
        }
    }
    Err(format!(
        "<{}> without a {} attribute",
        String::from_utf8_lossy(e.name().as_ref()),
        String::from_utf8_lossy(name)
    ))
}

fn attr_i64(e: &BytesStart, name: &[u8]) -> Result<i64, String> {
    let text = attr_string(e, name)?;
    text.parse()
        .map_err(|_| format!("not an id: {}={text:?}", String::from_utf8_lossy(name)))
}

fn attr_f64(e: &BytesStart, name: &[u8]) -> Result<f64, String> {
    let text = attr_string(e, name)?;
    text.parse()
        .map_err(|_| format!("not a number: {}={text:?}", String::from_utf8_lossy(name)))
}

impl OsmData {
    fn push_way(&mut self, way: Way) {
        self.way_index.insert(way.id, self.ways.len());
        self.ways.push(way);
    }

    /// The way with this id, if the file has it.
    pub fn way(&self, id: i64) -> Option<&Way> {
        self.way_index.get(&id).map(|&i| &self.ways[i])
    }

    /// The positions of a way's nodes, as `(lon, lat)`.
    ///
    /// A node the file does not have is left out rather than failing the
    /// whole way: an extract cut at its edge can name one. `None` where fewer
    /// than two are left, too few to draw.
    pub fn way_coords(&self, way: &Way) -> Option<Vec<(f64, f64)>> {
        let coords: Vec<_> = way
            .nodes
            .iter()
            .filter_map(|id| self.nodes.get(id).copied())
            .collect();
        (coords.len() >= 2).then_some(coords)
    }

    /// The rings of a multipolygon relation, each closed (its last position
    /// repeating its first), as `(lon, lat)`.
    ///
    /// Outer and inner rings come back together and unlabelled: drawn with
    /// the even-odd rule, as Mapper fills an area, a ring inside another is a
    /// hole whatever its role says, which is also what makes a relation with
    /// mislabelled roles come out right. A ring may be split over several
    /// ways; they are joined end to end, turning a way round where it runs
    /// the other way. Whatever cannot be closed — a ring with a way missing
    /// from the extract — is dropped.
    pub fn multipolygon_rings(&self, relation: &Relation) -> Vec<Vec<(f64, f64)>> {
        let mut open: Vec<Vec<i64>> = relation
            .members
            .iter()
            .filter(|m| {
                m.kind == MemberKind::Way
                    && (m.role == "outer" || m.role == "inner" || m.role.is_empty())
            })
            .filter_map(|m| self.way(m.id))
            .filter(|w| w.nodes.len() >= 2)
            .map(|w| w.nodes.clone())
            .collect();

        let mut rings = Vec::new();
        while let Some(mut ring) = open.pop() {
            while ring.first() != ring.last() {
                let end = *ring.last().expect("a ring has nodes");
                let Some(next) = open
                    .iter()
                    .position(|w| w.first() == Some(&end) || w.last() == Some(&end))
                else {
                    break;
                };
                let mut piece = open.swap_remove(next);
                if piece.first() != Some(&end) {
                    piece.reverse();
                }
                ring.extend_from_slice(&piece[1..]);
            }
            if ring.len() >= 4 && ring.first() == ring.last() {
                let coords: Vec<_> = ring
                    .iter()
                    .filter_map(|id| self.nodes.get(id).copied())
                    .collect();
                if coords.len() >= 4 && coords.first() == coords.last() {
                    rings.push(coords);
                }
            }
        }
        rings
    }

    /// The area the extract itself covers, for a file with no `<bounds>`.
    ///
    /// Not simply every node: an extract keeps every way that touches its
    /// area *whole*, so one river or one motorway leaving it can stretch the
    /// plain extent tens of kilometres out. The tagged nodes — trees, shops,
    /// addresses, crossings — are what an extract is dense with, and the few
    /// of them which do sit out along such a way are the outermost
    /// [`EXTENT_TRIM`] on each side, which are left out. A file with no
    /// tagged nodes at all falls back to the nodes of its ways.
    pub fn extent(&self) -> Option<BBox> {
        if let Some(bounds) = self.bounds {
            return Some(bounds);
        }
        let mut points: Vec<(f64, f64)> = self.tagged_nodes.iter().map(|n| n.position).collect();
        if points.is_empty() {
            points = self
                .ways
                .iter()
                .flat_map(|w| w.nodes.iter())
                .filter_map(|id| self.nodes.get(id).copied())
                .collect();
        }
        if points.is_empty() {
            return None;
        }
        let trim = (points.len() as f64 * EXTENT_TRIM) as usize;
        let last = points.len() - 1 - trim;
        let mut lons: Vec<f64> = points.iter().map(|p| p.0).collect();
        let mut lats: Vec<f64> = points.iter().map(|p| p.1).collect();
        lons.sort_by(f64::total_cmp);
        lats.sort_by(f64::total_cmp);
        Some(BBox {
            min_lon: lons[trim],
            min_lat: lats[trim],
            max_lon: lons[last],
            max_lat: lats[last],
        })
    }
}

/// The share of tagged nodes left out on each side of [`OsmData::extent`].
pub const EXTENT_TRIM: f64 = 0.0005;

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<osm version="0.6">
  <node id="1" lat="44.0" lon="11.0"/>
  <node id="2" lat="44.0" lon="11.1"/>
  <node id="3" lat="44.1" lon="11.1"/>
  <node id="4" lat="44.1" lon="11.0"/>
  <node id="5" lat="44.05" lon="11.05">
    <tag k="natural" v="tree"/>
  </node>
  <way id="10">
    <nd ref="1"/><nd ref="2"/><nd ref="3"/>
    <tag k="highway" v="residential"/>
  </way>
  <way id="11">
    <nd ref="1"/><nd ref="4"/><nd ref="3"/>
  </way>
  <relation id="20">
    <member type="way" ref="10" role="outer"/>
    <member type="way" ref="11" role="outer"/>
    <tag k="type" v="multipolygon"/>
    <tag k="landuse" v="grass"/>
  </relation>
</osm>"#;

    #[test]
    fn reads_nodes_ways_relations_and_their_tags() {
        let data = read_osm_str(SAMPLE).unwrap();
        assert_eq!(data.nodes.len(), 5);
        assert_eq!(data.nodes[&2], (11.1, 44.0));
        assert_eq!(data.tagged_nodes.len(), 1);
        assert!(data.tagged_nodes[0].tags.is("natural", "tree"));
        assert_eq!(data.ways.len(), 2);
        assert_eq!(data.way(10).unwrap().nodes, vec![1, 2, 3]);
        assert_eq!(
            data.way(10).unwrap().tags.get("highway"),
            Some("residential")
        );
        assert_eq!(data.relations[0].members.len(), 2);
        assert_eq!(data.relations[0].members[1].role, "outer");
    }

    #[test]
    fn a_ring_split_over_two_ways_is_joined_turning_one_round() {
        // Way 10 runs 1-2-3 and way 11 runs 1-4-3: the ring is 1-2-3-4-1
        // only with 11 reversed.
        let data = read_osm_str(SAMPLE).unwrap();
        let rings = data.multipolygon_rings(&data.relations[0]);
        assert_eq!(rings.len(), 1);
        let ring = &rings[0];
        assert_eq!(ring.len(), 5);
        assert_eq!(ring.first(), ring.last());
        let mut corners: Vec<_> = ring[..4].to_vec();
        corners.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            corners,
            vec![(11.0, 44.0), (11.0, 44.1), (11.1, 44.0), (11.1, 44.1)]
        );
    }

    #[test]
    fn a_ring_that_cannot_be_closed_is_dropped() {
        let mut data = read_osm_str(SAMPLE).unwrap();
        data.relations[0].members.pop();
        assert!(data.multipolygon_rings(&data.relations[0]).is_empty());
    }

    #[test]
    fn the_extent_is_where_the_tagged_nodes_are() {
        let data = read_osm_str(SAMPLE).unwrap();
        // The ways' corners lie further out than the one tree.
        let extent = data.extent().unwrap();
        assert_eq!((extent.min_lon, extent.min_lat), (11.05, 44.05));
        assert_eq!((extent.max_lon, extent.max_lat), (11.05, 44.05));
    }

    #[test]
    fn a_bounding_box_parses_in_openstreetmap_order() {
        let bbox = BBox::parse("11.33, 44.48,11.36,44.50").unwrap();
        assert_eq!(bbox.min_lon, 11.33);
        assert_eq!(bbox.max_lat, 44.50);
        assert!(BBox::parse("11.36,44.48,11.33,44.50").is_err());
        assert!(BBox::parse("11.33,44.48,11.36").is_err());
    }
}
