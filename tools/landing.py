#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""Build the project site: the landing page, and every lab report beside it.

WHY THERE IS A LANDING PAGE AT ALL. GitHub Pages served `index.html` -- the
counter's own report -- at the root of the site, which is a page of numbers
with nothing on it saying what produced them. Somebody arriving from a search
for "GMC-320 Plus Linux" or "geiger counter data logging" landed on a table of
counts per minute and a plot, and had to guess. The report has moved one click
along to `monitor.html`, and this is what greets them instead.

WHY IT IS THE README AND NOT A COPY OF IT. The argument this project makes
about its log format -- one generator, so the page cannot drift from the rows
-- is the same argument here. A hand-written landing page is a second
description of the program that goes stale the first time the first one
changes. So the page IS `README.md`, rendered, under a header carrying the
things a README is the wrong shape for: a meta description, OpenGraph tags and
a JSON-LD record for the machines that read them.

And the lab reports come with it. They are linked from the README, so a
renderer that stopped at the README would put a visitor one click outside the
site it just built -- `docs/the-log.md` becomes `docs/the-log.html`, and the
links between them are rewritten to match.

Stdlib only, like everything else in tools/. The markdown subset is the one
these documents actually use, checked against all of them rather than guessed
at: headings, paragraphs, fenced and indented code, tables, lists, block
quotes, rules, images, links, inline emphasis and code, and the one raw HTML
block the README opens with.
"""
import argparse
import html
import os
import re
import sys

REPO = "https://github.com/vonglurt/radbeeper"
SITE = "https://vonglurt.github.io/radbeeper/"
CRATE = "https://crates.io/crates/radbeeper"
COPAL = "https://github.com/vonglurt/copal"

# What a search engine is handed before it reads a word of the README. It is
# one sentence and it has to carry the hardware, the job and the platform,
# because those are the three things somebody actually searches for.
DESCRIPTION = (
    "RadBeeper reads a GQ GMC-320 Plus Geiger-Muller counter over USB serial "
    "on Linux: a terminal monitor and a Wayland window, five averaging "
    "windows at once, an interleaved cascade of counts per second, and "
    "tab-separated data logging you can publish. Built in Rust for Copal "
    "Linux, an Alpine distillation."
)

KEYWORDS = (
    "geiger counter linux, GMC-320 Plus, GQ GMC, geiger muller counter USB, "
    "radiation monitor Linux, tty serial geiger counter, counts per minute "
    "logging, radiation data logging, Alpine Linux geiger, Copal Linux, "
    "Hyprland Wayland radiation monitor, rust geiger counter, CPM to uSv/h"
)


# ------------------------------------------------------------------ inline ---

def inline(text, link_map):
    """Emphasis, code, images and links, in that order and for a reason.

    CODE SPANS COME OUT FIRST AND GO BACK LAST. Everything else here is a
    regular expression over the whole line, and `**` inside a backtick span is
    two asterisks the reader asked for, not emphasis. Lifting the spans out and
    putting them back as the last act is what keeps `*args` from turning the
    rest of a sentence italic.
    """
    held = []

    def hold(m):
        held.append(m.group(1))
        return "\x00%d\x00" % (len(held) - 1)

    text = re.sub(r"`([^`]+)`", hold, text)
    text = html.escape(text, quote=False)

    # Images before links: the syntax differs by one leading character, and a
    # link pattern applied first eats the alt text and leaves the bang behind.
    text = re.sub(
        r"!\[([^\]]*)\]\(([^)]+)\)",
        lambda m: '<img src="%s" alt="%s" loading="lazy">'
        % (html.escape(m.group(2), quote=True), html.escape(m.group(1), quote=True)),
        text,
    )
    text = re.sub(
        r"\[([^\]]+)\]\(([^)]+)\)",
        lambda m: '<a href="%s">%s</a>'
        % (html.escape(link_map(m.group(2)), quote=True), m.group(1)),
        text,
    )
    # BOLD FIRST AND NON-GREEDY, so that emphasis nested inside it -- which
    # these documents do, `**the floor of *nominal* is 30**` -- is still
    # matched by the pass that follows rather than left as four asterisks.
    text = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*([^*\n]+)\*(?![\w*])", r"<em>\1</em>", text)
    text = re.sub(r"(?<![\w_])_([^_\n]+)_(?![\w_])", r"<em>\1</em>", text)

    def give(m):
        return "<code>%s</code>" % html.escape(held[int(m.group(1))], quote=False)

    return re.sub(r"\x00(\d+)\x00", give, text)


def slug(text):
    """GitHub's anchor rule, so an in-page link written for the repository
    still lands in the right place here."""
    s = re.sub(r"[^\w\s-]", "", text.lower())
    return re.sub(r"[\s]+", "-", s).strip("-")


# ------------------------------------------------------------------- block ---

def render(md, link_map):
    """Markdown to HTML, over the subset these documents use.

    A LINE-AT-A-TIME STATE MACHINE, not a parser. The documents are written by
    hand in a house style and checked against this on every build; the failure
    mode of a missing feature is a visible one -- a stray pipe or asterisk on
    the page -- rather than a silent misreading.
    """
    out, lines, i = [], md.split("\n"), 0
    while i < len(lines):
        line = lines[i]

        # An HTML comment, and the licence header every document opens with.
        if line.startswith("<!--"):
            while i < len(lines) and "-->" not in lines[i]:
                i += 1
            i += 1
            continue

        # Raw HTML the document wrote on purpose: <details>, and its summary.
        if re.match(r"^</?(details|summary)\b", line):
            out.append(re.sub(r"<summary>(.*)</summary>",
                              lambda m: "<summary>%s</summary>" % inline(m.group(1), link_map),
                              line))
            i += 1
            continue

        if not line.strip():
            i += 1
            continue

        # A fenced block. The info string names the language, for nothing but
        # a class somebody may want to style.
        if line.startswith("```"):
            lang = line[3:].strip()
            i += 1
            body = []
            while i < len(lines) and not lines[i].startswith("```"):
                body.append(lines[i])
                i += 1
            i += 1
            cls = ' class="language-%s"' % html.escape(lang, quote=True) if lang else ""
            out.append("<pre><code%s>%s</code></pre>"
                       % (cls, html.escape("\n".join(body), quote=False)))
            continue

        # Four spaces of indent, outside a list: a display block.
        if re.match(r"^    \S", line):
            body = []
            while i < len(lines) and (re.match(r"^    ", lines[i]) or not lines[i].strip()):
                body.append(lines[i][4:])
                i += 1
            out.append("<pre><code>%s</code></pre>"
                       % html.escape("\n".join(body).strip("\n"), quote=False))
            continue

        m = re.match(r"^(#{1,6})\s+(.*)$", line)
        if m:
            n, text = len(m.group(1)), m.group(2)
            # An id on every heading, so the table of contents and every
            # in-page link the documents already contain still work.
            out.append('<h%d id="%s">%s</h%d>'
                       % (n, html.escape(slug(text), quote=True),
                          inline(text, link_map), n))
            i += 1
            continue

        if re.match(r"^---+\s*$", line):
            out.append("<hr>")
            i += 1
            continue

        # A table: the header row, the separator, then rows until a blank.
        if line.startswith("|") and i + 1 < len(lines) and re.match(r"^\|[\s:|-]+\|?\s*$", lines[i + 1]):
            head = [c.strip() for c in line.strip("|").split("|")]
            i += 2
            body = []
            while i < len(lines) and lines[i].startswith("|"):
                body.append([c.strip() for c in lines[i].strip("|").split("|")])
                i += 1
            cells = "".join("<th>%s</th>" % inline(c, link_map) for c in head)
            # EVERY ONE OF THESE TABLES HAS AN EMPTY HEADER ROW. It is the
            # house style -- `| | |` -- for a two-column list of a term and
            # what it means, so the header is dropped rather than drawn as a
            # blank band across the top of the table.
            thead = "" if not any(c.strip() for c in head) else "<thead><tr>%s</tr></thead>" % cells
            rows = "".join(
                "<tr>%s</tr>" % "".join("<td>%s</td>" % inline(c, link_map) for c in r)
                for r in body
            )
            out.append("<table>%s<tbody>%s</tbody></table>" % (thead, rows))
            continue

        if line.startswith("> "):
            body = []
            while i < len(lines) and lines[i].startswith(">"):
                body.append(lines[i].lstrip(">").strip())
                i += 1
            out.append("<blockquote><p>%s</p></blockquote>"
                       % inline(" ".join(body), link_map))
            continue

        m = re.match(r"^(\s*)([-*]|\d+\.)\s+(.*)$", line)
        if m:
            ordered = m.group(2)[-1] == "."
            tag = "ol" if ordered else "ul"
            items, i = [], i
            while i < len(lines):
                mm = re.match(r"^(\s*)([-*]|\d+\.)\s+(.*)$", lines[i])
                if mm:
                    items.append(mm.group(3))
                    i += 1
                # A continuation line: indented under the bullet it belongs to.
                elif lines[i].startswith("  ") and lines[i].strip() and items:
                    items[-1] += " " + lines[i].strip()
                    i += 1
                else:
                    break
            out.append("<%s>%s</%s>"
                       % (tag, "".join("<li>%s</li>" % inline(t, link_map) for t in items), tag))
            continue

        # Anything else is a paragraph, and runs until a blank line or the
        # start of something that is not one.
        body = []
        while i < len(lines) and lines[i].strip():
            if re.match(r"^(```|#{1,6}\s|---+\s*$|\||>\s|\s*([-*]|\d+\.)\s|</?(details|summary))",
                        lines[i]):
                break
            body.append(lines[i].strip())
            i += 1
        if body:
            out.append("<p>%s</p>" % inline(" ".join(body), link_map))
        else:
            i += 1
    return "\n".join(out)


# -------------------------------------------------------------------- page ---

# One stylesheet, inline, no web font and no CDN -- the same rule the counter's
# own exported page keeps, and for the same reasons: a page about a program
# that has one dependency should not pull twelve.
CSS = """
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
"""


def head(title, description, canonical, extra=""):
    return """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>%s</title>
<meta name="description" content="%s">
<link rel="canonical" href="%s">
<meta property="og:type" content="website">
<meta property="og:title" content="%s">
<meta property="og:description" content="%s">
<meta property="og:url" content="%s">
<meta property="og:image" content="%sdocs/screenshots/gui-dark.png">
<meta name="twitter:card" content="summary_large_image">
<style>%s</style>
%s</head>
<body>
""" % (html.escape(title, quote=True), html.escape(description, quote=True),
       html.escape(canonical, quote=True), html.escape(title, quote=True),
       html.escape(description, quote=True), html.escape(canonical, quote=True),
       SITE, CSS, extra)


# WHAT THE MACHINES READ. A search engine is told what kind of thing this is,
# what it runs on and what it costs; without it the page is prose and has to be
# guessed at. SoftwareSourceCode rather than SoftwareApplication because that
# is what a repository of Rust is.
def jsonld(version):
    return """<script type="application/ld+json">
{"@context":"https://schema.org","@type":"SoftwareSourceCode",
"name":"RadBeeper","description":%s,
"url":"%s","codeRepository":"%s","programmingLanguage":"Rust","license":
"https://opensource.org/licenses/MIT","version":"%s",
"runtimePlatform":"Linux","operatingSystem":"Linux, Alpine Linux, Copal Linux",
"applicationCategory":"Science","keywords":%s,
"author":{"@type":"Person","name":"Paul Richeson"},
"about":{"@type":"Thing","name":"Geiger-Muller counter data logging"}}
</script>
""" % (json_str(DESCRIPTION), SITE, REPO, version, json_str(KEYWORDS))


def json_str(s):
    return '"%s"' % s.replace("\\", "\\\\").replace('"', '\\"')


# THE OVERVIEW, WHICH THE README IS THE WRONG SHAPE FOR. A README opens by
# telling somebody who already has the hardware what to type. A landing page
# has to answer "what is this and is it for me?" first -- what the hardware is,
# what the program does with it, and which Linux it is built on and for.
HERO = """<header class="hero"><div class="wrap">
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
<a href="%s">Source on GitHub</a>
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
maintained for <a href="%s"><strong>Copal</strong></a>, a distillation of
<strong>Alpine Linux</strong> &mdash; which also provides its development
environment. Copal carries RadBeeper as a stage, and installs the
<code>linux-lts</code> kernel the counter needs to be seen at all.</p></div>

<div class="fact"><h2>What it targets</h2><p>Copal's own surface: a terminal,
and <strong>Hyprland on Wayland</strong> for the window. Rust from
<a href="%s">crates.io</a> with <code>cargo</code>, driven by
<code>make</code>, developed and recorded inside the <strong>Copal virtual
machine</strong> this project's own screenshots come from.</p></div>
</section></div>
""" % (REPO, COPAL, CRATE)


def nav(here):
    items = [("index.html", "Overview"), ("monitor.html", "Live report"),
             ("docs/the-log.html", "The log"), ("docs/the-stream.html", "The stream"),
             ("docs/cascade.html", "The cascade"), (REPO, "GitHub"), (CRATE, "crates.io")]
    out = []
    for href, label in items:
        if href.startswith("http"):
            out.append('<li><a href="%s">%s</a></li>' % (href, label))
        elif href == here:
            out.append("<li><strong>%s</strong></li>" % label)
        else:
            out.append('<li><a href="%s">%s</a></li>' % (relative(here, href), label))
    return '<ul class="nav">%s</ul>' % "".join(out)


def foot(here):
    up = relative(here, "docs/index.html")
    return """<footer><div class="wrap">%s
<p>RadBeeper &mdash; MIT. Copyright &copy; 2026 Paul Richeson.
Designed and maintained for <a href="%s">Copal Linux</a>, a distillation of
Alpine Linux, which is also its development environment.
<a href="%s">Lab reports</a> &middot;
<a href="%s">Source</a> &middot;
<a href="%s">crates.io</a></p>
</div></footer>
</body>
</html>
""" % (nav(here), COPAL, up, REPO, CRATE)


# ------------------------------------------------------------------- build ---

def linker(here, rendered):
    """Rewrite the links a document was written with for where it now sits.

    THE DOCUMENTS ARE WRITTEN FOR THE REPOSITORY, which is the right place for
    them to be readable: `docs/the-log.md` from the README, `../README.md` from
    a lab report. Every one of those is a page on this site too, so it is
    rewritten rather than left to send a visitor back to GitHub one click after
    arriving.

    AND EVERY ONE THAT IS NOT GOES TO GITHUB, spelt out in full. `RELEASING.md`
    and the archived README are repository files this site does not publish, and
    a link to `RELEASING.html` would be a 404 that nothing catches -- the
    rewriting has to know which pages exist rather than assume every `.md` is
    one of them.
    """
    base = os.path.dirname(here)

    def fix(href):
        if re.match(r"^(https?:|mailto:|#|/)", href):
            return href
        anchor = ""
        if "#" in href:
            href, anchor = href.split("#", 1)
            anchor = "#" + anchor
        # Where this link lands in the repository, which is what says whether
        # it is a page here.
        target = os.path.normpath(os.path.join(base, href)) if base else os.path.normpath(href)
        target = target.replace(os.sep, "/")
        if href.endswith("/") or target in ("docs", "docs/archive"):
            target = target.rstrip("/") + "/README.md"
        if target in rendered:
            out = rendered[target]
            return relative(here, out) + anchor
        if target.endswith(".md"):
            return "%s/blob/main/%s%s" % (REPO, target, anchor)
        return href + anchor

    return fix


def relative(here, there):
    """`there` as seen from the page at `here`, both site-relative."""
    rel = os.path.relpath(there, os.path.dirname(here) or ".")
    return rel.replace(os.sep, "/")


def page(md_path, root, here, title, description, version, rendered,
         body_md=None, noindex=False):
    """Write one page. `here` is where it sits on the SITE, not on disk."""
    md = body_md if body_md is not None else open(md_path, encoding="utf-8").read()
    out_path = os.path.join(root, *here.split("/"))
    canonical = SITE + ("" if here == "index.html" else here)
    is_home = here == "index.html"
    extra = jsonld(version) if is_home else ""
    if noindex:
        extra += '<meta name="robots" content="noindex">\n'
    doc = head(title, description, canonical, extra)
    if is_home:
        doc += HERO
        doc += '<div class="wrap"><main id="radbeeper">\n'
    else:
        doc += '<div class="wrap"><main>\n%s\n' % nav(here)
    doc += render(md, linker(here, rendered))
    doc += "\n</main></div>\n" + foot(here)
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    open(out_path, "w", encoding="utf-8").write(doc)
    return len(doc)


def first_heading(md):
    m = re.search(r"(?m)^#\s+(.*)$", md)
    return m.group(1).strip() if m else "RadBeeper"


def first_paragraph(md):
    """The document's own one-line summary, for its meta description."""
    for block in re.split(r"\n\s*\n", md):
        b = block.strip()
        if not b or b.startswith(("#", "<!--", "|", "```", ">", "![")):
            continue
        b = re.sub(r"[*`_]", "", re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", b))
        return " ".join(b.split())[:300]
    return DESCRIPTION


def version_of():
    try:
        for line in open("Cargo.toml", encoding="utf-8"):
            m = re.match(r'^version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    except OSError:
        pass
    return "0"


def docs_index(listing):
    """An index for docs/, so the folder is a page and not a 404."""
    rows = "\n".join("| [%s](%s) | %s |" % (t, h, d) for h, t, d in listing)
    return ("# Lab reports\n\nThe *why* behind each part of RadBeeper, written "
            "up properly \u2014 the arithmetic, the failures that shaped it, and "
            "the prior art.\n\n| | |\n|---|---|\n%s\n\n---\n\n"
            "[\u2190 back to the overview](../README.md)\n" % rows)


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("-o", "--output", default=".", help="where the site is written")
    p.add_argument("--quiet", action="store_true")
    args = p.parse_args()

    version = version_of()
    root = args.output

    # WHICH SOURCE FILE BECOMES WHICH PAGE, worked out before a single one is
    # rendered. The link rewriting needs the whole map: a document cannot know
    # whether `../RELEASING.md` is a page here until every page is known.
    rendered = {"README.md": "index.html", "docs/README.md": "docs/index.html"}
    reports = []
    for name in sorted(os.listdir("docs")):
        if name.endswith(".md"):
            rendered["docs/" + name] = "docs/" + name[:-3] + ".html"
            reports.append("docs/" + name)
    if os.path.exists(os.path.join("docs", "archive", "README.md")):
        rendered["docs/archive/README.md"] = "docs/archive/index.html"

    wrote = []

    def emit(src, here, title, desc, body=None, noindex=False):
        wrote.append((here, page(src, root, here, title, desc, version,
                                 rendered, body_md=body, noindex=noindex)))

    emit("README.md", "index.html",
         "RadBeeper \u2014 Geiger counter data logging for Linux",
         DESCRIPTION)

    listing = []
    for src in reports:
        md = open(src, encoding="utf-8").read()
        title, desc = first_heading(md), first_paragraph(md)
        emit(src, rendered[src], "%s \u2014 RadBeeper" % title, desc)
        listing.append((os.path.basename(rendered[src]), title, desc[:160]))

    emit(None, "docs/index.html", "Lab reports \u2014 RadBeeper",
         "The lab reports behind RadBeeper: the log format, the stream, the "
         "cascade strip, the spectrum and the entropy source.",
         body=docs_index(listing))

    # The archive is repository history, not a page anybody should arrive at
    # from a search: it is a second copy of documents that have moved on, and
    # indexing it would compete with the ones that did.
    if "docs/archive/README.md" in rendered:
        emit("docs/archive/README.md", "docs/archive/index.html",
             "Archive \u2014 RadBeeper",
             "Past versions of RadBeeper's documentation, kept as they stood.",
             noindex=True)

    # A sitemap and a robots line, because a site that is not crawled is a
    # site nobody arrives at, which is the whole point of the exercise. The
    # archive is left out of it on purpose.
    pages = [u for u, _ in wrote if "archive" not in u] + ["monitor.html"]
    urls = "".join("<url><loc>%s%s</loc></url>"
                   % (SITE, "" if u == "index.html" else u) for u in pages)
    open(os.path.join(root, "sitemap.xml"), "w", encoding="utf-8").write(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">%s</urlset>\n' % urls)
    open(os.path.join(root, "robots.txt"), "w", encoding="utf-8").write(
        "User-agent: *\nAllow: /\nSitemap: %ssitemap.xml\n" % SITE)
    # Jekyll is not running here and must not try: it would drop any file or
    # folder beginning with an underscore without saying so.
    open(os.path.join(root, ".nojekyll"), "w").close()

    if not args.quiet:
        for name, size in wrote:
            print("landing: %-34s %6.1f KB" % (name, size / 1024.0))
        print("landing: sitemap.xml, robots.txt, .nojekyll")


if __name__ == "__main__":
    main()
