//! `tiles_serve`: that it serves the viewer, the archive's description and drawn
//! tiles over HTTP, and exits with the documented code when it cannot.
//!
//! The server is started as a process on a free port, as the tool is used.
//! Like `mot.rs` this makes its map with `osm_to_omap`, so no file of ours
//! holds one.

use assert_cmd::Command;
use maur_o::mot::TileId;
use maur_o::orthophoto::web_mercator;
use predicates::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Stdio};

mod common;
use common::{make_archive, make_two_archives, HERE_LAT_LON};

/// A running server, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(archives: &[&Path]) -> Server {
        let program = Command::cargo_bin("tiles_serve").unwrap();
        let mut child = std::process::Command::new(program.get_program())
            .args(["--port", "0", "--workers", "2"])
            .args(archives)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        // "Serving FILE on http://127.0.0.1:PORT/"
        let port = line
            .trim()
            .rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches('/').parse().ok())
            .unwrap_or_else(|| panic!("no address in {line:?}"));
        Server { child, port }
    }

    /// The status line and the body of a GET.
    fn get(&self, path: &str) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&reply[..split]).to_string();
        (
            head.lines().next().unwrap().to_string(),
            reply[split + 4..].to_vec(),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tile_at(z: u8) -> TileId {
    let (x, y) = web_mercator(HERE_LAT_LON.0, HERE_LAT_LON.1);
    TileId::containing(z, x, y)
}

/// The fraction of the pixels in which two PNGs differ.
fn differing(a: &[u8], b: &[u8]) -> f64 {
    let (a, b) = (
        image::load_from_memory(a).unwrap().to_rgba8(),
        image::load_from_memory(b).unwrap().to_rgba8(),
    );
    assert_eq!(a.dimensions(), b.dimensions());
    let different = a.pixels().zip(b.pixels()).filter(|(p, q)| p != q).count();
    different as f64 / (a.width() * a.height()) as f64
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

#[test]
fn it_serves_the_viewer_and_the_description_of_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&[&make_archive(dir.path())]);
    let (status, page) = server.get("/");
    assert!(status.contains("200"), "{status}");
    assert!(String::from_utf8_lossy(&page).contains("MOT viewer"));
    let (status, info) = server.get("/info.json");
    assert!(status.contains("200"));
    let info = String::from_utf8(info).unwrap();
    assert!(
        info.contains("\"layer\":\"ISOM 2017-2\"")
            && info.contains("\"min_zoom\":0")
            && info.contains("\"object_min_zoom\":11"),
        "{info}"
    );
    assert!(info.contains("\"bounds\":["), "{info}");
}

#[test]
fn it_draws_a_tile_and_overzooms_the_finest_level() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&[&make_archive(dir.path())]);
    let t = tile_at(14);
    let (status, body) = server.get(&format!(
        "/tiles/14/{}/{}.png?scale=4000&size=256",
        t.x, t.y
    ));
    assert!(status.contains("200"), "{status}");
    assert!(body.starts_with(PNG));
    // Something is drawn: far more than the 70 bytes of an empty image.
    assert!(body.len() > 1000, "{} bytes", body.len());
    let t = tile_at(18);
    let (status, body) = server.get(&format!(
        "/tiles/18/{}/{}.png?scale=2000&size=256",
        t.x, t.y
    ));
    assert!(status.contains("200") && body.starts_with(PNG));
    assert!(body.len() > 1000, "{} bytes", body.len());
}

#[test]
fn a_tile_too_coarse_for_symbols_shows_what_the_map_covers() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&[&make_archive(dir.path())]);
    let t = tile_at(9);
    let (status, body) = server.get(&format!("/tiles/9/{}/{}.png?size=128", t.x, t.y));
    assert!(status.contains("200") && body.starts_with(PNG));
    // The coverage, tinted: more than an empty image.
    assert!(body.len() > 150, "{} bytes", body.len());
    // A tile of the same zoom somewhere else is empty.
    let (_, body) = server.get("/tiles/9/0/0.png?size=128");
    assert!(body.len() < 200, "{} bytes", body.len());
}

#[test]
fn a_tile_with_nothing_is_a_small_transparent_image_and_a_bad_path_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&[&make_archive(dir.path())]);
    for path in ["/tiles/14/0/0.png", "/tiles/5/1/1.png"] {
        let (status, body) = server.get(path);
        assert!(status.contains("200"), "{path}: {status}");
        assert!(body.starts_with(PNG) && body.len() < 200);
    }
    assert!(server.get("/nope").0.contains("404"));
    assert!(server.get("/tiles/14/99999999/0.png").0.contains("404"));
    assert!(server.get("/tiles/x/y/z.png").0.contains("404"));
    assert!(server.get("/tiles/14/0/0.png?scale=-3").0.contains("400"));
}

#[test]
fn archives_are_layers_and_the_first_has_the_priority() {
    let dir = tempfile::tempdir().unwrap();
    let (sprint, forest) = make_two_archives(dir.path());
    let server = Server::start(&[&sprint, &forest]);
    let (_, info) = server.get("/info.json");
    let info = String::from_utf8(info).unwrap();
    let first = info.find("\"layer\":\"ISSprOM 2019\"").unwrap();
    let second = info.rfind("\"layer\":\"ISOM 2017-2\"").unwrap();
    assert!(info.contains("\"layers\":[") && first < second, "{info}");

    let t = tile_at(16);
    let tile = |layers: &str| {
        let (status, body) = server.get(&format!(
            "/tiles/16/{}/{}.png?scale=3000&size=256{layers}",
            t.x, t.y
        ));
        assert!(status.contains("200"), "{status}");
        body
    };
    let both = tile("");
    let (top, bottom) = (tile("&layers=0"), tile("&layers=1"));
    assert!(both.starts_with(PNG) && both.len() > 1000);
    // The same ground is mapped in both, so where the first covers only it shows:
    // the stacked picture is the first's, but for the edge of what it covers,
    // where the second shows. The two symbol sets draw it differently.
    let (near, far) = (differing(&both, &top), differing(&top, &bottom));
    assert!(
        near < 0.03,
        "stacked and first differ in {near} of the pixels"
    );
    assert!(far > near * 3.0 && far > 0.01, "{near} against {far}");
    assert_eq!(tile("&layers=0,1"), both);
    assert!(server.get("/tiles/16/1/1.png?layers=9").0.contains("400"));
    assert!(server.get("/tiles/16/1/1.png?layers=x").0.contains("400"));
}

#[test]
fn it_exits_with_the_documented_codes() {
    Command::cargo_bin("tiles_serve").unwrap().assert().code(1);
    Command::cargo_bin("tiles_serve")
        .unwrap()
        .arg("no_such_file.pmtiles")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot read"));
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path());
    Command::cargo_bin("tiles_serve")
        .unwrap()
        .args(["--host", "256.256.256.256"])
        .arg(&archive)
        .assert()
        .code(4);
    // One layer is one archive: the same symbol set twice is refused.
    Command::cargo_bin("tiles_serve")
        .unwrap()
        .arg(&archive)
        .arg(&archive)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("one layer is one archive"));
}
