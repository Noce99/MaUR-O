//! Serves a MOT archive as a slippy map to look at in a browser: a viewer that
//! draws the tiles with this crate's own renderer.
//!
//! ```text
//! tiles_serve [--host 127.0.0.1] [--port 8080] [--workers N] <archive>
//! ```
//!
//! It prints the address it listens on (`--port 0` picks a free one) and serves
//! until it is stopped:
//!
//! | path | |
//! |---|---|
//! | `/` | the viewer, one HTML page |
//! | `/info.json` | the archive's layer, zoom range and bounds |
//! | `/tiles/{z}/{x}/{y}.png?scale=S&size=P` | a tile drawn at 1:S (10000 by default) into P pixels (512) |
//!
//! A tile of a zoom finer than the archive's finest is that level overzoomed;
//! one coarser than its coarsest, or with nothing in it, is a transparent
//! image. The scale is a parameter, as it is the viewer's choice: the page
//! offers fixed ones and one true to the screen, where a millimetre of paper
//! is as big on screen at every zoom.
//!
//! This is a tool for looking at an archive, not a server to expose: it listens
//! on `127.0.0.1` unless told otherwise and does no more than read requests
//! of its own kind. A line cut at a tile's border is drawn from the cut, so a
//! dashed line's dashes restart there.
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

use clap::Parser;
use maur_o::mot::TileId;
use maur_o::mot_archive::Archive;
use maur_o::mot_render::TileRenderer;
use maur_o::mot_symbols::SymbolTable;

const VIEWER: &str = include_str!("tiles_serve.html");

#[derive(Parser)]
#[command(
    version,
    about = "Serves a MOT archive as a slippy map, drawn with the crate's renderer."
)]
struct Args {
    /// The MOT archive (a PMTiles file written by omap_to_tiles).
    #[arg(value_name = "ARCHIVE")]
    archive: PathBuf,

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

/// What the workers share: the archive, the symbols, and the tiles drawn so far.
struct Shared {
    archive: Archive,
    table: SymbolTable,
    transparent: Vec<u8>,
    cache: Mutex<HashMap<String, Arc<Vec<u8>>>>,
}

/// A reply: status line, content type, body.
struct Reply {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    cacheable: bool,
}

fn text(status: &'static str, message: &str) -> Reply {
    Reply {
        status,
        content_type: "text/plain; charset=utf-8",
        body: message.as_bytes().to_vec(),
        cacheable: false,
    }
}

fn png(body: Vec<u8>) -> Reply {
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

fn info_json(shared: &Shared) -> String {
    let info = shared.archive.info();
    let bounds = shared.archive.bounds().map_or("null".to_string(), |b| {
        format!("[{},{},{},{}]", b[0], b[1], b[2], b[3])
    });
    format!(
        "{{\"layer\":\"{}\",\"min_zoom\":{},\"max_zoom\":{},\"bounds\":{}}}",
        info.layer.replace('"', "'"),
        info.min_zoom,
        info.max_zoom,
        bounds
    )
}

fn tile_reply(shared: &Shared, renderer: &mut TileRenderer, path: &str, query: &str) -> Reply {
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
    let scale: f64 = param(query, "scale")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000.0);
    let size: u32 = param(query, "size")
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
        .clamp(64, 1024);
    if !(scale >= 1.0 && scale.is_finite()) {
        return text("400 Bad Request", "bad scale");
    }
    let key = format!("{z}/{x}/{y}/{scale}/{size}");
    if let Some(hit) = shared.cache.lock().unwrap().get(&key) {
        return png(hit.as_ref().clone());
    }
    let info = shared.archive.info();
    let view = TileId { z, x, y };
    if z < info.min_zoom {
        return png(shared.transparent.clone());
    }
    let data = if z > info.max_zoom {
        let up = u32::from(z - info.max_zoom);
        TileId {
            z: info.max_zoom,
            x: x >> up,
            y: y >> up,
        }
    } else {
        view
    };
    let tile = match shared.archive.tile(data) {
        Ok(Some(tile)) => tile,
        Ok(None) => return png(shared.transparent.clone()),
        Err(e) => return text("500 Internal Server Error", &e),
    };
    let body = renderer
        .render(data, &tile, view, scale, size)
        .and_then(|p| p.encode_png().map_err(|e| e.to_string()));
    match body {
        Ok(body) => {
            let mut cache = shared.cache.lock().unwrap();
            if cache.len() > 400 {
                cache.clear();
            }
            cache.insert(key, Arc::new(body.clone()));
            png(body)
        }
        Err(e) => text("500 Internal Server Error", &e),
    }
}

fn handle(shared: &Shared, renderer: &mut TileRenderer, mut stream: TcpStream) {
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
                body: VIEWER.as_bytes().to_vec(),
                cacheable: false,
            },
            "/info.json" => Reply {
                status: "200 OK",
                content_type: "application/json",
                body: info_json(shared).into_bytes(),
                cacheable: false,
            },
            p if p.starts_with("/tiles/") => tile_reply(shared, renderer, p, query),
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
    let mut renderer = TileRenderer::new(&shared.table);
    loop {
        let next = queue.lock().unwrap().recv();
        let Ok(stream) = next else { return };
        handle(&shared, &mut renderer, stream);
    }
}

fn run() -> Result<(), (ExitCode, String)> {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) => {
            let _ = e.print();
            return Err((
                ExitCode::from(if e.exit_code() == 0 { 0 } else { 1 }),
                String::new(),
            ));
        }
    };
    let archive =
        Archive::open(&args.archive).map_err(|e| (ExitCode::from(2), format!("Error: {e}")))?;
    let table = SymbolTable::by_name(&archive.info().layer).ok_or_else(|| {
        (
            ExitCode::from(2),
            format!(
                "Error: the archive's symbols are {}, which this tool does not know",
                archive.info().layer
            ),
        )
    })?;
    let transparent = tiny_skia::Pixmap::new(1, 1)
        .and_then(|p| p.encode_png().ok())
        .unwrap_or_default();
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
        archive,
        table,
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
    println!("Serving {} on http://{}/", args.archive.display(), address);
    let _ = std::io::stdout().flush();
    for stream in listener.incoming().flatten() {
        if send.send(stream).is_err() {
            break;
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            code
        }
    }
}
