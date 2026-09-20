// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// THE FRAME BROWSER PAGE: `frames.html`, written by the Rust and only by the
// Rust.
//
// WHY THIS IS NOT IN export.rs. Every other page in this program exists
// twice -- once here and once in the one-file Python -- and
// tests/test_differential.py compares them byte for byte, because the site
// is rebuilt by a workflow that has no Rust toolchain. This page has no
// Python half on purpose: it is a reader for the `.bin` frames, which the
// Python has never decoded and is not going to start decoding, and dragging
// the parity apparatus over here would double the work to keep a file that
// nothing would read.
//
// SO IT IS OPT-IN, AND THAT IS WHAT KEEPS THE OTHER PAGES HONEST. `radbeeper
// export` writes exactly what it always wrote unless it is asked for this as
// well -- `--frames-page`. Both implementations still print the same lines
// and write the same files for the same arguments, the differential suite
// needs no exception, and the one thing that is Rust-only is the one thing
// you have to ask for. `make site` asks for it.
//
// WHAT THE WORKFLOW DOES WITH IT. The Action rebuilds the site with the
// Python, so it leaves `frames.html` exactly as it was committed: this page
// is refreshed by `make site` from a machine with a toolchain, and never by
// a push. That is a real limitation and it is written on the page itself
// rather than left for somebody to deduce from a stale date.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::audit;
use crate::clock;
use crate::export::{
    abspath, basename, bytes_text, dirname, esc, esc_payload, join, json_str, now,
    page_head, path_str, relpath,
};
use crate::frames;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const FRAMES_TITLE: &str = "The frames";

/// The browser, compiled in. See the head of src/browser.js for why this is
/// a second file rather than an addition to src/frames.js.
const BROWSER_JS: &str = include_str!("browser.js");

/// One page that was written.
pub struct Page {
    pub path: PathBuf,
    pub serial: String,
    pub frames: usize,
    pub months: usize,
}

/// `frames.html` for every counter that has frames, beside `output`.
///
/// The first counter keeps the plain name -- or whatever `--frames-output`
/// asked for -- and the rest go beside it as `frames-<serial>.html`, which is
/// the same rule the audit pages follow.
pub fn write_pages(
    logs: &Path,
    output: &Path,
    index_title: &str,
    budget: u64,
    first_output: Option<&Path>,
) -> io::Result<Vec<Page>> {
    let out = path_str(output);
    let out_dir = dirname(&abspath(&out));
    let mut pages = Vec::new();
    let serials = frames::counters(logs);
    for (i, serial) in serials.iter().enumerate() {
        let series = frames::Series::open(logs, serial);
        if series.frames() == 0 {
            continue;
        }
        let target = if i == 0 {
            match first_output.filter(|p| !p.as_os_str().is_empty()) {
                Some(p) => path_str(p),
                None => join(&out_dir, "frames.html"),
            }
        } else {
            join(&out_dir, &format!("frames-{}.html", serial))
        };
        let embed = audit::frames_to_embed(logs, serial, budget);
        let embedded: Vec<String> = embed
            .iter()
            .map(|p| basename(&path_str(p)).to_string())
            .collect();
        let html = render(
            serial,
            &series,
            &out_dir,
            basename(&out),
            index_title,
            &embedded,
            &audit::base64(&audit::frames_bytes(&embed)),
            &serials,
        );
        fs::write(&target, html)?;
        pages.push(Page {
            path: PathBuf::from(target),
            serial: serial.clone(),
            frames: series.frames(),
            months: series.months().len(),
        });
    }
    Ok(pages)
}

/// The manifest the browser reads: one entry a month, and nothing it could
/// not work out from the files themselves.
fn manifest(series: &frames::Series, out_dir: &str, embedded: &[String]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{{\"version\":{},\"serial\":{},\"generated\":{},\"months\":[",
        json_str(VERSION),
        json_str(series.serial()),
        json_str(&clock::format(now(), "%Y-%m-%d %H:%M"))
    ));
    for (i, m) in series.months().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let name = basename(&path_str(&m.path)).to_string();
        out.push_str(&format!(
            "{{\"label\":{},\"file\":{},\"bytes\":{},\"frames\":{},\
              \"first\":{},\"last\":{},\"embedded\":{}}}",
            json_str(&m.label),
            // RELATIVE TO THE PAGE, not to the log directory. The page is
            // opened from wherever it was put, and a month it has to fetch
            // is fetched from there.
            json_str(&relpath(&path_str(&m.path), out_dir)),
            m.bytes,
            m.frames,
            m.first,
            m.last,
            embedded.contains(&name)
        ));
    }
    out.push_str("]}");
    out
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    serial: &str,
    series: &frames::Series,
    out_dir: &str,
    index: &str,
    index_title: &str,
    embedded: &[String],
    frames_b64: &str,
    siblings: &[String],
) -> String {
    let mut out = page_head(FRAMES_TITLE, FRAMES_CSS);
    macro_rules! a {
        ($($arg:tt)*) => { out.push(format!($($arg)*)) };
    }
    let total_frames = series.frames();
    let months = series.months();
    let seconds: u64 = months
        .iter()
        .flat_map(|m| series.read(&m.label))
        .map(|f| f.samples.len() as u64)
        .sum();
    let bytes = series.bytes();

    a!("<h1>{}</h1>", esc(FRAMES_TITLE));
    a!(
        "<p class=\"sub\">radbeeper {} &middot; counter {} &middot; {} frames in \
         {} month{} &middot; generated {}</p>",
        VERSION,
        esc(serial),
        total_frames,
        months.len(),
        if months.len() == 1 { "" } else { "s" },
        esc(&clock::format(now(), "%Y-%m-%d %H:%M"))
    );
    a!("<p class=\"sub\"><a href=\"{}\">&larr; {}</a>", esc(index), esc(index_title));
    a!(" &middot; <a href=\"random.html\">the audit trail</a>");
    for s in siblings.iter().filter(|s| s.as_str() != serial) {
        a!(" &middot; <a href=\"frames-{}.html\">counter {}</a>", esc(s), esc(s));
    }
    a!("</p>");

    // WHAT A FRAME IS, ON THE PAGE THAT SHOWS THEM. A reader who arrives
    // here from a link has not read the lab report and should not have to.
    a!("<section><h2>What you are looking at</h2>");
    a!(
        "<p>Every 256-bit line this counter has published came out of a \
         particular stretch of decay. A <b>frame</b> is that stretch, kept \
         whole: how many pulses landed in each second, in order, with the \
         gaps where the counter was away marked as gaps rather than as \
         zeroes. {} seconds of it are here, in {} &mdash; about {} a second, \
         because an ordinary second is one byte.</p>",
        commas(seconds as i64),
        bytes_text(bytes),
        if seconds > 0 {
            format!("{:.2} bytes", bytes as f64 / seconds as f64)
        } else {
            "no bytes".to_string()
        }
    );
    a!(
        "<p>The <b>spectrum</b> view is the same arithmetic the monitor draws \
         while the counter is running: each frequency against the average, so \
         <b>flat &mdash; 1.0 &mdash; is the good answer</b>. A bin that stands \
         above what luck alone reaches is a period in the arrivals, which is \
         the one thing decay should not have, and a frame drawn while that was \
         true was recorded <code>suspect</code>.</p>"
    );
    a!("</section>");

    a!("<section><h2>The record</h2>");
    // THE SERVER-RENDERED TABLE IS THE FALLBACK AND IT IS INSIDE THE HOST.
    // The browser clears this element and builds itself in its place, so a
    // reader with no javascript gets the months and the files rather than an
    // empty box, and nobody has to maintain two copies of the same list.
    a!("<div id=\"rb-browser\">");
    a!("<table class=\"rb-table\"><thead><tr><th>month</th><th>frames</th>\
        <th>size</th><th>from</th><th>to</th><th>file</th></tr></thead><tbody>");
    for m in months {
        a!(
            "<tr><th>{}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
             <td><a href=\"{}\">{}</a></td></tr>",
            if m.label.is_empty() { "undated".to_string() } else { esc(&m.label) },
            m.frames,
            bytes_text(m.bytes),
            esc(&clock::format(m.first as f64, "%Y-%m-%d")),
            esc(&clock::format(m.last as f64, "%Y-%m-%d")),
            esc(&relpath(&path_str(&m.path), out_dir)),
            esc(basename(&path_str(&m.path)))
        );
    }
    a!("</tbody></table>");
    a!(
        "<p class=\"note\">This is the fallback list. With javascript the same \
         element becomes the browser: months, then days, then one frame at a \
         time.</p>"
    );
    a!("</div>");
    if embedded.is_empty() {
        a!(
            "<p class=\"note\">No frames are carried in this page &mdash; every \
             month is fetched, which needs a web server rather than a file \
             opened off a disk.</p>"
        );
    } else {
        a!(
            "<p class=\"note\">{} carried in this page, so it browses with \
             nothing to fetch. Older months are fetched when you ask for \
             them.</p>",
            embedded.join(", ")
        );
    }
    a!("</section>");

    a!("<section><h2>Getting this data out, and back in</h2>");
    a!(
        "<p>Everything above is a view of files that are already on the \
         machine that recorded them. The bytes are the record; this page, the \
         terminal and any other reader are all views of it.</p>"
    );
    a!("<pre class=\"rb-proc\">{}</pre>", esc(PROCEDURE));
    a!(
        "<p class=\"note\">A frame that cannot recompute the key it carries is \
         refused on the way in, whatever it says about itself, and the chain \
         link is recomputed from whatever this directory already ends with \
         &mdash; an imported record joins the chain here rather than claiming \
         somebody else's.</p>"
    );
    a!("</section>");

    a!(
        "<footer>Written by <code>radbeeper export --frames-page</code>, which \
         is the Rust build only: the workflow that rebuilds this site runs the \
         Python program, which has no frame reader, so it leaves this page \
         alone. If the date above is old, that is why &mdash; run \
         <code>make site</code>.</footer>"
    );

    a!("<script id=\"rb-manifest\" type=\"application/json\">{}</script>",
       esc_payload(&manifest(series, out_dir, embedded)));
    a!("<script id=\"rb-frames\" type=\"text/plain\">{}</script>", frames_b64);
    a!("<script>{}</script>", BROWSER_JS);
    a!("</main></body></html>");
    out.join("\n")
}

/// The whole in-and-out procedure, in the order somebody would do it.
const PROCEDURE: &str = "\
# what is here
radbeeper frames list

# one frame, in full: the seconds, the spectrum, the key
radbeeper frames show --seq 124 --month 2026-09

# out, as bytes (the canonical form), as a table, or as json
radbeeper frames export --month 2026-09 --bin -o september.bin
radbeeper frames export --month 2026-09 --tsv -o september.tsv
radbeeper frames export --month 2026-09 --json > september.json

# in: every frame is recomputed from its own counts before it is written,
# and the chain link is recomputed from this directory's own head
radbeeper frames import september.bin --serial F48824B8207F7E

# and the check that the whole record still holds up
radbeeper frames verify";

fn commas(n: i64) -> String {
    let text = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if i > 0 && (text.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{}", out) } else { out }
}

const FRAMES_CSS: &[&str] = &[
    ".rb-rail{display:flex;flex-wrap:wrap;gap:.5rem;margin:.6rem 0 1rem}",
    ".rb-chip{display:flex;flex-direction:column;align-items:flex-start;\
     gap:.15rem;padding:.45rem .7rem;border:1px solid var(--line);\
     border-radius:.4rem;background:var(--card);color:var(--fg);\
     font:inherit;cursor:pointer;text-align:left}",
    ".rb-chip.on{border-color:var(--accent);box-shadow:inset 0 0 0 1px var(--accent)}",
    ".rb-chip-label{font-weight:600}",
    ".rb-chip-sub{color:var(--dim);font-size:.82rem}",
    ".rb-days{display:flex;flex-wrap:wrap;align-items:flex-end;gap:.25rem;\
     margin:.4rem 0 1rem;padding-bottom:.2rem}",
    ".rb-day{display:flex;flex-direction:column;align-items:center;gap:.2rem;\
     border:0;background:none;color:var(--dim);font:inherit;font-size:.75rem;\
     cursor:pointer;padding:.1rem .15rem}",
    ".rb-day-bar{display:block;width:12px;min-height:6px;border-radius:2px;\
     background:var(--accent);opacity:.55}",
    ".rb-day.on{color:var(--fg)}",
    ".rb-day.on .rb-day-bar{opacity:1}",
    ".rb-day.warn .rb-day-bar{background:var(--warn)}",
    ".rb-day-all{align-self:flex-end;padding-bottom:.1rem}",
    ".rb-scroll{max-height:26rem;overflow:auto;border:1px solid var(--line);\
     border-radius:.4rem}",
    ".rb-table{border-collapse:collapse;width:100%;font-size:.9rem}",
    ".rb-table th,.rb-table td{padding:.3rem .55rem;text-align:left;\
     border-bottom:1px solid var(--line);white-space:nowrap}",
    ".rb-table thead th,.rb-sortable{position:sticky;top:0;background:var(--card);\
     cursor:pointer;font-weight:600}",
    ".rb-sortable.on{color:var(--accent)}",
    ".rb-row{cursor:pointer}",
    ".rb-row:hover{background:var(--card)}",
    ".rb-row.on{background:var(--card);box-shadow:inset 2px 0 0 var(--accent)}",
    ".rb-warn{color:var(--warn)}",
    ".rb-inspect{margin-top:1.4rem;padding:1rem;border:1px solid var(--line);\
     border-radius:.4rem;background:var(--card)}",
    ".rb-inspect h3{margin:0 0 .6rem;font-size:1rem}",
    ".rb-facts{display:grid;grid-template-columns:auto 1fr;gap:.2rem .8rem;\
     margin:0 0 .8rem;font-size:.88rem}",
    ".rb-facts dt{color:var(--dim)}",
    ".rb-facts dd{margin:0;overflow-wrap:anywhere;font-family:ui-monospace,monospace}",
    ".rb-tabs{display:flex;gap:.4rem;margin:.2rem 0 .6rem}",
    ".rb-tab{padding:.3rem .7rem;border:1px solid var(--line);border-radius:.3rem;\
     background:none;color:var(--dim);font:inherit;font-size:.85rem;cursor:pointer}",
    ".rb-tab.on{color:var(--fg);border-color:var(--accent)}",
    ".rb-canvas{display:block;width:100%;height:170px;touch-action:none}",
    ".rb-caption{margin:.3rem 0 .8rem;font-size:.82rem}",
    ".rb-digits{overflow-x:auto;padding:.6rem;border:1px solid var(--line);\
     border-radius:.3rem;background:var(--bg);font-size:.75rem;line-height:1.35}",
    ".rb-proc{overflow-x:auto;padding:.7rem;border:1px solid var(--line);\
     border-radius:.3rem;background:var(--card);font-size:.82rem}",
    "footer{margin-top:2.5rem;padding-top:1rem;border-top:1px solid var(--line);\
     color:var(--dim);font-size:.85rem}",
];

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entropy;

    fn corpus(tag: &str) -> (PathBuf, String) {
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-browser-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/frames.bin");
        let got = entropy::read_frames(&fixture);
        frames::import(&dir, "A1B2", &got).unwrap();
        (dir, "A1B2".to_string())
    }

    /// The page carries what the browser needs and nothing it has to fetch
    /// to show the first screen.
    #[test]
    fn the_page_carries_its_manifest_and_its_frames() {
        let (dir, serial) = corpus("carries");
        let out = dir.join("frames.html");
        let pages = write_pages(&dir, &out, "the report", 2 * 1024 * 1024, None).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].frames, 4);
        let html = fs::read_to_string(&pages[0].path).unwrap();
        assert!(html.contains("id=\"rb-manifest\""));
        assert!(html.contains("id=\"rb-frames\""));
        assert!(html.contains(&serial));
        // The fallback list is inside the host, so no-javascript sees months.
        let host = html.find("id=\"rb-browser\"").unwrap();
        let table = html.find("<table class=\"rb-table\"").unwrap();
        assert!(table > host, "the fallback table is inside the browser host");
        assert!(html.trim_end().ends_with("</html>"));
    }

    /// A page written somewhere else still points at the frames.
    #[test]
    fn a_month_is_addressed_relative_to_the_page() {
        let (dir, _) = corpus("relative");
        let site = dir.join("site");
        fs::create_dir_all(&site).unwrap();
        let out = site.join("frames.html");
        let pages = write_pages(&dir, &out, "the report", 2 * 1024 * 1024, None).unwrap();
        let html = fs::read_to_string(&pages[0].path).unwrap();
        assert!(html.contains("\"../random-A1B2-2023-11.bin\""),
                "the manifest addresses the month from the page's directory");
    }

    /// Nothing in the page can end the script element it sits in.
    #[test]
    fn no_payload_can_close_its_own_script_tag() {
        let (dir, _) = corpus("escape");
        let out = dir.join("frames.html");
        let pages = write_pages(&dir, &out, "</script> & <b>", 2 * 1024 * 1024, None).unwrap();
        let html = fs::read_to_string(&pages[0].path).unwrap();
        let body = html.split("id=\"rb-manifest\"").nth(1).unwrap();
        let payload = body.split("</script>").next().unwrap();
        assert!(!payload.contains("</"), "a payload closed its own tag");
    }

    /// A budget of nothing still leaves a page that says so rather than one
    /// that silently shows an empty browser.
    #[test]
    fn a_page_with_no_room_for_frames_says_so() {
        let (dir, _) = corpus("budget");
        let out = dir.join("frames.html");
        // The newest month goes in even over budget -- that is the audit
        // page's rule and this page keeps it -- so "no frames" here means a
        // counter with no files at all, which `write_pages` skips entirely.
        let pages = write_pages(&dir, &out, "the report", 0, None).unwrap();
        let html = fs::read_to_string(&pages[0].path).unwrap();
        assert!(html.contains("carried in this page"));
    }
}
