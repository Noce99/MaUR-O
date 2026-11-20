//! Serves a MOT archive as a slippy map to look at in a browser: a viewer that
//! draws the tiles with this crate's own renderer.
//!
//! ```text
//! mauro tiles serve [--host 127.0.0.1] [--port 8080] [--workers N] <archive>...
//! ```
//!
//! It prints the address it listens on (`--port 0` picks a free one) and serves
//! until it is stopped:
//!
//! | path | |
//! |---|---|
//! | `/` | the viewer, one HTML page |
//! | `/info.json` | each layer's symbol set, zoom range (and the coarsest zoom with symbols) and bounds |
//! | `/tiles/{z}/{x}/{y}.png?size=P&layers=I,J&scale=S` | a tile drawn into P pixels (512), of the layers numbered (all by default), each at its symbol set's own scale (1:10000 for ISOM, 1:4000 for ISSprOM) or at 1:S |
//!
//! One archive is one layer, one symbol set. With several, the one named first has
//! the highest priority: where two of them cover the same ground only the higher
//! shows, inside its coverage, and the lower shows elsewhere. The page can switch
//! layers on and off.
//!
//! A tile of a zoom finer than the archive's finest is that level overzoomed;
//! one with nothing in it, or coarser than the archive's coarsest, is a
//! transparent image. Where the zoom is too coarse for any symbol to appear, a
//! tile shows what the maps cover, tinted; over the symbols it is an outline.
//! Each layer is drawn at the scale of its symbol set's reference map, where its
//! symbols are as big on the ground as on the maps, so a tile looks as the maps
//! do at every zoom; `scale` draws them all at another, bigger or smaller.
//!
//! This is a tool for looking at an archive, not a server to expose: it listens
//! on `127.0.0.1` unless told otherwise and does no more than read requests
//! of its own kind.
//!
//! Exit codes: 0 after `--help`, 1 bad usage, 2 the archive cannot be read, 4 the
//! address cannot be used.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tiny_skia::{Mask, Pixmap};

use clap::Parser;
use maur_o::mot::TileId;
use maur_o::mot_archive::Archive;
use maur_o::mot_render::{composite, coverage_mask, CoverageStyle, TileRenderer};
use maur_o::mot_symbols::SymbolTable;

const VIEWER: &str = include_str!("tiles_serve.html");

#[derive(Parser)]
#[command(about = "Serves a MOT archive as a slippy map, drawn with the crate's renderer.")]
pub struct Args {
    /// The MOT archives (PMTiles files written by `mauro tiles build`), one per symbol
    /// set, the one with the highest priority first.
    #[arg(value_name = "ARCHIVE", required = true)]
    archives: Vec<PathBuf>,

    /// The address to listen on.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// The port to listen on; 0 picks a free one.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// How many threads draw tiles; one per core by default.
    #[arg(long, value_name = "N")]
    workers: Option<usize>,
}

/// One archive, one symbol set.
struct Layer {
    archive: Archive,
    table: SymbolTable,
    /// The coarsest zoom at which a symbol appears; coarser tiles have only coverage.
    object_min_zoom: u8,
}

/// What the workers share: the layers, highest priority first, and the tiles drawn
/// so far.
struct Shared {
    layers: Vec<Layer>,
    transparent: Arc<Vec<u8>>,
    cache: Mutex<HashMap<String, Arc<Vec<u8>>>>,
}

/// A reply: status line, content type, body.
struct Reply {
    status: &'static str,
    content_type: &'static str,
    /// Shared, so a tile in the cache is sent without copying it.
    body: Arc<Vec<u8>>,
    cacheable: bool,
}

fn text(status: &'static str, message: &str) -> Reply {
    Reply {
        status,
        content_type: "text/plain; charset=utf-8",
        body: Arc::new(message.as_bytes().to_vec()),
        cacheable: false,
    }
}

fn png(body: Arc<Vec<u8>>) -> Reply {
    Reply {
        status: "200 OK",
        content_type: "image/png",
        body,
        cacheable: true,
    }
}

/// The value of query parameter `name`.
fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn bounds_json(b: Option<[f64; 4]>) -> String {
    b.map_or("null".to_string(), |b| {
        format!("[{},{},{},{}]", b[0], b[1], b[2], b[3])
    })
}

fn info_json(shared: &Shared) -> String {
    let layers: Vec<String> = shared
        .layers
        .iter()
        .map(|l| {
            let info = l.archive.info();
            format!(
                "{{\"layer\":\"{}\",\"min_zoom\":{},\"max_zoom\":{},\"object_min_zoom\":{},\"bounds\":{}}}",
                info.layer.replace('"', "'"),
                info.min_zoom,
                info.max_zoom,
                l.object_min_zoom,
                bounds_json(l.archive.bounds())
            )
        })
        .collect();
    let min = |f: fn(&Layer) -> u8| shared.layers.iter().map(f).min().unwrap_or(0);
    let max = |f: fn(&Layer) -> u8| shared.layers.iter().map(f).max().unwrap_or(0);
    let boxes = shared.layers.iter().filter_map(|l| l.archive.bounds());
    let union = boxes.reduce(|a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    });
    let names: Vec<&str> = shared
        .layers
        .iter()
        .map(|l| l.archive.info().layer.as_str())
        .collect();
    format!(
        "{{\"layer\":\"{}\",\"min_zoom\":{},\"max_zoom\":{},\"object_min_zoom\":{},\"bounds\":{},\"layers\":[{}]}}",
        names.join(" + ").replace('"', "'"),
        min(|l| l.archive.info().min_zoom),
        max(|l| l.archive.info().max_zoom),
        min(|l| l.object_min_zoom),
        bounds_json(union),
        layers.join(",")
    )
}

/// One layer's drawing of `view` and the area it covers there, or `None` where
/// the layer has nothing for it.
fn layer_tile(
    layer: &Layer,
    renderer: &mut TileRenderer,
    view: TileId,
    scale: f64,
    size: u32,
) -> Result<Option<(Pixmap, Option<Mask>)>, String> {
    let info = layer.archive.info();
    if view.z < info.min_zoom {
        return Ok(None);
    }
    let data = if view.z > info.max_zoom {
        let up = u32::from(view.z - info.max_zoom);
        TileId {
            z: info.max_zoom,
            x: view.x >> up,
            y: view.y >> up,
        }
    } else {
        view
    };
    let Some(tile) = layer.archive.tile(data)? else {
        return Ok(None);
    };
    // Where the symbols are too small to appear yet, what a map covers is all
    // there is to see; over a map it is only an outline.
    let style = if view.z < layer.object_min_zoom {
        CoverageStyle::Filled
    } else {
        CoverageStyle::Outline
    };
    let drawing = renderer.render(data, &tile, view, scale, size, style)?;
    Ok(Some((drawing, coverage_mask(data, &tile, view, size))))
}

fn tile_reply(shared: &Shared, renderers: &mut [TileRenderer], path: &str, query: &str) -> Reply {
    let parts: Vec<&str> = path
        .trim_start_matches("/tiles/")
        .trim_end_matches(".png")
        .split('/')
        .collect();
    let [z, x, y] = parts[..] else {
        return text("404 Not Found", "not a tile");
    };
    let (Ok(z), Ok(x), Ok(y)) = (z.parse::<u8>(), x.parse::<u32>(), y.parse::<u32>()) else {
        return text("404 Not Found", "not a tile");
    };
    if z > 30 || (u64::from(x)) >= 1u64 << z || u64::from(y) >= 1u64 << z {
        return text("404 Not Found", "not a tile");
    }
    // Each layer at its own symbol set's scale, unless one is asked for.
    let scale: Option<f64> = match param(query, "scale") {
        None => None,
        Some(v) => match v.parse() {
            Ok(s) => Some(s),
            Err(_) => return text("400 Bad Request", "bad scale"),
        },
    };
    let size: u32 = param(query, "size")
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
        .clamp(64, 1024);
    if scale.is_some_and(|s| !(s >= 1.0 && s.is_finite())) {
        return text("400 Bad Request", "bad scale");
    }
    // The layers to show, by their place in the order given: all by default.
    let shown: Vec<usize> = match param(query, "layers") {
        None => (0..shared.layers.len()).collect(),
        Some(list) => {
            let parsed: Option<Vec<usize>> = list
                .split(',')
                .filter(|v| !v.is_empty())
                .map(|v| v.parse().ok().filter(|&i| i < shared.layers.len()))
                .collect();
            match parsed {
                Some(mut v) => {
                    v.sort_unstable();
                    v.dedup();
                    v
                }
                None => return text("400 Bad Request", "bad layers"),
            }
        }
    };
    let key = format!("{z}/{x}/{y}/{scale:?}/{size}/{shown:?}");
    if let Some(hit) = shared.cache.lock().unwrap().get(&key) {
        return png(Arc::clone(hit));
    }
    let view = TileId { z, x, y };
    // Lowest priority first, as `composite` wants them.
    let mut drawings = Vec::new();
    for &i in shown.iter().rev() {
        let renderer = &mut renderers[i];
        let scale = scale.unwrap_or_else(|| renderer.symbol_scale());
        match layer_tile(&shared.layers[i], renderer, view, scale, size) {
            Ok(Some(layer)) => drawings.push(layer),
            Ok(None) => {}
            Err(e) => return text("500 Internal Server Error", &e),
        }
    }
    let Some(picture) = composite(drawings) else {
        return png(shared.transparent.clone());
    };
    match picture.encode_png() {
        Ok(body) => {
            let body = Arc::new(body);
            let mut cache = shared.cache.lock().unwrap();
            if cache.len() > 400 {
                cache.clear();
            }
            cache.insert(key, Arc::clone(&body));
            png(body)
        }
        Err(e) => text("500 Internal Server Error", &e.to_string()),
    }
}

/// Whether the other end has closed `stream`, having sent its request: nothing
/// it could still be waiting for. A look that does not wait or take anything.
fn hung_up(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let looked = stream.peek(&mut [0u8; 1]);
    let _ = stream.set_nonblocking(false);
    match looked {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
    }
}

fn handle(shared: &Shared, renderers: &mut [TileRenderer], mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend_from_slice(&buffer[..n]),
        }
    }
    let head = String::from_utf8_lossy(&request);
    let line = head.lines().next().unwrap_or("");
    let mut words = line.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
    let reply = if method != "GET" {
        text("405 Method Not Allowed", "GET only")
    } else {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        match path {
            "/" | "/index.html" => Reply {
                status: "200 OK",
                content_type: "text/html; charset=utf-8",
                body: Arc::new(VIEWER.as_bytes().to_vec()),
                cacheable: false,
            },
            "/info.json" => Reply {
                status: "200 OK",
                content_type: "application/json",
                body: Arc::new(info_json(shared).into_bytes()),
                cacheable: false,
            },
            // A tile the page no longer wants (it scrolled away, or zoomed) has
            // had its connection closed while it waited: not drawn, so the
            // workers get on with the ones on screen.
            p if p.starts_with("/tiles/") && hung_up(&stream) => return,
            p if p.starts_with("/tiles/") => tile_reply(shared, renderers, p, query),
            _ => text("404 Not Found", "not found"),
        }
    };
    let cache = if reply.cacheable {
        "max-age=600"
    } else {
        "no-store"
    };
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.body.len(),
        cache
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&reply.body);
}

fn worker(shared: Arc<Shared>, queue: Arc<Mutex<Receiver<TcpStream>>>) {
    let mut renderers: Vec<TileRenderer> = shared
        .layers
        .iter()
        .map(|l| TileRenderer::new(&l.table))
        .collect();
    loop {
        let next = queue.lock().unwrap().recv();
        let Ok(stream) = next else { return };
        handle(&shared, &mut renderers, stream);
    }
}

fn run(args: Args) -> Result<(), (ExitCode, String)> {
    let mut layers: Vec<Layer> = Vec::new();
    for path in &args.archives {
        let archive =
            Archive::open(path).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
        let name = archive.info().layer.clone();
        let table = SymbolTable::by_name(&name).ok_or_else(|| {
            (
                ExitCode::from(2),
                format!("Error: the archive's symbols are {name}, which this tool does not know"),
            )
        })?;
        if layers.iter().any(|l| l.archive.info().layer == name) {
            return Err((
                ExitCode::from(2),
                format!("Error: two archives have the symbols {name}: one layer is one archive"),
            ));
        }
        let object_min_zoom = archive.info().min_zoom.max(table.min_object_zoom());
        layers.push(Layer {
            archive,
            table,
            object_min_zoom,
        });
    }
    let transparent = Arc::new(
        tiny_skia::Pixmap::new(1, 1)
            .and_then(|p| p.encode_png().ok())
            .unwrap_or_default(),
    );
    let listener = TcpListener::bind((args.host.as_str(), args.port)).map_err(|e| {
        (
            ExitCode::from(4),
            format!("Error: cannot listen on {}:{}: {e}", args.host, args.port),
        )
    })?;
    let address = listener
        .local_addr()
        .map_err(|e| (ExitCode::from(4), format!("Error: {e}")))?;
    let shared = Arc::new(Shared {
        layers,
        transparent,
        cache: Mutex::new(HashMap::new()),
    });
    let (send, receive) = channel::<TcpStream>();
    let queue = Arc::new(Mutex::new(receive));
    let workers = args
        .workers
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(2, |n| n.get()))
        .max(1);
    for _ in 0..workers {
        let (shared, queue) = (Arc::clone(&shared), Arc::clone(&queue));
        std::thread::spawn(move || worker(shared, queue));
    }
    let names: Vec<String> = args
        .archives
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    println!("Serving {} on http://{}/", names.join(", "), address);
    let _ = std::io::stdout().flush();
    for stream in listener.incoming().flatten() {
        if send.send(stream).is_err() {
            break;
        }
    }
    Ok(())
}

/// What an exit code of this subcommand means, as the module's own
/// documentation lists them.
pub fn exit_meaning(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "success",
        1 => "usage error",
        2 => "an archive could not be read",
        4 => "the address could not be used",
        _ => return None,
    })
}

/// Runs `mauro tiles serve`.
pub fn main(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
