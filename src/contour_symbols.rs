//! Classifying a map's symbols into the four families
//! [`Contours-to-Raster.md`](../../Contours-to-Raster.md) builds Step 0 out
//! of: Contours, Slope Lines, Jumps and Heavy Objects. Nothing in the rest of
//! the crate needs this classification, so it lives here rather than as
//! baked-in knowledge in `map.rs`.
//!
//! ISOM/ISSprOM symbol codes are hierarchical (`"101.1"`), and a family is
//! decided by the integer part alone -- the doc's own examples (Contour vs.
//! Index Contour, Cliff vs. Small Cliff) are all separate codes within one
//! family, not separate families. A symbol the file itself marks as a
//! drawing aid ([`Symbol::is_helper_symbol`]) is never part of any family:
//! that is what excludes rendering-only underlay/mask lines (e.g. a "mask
//! for small watercourse" symbol layered under the real one) without having
//! to recognise them by a language-specific name.

use crate::map::{LineSymbol, PointSymbol, Symbol};

/// Which of the doc's four symbol families a symbol belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolFamily {
    /// *\[Index\] Contour* -- what Step 0 builds a [`crate::gravity_model::Contour`] for.
    Contour,
    /// *Slope Line*, for a contour (not for a form line).
    SlopeLine,
    /// *Earth Bank \[minimum size\]*, *\[Small\] \[Impassable\] Cliff \[minimum size\]\[Small\]*.
    Jump,
    /// *Erosion Gully*, *\[Small\] \[Crossable\] Watercourse*, *Water Channel*.
    HeavyObject,
}

/// The integer part of a hierarchical symbol code, e.g. `"101"` for `"101.1"`.
fn code_family(code: &str) -> &str {
    code.split('.').next().unwrap_or(code)
}

/// Which family, if any, a symbol belongs to.
///
/// `None` covers both "not one of the four families" and "a helper symbol,
/// even if its code would otherwise match" -- a mask/underlay line carries
/// no real terrain information of its own.
pub fn classify_symbol(symbol: &Symbol) -> Option<SymbolFamily> {
    if symbol.is_helper_symbol() {
        return None;
    }
    let family = code_family(symbol.code());
    match symbol {
        Symbol::Line(_) => match family {
            "101" | "102" => Some(SymbolFamily::Contour),
            "104" | "105" | "106" | "201" | "202" => Some(SymbolFamily::Jump),
            "107" | "108" | "304" | "305" | "306" => Some(SymbolFamily::HeavyObject),
            _ => None,
        },
        Symbol::Point(_) => {
            if symbol.code() == "101.1" {
                Some(SymbolFamily::SlopeLine)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The first element with a non-negligible one's own outward tip, in a point
/// symbol's own coordinate space -- the direction its tick mark points in
/// before any placement rotation is applied.
///
/// A tick is a plain line segment (no arrowhead), one end at the local
/// origin (where it attaches to the main line) and the other sticking out to
/// mark the side -- and Mapper renders it identically regardless of which
/// end a symbol's own author happened to list first in its `coords`. Real
/// symbols disagree on that order (this crate's own test map has an "Earth
/// bank" tick listing its tip *before* the origin, and an "Impassable cliff"
/// tick listing the origin first, for ticks on the same physical side), so
/// reading `last - first` as earlier code here did would silently flip sign
/// for one of them. Picking whichever endpoint sits farther from the origin
/// as the tip, and reading its own `(x, y)` directly, is invariant to that
/// ordering.
fn tick_local_delta(point_symbol: &PointSymbol) -> Option<(f64, f64)> {
    const EPS: f64 = 1e-9;
    for element in &point_symbol.elements {
        let coords = &element.object.coords;
        let (Some(first), Some(last)) = (coords.first(), coords.last()) else {
            continue;
        };
        let tip = if last.x.hypot(last.y) >= first.x.hypot(first.y) {
            last
        } else {
            first
        };
        let (dx, dy) = (tip.x, tip.y);
        if dx.hypot(dy) > EPS {
            return Some((dx, dy));
        }
    }
    None
}

/// Which side of its own digitizing direction (`ls[0] -> ls[1]`) a Jump
/// symbol's gravity always lies on: `+1.0` (the side [`crate::gravity_model::side_of_tangent`]
/// calls "left") or `-1.0` ("right"). `None` when the symbol carries no
/// derivable direction (e.g. a border/fill-only cliff variant with no
/// rotatable tick).
///
/// A Jump's little directional tick (its `mid_symbol`, or `start_symbol`/
/// `end_symbol` where there is no mid symbol) is a rotatable point symbol
/// Mapper places at `rotation = atan2(tangent.y, tangent.x)` for the local
/// tangent at that spot on the line (`renderer.rs`'s `add_symbol`/
/// `render_line_part`). Working through that placement's rotation matrix for
/// an arbitrary local direction `(lx, ly)` and an arbitrary tangent `(tx,
/// ty)` shows the resulting global direction's side of the tangent (by
/// [`crate::gravity_model::side_of_tangent`]'s own sign convention) has
/// exactly the sign of `ly` alone, regardless of `lx` and regardless of
/// which tangent Mapper placed it at:
///
/// ```text
/// side_of_tangent(0, T, rotate(L, atan2(T.y, T.x)))
///     = Tx*Gy - Ty*Gx                              (definition)
///     = Tx*(ly*Tx + lx*Ty)/|T| - Ty*(lx*Tx - ly*Ty)/|T|
///     = ly*(Tx^2 + Ty^2)/|T| = ly*|T|              (same sign as ly)
/// ```
///
/// So the side is read directly off the tick's own local coordinates, with
/// no placement math needed at all: local "down" (`ly > 0`, the paper's
/// y-down convention) is the side [`crate::gravity_model::side_of_tangent`]
/// calls left, local "up" is right.
pub fn jump_gravity_side(line_symbol: &LineSymbol) -> Option<f64> {
    let tick = line_symbol
        .mid_symbol
        .as_deref()
        .or(line_symbol.start_symbol.as_deref())
        .or(line_symbol.end_symbol.as_deref())?;
    if !tick.is_rotatable {
        return None;
    }
    let (_, ly) = tick_local_delta(tick)?;
    if ly > 0.0 {
        Some(1.0)
    } else if ly < 0.0 {
        Some(-1.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{AreaSymbol, Coord, Element, Object, ObjectKind, PointSymbol, TextSymbol};

    fn line_symbol(code: &str, is_helper_symbol: bool) -> Symbol {
        Symbol::Line(LineSymbol {
            code: code.to_string(),
            is_helper_symbol,
            ..Default::default()
        })
    }

    fn point_symbol(code: &str) -> Symbol {
        Symbol::Point(PointSymbol {
            code: code.to_string(),
            ..PointSymbol::new()
        })
    }

    #[test]
    fn classifies_contours() {
        assert_eq!(
            classify_symbol(&line_symbol("101.0", false)),
            Some(SymbolFamily::Contour)
        );
        assert_eq!(
            classify_symbol(&line_symbol("102.0", false)),
            Some(SymbolFamily::Contour)
        );
    }

    #[test]
    fn classifies_slope_line_but_not_form_line_slope() {
        assert_eq!(
            classify_symbol(&point_symbol("101.1")),
            Some(SymbolFamily::SlopeLine)
        );
        assert_eq!(classify_symbol(&point_symbol("103.1")), None);
    }

    #[test]
    fn classifies_jumps_and_heavy_objects() {
        for code in ["104.0", "105.0", "106.0", "201.0", "202.1"] {
            assert_eq!(
                classify_symbol(&line_symbol(code, false)),
                Some(SymbolFamily::Jump)
            );
        }
        for code in ["107.0", "108.0", "304.0", "305.0", "306.0"] {
            assert_eq!(
                classify_symbol(&line_symbol(code, false)),
                Some(SymbolFamily::HeavyObject)
            );
        }
    }

    #[test]
    fn helper_symbols_are_never_classified() {
        assert_eq!(classify_symbol(&line_symbol("101.0", true)), None);
        assert_eq!(classify_symbol(&line_symbol("306.3", true)), None);
    }

    #[test]
    fn point_kind_with_line_family_code_is_unclassified() {
        // A point-kind object never counts as a Contour/Jump/HeavyObject,
        // even if it happened to carry one of those codes.
        assert_eq!(classify_symbol(&point_symbol("101.0")), None);
    }

    #[test]
    fn area_and_text_symbols_are_unclassified() {
        assert_eq!(
            classify_symbol(&Symbol::Area(AreaSymbol {
                code: "101.0".to_string(),
                ..AreaSymbol::new()
            })),
            None
        );
        assert_eq!(
            classify_symbol(&Symbol::Text(TextSymbol {
                code: "101.0".to_string(),
                ..Default::default()
            })),
            None
        );
    }

    fn tick_element(dx: f64, dy: f64) -> Element {
        Element {
            symbol: line_symbol("0", false),
            object: Object {
                coords: vec![Coord::new(0.0, 0.0, 0), Coord::new(dx, dy, 0)],
                ..Object::new(ObjectKind::Path(Default::default()))
            },
        }
    }

    fn tick_point_symbol(dx: f64, dy: f64, is_rotatable: bool) -> Box<PointSymbol> {
        Box::new(PointSymbol {
            is_rotatable,
            elements: vec![tick_element(dx, dy)],
            ..PointSymbol::new()
        })
    }

    /// A tick whose `coords` list the outward tip *before* the local origin
    /// -- the reverse of [`tick_element`] -- matching how a real symbol
    /// (this crate's own test map's "Earth bank", `104`) actually stores it,
    /// unlike "Impassable cliff" (`201`)'s origin-first order for a tick on
    /// the same physical side.
    fn reversed_tick_point_symbol(dx: f64, dy: f64) -> Box<PointSymbol> {
        Box::new(PointSymbol {
            is_rotatable: true,
            elements: vec![Element {
                symbol: line_symbol("0", false),
                object: Object {
                    coords: vec![Coord::new(dx, dy, 0), Coord::new(0.0, 0.0, 0)],
                    ..Object::new(ObjectKind::Path(Default::default()))
                },
            }],
            ..PointSymbol::new()
        })
    }

    #[test]
    fn jump_gravity_side_reads_local_tick_direction() {
        // Local "down" (+y, paper convention) -> left, matching the
        // 201.1 "Opasserbar brant" tick (`0 0;0 440;`) this was derived from.
        let down = LineSymbol {
            mid_symbol: Some(tick_point_symbol(0.0, 440.0, true)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&down), Some(1.0));

        // Local "up" (-y) -> right.
        let up = LineSymbol {
            mid_symbol: Some(tick_point_symbol(0.0, -470.0, true)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&up), Some(-1.0));

        // lx alone (a purely horizontal local tick) carries no side.
        let horizontal = LineSymbol {
            mid_symbol: Some(tick_point_symbol(440.0, 0.0, true)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&horizontal), None);
    }

    #[test]
    fn jump_gravity_side_is_invariant_to_the_ticks_own_coordinate_order() {
        // Same physical side (local "down", +y), one tick listing origin
        // then tip (like "Impassable cliff"), the other tip then origin
        // (like "Earth bank") -- both a plain line segment with no
        // arrowhead, so they render identically and must agree.
        let origin_first = LineSymbol {
            mid_symbol: Some(tick_point_symbol(0.0, 440.0, true)),
            ..Default::default()
        };
        let tip_first = LineSymbol {
            mid_symbol: Some(reversed_tick_point_symbol(0.0, 440.0)),
            ..Default::default()
        };
        assert_eq!(
            jump_gravity_side(&origin_first),
            jump_gravity_side(&tip_first)
        );
        assert_eq!(jump_gravity_side(&tip_first), Some(1.0));
    }

    #[test]
    fn jump_gravity_side_needs_a_rotatable_tick() {
        let not_rotatable = LineSymbol {
            mid_symbol: Some(tick_point_symbol(0.0, 440.0, false)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&not_rotatable), None);
    }

    #[test]
    fn jump_gravity_side_falls_back_to_start_then_end_symbol() {
        let start_only = LineSymbol {
            start_symbol: Some(tick_point_symbol(0.0, 440.0, true)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&start_only), Some(1.0));

        let end_only = LineSymbol {
            end_symbol: Some(tick_point_symbol(0.0, -440.0, true)),
            ..Default::default()
        };
        assert_eq!(jump_gravity_side(&end_only), Some(-1.0));
    }

    #[test]
    fn jump_gravity_side_none_with_no_tick_at_all() {
        assert_eq!(jump_gravity_side(&LineSymbol::default()), None);
    }
}
