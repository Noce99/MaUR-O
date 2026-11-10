//! Adding a georeferenced image to a Mapper map file as a template: the
//! picture a mapper draws over.
//!
//! This edits the file's own text instead of going through [`crate::map`].
//! The model holds what a map *draws*, and a file written back from it would
//! lose everything else Mapper keeps there: print settings, views, the
//! templates already open. Only three places change:
//!
//! - the `<template>` itself goes first in the top-level `<templates>` list,
//!   so it is the bottom-most template, behind the map and behind every other
//!   template. The list's `count` grows by one, and so does
//!   `first_front_template`, the index of the first template drawn *over*
//!   the map: everything already behind the map stays behind it.
//! - every `<ref template="i">` in the saved view (`<view><map_view>`) names
//!   its template by that index, so each one moves up by one.
//! - the view gets a `<ref>` of its own for the new template, making it
//!   visible when the map is opened.
//!
//! The template is written the way Mapper writes a georeferenced image
//! (`TemplateImage::saveTypeSpecificTemplateConfiguration`): `georef="true"`
//! and a `<crs_spec>`. Where the image *is* comes from the world file beside
//! it, which Mapper finds by the image's own name (`world_file.cpp`:
//! `.jpg` → `.jgw`).

/// The image template defaults Mapper writes at the end of every
/// `<templates>` list.
const DEFAULTS: &str =
    "<defaults use_meters_per_pixel=\"true\" meters_per_pixel=\"0\" dpi=\"0\" scale=\"0\"/>";

/// A georeferenced image to add to a map as a template.
#[derive(Clone, Debug)]
pub struct ImageTemplate<'a> {
    /// What the template list shows: the image's file name.
    pub name: &'a str,
    /// The image's absolute path.
    pub path: &'a str,
    /// The image's path relative to the map file. What Mapper tries first.
    pub relpath: &'a str,
    /// The EPSG code of the CRS the image's world file is in.
    pub epsg: i32,
}

/// What [`add_image_template`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// The template was added.
    Added,
    /// The map already had a template of that name; the file is unchanged.
    AlreadyThere,
}

/// `xml` (a whole `.omap`/`.xmap` file) with `template` added as its
/// bottom-most template. See the module documentation for what changes.
///
/// A file with no `<templates>` list at all (a hand-written one; Mapper
/// always writes one) gets one, before its `<view>` -- which refers to
/// templates by index, so Mapper must read them first -- or else at the end
/// of the `<map>`. An error where there is no `</map>` to put it in.
pub fn add_image_template(xml: &str, template: &ImageTemplate) -> Result<(String, Added), String> {
    let Some(list) = find_element(xml, "templates", 0) else {
        let at = match find_element(xml, "view", 0) {
            Some(view) => view.start,
            None => xml.rfind("</map>").ok_or("the file is not a Mapper map")?,
        };
        let with_list = format!(
            "{}<templates count=\"0\" first_front_template=\"0\">\n{}\n</templates>\n{}",
            &xml[..at],
            DEFAULTS,
            &xml[at..]
        );
        return add_image_template(&with_list, template);
    };
    let list_end = list.end.unwrap_or(list.open_end);
    let name_attr = format!(" name=\"{}\"", escape(template.name));
    if xml[list.open_end..list_end].contains(&name_attr) {
        return Ok((xml.to_owned(), Added::AlreadyThere));
    }

    let open_tag = &xml[list.start..list.open_end];
    let count = attr(open_tag, "count")
        .and_then(|c| c.parse::<u32>().ok())
        .unwrap_or(0);
    let front = attr(open_tag, "first_front_template")
        .and_then(|c| c.parse::<u32>().ok())
        .unwrap_or(0);
    let mut new_open = set_attr(open_tag, "count", &(count + 1).to_string());
    new_open = set_attr(&new_open, "first_front_template", &(front + 1).to_string());

    let element = format!(
        "\n<template type=\"TemplateImage\" open=\"true\" name=\"{}\" path=\"{}\" relpath=\"{}\" \
         georef=\"true\"><crs_spec>+init=epsg:{}</crs_spec></template>",
        escape(template.name),
        escape(template.path),
        escape(template.relpath),
        template.epsg
    );

    let mut out = String::with_capacity(xml.len() + element.len() + 256);
    out.push_str(&xml[..list.start]);
    match list.end {
        Some(_) => {
            out.push_str(&new_open);
            out.push_str(&element);
        }
        // `<templates .../>`: open it, and give it the defaults Mapper
        // always writes inside.
        None => {
            out.push_str(&new_open[..new_open.len() - 2]);
            out.push('>');
            out.push_str(&element);
            out.push('\n');
            out.push_str(DEFAULTS);
            out.push_str("\n</templates>");
        }
    }
    let rest_start = list.open_end;
    out.push_str(&add_view_ref(&xml[rest_start..]));
    Ok((out, Added::Added))
}

/// `rest` (the file after the top-level `<templates>` opening tag) with the
/// saved view's template references shifted up by one and one added, at
/// index 0, for the new template. Unchanged where there is no view, or it
/// keeps no per-template visibility (no `count` on its `<templates>`).
fn add_view_ref(rest: &str) -> String {
    let Some(view) = find_element(rest, "map_view", 0) else {
        return rest.to_owned();
    };
    let view_end = view.end.unwrap_or(view.open_end);
    let Some(list) = find_element(&rest[..view_end], "templates", view.open_end) else {
        return rest.to_owned();
    };
    let open_tag = &rest[list.start..list.open_end];
    let Some(count) = attr(open_tag, "count").and_then(|c| c.parse::<u32>().ok()) else {
        return rest.to_owned();
    };
    let new_open = set_attr(open_tag, "count", &(count + 1).to_string());
    let reference = "<ref template=\"0\" visible=\"true\" opacity=\"1\"/>";

    let mut out = String::with_capacity(rest.len() + 64);
    out.push_str(&rest[..list.start]);
    match list.end {
        Some(end) => {
            out.push_str(&new_open);
            out.push_str(reference);
            out.push_str(&shift_refs(&rest[list.open_end..end]));
            out.push_str(&rest[end..]);
        }
        None => {
            out.push_str(&new_open[..new_open.len() - 2]);
            out.push('>');
            out.push_str(reference);
            out.push_str("</templates>");
            out.push_str(&rest[list.open_end..]);
        }
    }
    out
}

/// `body` with every `<ref template="i"` made `i + 1`.
fn shift_refs(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 16);
    let mut rest = body;
    while let Some(at) = rest.find("<ref ") {
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e + 1);
        let tag = &rest[at..tag_end];
        out.push_str(&rest[..at]);
        match attr(tag, "template").and_then(|i| i.parse::<u32>().ok()) {
            Some(index) => out.push_str(&set_attr(tag, "template", &(index + 1).to_string())),
            None => out.push_str(tag),
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

/// Where an element is in a document's text.
struct Element {
    /// Its opening tag's `<`.
    start: usize,
    /// Just past its opening tag's `>`.
    open_end: usize,
    /// Its closing tag's `<`; `None` for a self-closing `<name .../>`.
    end: Option<usize>,
}

/// The first `<name>` element at or after `from`. Not a general XML parser:
/// it relies on a `<` in a Mapper file only ever starting a tag (text and
/// attribute values have theirs escaped) and on the element not nesting
/// inside itself, both true of `templates` and `map_view`.
fn find_element(xml: &str, name: &str, from: usize) -> Option<Element> {
    let opening = format!("<{name}");
    let mut search = from;
    let start = loop {
        let at = search + xml[search..].find(&opening)?;
        // `<templates` must not match `<templatesfoo`.
        match xml[at + opening.len()..].chars().next() {
            Some(c) if c == '>' || c == '/' || c.is_whitespace() => break at,
            _ => search = at + opening.len(),
        }
    };
    let open_end = start + xml[start..].find('>')? + 1;
    if xml[..open_end].ends_with("/>") {
        return Some(Element {
            start,
            open_end,
            end: None,
        });
    }
    let end = open_end + xml[open_end..].find(&format!("</{name}>"))?;
    Some(Element {
        start,
        open_end,
        end: Some(end),
    })
}

/// The value of attribute `name` in the opening tag `tag`.
fn attr<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

/// `tag` with attribute `name` set to `value`: replaced where it is there,
/// added at the end where it is not.
fn set_attr(tag: &str, name: &str, value: &str) -> String {
    let key = format!(" {name}=\"");
    if let Some(at) = tag.find(&key) {
        let start = at + key.len();
        let len = tag[start..].find('"').unwrap_or(0);
        return format!("{}{value}{}", &tag[..start], &tag[start + len..]);
    }
    let close = if tag.ends_with("/>") {
        tag.len() - 2
    } else {
        tag.len() - 1
    };
    format!("{}{key}{value}\"{}", &tag[..close], &tag[close..])
}

/// `text` safe inside a double-quoted attribute.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORTHO: ImageTemplate<'static> = ImageTemplate {
        name: "forest_agea2023.jpg",
        path: "/maps/forest_agea2023.jpg",
        relpath: "forest_agea2023.jpg",
        epsg: 32632,
    };

    /// A map with two templates, one behind it and one in front, as Mapper
    /// saves it (trimmed).
    const MAP: &str = r#"<map><barrier>
<templates count="2" first_front_template="1">
<template type="TemplateImage" open="true" name="behind.png" path="behind.png" relpath="behind.png"><transformations/></template>
<template type="TemplateImage" open="true" name="front.png" path="front.png" relpath="front.png"><transformations/></template>
<defaults use_meters_per_pixel="true" meters_per_pixel="0" dpi="0" scale="0"/>
</templates>
<view>
<map_view zoom="1" position_x="0" position_y="0"><map opacity="1" visible="true"/><templates hidden="false" count="2"><ref template="1" visible="true" opacity="1"/><ref template="0" visible="true" opacity="0.5"/></templates></map_view>
</view>
</barrier></map>"#;

    #[test]
    fn the_image_goes_first_and_every_index_moves_up() {
        let (out, added) = add_image_template(MAP, &ORTHO).unwrap();
        assert_eq!(added, Added::Added);
        assert!(out.contains(r#"<templates count="3" first_front_template="2">
<template type="TemplateImage" open="true" name="forest_agea2023.jpg" path="/maps/forest_agea2023.jpg" relpath="forest_agea2023.jpg" georef="true"><crs_spec>+init=epsg:32632</crs_spec></template>
<template type="TemplateImage" open="true" name="behind.png""#), "{out}");
        assert!(out.contains(r#"<templates hidden="false" count="3"><ref template="0" visible="true" opacity="1"/><ref template="2" visible="true" opacity="1"/><ref template="1" visible="true" opacity="0.5"/></templates>"#), "{out}");
        assert!(out.ends_with("</map_view>\n</view>\n</barrier></map>"));
    }

    #[test]
    fn adding_it_twice_changes_nothing() {
        let (once, _) = add_image_template(MAP, &ORTHO).unwrap();
        let (twice, added) = add_image_template(&once, &ORTHO).unwrap();
        assert_eq!(added, Added::AlreadyThere);
        assert_eq!(once, twice);
    }

    /// What `xml_writer` (and so `osm_to_omap`) writes: no templates and no
    /// view.
    #[test]
    fn a_map_without_templates_gets_its_first() {
        let map = "<map><barrier>\n<templates count=\"0\" first_front_template=\"0\">\n<defaults/>\n</templates>\n</barrier></map>";
        let (out, _) = add_image_template(map, &ORTHO).unwrap();
        assert!(out.starts_with("<map><barrier>\n<templates count=\"1\" first_front_template=\"1\">\n<template type=\"TemplateImage\""));
        assert!(out.ends_with("</template>\n<defaults/>\n</templates>\n</barrier></map>"));
    }

    #[test]
    fn self_closing_lists_are_opened() {
        let map = "<map><templates count=\"0\" first_front_template=\"0\"/><view><map_view zoom=\"1\"><templates hidden=\"false\" count=\"0\"/></map_view></view></map>";
        let (out, _) = add_image_template(map, &ORTHO).unwrap();
        assert!(out.contains("<templates count=\"1\" first_front_template=\"1\">\n<template "));
        assert!(out.contains("<defaults use_meters_per_pixel=\"true\" meters_per_pixel=\"0\" dpi=\"0\" scale=\"0\"/>\n</templates><view>"));
        assert!(out.contains("<templates hidden=\"false\" count=\"1\"><ref template=\"0\" visible=\"true\" opacity=\"1\"/></templates></map_view>"));
    }

    /// A view that keeps no per-template visibility is left alone.
    #[test]
    fn a_view_without_details_is_left_alone() {
        let map = "<map><templates count=\"0\" first_front_template=\"0\"></templates><view><map_view><templates hidden=\"true\"/></map_view></view></map>";
        let (out, _) = add_image_template(map, &ORTHO).unwrap();
        assert!(
            out.ends_with("<view><map_view><templates hidden=\"true\"/></map_view></view></map>")
        );
    }

    #[test]
    fn names_are_escaped() {
        let odd = ImageTemplate {
            name: "a&b \"c\".jpg",
            ..ORTHO
        };
        let (out, _) = add_image_template(MAP, &odd).unwrap();
        assert!(out.contains(r#"name="a&amp;b &quot;c&quot;.jpg""#));
        let (again, added) = add_image_template(&out, &odd).unwrap();
        assert_eq!((again, added), (out, Added::AlreadyThere));
    }

    #[test]
    fn a_file_without_templates_gets_a_list() {
        let (out, added) = add_image_template("<map><barrier/></map>", &ORTHO).unwrap();
        assert_eq!(added, Added::Added);
        assert!(out.starts_with("<map><barrier/><templates count=\"1\" first_front_template=\"1\">\n<template type=\"TemplateImage\""), "{out}");
        assert!(out.ends_with("</template>\n<defaults use_meters_per_pixel=\"true\" meters_per_pixel=\"0\" dpi=\"0\" scale=\"0\"/>\n</templates>\n</map>"), "{out}");

        // Before the view, which needs the templates read first.
        let (out, _) = add_image_template("<map><view><map_view/></view></map>", &ORTHO).unwrap();
        assert!(
            out.contains("</templates>\n<view><map_view/></view></map>"),
            "{out}"
        );

        assert!(add_image_template("<html/>", &ORTHO).is_err());
    }
}
