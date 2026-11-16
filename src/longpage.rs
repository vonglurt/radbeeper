// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// THE DRUMS PAGE: `drums.html`, the day paper, the month paper and the
// temperature drum as the window draws them, written by the Rust and only by
// the Rust.
//
// WHY THERE IS A PAGE. The window shows a line of spectrum for every day and
// every month there is a log for, and the temperature a line a day; the site
// showed none of it. Somebody reading the published record should be able to
// see whether anything has been arriving on a schedule for the last two
// months without installing a window to ask.
//
// WHY IT IS NOT IN export.rs, AND WHY IT IS ASKED FOR. The same reasons as
// the frame browser's, at the head of src/browser.rs: every page in export.rs
// exists a second time in the one-file Python and is compared with it byte
// for byte, and the long drums have no Python half -- the transform is here
// and nowhere else. So `radbeeper export` writes what it always wrote, and
// this page is one more flag: `--drums-page`.
//
// NO SCRIPT, AND NOTHING FETCHED. The papers are inline SVG, drawn once when
// the page is written: a page of a record should still be that record when
// it is saved to a disk and opened in ten years.
//
// THE PAPERS ARE ON THEIR OWN DARK GROUND, in both of the page's skins. The
// pen's colours (`drum::heat`) were chosen against the window's shaded
// ground: its floor is a teal that is hardly there, which on a white page is
// not there at all. So a paper brings its ground with it, as a photograph of
// the window would.
//
// THE RECORD'S LAST DAY IS THE FOOT OF EVERY PAPER, NOT THE DAY THE PAGE WAS
// WRITTEN. The window is beside a counter that is running; this page is
// built from whatever logs were last copied into a directory, which may be
// a fortnight old. Papers that ended today would be ten empty tracks of
// temperature over a record that has plenty.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::analysis::{self, fahrenheit, temp_drum, TempDrum};
use crate::clock;
use crate::drum::{heat, pen, shade_of, SHADES};
use crate::export::{abspath, basename, dirname, esc, join, now, page_head, path_str};
use crate::log;
use crate::longdrum::{self, LongRow, Paper};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const DRUMS_TITLE: &str = "Days, months and the temperature";

/// The most lines the day paper has: two months of days, which at nine
/// units a line is a paper about as high as it is easy to take in at once.
pub const DAY_LINES: usize = 60;
/// The most lines the month paper has: every month `longdrum::look_back`
/// reaches.
pub const MONTH_LINES: usize = 36;
/// The days on the temperature drum, and the cells a day is cut into.
///
/// TEN, AS THE WINDOW HAS, and for the window's reason: a track has to be
/// high enough for a day's rise and fall to be read in it, and thirty of
/// them that high are three screens.
pub const TEMP_DAYS: usize = 10;
pub const TEMP_COLS: usize = 240;

/// What chance reaches on a long paper, against the 1.0 it puts in a band:
/// the window's `LONG_LUCK`, and the same number so that green and hot mean
/// here what they mean there.
pub const LUCK: f32 = 5.0;

/// The papers' own units: an SVG is this wide and is scaled to the column.
const WIDE: f32 = 1000.0;
/// The margin the dates are written in, and the one at the right.
const LEFT: f32 = 62.0;
const RIGHT: f32 = 16.0;
/// The strip at a long paper's head, where its periods are named, and the
/// one at its foot, where the pen's colours are.
const HEAD: f32 = 26.0;
const FOOT: f32 = 40.0;
/// The ground: the window's dark panel, half way to black.
const GROUND: &str = "#0c0e12";

/// Everything the page is drawn from.
pub struct Drums {
    /// The moment the papers end at: the last row of the record, or now.
    pub as_of: f64,
    /// Newest first, as `longdrum::rows` gives them.
    pub days: Vec<LongRow>,
    pub months: Vec<LongRow>,
    /// A drum for each counter with a temperature log: its serial, the
    /// drum, and how many readings it was laid down from.
    pub temps: Vec<(String, TempDrum, usize)>,
}

/// Read a log directory into the three papers.
pub fn gather(logs: &Path, now: f64) -> Drums {
    let last = longdrum::Counts::load(logs, 0.0, now).reach().map(|r| r.1);
    // A second past the last row, so the row itself is inside the span.
    let as_of = last.map(|t| (t + 1.0).min(now)).unwrap_or(now);
    let lines = |paper, most| -> Vec<LongRow> {
        longdrum::rows(logs, paper, as_of, most).into_iter().flatten().collect()
    };
    let (days, months) = if last.is_some() {
        (lines(Paper::Day, DAY_LINES), lines(Paper::Month, MONTH_LINES))
    } else {
        (Vec::new(), Vec::new())
    };
    let mut temps = Vec::new();
    for serial in log::temp_serials(logs) {
        let all = log::read_temps(logs, &serial, 0.0);
        // Each counter's drum ends on its own last reading: one unplugged a
        // week before the other still has ten days to show.
        let Some(end) = all.last().map(|s| s.0.min(now)) else { continue };
        let since = clock::day_start(end) - TEMP_DAYS as f64 * 86400.0;
        let recent: Vec<(f64, f64)> = all.into_iter().filter(|s| s.0 >= since && s.0 <= end).collect();
        let drum = temp_drum(&recent, end, TEMP_COLS, TEMP_DAYS);
        let used = recent.iter().filter(|s| drum.lines.last().map(|l| s.0 >= l.day).unwrap_or(false)).count();
        temps.push((serial, drum, used));
    }
    Drums { as_of, days, months, temps }
}

/// The two ends of a paper's period axis, in seconds, longest first: the
/// window's `long_ends`. The long end is the longest period a finished
/// line has -- a third of the line, see `longdrum::SLOWEST` -- so the head
/// of the paper cannot name an end the lines do not reach; the short end is
/// four of the paper's bins.
pub fn long_ends(paper: Paper, rows: &[LongRow]) -> (f64, f64) {
    let (whole, short) = match paper {
        Paper::Day => (86400.0, 4.0 * longdrum::DAY_BIN),
        Paper::Month => (31.0 * 86400.0, 4.0 * longdrum::MONTH_BIN),
    };
    let long = rows
        .iter()
        .filter(|r| r.whole)
        .filter_map(|r| r.bands.first())
        .map(|b| b.0 as f64)
        .fold(0.0f64, f64::max);
    (if long > short { long } else { whole / longdrum::SLOWEST as f64 }, short)
}

/// Where a period stands across a long paper, from nought at its longest to
/// one at its shortest, by its logarithm; None where it is off either end.
pub fn long_place(ends: (f64, f64), period: f64) -> Option<f32> {
    let (long, short) = ends;
    // A hair over the ends, so a band that stands on one is not lost to
    // the last bit of a float.
    if !(period > 0.0) || period > long * 1.02 || period < short * 0.98 {
        return None;
    }
    Some((((long / period).ln() / (long / short).ln()) as f32).clamp(0.0, 1.0))
}

/// A period in the fewest letters: `8h`, `10d`, `20m`.
pub fn period_name(seconds: f64) -> String {
    for (unit, letter) in [(86400.0, "d"), (3600.0, "h"), (60.0, "m")] {
        if seconds >= unit * 0.95 {
            return format!("{:.0}{}", seconds / unit, letter);
        }
    }
    format!("{:.0}s", seconds)
}

/// The periods named along a paper's head, longest first: its two ends, and
/// between them the round ones that stand clear of both.
pub fn long_ticks(paper: Paper, ends: (f64, f64)) -> Vec<(f64, String)> {
    let between: &[f64] = match paper {
        Paper::Day => &[10800.0, 3600.0, 1200.0, 300.0],
        Paper::Month => &[3.0 * 86400.0, 86400.0, 21600.0, 3600.0],
    };
    let mut out = vec![(ends.0, period_name(ends.0))];
    out.extend(
        between
            .iter()
            .filter(|p| **p < ends.0 * 0.7 && **p > ends.1 * 1.4)
            .map(|p| (*p, period_name(*p))),
    );
    out.push((ends.1, period_name(ends.1)));
    out
}

/// Which shade the pen is for a temperature `offset` tracks from the drum's
/// reference: the window's `temp_shade`. Inside its track the pen is green,
/// at the edge of it the last of the greens, and out on its neighbour's
/// ground it is hot.
pub fn temp_shade(offset: f32) -> usize {
    const REST: f32 = 0.22;
    const EDGE: f32 = 0.66;
    let t = (REST + (EDGE - REST) * offset.abs() / 0.5).min(1.0);
    ((t * SHADES as f32) as usize).min(SHADES - 1)
}

/// How a long paper is laid out: how far apart its lines are and how far
/// the loudest thing on it rises from its own line.
///
/// A MONTH HAS MORE ROOM THAN A DAY because there are few of them: three
/// lines nine units apart are a smear at the foot of an empty strip.
fn pitch(paper: Paper) -> (f32, f32) {
    match paper {
        Paper::Day => (9.0, 30.0),
        Paper::Month => (18.0, 40.0),
    }
}

/// How high a long paper of `lines` lines is, and where line `k`'s own
/// baseline is down it: the newest, line 0, is at the foot.
fn long_height(paper: Paper, lines: usize) -> f32 {
    let (step, reach) = pitch(paper);
    HEAD + reach + lines.saturating_sub(1) as f32 * step + FOOT
}

fn long_base(paper: Paper, lines: usize, k: usize) -> f32 {
    long_height(paper, lines) - FOOT - k as f32 * pitch(paper).0
}

/// The loudest band in view on a paper, and never less than half as much
/// again as luck reaches: what `analysis::gain` holds every height against,
/// so a paper of nothing but floor is not blown up to fill its room.
pub fn loudest(rows: &[LongRow], ends: (f64, f64)) -> f64 {
    rows.iter()
        .flat_map(|r| r.bands.iter())
        .filter(|b| long_place(ends, b.0 as f64).is_some())
        .map(|b| b.1)
        .fold(LUCK * 1.5, f32::max) as f64
}

/// A line's points: (x, y, power) for each band that is on the axis, the
/// longest period first. `left` and `across` are the paper's, `base` the
/// line's own baseline and `reach` how far the loudest rises from it.
pub fn line_points(
    row: &LongRow,
    ends: (f64, f64),
    loudest: f64,
    left: f32,
    across: f32,
    base: f32,
    reach: f32,
) -> Vec<(f32, f32, f32)> {
    row.bands
        .iter()
        .filter_map(|(period, power)| {
            let x = left + long_place(ends, *period as f64)? * across;
            let y = base - analysis::gain(*power as f64, loudest) as f32 * reach;
            Some((x, y, *power))
        })
        .collect()
}

/// A coordinate to the tenth, in as few characters as say it.
///
/// A TENTH IS FINER THAN THE PEN, and the papers are most of the page's
/// bytes: sixty lines of two hundred points are twelve thousand pairs.
fn tenths(v: f32) -> i32 {
    (v * 10.0).round() as i32
}

fn coord(t: i32) -> String {
    if t % 10 == 0 {
        format!("{}", t / 10)
    } else {
        format!("{}.{}", t / 10, t.abs() % 10)
    }
}

/// One line's ink, sorted into runs of one shade.
///
/// THE WINDOW'S WAY OF STROKING, for the window's reason: a path has one
/// colour and one width, and the pen changes both along a line. So each
/// shade has a path of its own, of every stretch of the line that is that
/// shade; a stretch that carries on from where the last one ended is the
/// same sub-path, and so is joined round and not butted.
struct Ink {
    paths: Vec<String>,
    ends: Vec<Option<(i32, i32)>>,
}

impl Ink {
    fn new() -> Ink {
        Ink { paths: vec![String::new(); SHADES], ends: vec![None; SHADES] }
    }

    fn stroke(&mut self, shade: usize, from: (f32, f32), to: (f32, f32)) {
        let shade = shade.min(SHADES - 1);
        let a = (tenths(from.0), tenths(from.1));
        let b = (tenths(to.0), tenths(to.1));
        let d = &mut self.paths[shade];
        if self.ends[shade] != Some(a) {
            d.push_str(&format!("M{} {}", coord(a.0), coord(a.1)));
        }
        d.push_str(&format!("L{} {}", coord(b.0), coord(b.1)));
        self.ends[shade] = Some(b);
    }

    /// The paths, the coolest first, at `alpha`; a shade from `glow` up has
    /// the wider, fainter stroke under it that the window gives what is
    /// hot.
    fn svg(&self, alpha: f32, glow: usize) -> String {
        let mut out = format!("<g class=\"pen\" stroke-opacity=\"{:.2}\">", alpha);
        for (shade, d) in self.paths.iter().enumerate() {
            if d.is_empty() {
                continue;
            }
            if shade >= glow {
                out.push_str(&format!(
                    "<path class=\"h{}\" stroke-width=\"{:.1}\" stroke-opacity=\"{:.2}\" d=\"{}\"/>",
                    shade,
                    pen(shade) + 3.0,
                    0.22 * alpha,
                    d
                ));
            }
            out.push_str(&format!("<path class=\"h{}\" d=\"{}\"/>", shade, d));
        }
        out.push_str("</g>");
        out
    }
}

/// A shade's colour as the page writes it.
fn hex(shade: usize) -> String {
    let [r, g, b] = heat(shade);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// A date without the nought a strftime day begins with.
fn date(t: f64, fmt: &str) -> String {
    clock::format(t, fmt).trim_start_matches('0').to_string()
}

fn svg_open(height: f32, label: &str) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" role=\"img\" \
         aria-label=\"{l}\"><rect width=\"{w}\" height=\"{h}\" rx=\"6\" fill=\"{g}\"/>",
        w = coord(tenths(WIDE)),
        h = coord(tenths(height)),
        l = esc(label),
        g = GROUND
    )
}

/// The pen's colours, named, at the foot of a long paper: a strip from
/// nothing to over what luck reaches, with a mark where chance stands and
/// one where luck's reach is.
fn legend(top: f32) -> String {
    let (wide, right) = (280.0f32, WIDE - RIGHT);
    let left = right - wide;
    let at = |power: f32| left + wide * shade_of(power, LUCK) as f32 / (SHADES - 1) as f32;
    let mut out = String::new();
    // A strip of every shade, each a hair over its own width so no ground
    // shows between two of them.
    for shade in 0..SHADES {
        out.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"3\" fill=\"{}\"/>",
            coord(tenths(left + wide * shade as f32 / SHADES as f32)),
            coord(tenths(top)),
            coord(tenths(wide / SHADES as f32 + 0.5)),
            hex(shade)
        ));
    }
    for (power, words, anchor) in [
        (1.0f32, "chance", "middle"),
        (LUCK, "luck's reach", "middle"),
    ] {
        let x = coord(tenths(at(power)));
        out.push_str(&format!(
            "<path class=\"tick\" d=\"M{x} {a}V{b}\"/>\
             <text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\">{words}</text>",
            a = coord(tenths(top - 3.0)),
            b = coord(tenths(top + 6.0)),
            y = coord(tenths(top + 18.0)),
        ));
    }
    out
}

/// A long paper as an SVG: a line of spectrum a day, or a month, the newest
/// at its foot and the older ones stepping up and away.
///
/// AS `Chart::long_paper` DRAWS IT IN THE WINDOW. A line rises from its own
/// baseline by `analysis::gain` against the loudest thing on the paper; it
/// is stroked in runs of one shade, in `heat` at the width `pen` gives; and
/// it puts the ground back under itself, so the line in front hides what is
/// behind it. A line made from part of its span is fainter by how much of
/// it was missing, and an older line a little fainter than a newer one. A
/// line not yet finished has bands only for the periods it has had time
/// for, so it is drawn over those and no further.
pub fn long_svg(rows: &[LongRow], paper: Paper) -> String {
    let lines = rows.len();
    let reach = pitch(paper).1;
    let height = long_height(paper, lines);
    let across = WIDE - LEFT - RIGHT;
    let ends = long_ends(paper, rows);
    let top = loudest(rows, ends);
    let hot = shade_of(LUCK, LUCK);
    let name = match paper {
        Paper::Day => "The day paper: a line of spectrum for each day",
        Paper::Month => "The month paper: a line of spectrum for each month",
    };
    let mut out = svg_open(height, name);
    // The head: what the axis is, and the named periods with a rule down
    // the paper under each.
    out.push_str(&format!(
        "<text x=\"{}\" y=\"{}\" text-anchor=\"end\">every</text>",
        coord(tenths(LEFT - 10.0)),
        coord(tenths(HEAD - 9.0))
    ));
    let ticks = long_ticks(paper, ends);
    for (i, (period, label)) in ticks.iter().enumerate() {
        let Some(x) = long_place(ends, *period) else { continue };
        let x = (LEFT + x * across).round();
        let anchor = if i == 0 { "start" } else if i + 1 == ticks.len() { "end" } else { "middle" };
        out.push_str(&format!(
            "<path class=\"rule\" d=\"M{x} {a}V{b}\"/>\
             <text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\">{label}</text>",
            x = coord(tenths(x)),
            a = coord(tenths(HEAD)),
            b = coord(tenths(height - FOOT + 6.0)),
            y = coord(tenths(HEAD - 9.0)),
        ));
    }
    out.push_str(&format!(
        "<path class=\"hair\" d=\"M{} {h}H{}\"/>",
        coord(tenths(LEFT)),
        coord(tenths(WIDE - RIGHT)),
        h = coord(tenths(HEAD))
    ));
    let dated = match paper {
        Paper::Day => 3,
        Paper::Month => 1,
    };
    for (k, row) in rows.iter().enumerate().rev() {
        let base = long_base(paper, lines, k);
        let points = line_points(row, ends, top, LEFT, across, base, reach);
        if points.len() < 2 {
            continue;
        }
        let what = match paper {
            Paper::Day => "of the day",
            Paper::Month => "of the month",
        };
        out.push_str(&format!(
            "<g><title>{} \u{b7} {:.1} counts a minute \u{b7} made from {:.0}% {}{}</title>",
            esc(&date(row.start, if paper == Paper::Day { "%d %b %Y" } else { "%B %Y" })),
            row.cps * 60.0,
            (row.covered as f64 * 100.0).min(100.0),
            what,
            if row.whole { "" } else { ", so far" },
        ));
        // The ground, put back under the line.
        let mut under = format!("M{} {}", coord(tenths(points[0].0)), coord(tenths(base)));
        for (x, y, _) in &points {
            under.push_str(&format!("L{} {}", coord(tenths(*x)), coord(tenths(*y))));
        }
        under.push_str(&format!("L{} {}Z", coord(tenths(points[points.len() - 1].0)), coord(tenths(base))));
        out.push_str(&format!("<path fill=\"{}\" d=\"{}\"/>", GROUND, under));
        let mut ink = Ink::new();
        for pair in points.windows(2) {
            let ((x0, y0, p0), (x1, y1, p1)) = (pair[0], pair[1]);
            ink.stroke(shade_of(p0.max(p1), LUCK), (x0, y0), (x1, y1));
        }
        let away = k as f32 / lines.max(1) as f32;
        let alpha = (0.55 + 0.45 * row.covered.clamp(0.0, 1.0)) * (1.0 - 0.35 * away);
        out.push_str(&ink.svg(alpha, hot));
        out.push_str("</g>");
        if k % dated == 0 {
            out.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" text-anchor=\"end\">{}</text>",
                coord(tenths(LEFT - 10.0)),
                coord(tenths(base + 3.0)),
                esc(&date(row.start, if paper == Paper::Day { "%d %b" } else { "%b %Y" }))
            ));
        }
    }
    out.push_str(&legend(height - FOOT + 14.0));
    out.push_str("</svg>");
    out
}

/// A temperature both ways, as the window writes it: 79.0 F and 26.1 C.
fn both_ways(celsius: f64) -> String {
    format!("{:.1}\u{b0}F {:.1}\u{b0}C", fahrenheit(celsius), celsius)
}

/// What a track is worth and what every line stands on, in words.
///
/// A TRACK IS A DIFFERENCE, so its Fahrenheit is nine fifths of its Celsius
/// and no thirty-two added. The reference is a temperature, and has them.
pub fn temp_words(drum: &TempDrum) -> String {
    format!(
        "one track = {:.2} \u{b0}C ({:.2} \u{b0}F) \u{b7} each line stands on {}",
        drum.span,
        drum.span * 9.0 / 5.0,
        both_ways(drum.reference)
    )
}

/// The margins of the temperature paper: the date on the left, and on the
/// right the day's mean, both ways.
const TEMP_RIGHT: f32 = 118.0;
/// How far apart its lines are, and the room under the last for the hours.
const TEMP_PITCH: f32 = 30.0;
const TEMP_FOOT: f32 = 24.0;

/// The temperature drum as an SVG: a line a day, the newest at the foot.
///
/// AS `Chart::temp_paper` DRAWS IT. Every line stands on the same
/// temperature and is drawn to the same degrees a track, and the pen is
/// green inside its own track and hot out of it (`temp_shade`). THE LINES
/// MAY CROSS, AND NOTHING IS HIDDEN WHERE THEY DO: a day's temperature goes
/// both ways from its line, and a line that has left its track for its
/// neighbour's is the one thing this paper is kept to show. An empty day is
/// an empty track, ruled faintly; a stretch nothing was read in is a gap in
/// its line.
pub fn temp_svg(drum: &TempDrum) -> String {
    let days = drum.lines.len();
    let paper = (days as f32 + 1.0) * TEMP_PITCH;
    let height = paper + TEMP_FOOT;
    let across = WIDE - LEFT - TEMP_RIGHT;
    let mut out = svg_open(height, "The temperature drum: a line for each day");
    // The hours: a rule down the paper at every sixth, over its figure.
    for h in 0..=4 {
        let x = coord(tenths((LEFT + across * h as f32 / 4.0).round()));
        out.push_str(&format!(
            "<path class=\"rule\" d=\"M{x} 0V{b}\"/>\
             <text x=\"{x}\" y=\"{y}\" text-anchor=\"middle\">{:02}:00</text>",
            h * 6,
            b = coord(tenths(paper)),
            y = coord(tenths(paper + 15.0)),
        ));
    }
    for (k, line) in drum.lines.iter().enumerate().rev() {
        let base = paper - (k as f32 + 1.0) * TEMP_PITCH;
        out.push_str(&format!(
            "<path class=\"track\" d=\"M{} {y}H{}\"/>",
            coord(tenths(LEFT)),
            coord(tenths(LEFT + across)),
            y = coord(tenths(base.round()))
        ));
        let cells = line.cells.len().max(1) as f32;
        let at = |c: usize, off: f32| {
            (
                LEFT + across * (c as f32 + 0.5) / cells,
                (base - off * TEMP_PITCH).clamp(1.0, paper - 1.0),
            )
        };
        let alpha = 1.0 - 0.45 * k as f32 / days.max(1) as f32;
        let mut ink = Ink::new();
        let mut dots = String::new();
        for c in 0..line.cells.len() {
            let Some(here) = line.cells[c] else { continue };
            match line.cells.get(c + 1).copied().flatten() {
                Some(next) => {
                    let far = if here.abs() > next.abs() { here } else { next };
                    ink.stroke(temp_shade(far), at(c, here), at(c + 1, next));
                }
                // A reading with nothing either side of it is a dot, or it
                // would not be on the paper at all.
                None if c == 0 || line.cells[c - 1].is_none() => {
                    let (x, y) = at(c, here);
                    dots.push_str(&format!(
                        "<circle cx=\"{}\" cy=\"{}\" r=\"1.2\" fill=\"{}\" fill-opacity=\"{:.2}\"/>",
                        coord(tenths(x)),
                        coord(tenths(y)),
                        hex(temp_shade(here)),
                        alpha
                    ));
                }
                None => {}
            }
        }
        out.push_str("<g>");
        if let (Some(mean), Some(low), Some(high)) = (line.mean, line.low, line.high) {
            out.push_str(&format!(
                "<title>{} \u{b7} mean {} \u{b7} from {} to {}</title>",
                esc(&date(line.day, "%d %b %Y")),
                both_ways(mean),
                both_ways(low),
                both_ways(high)
            ));
        }
        out.push_str(&ink.svg(alpha, temp_shade(0.5) + 1));
        out.push_str(&dots);
        out.push_str("</g>");
        let y = coord(tenths(base + 3.5));
        out.push_str(&format!(
            "<text x=\"{}\" y=\"{y}\" text-anchor=\"end\">{}</text>",
            coord(tenths(LEFT - 10.0)),
            esc(&date(line.day, "%d %b"))
        ));
        if let Some(mean) = line.mean {
            out.push_str(&format!(
                "<text x=\"{}\" y=\"{y}\" text-anchor=\"end\">{}</text>",
                coord(tenths(WIDE - RIGHT)),
                both_ways(mean)
            ));
        }
    }
    out.push_str("</svg>");
    out
}

/// How much record a paper was made from, in a sentence.
fn made_from(rows: &[LongRow], paper: Paper) -> String {
    let (Some(newest), Some(oldest)) = (rows.first(), rows.last()) else { return String::new() };
    let (one, fmt) = match paper {
        Paper::Day => ("day", "%d %b %Y"),
        Paper::Month => ("month", "%B %Y"),
    };
    let hours: f64 = rows
        .iter()
        .map(|r| r.covered as f64 * (paper.next(r.start) - r.start) / 3600.0)
        .sum();
    let part = rows.iter().filter(|r| r.covered < 0.99).count();
    let mut out = format!(
        "{} line{}, {}, made from {} hours of counts.",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        if rows.len() == 1 {
            esc(&date(newest.start, fmt))
        } else {
            format!("{} to {}", esc(&date(oldest.start, fmt)), esc(&date(newest.start, fmt)))
        },
        commas(hours.round() as i64)
    );
    if part > 0 {
        out.push_str(&format!(
            " {} of them {} made from less than the whole {}, and {} fainter for it.",
            part,
            if part == 1 { "is" } else { "are" },
            one,
            if part == 1 { "is" } else { "are" }
        ));
    }
    if !newest.whole {
        out.push_str(&format!(
            " The line at the foot is of a {} the record stops in: it reaches only the \
             periods that had time to come round three times.",
            one
        ));
    }
    out
}

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

/// The whole page.
///
/// `index` is the report this page stands beside, and `index_title` what it
/// is called; `generated` is the moment the page says it was written.
pub fn render(drums: &Drums, index: &str, index_title: &str, generated: f64) -> String {
    let mut css: Vec<String> = DRUMS_CSS.iter().map(|s| s.to_string()).collect();
    // The pen, once: a class a shade, so a path says which shade it is in
    // three characters and not its colour and its width each time.
    for shade in 0..SHADES {
        css.push(format!(".h{}{{stroke:{};stroke-width:{:.2}}}", shade, hex(shade), pen(shade)));
    }
    let rules: Vec<&str> = css.iter().map(String::as_str).collect();
    let mut out = page_head(DRUMS_TITLE, &rules);
    macro_rules! a {
        ($($arg:tt)*) => { out.push(format!($($arg)*)) };
    }
    a!("<h1>{}</h1>", esc(DRUMS_TITLE));
    a!(
        "<p class=\"sub\">radbeeper {} &middot; the record up to {} &middot; generated {}</p>",
        VERSION,
        esc(&clock::format(drums.as_of, "%Y-%m-%d %H:%M")),
        esc(&clock::format(generated, "%Y-%m-%d %H:%M"))
    );
    a!(
        "<p class=\"sub\"><a href=\"{}\">&larr; {}</a> &middot; \
         <a href=\"docs/reading-the-drum-spectrogram.html\">how to read the drum</a></p>",
        esc(index),
        esc(index_title)
    );

    if drums.days.is_empty() && drums.months.is_empty() && drums.temps.is_empty() {
        a!(
            "<section><p class=\"words\">There is no record here yet. A day has a line \
             once the count log holds a quarter of an hour of it, and the temperature \
             has one once the service has read the counter's thermometer.</p></section>"
        );
    }

    if !drums.days.is_empty() {
        a!("<section><h2>Days</h2>");
        a!(
            "<p class=\"words\">A line is one whole day, midnight to midnight, the newest \
             at the foot. Left to right is how often something comes round: once every \
             {} at the left, to once every {} at the right. A line stands up, and its pen \
             turns hot and wide, where the counts of that day rose and fell at that \
             period more than chance makes them. <b>A stripe that runs up through line \
             after line, in the same place, is something arriving on that schedule day \
             after day.</b> Green is ordinary: decay alone leaves every line green.</p>",
            period_name(long_ends(Paper::Day, &drums.days).0),
            period_name(long_ends(Paper::Day, &drums.days).1)
        );
        a!("<figure class=\"paper\"><div class=\"roll\">{}</div>", long_svg(&drums.days, Paper::Day));
        a!(
            "<figcaption>{} A day when the counter was away has no line, and the next \
             day that has one sits directly above; the dates down the left say which \
             day a line is, and pointing at a line names it.</figcaption></figure>",
            made_from(&drums.days, Paper::Day)
        );
        a!("</section>");
    }

    if !drums.months.is_empty() {
        a!("<section><h2>Months</h2>");
        a!(
            "<p class=\"words\">The same paper and the same pen over a longer axis: a line \
             is one calendar month, from once every {} at the left to once every {} at \
             the right. What a day does once a day is here, where it comes round thirty \
             times: a line that stands up at <code>1d</code> is something that rises and \
             falls once a day.</p>",
            period_name(long_ends(Paper::Month, &drums.months).0),
            period_name(long_ends(Paper::Month, &drums.months).1)
        );
        a!("<figure class=\"paper\"><div class=\"roll\">{}</div>", long_svg(&drums.months, Paper::Month));
        a!("<figcaption>{}</figcaption></figure>", made_from(&drums.months, Paper::Month));
        a!("</section>");
    }

    if !drums.days.is_empty() || !drums.months.is_empty() {
        a!(
            "<p class=\"made\">Both papers are worked out from the log the counter has \
             been keeping all along, a row every thirty seconds, every counter's rows \
             together. A line is not asked about anything that has not had time to come \
             round three times, and thirty seconds nobody counted are left out, not \
             taken for thirty seconds of nothing.</p>"
        );
    }

    for (serial, drum, readings) in &drums.temps {
        a!("<section><h2>Temperature &middot; counter {}</h2>", esc(serial));
        a!(
            "<p class=\"words\">A GMC-320 has a thermometer, and the service reads it. \
             These are the last {} days there are readings for, laid down the way a drum \
             recorder would: <b>a line a day, the newest at the foot, the day before one \
             track above.</b> Every line is drawn to the same scale about the same \
             temperature, so a day can be read against the one directly above it. A \
             track is worth an ordinary day's rise and fall, so a day like most days \
             stays inside its own track. A day that went out of the ordinary crosses \
             into its neighbour's, and its pen turns hot where it does.</p>",
            drum.lines.len()
        );
        a!("<figure class=\"paper\"><div class=\"roll\">{}</div>", temp_svg(drum));
        let newest = drum.lines.first().map(|l| l.day).unwrap_or(drums.as_of);
        let oldest = drum.lines.last().map(|l| l.day).unwrap_or(drums.as_of);
        a!(
            "<figcaption>{} &middot; {} to {}, from {} readings; each day's mean is at \
             the right of its line. The temperature is the inside of the counter's \
             case, not the air of the room.</figcaption></figure>",
            esc(&temp_words(drum)),
            esc(&date(oldest, "%d %b")),
            esc(&date(newest, "%d %b %Y")),
            commas(*readings as i64)
        );
        a!("</section>");
    }

    a!(
        "<footer>Written by <code>radbeeper export --drums-page</code>, which is the \
         Rust build only: the transform behind these lines has no Python half. \
         The papers are pictures drawn when the page was written, with no script \
         and nothing fetched.</footer>"
    );
    a!("</main></body></html>");
    out.join("\n")
}

/// What was written.
pub struct Page {
    pub path: PathBuf,
    pub days: usize,
    pub months: usize,
    pub temps: usize,
    pub bytes: usize,
}

/// `drums.html` beside `output`, or at `drums_output` where that is given.
///
/// WRITTEN EVEN WHEN THERE IS NOTHING TO DRAW, saying so. The landing page
/// links here whether or not a fork has copied its logs in yet, and a page
/// that says there is no record is better than a link to nothing.
pub fn write_page(
    logs: &Path,
    output: &Path,
    index_title: &str,
    drums_output: Option<&Path>,
) -> io::Result<Page> {
    let out = path_str(output);
    let target = match drums_output.filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => path_str(p),
        None => join(&dirname(&abspath(&out)), "drums.html"),
    };
    let generated = now();
    let drums = gather(logs, generated);
    let html = render(&drums, basename(&out), index_title, generated);
    fs::write(&target, &html)?;
    Ok(Page {
        path: PathBuf::from(target),
        days: drums.days.len(),
        months: drums.months.len(),
        temps: drums.temps.len(),
        bytes: html.len(),
    })
}

/// What this page adds to the stylesheet every page has.
///
/// THE WORDS ON A PAPER ARE SMALL AND GREY AND THE RULES ARE NEARLY NOT
/// THERE: the ink is the lines, and whatever else is on the ground is there
/// to say where a line is and then get out of its way. Their colours are
/// written out and not taken from the page's, because the ground under them
/// is the same in both of its skins.
const DRUMS_CSS: &[&str] = &[
    ".words{max-width:74ch}",
    ".sub a{color:var(--accent)}",
    ".made{color:var(--dim);font-size:.85rem;max-width:74ch}",
    ".paper{margin:1rem 0 1.4rem}",
    // A PAPER NARROWER THAN ITS WORDS CAN BE READ AT IS ROLLED, NOT SHRUNK:
    // on a telephone the dates would be three pixels high.
    ".roll{overflow-x:auto;border-radius:6px}",
    ".paper svg{display:block;width:100%;min-width:640px;height:auto}",
    ".paper text{font:10.5px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;\
     fill:#8b93a1}",
    ".paper .rule{stroke:#fff;stroke-opacity:.07;stroke-width:1;fill:none}",
    ".paper .hair{stroke:#fff;stroke-opacity:.2;stroke-width:1;fill:none}",
    ".paper .tick{stroke:#fff;stroke-opacity:.55;stroke-width:1;fill:none}",
    ".paper .track{stroke:#29665e;stroke-opacity:.3;stroke-width:1;fill:none}",
    ".pen path{fill:none;stroke-linecap:round;stroke-linejoin:round}",
    ".paper figcaption{color:var(--dim);font-size:.85rem;margin-top:.5rem;max-width:74ch}",
];

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radbeeper-page-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Every `<tag` there is, less every `</tag` and every `/>`: nought for
    /// markup that closes what it opens.
    fn unclosed(svg: &str) -> isize {
        let opened = svg.matches('<').count() as isize - svg.matches("</").count() as isize;
        opened - svg.matches("</").count() as isize - svg.matches("/>").count() as isize
    }

    /// Every number in every `d="..."` of a picture.
    fn path_numbers(svg: &str) -> Vec<f32> {
        svg.split(" d=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .flat_map(|d| {
                d.split(|c: char| c.is_ascii_alphabetic() || c == ' ')
                    .filter(|n| !n.is_empty())
                    .map(|n| n.parse::<f32>().expect("a path holds numbers and letters"))
                    .collect::<Vec<f32>>()
            })
            .collect()
    }

    #[test]
    fn an_empty_directory_is_a_page_that_says_so() {
        let dir = tmp("empty");
        let drums = gather(&dir, 1_790_000_000.0);
        assert!(drums.days.is_empty() && drums.months.is_empty() && drums.temps.is_empty());
        let html = render(&drums, "monitor.html", "the report", 1_790_000_000.0);
        assert!(html.contains("There is no record here yet"));
        assert!(!html.contains("<svg"), "nothing to draw is no paper");
        assert!(html.trim_end().ends_with("</html>"));
        let page = write_page(&dir, &dir.join("monitor.html"), "the report", None).unwrap();
        assert_eq!(page.path.file_name().and_then(|n| n.to_str()), Some("drums.html"));
        assert_eq!(fs::read_to_string(&page.path).unwrap().len(), page.bytes);
    }

    #[test]
    fn counts_alone_are_two_papers_and_no_thermometer() {
        let dir = tmp("counts");
        let now = Paper::Month.start(1_790_000_000.0) + 8.0 * 86400.0 + 7.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 40.0 * 86400.0, now, 0.7, &[(1200.0, 0.06)], &[], 4);
        let drums = gather(&dir, now);
        // Forty whole days and the two the record begins and ends in.
        assert!((40..=42).contains(&drums.days.len()), "{}", drums.days.len());
        assert!(!drums.days[0].whole, "the day the record stops in is not over");
        assert!(drums.months.len() >= 2);
        let html = render(&drums, "monitor.html", "the report", now);
        assert_eq!(html.matches("<svg").count(), 2);
        assert_eq!(html.matches("</svg>").count(), 2);
        assert!(html.contains("<h2>Days</h2>") && html.contains("<h2>Months</h2>"));
        assert!(!html.contains("Temperature"), "no temperature log is no temperature drum");
        assert!(!html.to_lowercase().contains("<script"), "this page has no script");
        assert!(html.contains("href=\"monitor.html\""));
    }

    #[test]
    fn a_temperature_log_is_a_drum_for_its_counter() {
        let dir = tmp("temps");
        let now = clock::day_start(1_790_000_000.0) + 15.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 3.0 * 86400.0, now, 0.7, &[], &[], 6);
        let odd = clock::day_start(now - 2.0 * 86400.0) + 14.0 * 3600.0;
        sample::temps(&dir, "AAAAAAAAAAAAAA", now - 6.0 * 86400.0, now, 22.0, 2.4,
                      &[(odd, odd + 3.0 * 3600.0, 4.0)], 7);
        let drums = gather(&dir, now);
        assert_eq!(drums.temps.len(), 1);
        let (serial, drum, readings) = &drums.temps[0];
        assert_eq!(serial, "AAAAAAAAAAAAAA");
        assert_eq!(drum.lines.len(), TEMP_DAYS);
        assert!(*readings > 6 * 2800, "six days of a reading every thirty seconds: {}", readings);
        let svg = temp_svg(drum);
        assert_eq!(unclosed(&svg), 0, "the picture closes what it opens");
        // Seven days have readings and three are empty tracks: a mean at the
        // right of each of the seven, and a date at the left of all ten.
        let texts = svg.matches("<text").count();
        assert_eq!(texts, 5 + TEMP_DAYS + 7, "hours, dates and means");
        assert!(svg.contains("\u{b0}F") && svg.contains("\u{b0}C"));
        // The odd afternoon left its track, and the pen is hot there.
        let hot = (temp_shade(0.5) + 1..SHADES).any(|s| svg.contains(&format!("class=\"h{}\"", s)));
        assert!(hot, "four degrees over an ordinary day should be out of its track");
        let html = render(&drums, "monitor.html", "the report", now);
        assert_eq!(html.matches("<svg").count(), 3);
        assert!(html.contains("one track = "));
        assert!(html.contains("not the air of the room"));
        assert!(!html.to_lowercase().contains("<script"));
    }

    #[test]
    fn a_rhythm_stands_tallest_where_its_period_is_on_the_axis() {
        let dir = tmp("rhythm");
        let now = clock::day_start(1_790_000_000.0) + 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 3.0 * 86400.0, now, 0.7, &[(1200.0, 0.2)], &[], 5);
        let drums = gather(&dir, now);
        let ends = long_ends(Paper::Day, &drums.days);
        assert!((ends.0 - 28800.0).abs() < 600.0 && ends.1 == 120.0, "{:?}", ends);
        let top = loudest(&drums.days, ends);
        let yesterday = &drums.days[1];
        let points = line_points(yesterday, ends, top, 100.0, 800.0, 50.0, 30.0);
        let tallest = points.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        // Twenty minutes, by its logarithm between eight hours and two.
        let want = 100.0 + 800.0 * ((ends.0 / 1200.0).ln() / (ends.0 / 120.0).ln()) as f32;
        assert!((tallest.0 - want).abs() < 6.0, "tallest at {}, twenty minutes is at {}", tallest.0, want);
        assert!(points.iter().all(|p| (100.0..=900.0).contains(&p.0)), "a point off the paper");
        assert!(points.iter().all(|p| (20.0..=50.0).contains(&p.1)), "a point out of its reach");
        assert!(points.windows(2).all(|w| w[0].0 < w[1].0), "the longest period is at the left");
    }

    #[test]
    fn a_paper_is_well_formed_and_stays_on_its_ground() {
        let dir = tmp("svg");
        let now = clock::day_start(1_790_000_000.0) + 9.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 12.0 * 86400.0, now, 0.7,
                       &[(3600.0, 0.2)], &[], 8);
        let drums = gather(&dir, now);
        for (rows, paper) in [(&drums.days, Paper::Day), (&drums.months, Paper::Month)] {
            let svg = long_svg(rows, paper);
            assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1000 "));
            assert!(svg.ends_with("</svg>"));
            assert_eq!(unclosed(&svg), 0, "the picture closes what it opens");
            let high = long_height(paper, rows.len());
            let numbers = path_numbers(&svg);
            assert!(numbers.len() > 100);
            assert!(numbers.iter().all(|n| n.is_finite() && *n >= 0.0 && *n <= WIDE),
                    "a coordinate off the paper");
            assert!(svg.matches("<title>").count() == rows.len(), "a line says which it is");
            assert!(high > HEAD + FOOT);
        }
        // Today's nine hours reach three hours and no further: its line
        // begins to the right of where a whole day's does.
        let ends = long_ends(Paper::Day, &drums.days);
        let top = loudest(&drums.days, ends);
        let first = |r: &LongRow| line_points(r, ends, top, 0.0, 1000.0, 0.0, 1.0)[0].0;
        assert!(!drums.days[0].whole);
        assert!(first(&drums.days[0]) > first(&drums.days[1]) + 100.0);
        // An hourly fifth is over what luck reaches, and the pen says so.
        let svg = long_svg(&drums.days, Paper::Day);
        let hot = (shade_of(LUCK, LUCK)..SHADES).any(|s| svg.contains(&format!("class=\"h{}\"", s)));
        assert!(hot);
    }

    #[test]
    fn the_papers_end_where_the_record_ends() {
        let dir = tmp("stale");
        let end = clock::day_start(1_790_000_000.0) + 5.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", end - 4.0 * 86400.0, end, 0.7, &[], &[], 2);
        sample::temps(&dir, "AAAAAAAAAAAAAA", end - 4.0 * 86400.0, end, 21.0, 2.0, &[], 3);
        // The page is written three weeks after the last row.
        let drums = gather(&dir, end + 21.0 * 86400.0);
        assert!(drums.as_of <= end + 1.0 && drums.as_of > end - 60.0);
        assert_eq!(drums.days[0].start, clock::day_start(end));
        assert!(!drums.days[0].whole);
        let (_, drum, _) = &drums.temps[0];
        assert_eq!(drum.lines[0].day, clock::day_start(end));
        assert!(drum.lines[0].mean.is_some(), "the foot of the drum is a day with readings");
    }

    #[test]
    fn ink_joins_what_carries_on_and_lifts_the_pen_where_it_does_not() {
        let mut ink = Ink::new();
        ink.stroke(3, (1.0, 2.0), (3.04, 4.0));
        ink.stroke(3, (3.0, 4.0), (5.5, 6.25));
        ink.stroke(3, (9.0, 9.0), (10.0, 9.0));
        ink.stroke(40, (5.5, 6.25), (9.0, 9.0));
        assert_eq!(ink.paths[3], "M1 2L3 4L5.5 6.3M9 9L10 9");
        assert_eq!(ink.paths[40], "M5.5 6.3L9 9");
        let svg = ink.svg(0.5, 40);
        assert_eq!(svg.matches("<path").count(), 3, "two shades, and a glow under the hot one");
        assert_eq!(hex(SHADES - 1), "#fff5e6");
    }
}
