//! A localhost dashboard for `skout report`.
//!
//! Deliberately hand-rolled on `std::net`: skout ships as one static binary with
//! no runtime, and pulling in an async web stack to answer two routes would cost
//! more in build weight than the feature is worth. The server binds to loopback
//! only, serves an embedded page, and never touches the network.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use crate::config::Config;
use crate::report;

const INDEX: &str = include_str!("ui/index.html");

/// Requests are tiny (a path and a few headers); anything larger is not ours.
const MAX_HEADER_BYTES: usize = 8 * 1024;

pub fn run(port: u16, open_browser: bool) -> Result<()> {
    let listener = bind(port)?;
    let addr = listener.local_addr()?;
    let url = format!("http://127.0.0.1:{}", addr.port());

    println!();
    println!("  skout dashboard  {url}");
    println!("  loopback only · no telemetry · ctrl-c to stop");
    println!();

    if open_browser {
        let _ = open_url(&url);
    }

    for stream in listener.incoming() {
        match stream {
            // One connection at a time is plenty for a single-viewer dashboard,
            // and it keeps the SQLite handle uncontended.
            Ok(s) => {
                if let Err(e) = handle(s) {
                    eprintln!("skout: {e:#}");
                }
            }
            Err(e) => eprintln!("skout: accept failed: {e}"),
        }
    }
    Ok(())
}

/// Bind the requested port, then walk forward a little if it is taken, so a
/// stale tab holding 7331 does not stop the command.
fn bind(port: u16) -> Result<TcpListener> {
    let mut last = None;
    for p in port..port.saturating_add(10) {
        match TcpListener::bind(("127.0.0.1", p)) {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap()).with_context(|| format!("no free port in {port}..{}", port + 10))
}

fn handle(mut stream: TcpStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }

    // Drain headers so the client sees a clean response rather than a reset.
    let mut consumed = request_line.len();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        consumed += n;
        if n == 0 || line == "\r\n" || line == "\n" || consumed > MAX_HEADER_BYTES {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    if method != "GET" {
        return respond(&mut stream, 405, "text/plain; charset=utf-8", b"method not allowed");
    }

    match path {
        "/" | "/index.html" => respond(&mut stream, 200, "text/html; charset=utf-8", INDEX.as_bytes()),
        "/api/report" => match report_json(query) {
            Ok(body) => respond(&mut stream, 200, "application/json", body.as_bytes()),
            Err(e) => {
                let body = serde_json::json!({ "error": format!("{e:#}") }).to_string();
                respond(&mut stream, 500, "application/json", body.as_bytes())
            }
        },
        _ => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

fn report_json(query: &str) -> Result<String> {
    let window = param(query, "window").unwrap_or_else(|| "week".into());
    let scope_all = param(query, "scope").as_deref() != Some("project");

    let cfg = Config::load();
    let conn = crate::db::open()?;
    let cwd = std::env::current_dir()?.to_string_lossy().to_string();
    let (since, label) = crate::window_from(&window);

    let data = report::collect(
        &conn,
        &cfg,
        &report::Opts { scope_all, cwd, since, window_label: label, json: true },
    )?;
    Ok(data.to_string())
}

/// Minimal `application/x-www-form-urlencoded` lookup — the only values we take
/// are short enum-ish strings, and both are validated by the caller.
fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn open_url(url: &str) -> Result<()> {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd).arg(url).spawn()?;
    Ok(())
}
