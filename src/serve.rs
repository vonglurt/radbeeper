// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// A STATIC FILE SERVER, AND NOTHING ELSE. `frames.html` carries its newest
// month inside it as base64 and opens straight off the disk -- but a month
// that falls past the embedding budget is `fetch`ed instead, and a fetch from
// a `file://` page is refused by every browser that has shipped this decade.
// That is the whole reason this exists: the viewer needs an origin.
//
// NO DEPENDENCIES, because the manifest says one and it is libc. A static
// server over `std::net` is a request line, a path and a file; the crates
// that would do it instead bring an async runtime to move eight kilobytes of
// decay timing across a loopback interface.
//
// NOT CALLED `serve`. `radbeeper service` holds the serial port and writes
// the log, and a program that already warns about two readers fighting over
// one port has no business shipping a second verb two letters away from it.
// `preview` says what it is for, and what it is not for: it binds loopback
// unless told otherwise, speaks HTTP/1.0 with no keep-alive, and will not
// serve a byte from outside the directory it was pointed at.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// What the browser is told a file is.
///
/// THE `.bin` ENTRY IS THE LOAD-BEARING ONE. The viewer reads a month with
/// `arrayBuffer()`, so the bytes must arrive unmolested; served as any text
/// type a browser is entitled to decode them and the frame decoder meets
/// replacement characters where it expected a magic.
fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "md" | "hex" | "log" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/vnd.microsoft.icon",
        "xml" => "application/xml",
        // .bin, and anything else: bytes, and the browser is not to guess.
        _ => "application/octet-stream",
    }
}

/// `%20` and friends. Anything malformed is left as it stands rather than
/// guessed at -- a path that does not decode is a path that will not be found.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file a request line asks for, or `None` when it asks for something
/// outside the root.
///
/// TWO GATES, NOT ONE. The first walks the components and refuses `..`
/// outright, so nothing climbs even on a system where the target does not
/// exist yet. The second canonicalises what is left and checks it really did
/// land under the root, which is what catches a symlink pointing out of the
/// tree -- a check the component walk cannot make, because the escape is in
/// the filesystem rather than in the text.
pub fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next().unwrap_or("");
    if !path.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(path);
    let mut out = root.to_path_buf();
    for part in decoded.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        out.push(part);
    }
    // A directory is its index, and a directory without one is not listed:
    // this serves a site that was generated, and a generated site has an
    // index. Listing would be a second thing to get right for no reader.
    if out.is_dir() {
        out.push("index.html");
    }
    let real = fs::canonicalize(&out).ok()?;
    let base = fs::canonicalize(root).ok()?;
    if !real.starts_with(&base) {
        return None;
    }
    if real.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(real)
}

fn head(status: &str, kind: &str, len: usize) -> String {
    format!(
        "HTTP/1.0 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        status, kind, len
    )
}

fn fail(stream: &mut TcpStream, status: &str, why: &str) {
    let body = format!("{}\n", why);
    let _ = stream.write_all(head(status, "text/plain; charset=utf-8", body.len()).as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// One request, start to finish. Returns the line to print.
fn handle(stream: &mut TcpStream, root: &Path) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => return format!("-- -- (could not read: {})", e),
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return "-- -- (no request)".to_string();
    }
    // The headers are read and dropped: nothing here varies by them, and a
    // socket left with bytes in it is a client left waiting on a reset.
    let mut junk = String::new();
    while let Ok(n) = reader.read_line(&mut junk) {
        if n == 0 || junk.trim().is_empty() {
            break;
        }
        junk.clear();
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" && method != "HEAD" {
        fail(stream, "405 Method Not Allowed", "GET and HEAD only.");
        return format!("{} {} -> 405", method, target);
    }
    let Some(path) = resolve(root, target) else {
        fail(stream, "404 Not Found", "No such file here.");
        return format!("{} {} -> 404", method, target);
    };
    let Ok(body) = fs::read(&path) else {
        fail(stream, "404 Not Found", "No such file here.");
        return format!("{} {} -> 404", method, target);
    };
    let _ = stream.write_all(head("200 OK", mime_of(&path), body.len()).as_bytes());
    if method == "GET" {
        let _ = stream.write_all(&body);
    }
    format!("{} {} -> 200 ({} bytes)", method, target, body.len())
}

/// Serve `root` until interrupted.
pub fn run(root: &Path, bind: &str, port: u16) -> i32 {
    let root = match fs::canonicalize(root) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("radbeeper: {}: {}", root.display(), e);
            return 1;
        }
    };
    if !root.is_dir() {
        eprintln!("radbeeper: {} is not a directory", root.display());
        return 1;
    }
    let listener = match TcpListener::bind((bind, port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("radbeeper: cannot bind {}:{} -- {}", bind, port, e);
            return 1;
        }
    };
    println!("  serving {}", root.display());
    println!("  http://{}:{}/", bind, port);
    if bind != "127.0.0.1" && bind != "localhost" {
        // SAID OUT LOUD, EVERY TIME. Loopback is the default and anything
        // else is a machine on the network reading these logs.
        println!("  NOTE: bound to {} -- reachable from the network, not just this machine",
                 bind);
    }
    println!("  ctrl-c to stop");
    for conn in listener.incoming() {
        match conn {
            Ok(mut stream) => {
                let root = root.clone();
                // A thread each, because a browser opens several at once for
                // one page and a serial loop would make them queue.
                std::thread::spawn(move || {
                    let said = handle(&mut stream, &root);
                    println!("  {}", said);
                    let _ = stream.flush();
                });
            }
            Err(e) => eprintln!("  (connection failed: {})", e),
        }
    }
    0
}

/// Read a whole response off a socket. Test helper, and the only reader of
/// `Read` in this module.
#[cfg(test)]
fn slurp(mut s: TcpStream) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radbeeper-serve-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("logs")).unwrap();
        fs::write(d.join("index.html"), "<h1>root</h1>").unwrap();
        fs::write(d.join("frames.html"), "<h1>frames</h1>").unwrap();
        fs::write(d.join("logs/random-A-2026-09.bin"), [0x52, 0x42, 0x46, 0x32, 0x00]).unwrap();
        d
    }

    #[test]
    fn a_plain_file_resolves() {
        let d = corpus("plain");
        assert_eq!(resolve(&d, "/frames.html").unwrap(),
                   fs::canonicalize(d.join("frames.html")).unwrap());
    }

    /// THE ONE THAT MATTERS. A server that will hand out /etc/passwd because
    /// the path said so is not a preview, it is a hole.
    #[test]
    fn nothing_climbs_out_of_the_root() {
        let d = corpus("climb");
        for bad in ["/../../etc/passwd", "/logs/../../etc/passwd",
                    "/%2e%2e/%2e%2e/etc/passwd", "/..%2f..%2fetc%2fpasswd",
                    "/./../../etc/passwd"] {
            assert!(resolve(&d, bad).is_none(), "escaped with {}", bad);
        }
    }

    /// A path that is not a path at all, and a query string that is not part
    /// of the name.
    #[test]
    fn the_odd_shapes() {
        let d = corpus("odd");
        assert!(resolve(&d, "frames.html").is_none(), "no leading slash");
        assert!(resolve(&d, "/nope.html").is_none(), "missing file");
        assert_eq!(resolve(&d, "/frames.html?v=2").unwrap(),
                   fs::canonicalize(d.join("frames.html")).unwrap());
        assert_eq!(resolve(&d, "/").unwrap(),
                   fs::canonicalize(d.join("index.html")).unwrap());
    }

    /// The bytes a month is fetched as must not be text.
    #[test]
    fn a_bin_is_served_as_bytes() {
        assert_eq!(mime_of(Path::new("random-A-2026-09.bin")), "application/octet-stream");
        assert_eq!(mime_of(Path::new("frames.html")), "text/html; charset=utf-8");
        assert_eq!(mime_of(Path::new("browser.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime_of(Path::new("cpm-A-2026-09.tsv")), "text/tab-separated-values; charset=utf-8");
    }

    #[test]
    fn percent_escapes_come_back() {
        assert_eq!(percent_decode("/a%20b"), "/a b");
        assert_eq!(percent_decode("/a%2fb"), "/a/b");
        assert_eq!(percent_decode("/plain"), "/plain");
        assert_eq!(percent_decode("/half%2"), "/half%2");
    }

    /// End to end, over a real socket on a port the OS picked.
    #[test]
    fn it_serves_a_frame_file_over_http() {
        let d = corpus("http");
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = fs::canonicalize(&d).unwrap();
        std::thread::spawn(move || {
            if let Some(Ok(mut s)) = listener.incoming().next() {
                handle(&mut s, &root);
                let _ = s.flush();
            }
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /logs/random-A-2026-09.bin HTTP/1.0\r\n\r\n").unwrap();
        let got = slurp(c);
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("HTTP/1.0 200 OK"), "{}", text);
        assert!(text.contains("application/octet-stream"), "{}", text);
        assert!(got.ends_with(&[0x52, 0x42, 0x46, 0x32, 0x00]), "frame bytes did not survive");
    }

    #[test]
    fn a_climb_over_http_is_a_404() {
        let d = corpus("http404");
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = fs::canonicalize(&d).unwrap();
        std::thread::spawn(move || {
            if let Some(Ok(mut s)) = listener.incoming().next() {
                handle(&mut s, &root);
                let _ = s.flush();
            }
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /../../etc/passwd HTTP/1.0\r\n\r\n").unwrap();
        let got = String::from_utf8_lossy(&slurp(c)).into_owned();
        assert!(got.starts_with("HTTP/1.0 404"), "{}", got);
    }
}
