//! A minimal reader for ASCII DXF: just enough to read the vector files
//! Karttapullautin writes (`out2.dxf`, `formlines.dxf`, `dotknolls.dxf`),
//! which [`crate::pullauta_import`] turns into a map.
//!
//! A DXF file is a flat list of *group code* / *value* line pairs. Only the
//! entities matter here, and only four kinds of them: `POLYLINE` (followed by
//! its `VERTEX`es and a `SEQEND`), `LWPOLYLINE` (its vertices inline),
//! `LINE` and `POINT`. Everything else -- headers, tables, blocks, other
//! entity kinds -- is skipped. Coordinates are kept as the file has them:
//! for Karttapullautin, meters in the projected CRS of its input, `y` up.

/// One entity: a polyline (open or closed), a two-point line, or a point.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    /// The layer the entity is on (group code 8), e.g. `"contour_index"`.
    pub layer: String,
    /// What kind of entity it is.
    pub kind: EntityKind,
    /// The entity's elevation: a polyline's own (group code 38), else its
    /// first vertex's `z` (group code 30), else `None`.
    pub elevation: Option<f64>,
    /// The vertices, `(x, y)`, in file order. A closed polyline does not
    /// repeat its first vertex here; [`Entity::closed`] says it closes.
    pub points: Vec<(f64, f64)>,
    /// Whether the polyline is flagged closed (bit 1 of group code 70).
    pub closed: bool,
}

/// The kinds of entity [`parse`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityKind {
    /// A `POLYLINE` or `LWPOLYLINE`.
    Polyline,
    /// A `LINE`: always exactly two points.
    Line,
    /// A `POINT`: always exactly one point.
    Point,
}

impl Entity {
    /// The vertices, with the first repeated at the end if the polyline is
    /// flagged closed and does not already end where it starts.
    pub fn ring_or_path(&self) -> Vec<(f64, f64)> {
        let mut points = self.points.clone();
        if self.closed && points.len() > 2 && points.first() != points.last() {
            points.push(points[0]);
        }
        points
    }

    /// Whether the entity is a closed ring: flagged closed, or ending
    /// exactly where it starts.
    pub fn is_ring(&self) -> bool {
        self.points.len() > 2 && (self.closed || self.points.first() == self.points.last())
    }
}

/// The entity being read, before it is complete.
struct Pending {
    entity: Entity,
    /// `LINE`'s second point arrives as group codes 11/21.
    end: (Option<f64>, Option<f64>),
}

/// Reads every `POLYLINE`, `LWPOLYLINE`, `LINE` and `POINT` entity out of an
/// ASCII DXF text.
///
/// Fails on a group code that is not a number or a coordinate that is not
/// one, naming the line; an entity of a kind it does not know is skipped.
pub fn parse(text: &str) -> Result<Vec<Entity>, String> {
    let mut lines = text.lines().enumerate();
    let mut out = Vec::new();
    // The entity currently being filled in, and, inside a POLYLINE, the
    // vertex being filled in.
    let mut current: Option<Pending> = None;
    let mut in_polyline = false;
    let mut vertex: Option<(Option<f64>, Option<f64>, Option<f64>)> = None;
    // An LWPOLYLINE's x and y arrive one pair at a time.
    let mut lw_x: Option<f64> = None;

    let finish_vertex = |vertex: &mut Option<(Option<f64>, Option<f64>, Option<f64>)>,
                         current: &mut Option<Pending>| {
        if let (Some((Some(x), Some(y), z)), Some(p)) = (vertex.take(), current.as_mut()) {
            if p.entity.elevation.is_none() {
                p.entity.elevation = z;
            }
            p.entity.points.push((x, y));
        }
    };

    while let Some((i, code_line)) = lines.next() {
        let Some((_, value)) = lines.next() else {
            break;
        };
        let code: i32 = code_line
            .trim()
            .parse()
            .map_err(|_| format!("line {}: {:?} is not a group code", i + 1, code_line.trim()))?;
        let value = value.trim();
        let number = || -> Result<f64, String> {
            value
                .parse::<f64>()
                .map_err(|_| format!("line {}: {value:?} is not a number", i + 2))
        };

        if code == 0 {
            // A new entity (or section marker) starts: the previous one, or
            // the previous vertex, is complete.
            if in_polyline {
                finish_vertex(&mut vertex, &mut current);
                match value {
                    "VERTEX" => {
                        vertex = Some((None, None, None));
                        continue;
                    }
                    _ => {
                        // SEQEND, or anything else, ends the polyline.
                        in_polyline = false;
                    }
                }
            }
            if let Some(p) = current.take() {
                out.push(complete(p));
            }
            lw_x = None;
            let kind = match value {
                "POLYLINE" => {
                    in_polyline = true;
                    Some(EntityKind::Polyline)
                }
                "LWPOLYLINE" => Some(EntityKind::Polyline),
                "LINE" => Some(EntityKind::Line),
                "POINT" => Some(EntityKind::Point),
                _ => None,
            };
            current = kind.map(|kind| Pending {
                entity: Entity {
                    layer: String::new(),
                    kind,
                    elevation: None,
                    points: Vec::new(),
                    closed: false,
                },
                end: (None, None),
            });
            continue;
        }

        if let Some(v) = vertex.as_mut() {
            match code {
                10 => v.0 = Some(number()?),
                20 => v.1 = Some(number()?),
                30 => v.2 = Some(number()?),
                _ => {}
            }
            continue;
        }

        let Some(p) = current.as_mut() else {
            continue;
        };
        let is_lw = p.entity.kind == EntityKind::Polyline && !in_polyline;
        match code {
            8 => p.entity.layer = value.to_string(),
            38 => p.entity.elevation = Some(number()?),
            70 if p.entity.kind == EntityKind::Polyline => {
                let flags = number()? as i64;
                p.entity.closed = flags & 1 != 0;
            }
            10 if is_lw => lw_x = Some(number()?),
            20 if is_lw => {
                if let Some(x) = lw_x.take() {
                    p.entity.points.push((x, number()?));
                }
            }
            10 if p.entity.kind != EntityKind::Polyline => {
                p.entity.points.push((number()?, f64::NAN));
            }
            20 if p.entity.kind != EntityKind::Polyline => {
                if let Some(last) = p.entity.points.last_mut() {
                    last.1 = number()?;
                }
            }
            30 if p.entity.kind != EntityKind::Polyline && p.entity.elevation.is_none() => {
                p.entity.elevation = Some(number()?);
            }
            11 if p.entity.kind == EntityKind::Line => p.end.0 = Some(number()?),
            21 if p.entity.kind == EntityKind::Line => p.end.1 = Some(number()?),
            _ => {}
        }
    }
    if in_polyline {
        finish_vertex(&mut vertex, &mut current);
    }
    if let Some(p) = current.take() {
        out.push(complete(p));
    }
    out.retain(|e| {
        !e.points.is_empty() && e.points.iter().all(|(x, y)| x.is_finite() && y.is_finite())
    });
    Ok(out)
}

/// A finished entity: a `LINE` gains its end point.
fn complete(mut p: Pending) -> Entity {
    if p.entity.kind == EntityKind::Line {
        if let (Some(x), Some(y)) = p.end {
            p.entity.points.push((x, y));
        }
    }
    p.entity
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file the way Karttapullautin writes one: a header, then entities
    /// with indented group codes.
    const SAMPLE: &str = "  0\nSECTION\n  2\nHEADER\n  9\n$EXTMIN\n 10\n0\n 20\n0\n  0\nENDSEC\n\
  0\nSECTION\n  2\nENTITIES\n\
  0\nPOLYLINE\n 66\n1\n  8\ncontour\n 38\n15\n\
  0\nVERTEX\n  8\ncontour\n 10\n1.5\n 20\n2.5\n 30\n15\n\
  0\nVERTEX\n  8\ncontour\n 10\n3\n 20\n4\n 30\n15\n\
  0\nSEQEND\n\
  0\nPOINT\n  8\ndotknoll\n 10\n7\n 20\n8\n 30\n9\n\
  0\nLWPOLYLINE\n  8\nring\n 70\n1\n 10\n0\n 20\n0\n 10\n1\n 20\n0\n 10\n1\n 20\n1\n\
  0\nLINE\n  8\ntick\n 10\n0\n 20\n0\n 11\n0\n 21\n6\n\
  0\nTEXT\n  8\nlabel\n 10\n0\n 20\n0\n\
  0\nENDSEC\n  0\nEOF\n";

    #[test]
    fn reads_every_entity_kind_karttapullautin_writes() {
        let entities = parse(SAMPLE).unwrap();
        assert_eq!(entities.len(), 4, "{entities:?}");

        let polyline = &entities[0];
        assert_eq!(polyline.layer, "contour");
        assert_eq!(polyline.kind, EntityKind::Polyline);
        assert_eq!(polyline.elevation, Some(15.0));
        assert_eq!(polyline.points, vec![(1.5, 2.5), (3.0, 4.0)]);
        assert!(!polyline.closed);

        let point = &entities[1];
        assert_eq!(point.kind, EntityKind::Point);
        assert_eq!(point.points, vec![(7.0, 8.0)]);
        assert_eq!(point.elevation, Some(9.0));

        let ring = &entities[2];
        assert!(ring.closed && ring.is_ring());
        assert_eq!(ring.points.len(), 3);
        assert_eq!(ring.ring_or_path().len(), 4);
        assert_eq!(ring.ring_or_path().last(), Some(&(0.0, 0.0)));

        let line = &entities[3];
        assert_eq!(line.kind, EntityKind::Line);
        assert_eq!(line.points, vec![(0.0, 0.0), (0.0, 6.0)]);
    }

    #[test]
    fn a_polyline_without_its_own_elevation_takes_its_first_vertex_z() {
        let text = "0\nPOLYLINE\n8\na\n0\nVERTEX\n10\n0\n20\n0\n30\n2.5\n0\nVERTEX\n10\n1\n20\n1\n30\n2.5\n0\nSEQEND\n0\nEOF\n";
        let entities = parse(text).unwrap();
        assert_eq!(entities[0].elevation, Some(2.5));
        assert_eq!(entities[0].points.len(), 2);
    }

    #[test]
    fn a_polyline_without_any_elevation_has_none() {
        let text = "0\nPOLYLINE\n8\nformline\n0\nVERTEX\n10\n0\n20\n0\n0\nVERTEX\n10\n1\n20\n1\n0\nSEQEND\n";
        let entities = parse(text).unwrap();
        assert_eq!(entities[0].elevation, None);
    }

    #[test]
    fn a_garbled_group_code_is_an_error_naming_its_line() {
        let err = parse("0\nPOINT\nx\n7\n").unwrap_err();
        assert!(err.contains("line 3"), "{err}");
    }
}
