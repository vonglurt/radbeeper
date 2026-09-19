// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// `site`: the landing page and the lab reports, rendered from the documents.
//
// WHY THIS IS THE README AND NOT A DESCRIPTION OF IT. The argument this
// project makes about its log format -- one generator, so the page cannot
// drift from the rows -- is the same argument here. A hand-written landing
// page is a second description of the program that goes stale the first time
// the first one changes. So the page IS `README.md`, rendered, under a header
// carrying the things a README is the wrong shape for: a meta description,
// OpenGraph tags and a JSON-LD record for the machines that read them.
//
// AND IT IS BYTE-FOR-BYTE `tools/landing.py`. That tool stays, because the
// GitHub Action rebuilds the site when somebody pushes logs and it can do so
// with no toolchain at all -- stdlib Python, nothing to install. This is the
// same pages from the same sources for `make release`, and
// tests/test_differential.py compares the two on every run, exactly as it
// does for the log format. Two dialects of one site is the failure that
// arrangement exists to prevent.
//
// NO REGULAR EXPRESSIONS, because there is no regex crate here and there is
// not going to be: this crate has one dependency and it is libc. The patterns
// the documents actually use are small enough to scan for by hand, and doing
// so is what makes the Python's exact semantics -- including which characters
// `html.escape` touches at which call site -- something this can match rather
// than approximate.

use std::fs;
use std::path::{Path, PathBuf};

pub const REPO: &str = "https://github.com/vonglurt/radbeeper";
pub const SITE: &str = "https://vonglurt.github.io/radbeeper/";
pub const CRATE: &str = "https://crates.io/crates/radbeeper";
pub const COPAL: &str = "https://github.com/vonglurt/copal";

/// What a search engine is handed before it reads a word of the README. It is
/// one sentence and it has to carry the hardware, the job and the platform,
/// because those are the three things somebody actually searches for.
pub const DESCRIPTION: &str = "RadBeeper reads a GQ GMC-320 Plus Geiger-Muller \
counter over USB serial on Linux: a terminal monitor and a Wayland window, \
five averaging windows at once, an interleaved cascade of counts per second, \
and tab-separated data logging you can publish. Built in Rust for Copal \
Linux, an Alpine distillation.";

pub const KEYWORDS: &str = "geiger counter linux, GMC-320 Plus, GQ GMC, geiger \
muller counter USB, radiation monitor Linux, tty serial geiger counter, counts \
per minute logging, radiation data logging, Alpine Linux geiger, Copal Linux, \
Hyprland Wayland radiation monitor, rust geiger counter, CPM to uSv/h";

// ---------------------------------------------------------------- escaping ---
//
// TWO ESCAPERS, BECAUSE PYTHON HAS TWO. `html.escape(s, quote=False)` touches
// `&`, `<` and `>`; with `quote=True` it also turns `"` into `&quot;` AND `'`
// into `&#x27;`, which is the one everybody forgets. The site's attributes go
// through the second and its text through the first, so matching the Python
// byte for byte means matching which call site used which.
//
// Note this is NOT `export::esc`, which escapes the first four and leaves the
// apostrophe alone. That one is pinned to the exported report's bytes and is
// not ours to change.

/// `html.escape(s, quote=False)`.
pub fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `html.escape(s, quote=True)`.
pub fn esc_attr(s: &str) -> String {
    esc_text(s).replace('"', "&quot;").replace('\'', "&#x27;")
}

/// GitHub's anchor rule, so an in-page link written for the repository still
/// lands in the right place here: lowercased, everything that is not a word
/// character, whitespace or a hyphen dropped, runs of whitespace to hyphens.
pub fn slug(text: &str) -> String {
    let lower = text.to_lowercase();
    let kept: String = lower
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || c.is_whitespace() || *c == '-')
        .collect();
    // `re.sub(r"[\s]+", "-", s)` collapses each run of whitespace to one
    // hyphen; `.strip("-")` then takes them off both ends.
    let mut out = String::with_capacity(kept.len());
    let mut in_space = false;
    for c in kept.chars() {
        if c.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push('-');
        }
        in_space = false;
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

// ------------------------------------------------------------------ inline ---

/// Whether a char blocks emphasis on either side of a marker.
///
/// Python's `(?<![\w*])` and `(?![\w*])`: `\w` is a word character, which for
/// a `str` pattern means unicode alphanumerics and the underscore.
fn word_or(c: char, marker: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == marker
}

/// Replace `open ... close` runs, non-greedy, wrapping the inside in a tag.
///
/// `guard` is the emphasis rule: with it, the character before the opening
/// marker and after the closing one must not be a word character or another
/// marker, which is what keeps `*` inside `a*b` and `**bold**` from being
/// read as emphasis.
fn wrap(src: &[char], marker: char, run: usize, tag: &str, guard: bool) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(src.len());
    let mut i = 0usize;
    'scan: while i < src.len() {
        let starts = i + run <= src.len() && src[i..i + run].iter().all(|c| *c == marker);
        if starts {
            if guard {
                // The character before the opening marker.
                if i > 0 && word_or(src[i - 1], marker) {
                    out.push(src[i]);
                    i += 1;
                    continue;
                }
            }
            // The earliest closing run after at least one character.
            let mut j = i + run;
            while j + run <= src.len() {
                if src[j] == '\n' {
                    break;
                }
                let closes = src[j..j + run].iter().all(|c| *c == marker);
                // An empty body is not emphasis, and for the guarded forms
                // the marker must not repeat: `[^*\n]+` forbids it inside.
                if closes && j > i + run {
                    if guard {
                        let after = src.get(j + run).copied();
                        if after.map(|c| word_or(c, marker)).unwrap_or(false) {
                            // THE WHOLE MATCH FAILS HERE, rather than the
                            // scan carrying on to a later marker. The body of
                            // a guarded run is `[^*\n]+`, so the first marker
                            // after the opening one is the only close there
                            // can be -- if its lookahead fails there is no
                            // other candidate, and looking for one turns
                            // `*c*<subscript>` into emphasis that swallows
                            // the rest of the sentence.
                            break;
                        }
                    }
                    out.extend(format!("<{}>", tag).chars());
                    out.extend_from_slice(&src[i + run..j]);
                    out.extend(format!("</{}>", tag).chars());
                    i = j + run;
                    continue 'scan;
                }
                if guard && src[j] == marker {
                    // `[^*\n]+` cannot contain the marker at all.
                    break;
                }
                j += 1;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

/// A `[text](target)` or `![alt](target)` run, from `i`, if there is one.
/// Returns (text, target, index after the close).
fn bracket(src: &[char], i: usize) -> Option<(String, String, usize)> {
    if src.get(i) != Some(&'[') {
        return None;
    }
    let mut j = i + 1;
    while j < src.len() && src[j] != ']' {
        j += 1;
    }
    if j >= src.len() || src.get(j + 1) != Some(&'(') {
        return None;
    }
    let text: String = src[i + 1..j].iter().collect();
    let mut k = j + 2;
    while k < src.len() && src[k] != ')' {
        k += 1;
    }
    if k >= src.len() {
        return None;
    }
    let target: String = src[j + 2..k].iter().collect();
    Some((text, target, k + 1))
}

/// Emphasis, code, images and links, in Python's order and for its reasons.
///
/// CODE SPANS COME OUT FIRST AND GO BACK LAST, because everything else is a
/// scan over the whole line and `**` inside a backtick span is two asterisks
/// the reader asked for. Lifting the spans out and putting them back as the
/// last act is what keeps `*args` from turning the rest of a sentence italic.
///
/// THE ORDER AFTER THAT IS NOT ARBITRARY EITHER. The text is escaped BEFORE
/// links are found, so a target that contained an ampersand is already
/// `&amp;` by the time it reaches the attribute escaper and comes out
/// `&amp;amp;`. That is what the Python does, and a site that agreed with it
/// everywhere except there would fail the comparison at one byte in fifty
/// thousand.
pub fn inline(text: &str, link: &dyn Fn(&str) -> String) -> String {
    let mut held: Vec<String> = Vec::new();
    // ---- hold the code spans -------------------------------------------
    let src: Vec<char> = text.chars().collect();
    let mut staged: Vec<char> = Vec::with_capacity(src.len());
    let mut i = 0usize;
    while i < src.len() {
        if src[i] == '`' {
            if let Some(end) = (i + 1..src.len()).find(|k| src[*k] == '`') {
                if end > i + 1 {
                    held.push(src[i + 1..end].iter().collect());
                    staged.extend(format!("\u{0}{}\u{0}", held.len() - 1).chars());
                    i = end + 1;
                    continue;
                }
            }
        }
        staged.push(src[i]);
        i += 1;
    }
    let escaped: Vec<char> = esc_text(&staged.iter().collect::<String>()).chars().collect();

    // ---- images, then links; the bang is the only difference ------------
    let mut out: Vec<char> = Vec::with_capacity(escaped.len());
    let mut i = 0usize;
    while i < escaped.len() {
        if escaped[i] == '!' {
            if let Some((alt, target, next)) = bracket(&escaped, i + 1) {
                out.extend(
                    format!(
                        "<img src=\"{}\" alt=\"{}\" loading=\"lazy\">",
                        esc_attr(&target),
                        esc_attr(&alt)
                    )
                    .chars(),
                );
                i = next;
                continue;
            }
        }
        if escaped[i] == '[' {
            if let Some((label, target, next)) = bracket(&escaped, i) {
                if !label.is_empty() {
                    out.extend(
                        format!("<a href=\"{}\">{}</a>", esc_attr(&link(&target)), label)
                            .chars(),
                    );
                    i = next;
                    continue;
                }
            }
        }
        out.push(escaped[i]);
        i += 1;
    }

    // ---- emphasis, bold before italic -----------------------------------
    let out = wrap(&out, '*', 2, "strong", false);
    let out = wrap(&out, '*', 1, "em", true);
    let out = wrap(&out, '_', 1, "em", true);

    // ---- give the code spans back ---------------------------------------
    let mut text: String = String::with_capacity(out.len());
    let mut i = 0usize;
    while i < out.len() {
        if out[i] == '\u{0}' {
            let mut j = i + 1;
            let mut n = String::new();
            while j < out.len() && out[j].is_ascii_digit() {
                n.push(out[j]);
                j += 1;
            }
            if out.get(j) == Some(&'\u{0}') && !n.is_empty() {
                let k: usize = n.parse().unwrap_or(0);
                text.push_str(&format!(
                    "<code>{}</code>",
                    esc_text(held.get(k).map(|s| s.as_str()).unwrap_or(""))
                ));
                i = j + 1;
                continue;
            }
        }
        text.push(out[i]);
        i += 1;
    }
    text
}

// ------------------------------------------------------------------- block ---

fn is_blank(l: &str) -> bool {
    l.trim().is_empty()
}

/// `^(#{1,6})\s+(.*)$`
fn heading(l: &str) -> Option<(usize, &str)> {
    let hashes = l.len() - l.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &l[hashes..];
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    Some((hashes, rest.trim_start()))
}

/// `^---+\s*$`
fn is_rule(l: &str) -> bool {
    let t = l.trim_end();
    t.len() >= 3 && t.chars().all(|c| c == '-')
}

/// `^\|[\s:|-]+\|?\s*$` -- a table's separator row.
fn is_table_rule(l: &str) -> bool {
    l.starts_with('|')
        && l.len() > 1
        && l.trim_end()
            .chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
}

/// `^(\s*)([-*]|\d+\.)\s+(.*)$` -- returns (ordered, the item's text).
fn bullet(l: &str) -> Option<(bool, &str)> {
    let t = l.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((false, rest));
    }
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let after = &t[digits..];
        if let Some(rest) = after.strip_prefix(". ") {
            return Some((true, rest));
        }
    }
    None
}

/// `^</?(details|summary)\b`
fn is_raw_html(l: &str) -> bool {
    let t = l.strip_prefix('<').map(|r| r.strip_prefix('/').unwrap_or(r));
    match t {
        Some(r) => ["details", "summary"].iter().any(|tag| {
            r.starts_with(tag)
                && r[tag.len()..]
                    .chars()
                    .next()
                    .map(|c| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(true)
        }),
        None => false,
    }
}

/// Whether a line ends the paragraph it would otherwise continue.
fn breaks_paragraph(l: &str) -> bool {
    l.starts_with("```")
        || heading(l).is_some()
        || is_rule(l)
        || l.starts_with('|')
        || l.starts_with("> ")
        || bullet(l).is_some()
        || is_raw_html(l)
}

/// Markdown to HTML, over the subset these documents use.
///
/// A LINE-AT-A-TIME STATE MACHINE, not a parser. The documents are written by
/// hand in a house style and compared against the Python on every build; the
/// failure mode of a missing feature is a visible one -- a stray pipe or
/// asterisk on the page -- rather than a silent misreading.
pub fn render(md: &str, link: &dyn Fn(&str) -> String) -> String {
    let lines: Vec<&str> = md.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];

        // An HTML comment, and the licence header every document opens with.
        if line.starts_with("<!--") {
            while i < lines.len() && !lines[i].contains("-->") {
                i += 1;
            }
            i += 1;
            continue;
        }

        // Raw HTML the document wrote on purpose: <details>, and its summary.
        if is_raw_html(line) {
            if let (Some(a), Some(b)) = (line.find("<summary>"), line.find("</summary>")) {
                let inner = &line[a + "<summary>".len()..b];
                out.push(format!("<summary>{}</summary>", inline(inner, link)));
            } else {
                out.push(line.to_string());
            }
            i += 1;
            continue;
        }

        if is_blank(line) {
            i += 1;
            continue;
        }

        if let Some(info) = line.strip_prefix("```") {
            let lang = info.trim();
            i += 1;
            let mut body: Vec<&str> = Vec::new();
            while i < lines.len() && !lines[i].starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            i += 1;
            let cls = if lang.is_empty() {
                String::new()
            } else {
                format!(" class=\"language-{}\"", esc_attr(lang))
            };
            out.push(format!(
                "<pre><code{}>{}</code></pre>",
                cls,
                esc_text(&body.join("\n"))
            ));
            continue;
        }

        // Four spaces of indent, outside a list: a display block.
        if line.starts_with("    ") && !line[4..].starts_with(char::is_whitespace)
            && !line[4..].is_empty()
        {
            let mut body: Vec<String> = Vec::new();
            while i < lines.len() && (lines[i].starts_with("    ") || is_blank(lines[i])) {
                body.push(lines[i].chars().skip(4).collect());
                i += 1;
            }
            out.push(format!(
                "<pre><code>{}</code></pre>",
                esc_text(body.join("\n").trim_matches('\n'))
            ));
            continue;
        }

        if let Some((n, text)) = heading(line) {
            // An id on every heading, so the table of contents and every
            // in-page link the documents already contain still work.
            out.push(format!(
                "<h{} id=\"{}\">{}</h{}>",
                n,
                esc_attr(&slug(text)),
                inline(text, link),
                n
            ));
            i += 1;
            continue;
        }

        if is_rule(line) {
            out.push("<hr>".to_string());
            i += 1;
            continue;
        }

        // A table: the header row, the separator, then rows until a blank.
        if line.starts_with('|')
            && i + 1 < lines.len()
            && is_table_rule(lines[i + 1])
        {
            let cells = |l: &str| -> Vec<String> {
                l.trim_matches('|')
                    .split('|')
                    .map(|c| c.trim().to_string())
                    .collect()
            };
            let head = cells(line);
            i += 2;
            let mut body: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].starts_with('|') {
                body.push(cells(lines[i]));
                i += 1;
            }
            let th: String = head
                .iter()
                .map(|c| format!("<th>{}</th>", inline(c, link)))
                .collect();
            // EVERY ONE OF THESE TABLES HAS AN EMPTY HEADER ROW. It is the
            // house style -- `| | |` -- for a two-column list of a term and
            // what it means, so the header is dropped rather than drawn as a
            // blank band across the top of the table.
            let thead = if head.iter().all(|c| c.trim().is_empty()) {
                String::new()
            } else {
                format!("<thead><tr>{}</tr></thead>", th)
            };
            let rows: String = body
                .iter()
                .map(|r| {
                    format!(
                        "<tr>{}</tr>",
                        r.iter()
                            .map(|c| format!("<td>{}</td>", inline(c, link)))
                            .collect::<String>()
                    )
                })
                .collect();
            out.push(format!("<table>{}<tbody>{}</tbody></table>", thead, rows));
            continue;
        }

        if line.starts_with("> ") {
            let mut body: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].starts_with('>') {
                body.push(lines[i].trim_start_matches('>').trim().to_string());
                i += 1;
            }
            out.push(format!(
                "<blockquote><p>{}</p></blockquote>",
                inline(&body.join(" "), link)
            ));
            continue;
        }

        if let Some((ordered, first)) = bullet(line) {
            let tag = if ordered { "ol" } else { "ul" };
            let mut items: Vec<String> = Vec::new();
            items.push(first.to_string());
            i += 1;
            while i < lines.len() {
                if let Some((_, text)) = bullet(lines[i]) {
                    items.push(text.to_string());
                    i += 1;
                } else if lines[i].starts_with("  ") && !is_blank(lines[i]) && !items.is_empty() {
                    // A continuation line: indented under its bullet.
                    let last = items.len() - 1;
                    items[last] = format!("{} {}", items[last], lines[i].trim());
                    i += 1;
                } else {
                    break;
                }
            }
            out.push(format!(
                "<{}>{}</{}>",
                tag,
                items
                    .iter()
                    .map(|t| format!("<li>{}</li>", inline(t, link)))
                    .collect::<String>(),
                tag
            ));
            continue;
        }

        // Anything else is a paragraph, and runs until a blank line or the
        // start of something that is not one.
        let mut body: Vec<&str> = Vec::new();
        while i < lines.len() && !is_blank(lines[i]) {
            if breaks_paragraph(lines[i]) {
                break;
            }
            body.push(lines[i].trim());
            i += 1;
        }
        if body.is_empty() {
            i += 1;
        } else {
            out.push(format!("<p>{}</p>", inline(&body.join(" "), link)));
        }
    }
    out.join("\n")
}

// -------------------------------------------------------------------- page ---

/// One stylesheet, inline, no web font and no CDN -- the same rule the
/// counter's own exported page keeps, and for the same reasons: a page about
/// a program that has one dependency should not pull twelve.
///
/// VERBATIM THE PYTHON'S, down to the line breaks. The two generators are
/// compared byte for byte, so this is not "the same rules" -- it is the same
/// characters.
pub const CSS: &str = r##"
:root{--bg:#fbfaf7;--fg:#1e2a3a;--dim:#5b6472;--faint:#8a919c;--line:#e2ded4;
--card:#fff;--accent:#1e6b63;--code:#f4f2ec}
@media (prefers-color-scheme:dark){:root{--bg:#161822;--fg:#d6dbe6;--dim:#9ea3b8;
--faint:#6d7383;--line:#2a2e3d;--card:#1e2130;--accent:#8cf2e6;--code:#1a1d29}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);
font:16px/1.65 ui-sans-serif,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
.wrap{max-width:60rem;margin:0 auto;padding:0 1.25rem}
header.hero{border-bottom:1px solid var(--line);padding:3rem 0 2.25rem}
.eyebrow{font:600 .75rem/1 ui-monospace,SFMono-Regular,Menlo,monospace;
letter-spacing:.14em;text-transform:uppercase;color:var(--accent);margin:0 0 .9rem}
h1{font-size:2.6rem;line-height:1.12;margin:0 0 .75rem;letter-spacing:-.02em}
.lede{font-size:1.15rem;color:var(--dim);margin:0 0 1.5rem;max-width:44rem}
.badges{display:flex;flex-wrap:wrap;gap:.5rem;margin:0 0 1.5rem;padding:0;list-style:none}
.badges li{font:.8rem/1 ui-monospace,SFMono-Regular,Menlo,monospace;
border:1px solid var(--line);border-radius:999px;padding:.45rem .8rem;color:var(--dim)}
.cta{display:flex;flex-wrap:wrap;gap:.6rem}
.cta a{display:inline-block;text-decoration:none;border-radius:.4rem;
padding:.6rem 1.1rem;font-weight:600;font-size:.95rem;border:1px solid var(--line)}
.cta a.primary{background:var(--accent);color:var(--bg);border-color:var(--accent)}
.facts{display:grid;gap:1rem;grid-template-columns:repeat(auto-fit,minmax(15rem,1fr));
padding:2rem 0;border-bottom:1px solid var(--line)}
.fact{background:var(--card);border:1px solid var(--line);border-radius:.5rem;padding:1rem 1.1rem}
.fact h2{font-size:.78rem;letter-spacing:.1em;text-transform:uppercase;
color:var(--accent);margin:0 0 .4rem}
.fact p{margin:0;font-size:.92rem;color:var(--dim)}
main{padding:2.5rem 0 3rem}
main h1,main h2,main h3,main h4{line-height:1.25;margin:2rem 0 .7rem;letter-spacing:-.01em}
main h1{font-size:1.9rem} main h2{font-size:1.45rem;padding-top:.6rem;
border-top:1px solid var(--line)} main h3{font-size:1.12rem} main h4{font-size:1rem}
main>h1:first-child{margin-top:0;border:0}
p{margin:0 0 1rem}
a{color:var(--accent)}
img{max-width:100%;height:auto;border-radius:.4rem;display:block;margin:1.2rem 0}
code{font:.88em/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;
background:var(--code);padding:.13em .38em;border-radius:.25rem}
pre{background:var(--code);border:1px solid var(--line);border-radius:.45rem;
padding:.9rem 1rem;overflow-x:auto;margin:0 0 1.2rem}
pre code{background:none;padding:0;font-size:.85rem;line-height:1.55}
table{border-collapse:collapse;width:100%;margin:0 0 1.3rem;font-size:.93rem;display:block;overflow-x:auto}
th,td{border:1px solid var(--line);padding:.5rem .7rem;text-align:left;vertical-align:top}
th{background:var(--card);font-weight:600}
blockquote{margin:0 0 1.2rem;padding:.7rem 1rem;border-left:3px solid var(--accent);
background:var(--card);color:var(--dim)}
blockquote p{margin:0}
hr{border:0;border-top:1px solid var(--line);margin:2rem 0}
ul,ol{margin:0 0 1.1rem;padding-left:1.4rem} li{margin:.3rem 0}
details{border:1px solid var(--line);border-radius:.45rem;padding:.7rem 1rem;margin:0 0 1.2rem;
background:var(--card)} summary{cursor:pointer;font-weight:600}
footer{border-top:1px solid var(--line);padding:2rem 0 3rem;color:var(--faint);font-size:.88rem}
footer a{color:var(--dim)}
.nav{display:flex;flex-wrap:wrap;gap:1.1rem;margin:0 0 1rem;padding:0;list-style:none;font-size:.9rem}
@media (max-width:40rem){h1{font-size:2rem}.lede{font-size:1.05rem}}
"##;

/// The overview, which the README is the wrong shape for.
///
/// A README opens by telling somebody who already has the hardware what to
/// type. A landing page has to answer "what is this and is it for me?" first
/// -- what the hardware is, what the program does with it, and which Linux it
/// is built on and for.
///
/// `r##` rather than `r#`, because the page links its own anchor: `href="#`
/// would end a `r#` string two thousand characters early.
fn hero() -> String {
    format!(r##"<header class="hero"><div class="wrap">
<p class="eyebrow">GQ GMC-320 Plus &middot; USB serial &middot; Linux</p>
<h1>A Geiger&ndash;M&uuml;ller counter on the desk, and a record of what it heard</h1>
<p class="lede">RadBeeper reads a <strong>GQ GMC-320 Plus</strong> over its
USB-to-serial link on Linux, counts the beeps itself, and turns them into
something you can read at a glance and keep: a terminal monitor, a Wayland
window, five averaging windows at once, an <strong>interleaved cascade</strong>
of counts as they age, and tab-separated logs that publish as a web page.</p>
<ul class="badges">
<li>MIT</li><li>Rust, one dependency</li><li>crates.io</li>
<li>Alpine &amp; Copal Linux</li><li>Hyprland / Wayland</li>
</ul>
<div class="cta">
<a class="primary" href="#radbeeper">Read the documentation</a>
<a href="monitor.html">See a live counter's report</a>
<a href="{}">Source on GitHub</a>
</div>
</div></header>

<div class="wrap"><section class="facts">
<div class="fact"><h2>The hardware</h2><p>A <strong>GQ GMC-320 Plus</strong>
Geiger&ndash;M&uuml;ller counter, plugged into USB. It presents as a CH340
serial device at <code>/dev/ttyUSB0</code>; RadBeeper opens it with termios and
reads the counter's own heartbeat &mdash; two bytes, once a second.</p></div>

<div class="fact"><h2>The calculation</h2><p>The counter reports one rolling
60-second count. RadBeeper counts the arrivals itself and keeps <strong>five
windows at once</strong> &mdash; 3&nbsp;s, 30&nbsp;s, 300&nbsp;s, 3000&nbsp;s
and a working day &mdash; each with the Poisson precision of its own figure,
and CPM converted to &micro;Sv/h by the tube's own factor.</p></div>

<div class="fact"><h2>The interleaved log view</h2><p>Counts are drawn as a
cascade that <strong>compresses as it ages</strong>: a second a bar at the
right, twice as long in a bar at every step left. With two or more counters the
finest tier is <strong>arrival order</strong>, one tube's reading per bar in
that tube's own colour, so the interleave can be seen rather than assumed.</p></div>

<div class="fact"><h2>The record</h2><p>One tab-separated row every thirty
seconds per counter, plus a merged record of the room when there is more than
one tube. Nothing is averaged into an instrument's own file. The counter's
flash is read back to fill gaps from while nothing was listening.</p></div>

<div class="fact"><h2>Built on Copal Linux</h2><p>RadBeeper is designed and
maintained for <a href="{}"><strong>Copal</strong></a>, a distillation of
<strong>Alpine Linux</strong> &mdash; which also provides its development
environment. Copal carries RadBeeper as a stage, and installs the
<code>linux-lts</code> kernel the counter needs to be seen at all.</p></div>

<div class="fact"><h2>What it targets</h2><p>Copal's own surface: a terminal,
and <strong>Hyprland on Wayland</strong> for the window. Rust from
<a href="{}">crates.io</a> with <code>cargo</code>, driven by
<code>make</code>, developed and recorded inside the <strong>Copal virtual
machine</strong> this project's own screenshots come from.</p></div>
</section></div>
"##, REPO, COPAL, CRATE)
}

fn head(title: &str, description: &str, canonical: &str, extra: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
<title>{}</title>\n\
<meta name=\"description\" content=\"{}\">\n\
<link rel=\"canonical\" href=\"{}\">\n\
<meta property=\"og:type\" content=\"website\">\n\
<meta property=\"og:title\" content=\"{}\">\n\
<meta property=\"og:description\" content=\"{}\">\n\
<meta property=\"og:url\" content=\"{}\">\n\
<meta property=\"og:image\" content=\"{}docs/screenshots/gui-dark.png\">\n\
<meta name=\"twitter:card\" content=\"summary_large_image\">\n\
<style>{}</style>\n{}</head>\n<body>\n",
        esc_attr(title),
        esc_attr(description),
        esc_attr(canonical),
        esc_attr(title),
        esc_attr(description),
        esc_attr(canonical),
        SITE,
        CSS,
        extra
    )
}

/// A JSON string literal, escaped the way `json.dumps` of a plain string is.
fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// What the machines read.
///
/// A search engine is told what kind of thing this is, what it runs on and
/// what it costs; without it the page is prose and has to be guessed at.
/// SoftwareSourceCode rather than SoftwareApplication, because that is what a
/// repository of Rust is.
fn jsonld(version: &str) -> String {
    format!(
        "<script type=\"application/ld+json\">\n\
{{\"@context\":\"https://schema.org\",\"@type\":\"SoftwareSourceCode\",\n\
\"name\":\"RadBeeper\",\"description\":{},\n\
\"url\":\"{}\",\"codeRepository\":\"{}\",\"programmingLanguage\":\"Rust\",\"license\":\n\
\"https://opensource.org/licenses/MIT\",\"version\":\"{}\",\n\
\"runtimePlatform\":\"Linux\",\"operatingSystem\":\"Linux, Alpine Linux, Copal Linux\",\n\
\"applicationCategory\":\"Science\",\"keywords\":{},\n\
\"author\":{{\"@type\":\"Person\",\"name\":\"Paul Richeson\"}},\n\
\"about\":{{\"@type\":\"Thing\",\"name\":\"Geiger-Muller counter data logging\"}}}}\n\
</script>\n",
        json_str(DESCRIPTION),
        SITE,
        REPO,
        version,
        json_str(KEYWORDS)
    )
}

/// `there` as seen from the page at `here`, both site-relative.
pub fn relative(here: &str, there: &str) -> String {
    let base: Vec<&str> = here.split('/').collect();
    let base = &base[..base.len().saturating_sub(1)];
    let target: Vec<&str> = there.split('/').collect();
    let mut common = 0usize;
    while common < base.len() && common + 1 < target.len() && base[common] == target[common] {
        common += 1;
    }
    let mut parts: Vec<String> = vec!["..".to_string(); base.len() - common];
    parts.extend(target[common..].iter().map(|s| s.to_string()));
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn nav(here: &str) -> String {
    let items: [(&str, &str); 7] = [
        ("index.html", "Overview"),
        ("monitor.html", "Live report"),
        ("docs/the-log.html", "The log"),
        ("docs/the-stream.html", "The stream"),
        ("docs/cascade.html", "The cascade"),
        (REPO, "GitHub"),
        (CRATE, "crates.io"),
    ];
    let mut out = String::from("<ul class=\"nav\">");
    for (href, label) in items {
        if href.starts_with("http") {
            out.push_str(&format!("<li><a href=\"{}\">{}</a></li>", href, label));
        } else if href == here {
            out.push_str(&format!("<li><strong>{}</strong></li>", label));
        } else {
            out.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                relative(here, href),
                label
            ));
        }
    }
    out.push_str("</ul>");
    out
}

fn foot(here: &str) -> String {
    let up = relative(here, "docs/index.html");
    format!(
        "<footer><div class=\"wrap\">{}\n\
<p>RadBeeper &mdash; MIT. Copyright &copy; 2026 Paul Richeson.\n\
Designed and maintained for <a href=\"{}\">Copal Linux</a>, a distillation of\n\
Alpine Linux, which is also its development environment.\n\
<a href=\"{}\">Lab reports</a> &middot;\n\
<a href=\"{}\">Source</a> &middot;\n\
<a href=\"{}\">crates.io</a></p>\n\
</div></footer>\n</body>\n</html>\n",
        nav(here),
        COPAL,
        up,
        REPO,
        CRATE
    )
}

// ------------------------------------------------------------------- build ---

/// Which source file becomes which page. Worked out before a single one is
/// rendered, because the link rewriting needs the whole map: a document
/// cannot know whether `../RELEASING.md` is a page here until every page is.
fn page_map(root: &Path) -> Vec<(String, String)> {
    let mut map = vec![
        ("README.md".to_string(), "index.html".to_string()),
        ("docs/README.md".to_string(), "docs/index.html".to_string()),
    ];
    let mut names: Vec<String> = fs::read_dir(root.join("docs"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for n in &names {
        map.push((
            format!("docs/{}", n),
            format!("docs/{}.html", &n[..n.len() - 3]),
        ));
    }
    if root.join("docs/archive/README.md").exists() {
        map.push((
            "docs/archive/README.md".to_string(),
            "docs/archive/index.html".to_string(),
        ));
    }
    map
}

/// Normalise `a/b/../c` the way `os.path.normpath` does, lexically.
fn normpath(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for bit in p.split('/') {
        match bit {
            "" | "." => {}
            ".." => {
                if matches!(parts.last(), Some(&last) if last != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Rewrite the links a document was written with for where it now sits.
///
/// THE DOCUMENTS ARE WRITTEN FOR THE REPOSITORY, which is the right place for
/// them to be readable. Every one of those targets is a page on this site too,
/// so it is rewritten rather than left to send a visitor back to GitHub one
/// click after arriving -- AND EVERY ONE THAT IS NOT goes to GitHub, spelt out
/// in full. `RELEASING.md` is a repository file this site does not publish,
/// and a link to `RELEASING.html` would be a 404 that nothing catches.
fn linker<'a>(here: &'a str, map: &'a [(String, String)]) -> impl Fn(&str) -> String + 'a {
    let base: String = {
        let v: Vec<&str> = here.split('/').collect();
        v[..v.len() - 1].join("/")
    };
    move |href: &str| -> String {
        if href.starts_with("http://")
            || href.starts_with("https://")
            || href.starts_with("mailto:")
            || href.starts_with('#')
            || href.starts_with('/')
        {
            return href.to_string();
        }
        let (path, anchor) = match href.split_once('#') {
            Some((p, a)) => (p, format!("#{}", a)),
            None => (href, String::new()),
        };
        let joined = if base.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", base, path)
        };
        let mut target = normpath(&joined);
        if path.ends_with('/') || target == "docs" || target == "docs/archive" {
            target = format!("{}/README.md", target.trim_end_matches('/'));
        }
        if let Some((_, out)) = map.iter().find(|(src, _)| *src == target) {
            return format!("{}{}", relative(here, out), anchor);
        }
        if target.ends_with(".md") {
            return format!("{}/blob/main/{}{}", REPO, target, anchor);
        }
        format!("{}{}", href, anchor)
    }
}

/// The document's own first heading, for its title.
pub fn first_heading(md: &str) -> String {
    for line in md.split('\n') {
        if let Some(rest) = line.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    "RadBeeper".to_string()
}

/// The document's own one-line summary, for its meta description.
pub fn first_paragraph(md: &str) -> String {
    for block in md.split("\n\n") {
        let b = block.trim();
        if b.is_empty()
            || b.starts_with('#')
            || b.starts_with("<!--")
            || b.starts_with('|')
            || b.starts_with("```")
            || b.starts_with('>')
            || b.starts_with("![")
        {
            continue;
        }
        // Links to their text, then the emphasis characters away.
        let mut plain = String::new();
        let src: Vec<char> = b.chars().collect();
        let mut i = 0usize;
        while i < src.len() {
            if src[i] == '[' {
                if let Some((label, _t, next)) = bracket(&src, i) {
                    plain.push_str(&label);
                    i = next;
                    continue;
                }
            }
            plain.push(src[i]);
            i += 1;
        }
        let plain: String = plain.chars().filter(|c| !"*`_".contains(*c)).collect();
        let words: Vec<&str> = plain.split_whitespace().collect();
        let joined = words.join(" ");
        return joined.chars().take(300).collect();
    }
    DESCRIPTION.to_string()
}

/// The docs index, as markdown, so it goes through the same renderer.
fn docs_index(listing: &[(String, String, String)]) -> String {
    let rows: Vec<String> = listing
        .iter()
        .map(|(href, title, desc)| format!("| [{}]({}) | {} |", title, href, desc))
        .collect();
    format!(
        "# Lab reports\n\nThe *why* behind each part of RadBeeper, written up \
properly \u{2014} the arithmetic, the failures that shaped it, and the prior \
art.\n\n| | |\n|---|---|\n{}\n\n---\n\n[\u{2190} back to the overview](../README.md)\n",
        rows.join("\n")
    )
}

/// Write one page. `here` is where it sits on the SITE, not on disk.
#[allow(clippy::too_many_arguments)]
fn page(
    md: &str,
    root: &Path,
    here: &str,
    title: &str,
    description: &str,
    version: &str,
    map: &[(String, String)],
    noindex: bool,
) -> std::io::Result<usize> {
    let canonical = if here == "index.html" {
        SITE.to_string()
    } else {
        format!("{}{}", SITE, here)
    };
    let is_home = here == "index.html";
    let mut extra = if is_home { jsonld(version) } else { String::new() };
    if noindex {
        extra.push_str("<meta name=\"robots\" content=\"noindex\">\n");
    }
    let mut doc = head(title, description, &canonical, &extra);
    if is_home {
        doc.push_str(&hero());
        doc.push_str("<div class=\"wrap\"><main id=\"radbeeper\">\n");
    } else {
        doc.push_str(&format!("<div class=\"wrap\"><main>\n{}\n", nav(here)));
    }
    let link = linker(here, map);
    doc.push_str(&render(md, &link));
    doc.push_str("\n</main></div>\n");
    doc.push_str(&foot(here));
    let out = root.join(here.replace('/', std::path::MAIN_SEPARATOR_STR));
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, &doc)?;
    Ok(doc.len())
}

/// Everything the site is made of, written into `root`.
///
/// Returns each page and its size, in the order they were written.
pub fn build(root: &Path, version: &str) -> std::io::Result<Vec<(String, usize)>> {
    let map = page_map(root);
    let mut wrote: Vec<(String, usize)> = Vec::new();

    let readme = fs::read_to_string(root.join("README.md"))?;
    wrote.push((
        "index.html".to_string(),
        page(
            &readme,
            root,
            "index.html",
            "RadBeeper \u{2014} Geiger counter data logging for Linux",
            DESCRIPTION,
            version,
            &map,
            false,
        )?,
    ));

    let mut listing: Vec<(String, String, String)> = Vec::new();
    for (src, here) in map.iter() {
        if !src.starts_with("docs/") || src.contains("archive") || src == "docs/README.md" {
            continue;
        }
        let md = fs::read_to_string(root.join(src))?;
        let title = first_heading(&md);
        let desc = first_paragraph(&md);
        wrote.push((
            here.clone(),
            page(
                &md,
                root,
                here,
                &format!("{} \u{2014} RadBeeper", title),
                &desc,
                version,
                &map,
                false,
            )?,
        ));
        let short: String = desc.chars().take(160).collect();
        listing.push((
            here.rsplit('/').next().unwrap_or(here).to_string(),
            title,
            short,
        ));
    }

    wrote.push((
        "docs/index.html".to_string(),
        page(
            &docs_index(&listing),
            root,
            "docs/index.html",
            "Lab reports \u{2014} RadBeeper",
            "The lab reports behind RadBeeper: the log format, the stream, the \
cascade strip, the spectrum and the entropy source.",
            version,
            &map,
            false,
        )?,
    ));

    // The archive is repository history, not a page anybody should arrive at
    // from a search: it is a second copy of documents that have moved on, and
    // indexing it would compete with the ones that did.
    if root.join("docs/archive/README.md").exists() {
        let md = fs::read_to_string(root.join("docs/archive/README.md"))?;
        wrote.push((
            "docs/archive/index.html".to_string(),
            page(
                &md,
                root,
                "docs/archive/index.html",
                "Archive \u{2014} RadBeeper",
                "Past versions of RadBeeper's documentation, kept as they stood.",
                version,
                &map,
                true,
            )?,
        ));
    }

    // A sitemap and a robots line, because a site that is not crawled is a
    // site nobody arrives at, which is the whole point of the exercise. The
    // archive is left out of it on purpose.
    let mut urls = String::new();
    for (u, _) in wrote.iter().filter(|(u, _)| !u.contains("archive")) {
        urls.push_str(&format!(
            "<url><loc>{}{}</loc></url>",
            SITE,
            if u == "index.html" { "" } else { u.as_str() }
        ));
    }
    urls.push_str(&format!("<url><loc>{}monitor.html</loc></url>", SITE));
    fs::write(
        root.join("sitemap.xml"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{}</urlset>\n",
            urls
        ),
    )?;
    fs::write(
        root.join("robots.txt"),
        format!("User-agent: *\nAllow: /\nSitemap: {}sitemap.xml\n", SITE),
    )?;
    // Jekyll is not running here and must not try: it would drop any file or
    // folder beginning with an underscore without saying so.
    fs::write(root.join(".nojekyll"), "")?;
    Ok(wrote)
}

/// The version the site is stamped with.
///
/// READ FROM `Cargo.toml`, NOT BAKED IN, and that is not an oversight. The
/// Python generator reads the manifest because it has no compiled-in version
/// to consult, and these two are compared byte for byte -- so this reads the
/// same file rather than a value that would agree with it only as long as the
/// binary and the checkout were in step. `make release` bumps the manifest
/// before it rebuilds the site precisely so this picks the new one up.
///
/// Falling back to what this binary was built as, for a copy of the documents
/// with no manifest beside them.
pub fn version_of(root: &Path) -> String {
    if let Ok(text) = fs::read_to_string(root.join("Cargo.toml")) {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("version = \"") {
                if let Some(v) = rest.split('"').next() {
                    return v.to_string();
                }
            }
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// `radbeeper pages`, as the command line runs it.
pub fn run(root: Option<&Path>) -> i32 {
    let here = PathBuf::from(".");
    let root = root.unwrap_or(&here);
    let version = version_of(root);
    match build(root, &version) {
        Ok(pages) => {
            for (name, size) in &pages {
                println!("site: {:<34} {:>6.1} KB", name, *size as f64 / 1024.0);
            }
            println!("site: sitemap.xml, robots.txt, .nojekyll");
            0
        }
        Err(e) => {
            eprintln!("radbeeper: could not build the site -- {}", e);
            1
        }
    }
}

// ------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    fn idt(s: &str) -> String {
        s.to_string()
    }

    /// TWO ESCAPERS, BECAUSE PYTHON HAS TWO, and the apostrophe is the one
    /// everybody forgets. `html.escape(s, quote=True)` turns `'` into
    /// `&#x27;`, and a site that agreed with the Python everywhere except
    /// there would fail the byte comparison on the first possessive.
    #[test]
    fn attributes_escape_the_apostrophe_and_text_does_not() {
        assert_eq!(esc_text("a & b < c > d \" e ' f"), "a &amp; b &lt; c &gt; d \" e ' f");
        assert_eq!(
            esc_attr("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &#x27; f"
        );
    }

    /// EMPHASIS NESTS ONE WAY AND NOT THE OTHER. `**bold with *italic* in
    /// it**` is a thing these documents do; `*c*` followed by a letter is not
    /// emphasis at all, and reading it as one swallowed the rest of a
    /// sentence in cascade.md until the guard was made to abandon the match
    /// rather than look for a later close.
    #[test]
    fn emphasis_nests_but_a_failed_guard_abandons_the_match() {
        assert_eq!(
            inline("**the floor of *nominal* is 30**", &idt),
            "<strong>the floor of <em>nominal</em> is 30</strong>"
        );
        // A subscript after the closing marker is a word character, so this
        // is not emphasis and nothing after it becomes emphasis either.
        assert_eq!(inline("count *c*\u{1d62} for each", &idt), "count *c*\u{1d62} for each");
        assert_eq!(inline("snake_case_word", &idt), "snake_case_word");
        assert_eq!(inline("1/_N_ steadier", &idt), "1/<em>N</em> steadier");
    }

    /// A CODE SPAN IS NOT MARKDOWN. Lifting the spans out before anything
    /// else runs is what keeps `*args` from turning a sentence italic and
    /// `**` from becoming bold.
    #[test]
    fn a_code_span_keeps_its_asterisks() {
        assert_eq!(
            inline("Python's `**` does", &idt),
            "Python's <code>**</code> does"
        );
        assert_eq!(inline("`a & b`", &idt), "<code>a &amp; b</code>");
    }

    /// The target is escaped, then escaped again as an attribute, because
    /// that is the order the Python does it in.
    #[test]
    fn a_link_target_is_escaped_twice_exactly_as_python_does_it() {
        assert_eq!(
            inline("[x](a.md?q=1&r=2)", &idt),
            "<a href=\"a.md?q=1&amp;amp;r=2\">x</a>"
        );
    }

    /// GitHub's anchor rule, so an in-page link written for the repository
    /// still lands in the right place here.
    #[test]
    fn a_heading_gets_the_anchor_github_would_have_given_it() {
        assert_eq!(slug("What you need"), "what-you-need");
        assert_eq!(slug("The two meters"), "the-two-meters");
        assert_eq!(slug("From a beep to a frame"), "from-a-beep-to-a-frame");
        assert_eq!(slug("`code` and & punctuation!"), "code-and-punctuation");
    }

    /// Relative links from a page in docs/ have to climb out of it, and a
    /// page linking itself gets its own name rather than an empty href.
    #[test]
    fn a_page_links_its_neighbours_from_where_it_actually_sits() {
        assert_eq!(relative("index.html", "docs/the-log.html"), "docs/the-log.html");
        assert_eq!(relative("docs/the-log.html", "index.html"), "../index.html");
        assert_eq!(relative("docs/the-log.html", "docs/cascade.html"), "cascade.html");
        assert_eq!(
            relative("docs/archive/index.html", "index.html"),
            "../../index.html"
        );
    }

    /// A LINK TO A DOCUMENT THIS SITE DOES NOT PUBLISH GOES TO GITHUB. The
    /// rewriting has to know which pages exist rather than assume every `.md`
    /// is one, or `RELEASING.md` becomes a 404 nothing catches.
    #[test]
    fn a_document_that_is_not_a_page_here_links_to_the_repository() {
        let map = vec![
            ("README.md".to_string(), "index.html".to_string()),
            ("docs/the-log.md".to_string(), "docs/the-log.html".to_string()),
        ];
        let from_readme = linker("index.html", &map);
        assert_eq!(from_readme("docs/the-log.md"), "docs/the-log.html");
        assert_eq!(
            from_readme("RELEASING.md"),
            format!("{}/blob/main/RELEASING.md", REPO)
        );
        assert_eq!(from_readme("https://example.com/x.md"), "https://example.com/x.md");
        assert_eq!(from_readme("#what-you-need"), "#what-you-need");

        let from_doc = linker("docs/the-log.md.html", &map);
        assert_eq!(from_doc("../README.md"), "../index.html");
        assert_eq!(
            from_doc("the-log.md#backfill"),
            "the-log.html#backfill"
        );
    }

    /// The house style is `| | |` for a two-column list of a term and what it
    /// means, so an all-empty header is dropped rather than drawn as a blank
    /// band across the top of the table.
    #[test]
    fn a_table_with_an_empty_header_row_does_not_draw_one() {
        let md = "| | |\n|---|---|\n| **a** | one |\n| b | two |\n";
        let html = render(md, &idt);
        assert!(!html.contains("<thead>"), "{}", html);
        assert!(html.contains("<td><strong>a</strong></td><td>one</td>"), "{}", html);
        let titled = "| Col | Other |\n|---|---|\n| a | b |\n";
        assert!(render(titled, &idt).contains("<thead><tr><th>Col</th>"));
    }
}
