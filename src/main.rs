// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// radbeeper -- a GQ GMC Geiger-Muller counter on the desk. The read side,
// native: find it, ask what it is, and watch it.
//
// Everything that writes the log format -- service, backfill, export, site,
// random, hotplug -- is in the one-file Python program in the same repository
// and stays there while that format is still moving. Asking this binary for
// one of them says so rather than pretending.
mod counter;
mod serial;

use radbeeper::{analysis, broker, clock, drum, entropy, history, log, longdrum};
use radbeeper::export::bytes_text;
use analysis::{
    bar_rows, bar_rows_to, level, spectrum_columns, Ladder, Level, Windows,
};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BIG_ROWS: usize = 12;
const SERVICE_WAIT: f64 = 10.0;
/// Samples kept for the cascade strip. Eleven tiers of doubling reach back
/// half a day across a wide terminal, and every tube sends a sample a
/// second: this is that half day for three of them. The rest is the
/// windows' business, not the strip's.
const STRIP_KEEP: usize = 131072;
/// How often a running service looks for a counter that was not there before.
///
/// ONE LOG CYCLE. Often enough that plugging a tube in and looking at the
/// screen feels like one action, rare enough that the scan -- an open and a
/// close on every candidate port -- costs nothing anybody can measure.
const RESCAN: f64 = log::DEFAULT_LOG_EVERY;
/// How often `--wait` looks at a busy port again.
///
/// Half a second, where the service takes ten. The service is waiting for a
/// person to finish watching and can afford to be slow about it; `--wait` is a
/// person at a second window who has just run the command that frees the port,
/// and a wait that visibly lags the handover reads as a wait that did not
/// work. Polling this fast is safe because an open that fails at the flock
/// never reaches termios and never writes a byte -- see serial::Serial::open,
/// where the lock is taken before configure -- so the holder cannot tell.
const WAIT_POLL: f64 = 0.5;

// A twelve-row digit, six columns wide, with the three horizontal bars drawn
// TWO rows thick -- a one-row bar between three-row uprights reads as a
// scratch once the digit is this tall.
fn glyph(ch: char) -> Option<&'static [&'static str]> {
    Some(match ch {
        '0' => &["111111","111111","100001","100001","100001","100001","100001","100001","100001","100001","111111","111111"],
        '1' => &["000110","001110","010110","000110","000110","000110","000110","000110","000110","000110","011111","011111"],
        '2' => &["111111","111111","000001","000001","000001","111111","111111","100000","100000","100000","111111","111111"],
        '3' => &["111111","111111","000001","000001","000001","111111","111111","000001","000001","000001","111111","111111"],
        '4' => &["100001","100001","100001","100001","100001","111111","111111","000001","000001","000001","000001","000001"],
        '5' => &["111111","111111","100000","100000","100000","111111","111111","000001","000001","000001","111111","111111"],
        '6' => &["111111","111111","100000","100000","100000","111111","111111","100001","100001","100001","111111","111111"],
        '7' => &["111111","111111","000001","000010","000010","000100","000100","001000","001000","010000","010000","010000"],
        '8' => &["111111","111111","100001","100001","100001","111111","111111","100001","100001","100001","111111","111111"],
        '9' => &["111111","111111","100001","100001","100001","111111","111111","000001","000001","000001","111111","111111"],
        '.' => &["00","00","00","00","00","00","00","00","00","00","11","11"],
        '-' => &["000000","000000","000000","000000","000000","111111","111111","000000","000000","000000","000000","000000"],
        _ => return None,
    })
}

/// The column the big digits start at: clear of the header on the same row.
///
/// Three spaces of air, because a digit butted against the serial number
/// reads as part of it.
fn digits_left(head: &str) -> usize {
    head.chars().count() + 3
}

/// The widest reading there is. `<GETCPM>>` answers in two bytes, so 65535
/// is the most this counter can ever say, and the big readout keeps room for
/// all five digits whether or not this second needs them.
const MAX_READOUT: &str = "65535";

/// How many columns the readout is allowed, and always takes.
fn readout_width() -> usize {
    big_number(MAX_READOUT)
        .iter()
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0)
}

/// What is drawing this screen, for a screen that outlives its terminal: a
/// recording, a screenshot pasted into somebody's issue. Out of the manifest,
/// so it cannot drift from the binary.
fn nameplate() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

/// Where the nameplate goes: hard against the right edge of the top row, in
/// the corner the readout does not reach -- and nowhere at all if it would
/// come within two columns of `busy_to`. The counter has priority; a version
/// string is worth nothing beside a number somebody is watching.
fn nameplate_left(width: usize, busy_to: usize, plate: usize) -> Option<usize> {
    let x = width.checked_sub(plate)?;
    (x > busy_to + 1).then_some(x)
}

fn big_number(text: &str) -> Vec<String> {
    let mut rows = vec![String::new(); BIG_ROWS];
    for ch in text.chars() {
        if let Some(g) = glyph(ch) {
            for r in 0..BIG_ROWS {
                for bit in g[r].chars() {
                    rows[r].push(if bit == '1' { '█' } else { ' ' });
                }
                rows[r].push(' ');
            }
        }
    }
    rows
}

// ---------------------------------------------------------------- screen ---
struct Screen {
    saved: libc::termios,
}

impl Screen {
    fn enter() -> Screen {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            libc::tcgetattr(0, &mut saved);
            let mut raw = saved;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &raw);
        }
        print!("\x1b[?1049h\x1b[?25l");
        let _ = std::io::stdout().flush();
        Screen { saved }
    }

    fn size(&self) -> (usize, usize) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            (ws.ws_row as usize, ws.ws_col as usize)
        } else {
            (24, 80)
        }
    }

    /// The keys pressed since the last frame, if any. The terminal is in
    /// raw mode and the read does not wait.
    fn keys(&self) -> Vec<u8> {
        let mut b = [0u8; 8];
        match std::io::stdin().read(&mut b) {
            Ok(n) if n > 0 => b[..n].to_vec(),
            _ => Vec::new(),
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        print!("\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved) };
    }
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

fn colour_for(l: Level) -> &'static str {
    match l {
        Level::Calm => GREEN,
        Level::Raised => YELLOW,
        Level::High => RED,
    }
}

fn at(row: usize, col: usize) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

// ----------------------------------------------------------------- paper ---
/// How many colours the terminal has said it has.
///
/// ASKED OF THE ENVIRONMENT, BECAUSE THERE IS NOTHING ELSE TO ASK. A
/// terminal that draws sixteen million colours says so in COLORTERM, and
/// one that does not say so may print a 24-bit escape as noise or as the
/// wrong colour. The drum's pen is a gradient of sixty-four shades, so it
/// is drawn in the exact colours where they are promised, in the nearest
/// of the 256 where TERM promises those, and in the eight every terminal
/// has otherwise -- where it is still green, yellow, red and white in the
/// right places.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Depth {
    True,
    Cube,
    Eight,
}

fn depth_of(colorterm: Option<&str>, term: Option<&str>) -> Depth {
    let said = colorterm.unwrap_or("").to_ascii_lowercase();
    let term = term.unwrap_or("");
    if said.contains("truecolor") || said.contains("24bit") || term.ends_with("-direct") {
        Depth::True
    } else if term.contains("256color") {
        Depth::Cube
    } else {
        Depth::Eight
    }
}

/// The nearest of the 256: the grey ramp for what is grey, the six-step
/// cube for the rest.
fn cube_index(rgb: [u8; 3]) -> u8 {
    const STEPS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let [r, g, b] = rgb.map(|v| v as i32);
    if (r - g).abs() < 10 && (g - b).abs() < 10 && (r - b).abs() < 10 {
        let v = (r + g + b) / 3;
        if v < 4 {
            return 16;
        }
        if v > 243 {
            return 231;
        }
        return (232 + ((v - 8).max(0) + 5) / 10).min(255) as u8;
    }
    let near = |v: i32| -> u8 {
        (0..6).min_by_key(|k| (STEPS[*k] - v).abs()).unwrap_or(0) as u8
    };
    16 + 36 * near(r) + 6 * near(g) + near(b)
}

/// One of the eight, for a terminal that has promised no more: by which
/// of red, green and blue leads, dim where there is little of any.
fn basic_ink(rgb: [u8; 3]) -> String {
    let [r, g, b] = rgb.map(|v| v as f32 / 255.0);
    let most = r.max(g).max(b);
    let hue = if r > 0.8 && g > 0.8 && b > 0.7 {
        "37"
    } else if r >= g {
        if g > 0.7 * r { "33" } else { "31" }
    } else if b > 0.75 * g {
        "36"
    } else {
        "32"
    };
    let weight = if most < 0.45 { "2;" } else if hue == "37" { "1;" } else { "" };
    format!("\x1b[{}{}m", weight, hue)
}

/// The escape that makes `rgb` the colour written in.
fn ink(depth: Depth, rgb: [u8; 3]) -> String {
    match depth {
        Depth::True => format!("\x1b[38;2;{};{};{}m", rgb[0], rgb[1], rgb[2]),
        Depth::Cube => format!("\x1b[38;5;{}m", cube_index(rgb)),
        Depth::Eight => basic_ink(rgb),
    }
}

/// The escape that makes `rgb` the ground written on. Nothing in eight
/// colours: a ground that is one of eight is a slab, and the terminal's
/// own is better.
fn ground_ink(depth: Depth, rgb: [u8; 3]) -> String {
    match depth {
        Depth::True => format!("\x1b[48;2;{};{};{}m", rgb[0], rgb[1], rgb[2]),
        Depth::Cube => format!("\x1b[48;5;{}m", cube_index(rgb)),
        Depth::Eight => String::new(),
    }
}

fn bytes_of(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// `ink` at `strength` over `ground`: what a transparent pen leaves.
fn mixed(ground: [f32; 3], ink: [f32; 3], strength: f32) -> [u8; 3] {
    let s = strength.clamp(0.0, 1.0);
    bytes_of([
        ground[0] + (ink[0] - ground[0]) * s,
        ground[1] + (ink[1] - ground[1]) * s,
        ground[2] + (ink[2] - ground[2]) * s,
    ])
}

/// One dot of ink: its colour, how much it matters, and whether the pen
/// was a heavy one.
///
/// A CHARACTER HAS ONE COLOUR AND EIGHT DOTS, so where two lines cross in
/// one character one of them lends the other its colour. `rank` says
/// which: the hotter of two rows of the drum, the newer of two days of
/// temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Dot {
    rgb: [u8; 3],
    rank: u8,
    heavy: bool,
}

/// A sheet of Braille: two dots across a character and four down it.
///
/// WHY BRAILLE. The drum is lines that rise from their own baselines and
/// run into the lines above them, and a line is not a bar: the eighth
/// blocks the rest of this screen is made of can only stand on the foot
/// of a character, so two rows of them cannot overlap. Braille puts a dot
/// anywhere on a grid four times as fine down the screen and twice across
/// it, and a line is a run of dots.
struct Paper {
    cols: usize,
    rows: usize,
    dots: Vec<Option<Dot>>,
}

/// Where a line is in one column of dots: how far down the paper, in what
/// ink, and how many dots wide the pen.
type Mark = Option<(i32, Dot, i32)>;

impl Paper {
    fn new(cols: usize, rows: usize) -> Paper {
        Paper { cols, rows, dots: vec![None; cols * 2 * rows * 4] }
    }

    fn width(&self) -> usize {
        self.cols * 2
    }

    fn height(&self) -> usize {
        self.rows * 4
    }

    fn place(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.width() && (y as usize) < self.height())
            .then(|| y as usize * self.width() + x as usize)
    }

    fn set(&mut self, x: i32, y: i32, dot: Dot) {
        if let Some(i) = self.place(x, y) {
            self.dots[i] = Some(dot);
        }
    }

    fn clear(&mut self, x: i32, y: i32) {
        if let Some(i) = self.place(x, y) {
            self.dots[i] = None;
        }
    }

    fn get(&self, x: i32, y: i32) -> Option<Dot> {
        self.place(x, y).and_then(|i| self.dots[i])
    }

    /// One line across the paper, a mark for each column of dots from
    /// `left`.
    ///
    /// JOINED UP AND DOWN, so a line that climbs six dots between two
    /// columns is a line and not two dots: the column is inked from its
    /// own height to one short of its neighbour's.
    ///
    /// AND WHAT IS BEHIND IT IS RUBBED OUT, when `hide_to` gives the line
    /// its own baseline: every dot from the line down to there is cleared
    /// first. A row of the drum rises over the rows above its own, and a
    /// row in front hides what is behind it, which is all the depth the
    /// paper has. The window paints its ground back under each trace for
    /// the same reason.
    fn trace(&mut self, marks: &[Mark], left: i32, hide_to: Option<i32>) {
        let mut before: Option<i32> = None;
        for (c, mark) in marks.iter().enumerate() {
            let Some((y, dot, wide)) = *mark else {
                before = None;
                continue;
            };
            let x = left + c as i32;
            let (mut top, mut foot) = (y, y);
            if let Some(p) = before {
                if p > y + 1 {
                    foot = p - 1;
                } else if p < y - 1 {
                    top = p + 1;
                }
            }
            top -= (wide - 1).max(0);
            if let Some(base) = hide_to {
                for yy in top..=base.max(foot) {
                    self.clear(x, yy);
                }
            }
            for yy in top..=foot {
                self.set(x, yy, dot);
            }
            before = Some(y);
        }
    }

    /// The character at one place, and the ink it is written in: the
    /// dots that are set, and the colour of the one that matters most.
    fn cell(&self, cx: usize, cy: usize) -> Option<(char, Dot)> {
        // Braille numbers its dots down the left column and then down the
        // right, with the fourth row added later and so out of order.
        const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        let mut bits = 0u32;
        let mut best: Option<Dot> = None;
        for (dx, column) in BITS.iter().enumerate() {
            for (dy, bit) in column.iter().enumerate() {
                let Some(d) = self.get((cx * 2 + dx) as i32, (cy * 4 + dy) as i32) else {
                    continue;
                };
                bits |= bit;
                let heavy = d.heavy || best.map_or(false, |b| b.heavy);
                let lead = match best {
                    Some(b) if b.rank >= d.rank => b,
                    _ => d,
                };
                best = Some(Dot { heavy, ..lead });
            }
        }
        best.map(|d| (char::from_u32(0x2800 + bits).unwrap_or(' '), d))
    }

    /// Write the sheet with its top left corner at (`top`, `left`), on
    /// `ground` where the terminal can colour one.
    fn render(&self, out: &mut String, top: usize, left: usize, depth: Depth,
              ground: Option<[u8; 3]>) {
        let under = ground.map(|g| ground_ink(depth, g)).unwrap_or_default();
        for cy in 0..self.rows {
            out.push_str(&at(top + cy, left));
            out.push_str(&under);
            let mut pen: Option<([u8; 3], bool)> = None;
            for cx in 0..self.cols {
                let Some((glyph, dot)) = self.cell(cx, cy) else {
                    out.push(' ');
                    continue;
                };
                if pen != Some((dot.rgb, dot.heavy)) {
                    // Bold off and on by its own codes, so the ground set
                    // at the head of the row stays under what follows.
                    out.push_str(if dot.heavy { BOLD } else { "\x1b[22m" });
                    out.push_str(&ink(depth, dot.rgb));
                    pen = Some((dot.rgb, dot.heavy));
                }
                out.push(glyph);
            }
            out.push_str(OFF);
        }
    }
}

// ------------------------------------------------------------------ drum ---
/// Dots between one row of the drum and the next, and the most a row
/// rises from its own line.
///
/// TWO DOTS, the window's two pixels: at one, a flat row touches the row
/// over it everywhere and the paper is a solid block; at two there is
/// ground between them. THE REACH IS THE WINDOW'S TWENTY-FOUR where the
/// paper is tall enough for it, which is twelve rows' worth, so a peak
/// stands across a dozen lines as it does there. On a short paper it is
/// five eighths of the height, and the rows over that are cut at the top.
const DRUM_STEP: i32 = 2;
const DRUM_REACH: i32 = 24;

fn drum_reach(height: i32) -> i32 {
    (height * 5 / 8).clamp(3, DRUM_REACH)
}

/// The ground the drum is drawn on: nearly black, so a bright line is
/// bright against something. The window shades its panel half way to
/// black for the same reason; a terminal's own ground is not known, so
/// this one is given.
const DRUM_GROUND: [f32; 3] = [0.055, 0.065, 0.085];

/// How many dots wide the pen is for a shade: `drum::pen` in the
/// terminal's units. One for the floor, two from where luck reaches, and
/// three for what is half as much again -- the window's under a pixel to
/// three, on a grid that has no fractions.
fn pen_dots(shade: usize) -> i32 {
    let wide = drum::pen(shade);
    if wide < 2.0 {
        1
    } else if wide < 2.3 {
        2
    } else {
        3
    }
}

/// One row of the drum as marks: (period, power) points put on the axis,
/// joined by straight lines, a mark for every column of dots between the
/// first point and the last.
///
/// THE WINDOW'S OWN ARITHMETIC, IN DOTS. Across by `drum::place`; up by
/// `analysis::gain` against the loudest thing on the paper; the shade of a
/// stretch between two points is the louder of the two, by
/// `drum::shade_of` against the luck line; the colour is `drum::heat` at
/// the row's strength over the ground.
fn row_marks(bins: &[(f32, f32)], axis: &[drum::Stretch], width: usize,
             loudest: f64, reach: i32, line: i32, luck: f32, strength: f32) -> Vec<Mark> {
    let rise = |power: f32| analysis::gain(power as f64, loudest) as f32;
    row_marks_by(bins, axis, width, &rise, reach, line, luck, strength)
}

/// `row_marks` with the height given from outside: `rise` is how far up
/// the reach a power stands, from nought to one.
fn row_marks_by(bins: &[(f32, f32)], axis: &[drum::Stretch], width: usize,
                rise: &dyn Fn(f32) -> f32, reach: i32, line: i32, luck: f32,
                strength: f32) -> Vec<Mark> {
    use drum::{heat, place, shade_of};
    // The loudest point in each column: at the long end of a narrow paper
    // two bins can share one.
    let mut knots: Vec<Option<f32>> = vec![None; width];
    for (period, power) in bins {
        let Some(x) = place(axis, *period as f64) else { continue };
        let c = ((x * (width.max(1) - 1) as f32).round() as usize).min(width.max(1) - 1);
        if knots[c].map_or(true, |p| *power > p) {
            knots[c] = Some(*power);
        }
    }
    let at: Vec<usize> = (0..width).filter(|c| knots[*c].is_some()).collect();
    let mut marks: Vec<Mark> = vec![None; width];
    let mark = |rise: f32, power: f32| -> Mark {
        let shade = shade_of(power, luck);
        let dot = Dot {
            rgb: mixed(DRUM_GROUND, heat(shade), strength),
            rank: shade as u8,
            heavy: shade >= shade_of(luck, luck),
        };
        Some(((line - (rise * reach as f32).round() as i32).max(0), dot, pen_dots(shade)))
    };
    for pair in at.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (pa, pb) = (knots[a].unwrap_or(0.0), knots[b].unwrap_or(0.0));
        for c in a + 1..b {
            let t = (c - a) as f32 / (b - a) as f32;
            marks[c] = mark(rise(pa) + (rise(pb) - rise(pa)) * t, pa.max(pb));
        }
    }
    for c in at {
        let p = knots[c].unwrap_or(0.0);
        marks[c] = mark(rise(p), p);
    }
    marks
}

/// What the drum is drawn from.
struct Drum<'a> {
    /// The trail's rows, newest first.
    rows: &'a [analysis::TrailRow],
    axis: &'a [drum::Stretch],
    /// How tall the tallest bin of one row gets by luck.
    luck: f32,
    /// How far the paper has moved towards the next row, nought to one.
    slide: f32,
}

/// The drum spectrogram on a sheet of Braille: the window's trail, in
/// dots.
///
/// THE SAME PAPER. A row is a spectrum of the last 512 seconds, taken
/// every eight; the newest is at the foot and each older one is two dots
/// further up and away, moving up a dot at a time as the next one comes.
/// A row rises from its own line where there is more power, into the rows
/// above it, and hides what is behind it. The pen is hotter and wider with
/// power, and the whole row is as bright as the room counted.
///
/// THE HOURS-LONG AVERAGE IS NOT ON THIS SHEET. It was, on the last line of
/// dots, as the row in front of every other, and what it stood over it
/// hid: a black shape of the average with the newest rows -- the ones just
/// counted -- behind it. It has a strip of its own under this one now, on
/// the same axis and to the same scale: see `draw_average`.
///
/// WHAT IT DOES NOT HAVE. The window's glow under a line over the luck
/// line, and its dot on every peak: a dot is the whole of what a Braille
/// cell has to draw with. The one loudest peak on the paper is white, as
/// the window's ring is.
fn draw_drum(paper: &mut Paper, d: &Drum) {
    use radbeeper::drum;
    let (width, height) = (paper.width(), paper.height() as i32);
    if width < 2 || height < 1 {
        return;
    }
    let base = height - 1;
    let reach = drum_reach(height);
    let loudest = drum::loudest(d.rows, d.axis, d.luck);
    let busiest = d.rows.iter().map(|r| r.level).fold(2.0f32, f32::max) as f64;
    let slide = d.slide.clamp(0.0, 1.0);
    let moved = ((slide * DRUM_STEP as f32) as i32).min(DRUM_STEP - 1);
    // The loudest peak over the luck line, and where it was drawn.
    let mut extreme: Option<(f32, i32, i32)> = None;
    for (k, row) in d.rows.iter().enumerate().rev() {
        let line = base - k as i32 * DRUM_STEP - moved;
        if line < 0 {
            continue;
        }
        let away = (k as f32 + slide) / drum::FALL_DEPTH as f32;
        let strength = drum::strength(row.level, busiest, away);
        let marks = row_marks(&row.bins, d.axis, width, loudest, reach, line, d.luck, strength);
        paper.trace(&marks, 0, Some(line));
        for i in row.peaks(d.luck) {
            let (period, power) = row.bins[i];
            let Some(x) = drum::place(d.axis, period as f64) else { continue };
            let c = (x * (width - 1) as f32).round() as usize;
            if let Some(Some((y, _, _))) = marks.get(c) {
                if extreme.map_or(true, |(p, _, _)| power > p) {
                    extreme = Some((power, c as i32, *y));
                }
            }
        }
    }
    if let Some((_, x, y)) = extreme {
        // Only where it is still in view: a row in front may stand over it.
        if paper.get(x, y).is_some() {
            paper.set(x, y, Dot { rgb: [255, 255, 255], rank: u8::MAX, heavy: true });
        }
    }
}

/// The period axis under the drum, in words: the period at the left of
/// each stretch, the doublings under 512 seconds where there is room for
/// them, and the floor at the right.
///
/// THE AXIS IS CUT INTO STRETCHES and the labels are where the cuts are:
/// the long periods are squeezed into the left quarter, and a label that
/// was only at the two ends would leave the reader to assume one even
/// logarithm between them.
fn axis_words(axis: &[drum::Stretch], cols: usize) -> String {
    let mut labels = Labels::new(cols);
    for s in axis {
        let col = labels.col_of(s.left);
        labels.put(col, &analysis::span_words(s.long));
    }
    // What the figures are, said once, after the first of them that has
    // the room: the long stretches are too narrow for another word.
    for s in axis {
        let after = labels.col_of(s.left) + analysis::span_words(s.long).chars().count() + 1;
        if labels.put(after, "period") {
            break;
        }
    }
    if let Some(last) = axis.last() {
        let floor = analysis::span_words(last.short);
        labels.put(cols.saturating_sub(floor.chars().count()), &floor);
        let mut period = last.long / 2.0;
        while period > last.short {
            if let Some(x) = drum::place(axis, period) {
                let col = labels.col_of(x);
                labels.put(col, &analysis::span_words(period));
            }
            period /= 2.0;
        }
    }
    labels.words()
}

/// The hours-long average on a strip of its own, under the live drum: one
/// line, from `drum::front_row`, across the same axis, to the paper's own
/// top and in the same pen, so a ridge in the rows stands over its own
/// peak here and is the same colour.
///
/// IT RISES THE HEIGHT OF ITS STRIP AND NO FURTHER, which on one row of
/// characters is three dots. That is coarse for a height and enough for a
/// colour: what is flat is a dot up and green, what luck reaches is two,
/// and a line that is really there is at the top and hot.
fn draw_average(paper: &mut Paper, front: &[(f32, f32)], axis: &[drum::Stretch],
                loudest: f64, luck: f32) {
    let (width, height) = (paper.width(), paper.height() as i32);
    if width < 2 || height < 1 {
        return;
    }
    let reach = (height - 1).max(1);
    let marks = row_marks(front, axis, width, loudest, reach, height - 1, luck, 1.0);
    paper.trace(&marks, 0, None);
}

// ------------------------------------------------------------ long drums ---
/// Lines asked of each long paper, which is more than any screen draws, and
/// how often they are asked for again while a paper is on show. A line is
/// a day or a month: nothing about it changes in five minutes but the
/// unfinished one, and making them costs up to a second.
const DAY_LINES: usize = 31;
const MONTH_LINES: usize = 12;
const LONG_REREAD: f64 = 300.0;

/// A long paper's name, as its axis and its folded row say it.
fn long_name(which: longdrum::Paper) -> &'static str {
    match which {
        longdrum::Paper::Day => "day",
        longdrum::Paper::Month => "month",
    }
}

/// A long paper's axis: one even logarithm of the period, from the longest
/// period a finished line has, at the left, down to the shortest any line
/// has, at the right.
///
/// FROM THE LINES AND NOT FROM A TABLE. How long a period a line starts at
/// is the library's to decide -- a third of the line's own length, so that
/// a drift which came round once is not drawn as a rhythm -- and an axis
/// written down here would be wrong the day that changed. The line still
/// being counted reaches less far and is not asked, unless it is all there
/// is; with no line at all the ends are what a whole one would have.
fn long_axis(which: longdrum::Paper, rows: &[Option<longdrum::LongRow>]) -> [drum::Stretch; 1] {
    let ends = |whole_only: bool| -> Option<(f64, f64)> {
        let mut seen = rows.iter().flatten().filter(|r| r.whole || !whole_only).peekable();
        seen.peek()?;
        let (mut long, mut short) = (0.0f32, f32::MAX);
        for r in seen {
            long = long.max(r.bands.first().map_or(0.0, |b| b.0));
            short = short.min(r.bands.last().map_or(f32::MAX, |b| b.0));
        }
        (long > short && short > 0.0).then_some((long as f64, short as f64))
    };
    let span = match which {
        longdrum::Paper::Day => 86400.0,
        longdrum::Paper::Month => 31.0 * 86400.0,
    };
    // The short end from every line, the unfinished one too: they all stop
    // at the same four bins, and a hair's difference in how each was worked
    // out must not put a line's last band off the paper.
    let short = ends(false).map(|e| e.1);
    let (long, short) = match (ends(true).or_else(|| ends(false)), short) {
        (Some((long, _)), Some(short)) => (long, short),
        _ => (span / longdrum::SLOWEST as f64, 4.0 * which.bin()),
    };
    [drum::Stretch { long, short, left: 0.0, right: 1.0 }]
}

/// The periods a long paper's axis may name between its two ends: the
/// ones a person asks about.
const LONG_TICKS: [f64; 11] = [
    14.0 * 86400.0, 7.0 * 86400.0, 2.0 * 86400.0, 86400.0, 43200.0, 21600.0, 10800.0, 3600.0,
    1200.0, 300.0, 120.0,
];

/// When each line of a paper began, newest first, for as many lines as
/// `rows` has: worked from a line that is there, so the dates are the
/// lines' own even when the day has turned since they were read.
fn long_starts(which: longdrum::Paper, rows: &[Option<longdrum::LongRow>], now: f64) -> Vec<f64> {
    let mut starts = vec![0.0; rows.len()];
    let (at, start) = rows
        .iter()
        .enumerate()
        .find_map(|(k, r)| r.as_ref().map(|r| (k, r.start)))
        .unwrap_or((0, which.start(now)));
    let mut t = start;
    for k in at..rows.len() {
        starts[k] = t;
        t = which.before(t);
    }
    let mut t = start;
    for k in (0..at).rev() {
        t = which.next(t);
        starts[k] = t;
    }
    starts
}

/// The small dates down the left of a long paper of `deep` rows of
/// characters: (the row, from the top; the date of the line whose own
/// baseline is at that row's foot). Every row of a paper of three rows or
/// fewer, and every other row of a taller one, from the foot up, so that
/// weeks back can be read off without counting lines.
fn long_date_labels(which: longdrum::Paper, starts: &[f64], deep: usize) -> Vec<(usize, String)> {
    let every = if deep <= 3 { 1 } else { 2 };
    (0..deep)
        .step_by(every)
        .filter_map(|up| {
            let start = starts.get(up * 2)?;
            // Noon, or the middle of the month: the span's own date
            // whatever the clocks did at its midnight.
            let fmt = if which == longdrum::Paper::Day { "%d %b" } else { "%b %Y" };
            Some((deep - 1 - up, clock::format(start + 43200.0, fmt)))
        })
        .collect()
}

/// How tall the tallest band of one long line gets by luck: the long
/// papers' luck line, as `Trail::chance_max` is the live drum's.
fn long_luck() -> f32 {
    analysis::chance_max_of(longdrum::BANDS, 1.0) as f32
}

/// A period in the fewest characters that say it: `31d`, `24h`, `20m`.
fn long_words(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as u64;
    if s >= 2 * 86400 || (s >= 86400 && s % 86400 == 0) {
        format!("{}d", (s as f64 / 86400.0).round() as u64)
    } else if s >= 3600 {
        format!("{}h", (s as f64 / 3600.0).round() as u64)
    } else if s >= 60 {
        format!("{}m", (s as f64 / 60.0).round() as u64)
    } else {
        format!("{}s", s)
    }
}

/// How many lines of a paper there are to show: down to the oldest with a
/// record. The empty tracks between are lines; the ones before the record
/// began are not.
///
/// NONE UNTIL ONE IS FINISHED. A paper comes onto the screen when it has a
/// whole line, and the unfinished one is then drawn with it. On its own the
/// unfinished line is a few hours standing for a month: all it has at its
/// long end is the drift of those hours, and it drew that as a wall.
fn long_lines(rows: &[Option<longdrum::LongRow>]) -> usize {
    if !rows.iter().flatten().any(|r| r.whole) {
        return 0;
    }
    rows.iter().rposition(|r| r.is_some()).map_or(0, |k| k + 1)
}

/// The date of a line, as the folded row says it: `16 Nov`, `Nov 2026`.
fn long_date(which: longdrum::Paper, start: f64) -> String {
    let fmt = if which == longdrum::Paper::Day { "%d %b" } else { "%b %Y" };
    clock::format(start + 43200.0, fmt)
}

/// A long paper on a sheet of Braille: the newest line at the foot -- the
/// one still being counted, drawn over only the periods it has had time
/// for -- and each older one two dots further up, as the live drum's rows
/// are, by the same pen. A span with no record is an empty track: its two
/// dots are left as ground, so the paper does not close up over a break.
///
/// A LINE IS INKED AS STRONGLY AS IT WAS COUNTED: one made from half its
/// day is fainter than one made from all of it, and all of them a little
/// fainter the further up.
///
/// A LINE KEEPS TO ITS OWN TRACK UNTIL IT HAS SOMETHING TO SAY. The live
/// drum's rows share sixty-three seconds in sixty-four with their
/// neighbours and lie over one another like a surface; two days share
/// nothing, and drawn by the live drum's heights their noise was a tangle
/// a dozen lines deep with the rhythms lost in it. So here what chance
/// reaches is one track's height and no more -- a day of nothing is a
/// line, coloured by the same pen -- and only what stands over the luck
/// line climbs out of its track, the loudest thing on the paper the whole
/// reach.
fn draw_long(paper: &mut Paper, rows: &[Option<longdrum::LongRow>], axis: &[drum::Stretch],
             luck: f32) {
    let (width, height) = (paper.width(), paper.height() as i32);
    if width < 2 || height < 1 {
        return;
    }
    let (base, reach) = (height - 1, drum_reach(height));
    let loudest = rows
        .iter()
        .flatten()
        .flat_map(|r| r.bands.iter())
        .filter(|(period, _)| drum::place(axis, *period as f64).is_some())
        .map(|(_, power)| *power)
        .fold(luck, f32::max) as f64;
    let track = (DRUM_STEP as f32 / reach as f32).min(1.0);
    let rise = |power: f32| -> f32 {
        if power <= luck {
            track * power / luck
        } else {
            track + (1.0 - track) * analysis::gain((power - luck) as f64, loudest - luck as f64) as f32
        }
    };
    for (k, row) in rows.iter().enumerate().rev() {
        let Some(row) = row else { continue };
        let line = base - k as i32 * DRUM_STEP;
        if line < 0 {
            continue;
        }
        let away = k as f32 / rows.len().max(1) as f32;
        let strength = (0.55 + 0.45 * row.covered.clamp(0.0, 1.0)) * (1.0 - 0.35 * away);
        let marks = row_marks_by(&row.bands, axis, width, &rise, reach, line, luck, strength);
        paper.trace(&marks, 0, Some(line));
    }
}

/// A row of labels: each goes where it is asked for or nowhere -- never
/// over another, nor against one, nor past the edge.
struct Labels {
    line: Vec<char>,
}

impl Labels {
    fn new(cols: usize) -> Labels {
        Labels { line: vec![' '; cols] }
    }

    fn put(&mut self, col: usize, text: &str) -> bool {
        let (n, cols) = (text.chars().count(), self.line.len());
        if n == 0 || col + n > cols {
            return false;
        }
        let from = col.saturating_sub(1);
        let to = (col + n + 1).min(cols);
        if self.line[from..to].iter().any(|c| *c != ' ') {
            return false;
        }
        for (i, ch) in text.chars().enumerate() {
            self.line[col + i] = ch;
        }
        true
    }

    /// The column of characters a place across the paper is in.
    fn col_of(&self, x: f32) -> usize {
        let cols = self.line.len();
        ((x * (cols * 2).saturating_sub(1) as f32) as usize / 2).min(cols.saturating_sub(1))
    }

    fn words(self) -> String {
        self.line.into_iter().collect::<String>().trim_end().to_string()
    }
}

/// The row under a long paper: its name and the period at its left edge,
/// the periods between, and the shortest at the right.
/// `day 8h       6h      3h         1h       20m        5m        2m`.
fn long_axis_words(which: longdrum::Paper, axis: &[drum::Stretch; 1], cols: usize) -> String {
    let mut labels = Labels::new(cols);
    let head = format!("{} {}", long_name(which), long_words(axis[0].long));
    if !labels.put(0, &head) {
        labels.put(0, long_name(which));
    }
    let floor = long_words(axis[0].short);
    labels.put(cols.saturating_sub(floor.chars().count()), &floor);
    for period in LONG_TICKS {
        // Not the ends over again, nor a tick against one.
        if period > axis[0].long / 1.2 || period < axis[0].short * 1.2 {
            continue;
        }
        if let Some(x) = drum::place(axis, period) {
            let col = labels.col_of(x);
            labels.put(col, &long_words(period));
        }
    }
    labels.words()
}

/// A long paper folded to one row: its name, the date of its newest line,
/// that line as a strip of blocks across the paper's own axis -- taller
/// and hotter where there is more power, by the drum's own pen -- and the
/// loudest period in it, named, with how many times chance it stands.
///
/// STILL TRUE, AND STILL THE NEWEST LINE. Folded is for a screen with no
/// rows to give a paper, not for a paper with nothing to say: a rhythm
/// that is in today's counts is a tall hot block here and is named.
fn folded_words(which: longdrum::Paper, rows: &[Option<longdrum::LongRow>], cols: usize,
                depth: Depth) -> String {
    let Some(row) = rows.iter().flatten().next() else {
        return String::new();
    };
    let luck = long_luck();
    let axis = long_axis(which, rows);
    let head = format!("{} {} ", long_name(which), long_date(which, row.start));
    let top = row.bands.iter().fold((0.0f32, 0.0f32), |a, b| if b.1 > a.1 { *b } else { a });
    let tail = if top.1 >= luck {
        format!(" {} \u{d7}{:.0}", long_words(top.0 as f64), top.1)
    } else {
        " flat".to_string()
    };
    let room = cols.saturating_sub(head.chars().count() + tail.chars().count());
    let mut cells: Vec<f32> = vec![0.0; room];
    for (period, power) in &row.bands {
        let Some(x) = drum::place(&axis, *period as f64) else { continue };
        if room > 0 {
            let c = ((x * room as f32) as usize).min(room - 1);
            cells[c] = cells[c].max(*power);
        }
    }
    let scale = top.1.max(luck) as f64;
    let mut out = format!("{}{}{}", DIM, head, OFF);
    for power in cells {
        if power <= 0.0 {
            out.push(' ');
            continue;
        }
        let eighths = ((analysis::gain(power as f64, scale) * 8.0).ceil() as usize).clamp(1, 8);
        out.push_str(&ink(depth, bytes_of(drum::heat(drum::shade_of(power, luck)))));
        out.push(analysis::SPARK[eighths]);
    }
    out.push_str(&format!("{}{}{}{}", OFF, DIM, tail, OFF));
    out
}

/// The text rows each paper has in the drum's room, top to bottom: the
/// month paper, the day paper, the live drum and the average's strip. A
/// paper that is drawn has a row of labels under it as well, and a folded
/// one is that one row; the live drum's labels are outside the room, where
/// they were.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rooms {
    month: usize,
    month_folded: bool,
    day: usize,
    day_folded: bool,
    live: usize,
    average: usize,
}

#[cfg(test)]
impl Rooms {
    fn rows(&self) -> usize {
        let labels = (self.month > 0) as usize + (self.day > 0) as usize;
        let folded = self.month_folded as usize + self.day_folded as usize;
        self.month + self.day + self.live + self.average + labels + folded
    }
}

/// Share `rows` text rows among the papers. `day` and `month` are how each
/// is asked for and how many lines it has to show.
///
/// THE LINES ARE SHARED BY `longdrum::share`, two lines to a row of
/// characters as the live drum's are, so the terminal and the window give
/// way to one another by one rule: the live drum is never squeezed out, the
/// day paper takes up to a third, the month paper less. What is the
/// terminal's own is the rows that are not lines: the average's strip,
/// two rows of it on a tall paper; a row of labels under each paper drawn;
/// and one row for each paper folded. A paper with no line to show, or no
/// room for one, has no rows at all, and what it would have had is the
/// live drum's.
fn drum_rooms(rows: usize, day: (View, usize), month: (View, usize)) -> Rooms {
    let mut rooms = Rooms { average: if rows >= 16 { 2 } else { (rows >= 2) as usize }, ..Rooms::default() };
    let mut left = rows - rooms.average;
    // Folded first, the day before the month: a row each while the live
    // drum would still have two.
    if day.0 == View::Folded && day.1 > 0 && left >= 3 {
        rooms.day_folded = true;
        left -= 1;
    }
    if month.0 == View::Folded && month.1 > 0 && left >= 3 {
        rooms.month_folded = true;
        left -= 1;
    }
    let mut days = if day.0 == View::Shown { day.1 } else { 0 };
    let mut months = if month.0 == View::Shown { month.1 } else { 0 };
    loop {
        let labels = (days > 0) as usize + (months > 0) as usize;
        if labels > 0 && left < labels + 2 {
            // No room for a paper, its labels and a live drum: the month
            // goes first.
            if months > 0 { months = 0 } else { days = 0 }
            continue;
        }
        let paper = left - labels;
        let (_, d, m) = longdrum::share(paper * 2, paper * 2, days, months);
        if days > 0 && d == 0 {
            days = 0;
            continue;
        }
        if months > 0 && m == 0 {
            months = 0;
            continue;
        }
        rooms.day = (d + 1) / 2;
        rooms.month = (m + 1) / 2;
        rooms.live = paper - rooms.day - rooms.month;
        return rooms;
    }
}

// --------------------------------------------------------------- prefill ---
/// How far back the count log is read to fill what the replay did not
/// reach -- eight hours, which is as far as the counts' strip goes on a
/// wide screen, or the longest average if that is longer -- and the two
/// gaps it will close: between the log's last row and the first sample,
/// and between one row and the next.
///
/// TEN MINUTES AT THE JOIN, because that is a service being restarted, and
/// the hours before it are still the room's. Longer than that and the log
/// is of another sitting; nothing is filled and the averages fill as they
/// always did. NINETY SECONDS INSIDE, which is a row or two late; at a
/// longer hole the filling stops, and what is older is left out.
const PREFILL_REACH: f64 = 8.0 * 3600.0;
const PREFILL_JOIN: f64 = 600.0;
const PREFILL_HOLE: f64 = 90.0;

/// One counter's rows of the count log that could matter to a monitor
/// opening at `until`: (when the row was written, its counts, its
/// seconds), oldest first, from this month's file and, where the reach
/// goes back into it, last month's.
fn log_rows(dir: &Path, serial: &str, until: f64, reach: f64) -> Vec<(f64, u32, u32)> {
    let names = vec!["counts".to_string(), "seconds".to_string()];
    let mut paths = vec![log::path(until - reach, dir, Some(serial))];
    let this = log::path(until, dir, Some(serial));
    if paths[0] != this {
        paths.push(this);
    }
    let mut rows = Vec::new();
    for path in paths {
        for (when, cells) in log::read_table(&path, &names) {
            if when < until - reach - 3600.0 || when > until {
                continue;
            }
            if let (Ok(counts), Ok(seconds)) = (cells[0].parse::<u32>(), cells[1].parse::<u32>()) {
                if seconds > 0 {
                    rows.push((when, counts, seconds));
                }
            }
        }
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows
}

/// The seconds before `until`, from the count log: counts a second, oldest
/// first, the last of them the second before `until`; `reach` of them at
/// most. Empty where the log does not come up to `until`.
///
/// WHAT THE REPLAY DOES NOT REACH IS STILL ON THE DISK. A monitor that
/// attaches to a service which has just started is handed a few seconds,
/// and showed the 3000-second average as filling for fifty minutes and the
/// left of the counts as bare, with the hours in question in the log
/// beside it. A row of the log is a count and the seconds it was counted
/// over, so the rows are laid end to end back from the first sample and
/// each is spread over its seconds as evenly as whole counts allow.
///
/// SPREAD, NOT COUNTED, and so good only for what is coarser than a row. An
/// average over five minutes or more is the same number either way, and so
/// is a bar of 32 seconds. A bar of one second is not -- the log does not
/// know which second a count arrived in -- so the fine tiers of the strip
/// are left empty over these seconds (`blank_fine`), and they are never
/// given to the spectrum, the drum or the random pool.
///
/// A BREAK IS WHERE IT STOPS. The rows are taken back from `until` for as
/// long as each follows the one before; at a hole the filling ends, and
/// the screen starts there. The join itself is closed -- see PREFILL_JOIN.
fn prefill_seconds(rows: &[(f64, u32, u32)], until: f64, reach: f64) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    // Where the row after this one began; for the newest, where the
    // samples begin.
    let mut next_began = until;
    let mut allowed = PREFILL_JOIN;
    for &(when, counts, seconds) in rows.iter().rev() {
        if when > until {
            continue;
        }
        if next_began - when > allowed || out.len() as f64 >= reach {
            break;
        }
        // Newest second first, to be turned round at the end: the count up
        // to each second less the count up to the one before.
        let (c, n) = (counts as u64, seconds as u64);
        for i in (0..n).rev() {
            out.push(((i + 1) * c / n - i * c / n) as u32);
        }
        next_began = when - seconds as f64;
        allowed = PREFILL_HOLE;
    }
    out.reverse();
    out
}

/// Empty the bars finer than a log row that stand on seconds which came
/// from the log: every bar of under 32 seconds that begins before
/// `prefilled`, in the strip's own count of seconds, whose newest is the
/// one before `end`. See `prefill_seconds`.
fn blank_fine(strip: &mut [analysis::Tier], end: i64, prefilled: i64) {
    let spans = analysis::bar_spans(strip, end);
    for (tier, spans) in strip.iter_mut().zip(&spans) {
        if tier.seconds >= 32.0 {
            continue;
        }
        for (value, (begins, _)) in tier.values.iter_mut().zip(spans) {
            if *begins < prefilled {
                *value = None;
            }
        }
    }
}

/// How many text rows the drum and the counts under it get, out of `room`
/// rows for the two of them: (drum, counts). None when there is not a row
/// for each, and then the screen is laid out as it was before there was a
/// drum.
///
/// THE COUNTS KEEP THEIR FIVE, THREE OR ONE, as they always have, and the
/// drum has the rest: it is the chart that is better for every row it is
/// given, where the counts say what they say in three.
fn drum_rows(room: usize) -> Option<(usize, usize)> {
    let counts = if room >= 15 { 5 } else if room >= 7 { 3 } else { 1 };
    (room > counts).then(|| (room - counts, counts))
}

// ----------------------------------------------------------- temperature ---
/// The mercury of a thermometer `height` characters tall, top first, on
/// the scale of `analysis::thermo_fraction`: 50 F at its foot and 100 F at
/// its top, in eighths of a character.
///
/// NEVER QUITE EMPTY. A room at 50 F or under would be a tube with nothing
/// in it, which reads as no reading; the foot keeps an eighth.
fn mercury(celsius: f64, height: usize) -> Vec<char> {
    let eighths = ((analysis::thermo_fraction(celsius) * (height * 8) as f64).round() as usize)
        .max(1);
    (0..height)
        .map(|row| {
            let under = (height - 1 - row) * 8;
            analysis::SPARK[eighths.saturating_sub(under).min(8)]
        })
        .collect()
}

/// A temperature in both scales, as the thermometer and the drum's
/// margins print it.
fn both_scales(celsius: f64) -> (String, String) {
    (format!("{:.1}\u{b0}F", analysis::fahrenheit(celsius)), format!("{:.1}\u{b0}C", celsius))
}

/// The colour of the mercury: cold, a room, warm, hot.
fn mercury_colour(celsius: f64) -> &'static str {
    let f = analysis::fahrenheit(celsius);
    if f < 60.0 {
        CYAN
    } else if f < 80.0 {
        GREEN
    } else if f < 90.0 {
        YELLOW
    } else {
        RED
    }
}

/// The column the thermometers start at: clear of the averages, whose
/// widest line ends at the thirty-eighth.
const THERMO_LEFT: usize = 42;

/// How wide the thermometers are: the scale's words, a tube and a space
/// for each reading, and the widest reading there is beside them.
fn thermometer_width(tubes: usize) -> usize {
    6 + 2 * tubes + 8
}

/// How long a temperature is the room's. The service asks every log
/// interval; one older than this is from a counter that has stopped
/// answering, and is drawn dim.
const TEMP_STALE: f64 = 600.0;

/// The thermometers: a tube for each counter that has given a
/// temperature, on one scale from 50 F to 100 F, and each reading beside
/// them in Fahrenheit and in Celsius.
///
/// SMALL, AND BESIDE THE AVERAGES, because the temperature is not what
/// this instrument is for: it is the room the tube is in, worth a glance.
/// The ten days of it are on a key: see `draw_temp_drum`.
fn draw_thermometer(out: &mut String, top: usize, left: usize, height: usize,
                    readings: &[(f64, bool)]) {
    if height < 2 || readings.is_empty() {
        return;
    }
    out.push_str(&format!(
        "{}{}{:>3}\u{b0}F{}{}{:>3}\u{b0}F{}",
        at(top, left), DIM, analysis::THERMO_HIGH_F as i64,
        at(top + height - 1, left), DIM, analysis::THERMO_LOW_F as i64, OFF
    ));
    for (k, (celsius, stale)) in readings.iter().enumerate() {
        let tint = mercury_colour(*celsius);
        for (row, glyph) in mercury(*celsius, height).into_iter().enumerate() {
            out.push_str(&at(top + row, left + 6 + 2 * k));
            if glyph == ' ' {
                out.push_str(&format!("{}\u{2502}{}", DIM, OFF));
            } else {
                out.push_str(&format!("{}{}{}{}", if *stale { DIM } else { "" }, tint, glyph, OFF));
            }
        }
        // Two rows a reading, from the middle of the tube down, so one
        // counter's is beside the middle of its own tube.
        let row = top + (height.saturating_sub(2 * readings.len())) / 2 + 2 * k;
        if row + 1 < top + height {
            let (f, c) = both_scales(*celsius);
            let x = left + 6 + 2 * readings.len();
            out.push_str(&format!(
                "{}{}{}{}{}{}{}{}{}",
                at(row, x), if *stale { DIM } else { BOLD }, tint, f, OFF,
                at(row + 1, x), DIM, c, OFF
            ));
        }
    }
}

/// Days on the temperature drum, and how often the log behind it is read
/// again while it is open. The live readings are added as they arrive, so
/// the reading again is only for what was written while this was not
/// looking.
const TEMP_DAYS: usize = 10;
const TEMP_REREAD: f64 = 300.0;

/// The columns the temperature drum keeps for each line's date, at the
/// left, and for its mean in both scales, at the right.
const TEMP_LEFT: usize = 7;
const TEMP_RIGHT: usize = 15;

/// The ink of one day's line: today's the brightest, yesterday's a colour
/// of its own, and the days before in two tints by turns, fainter the
/// further back, so two neighbours that cross can still be told apart.
fn day_ink(k: usize, days: usize) -> Dot {
    let rgb = match k {
        0 => [1.0, 0.90, 0.55],
        1 => [0.45, 0.85, 1.0],
        _ => {
            let tint: [f32; 3] = if k % 2 == 0 { [0.66, 0.68, 0.90] } else { [0.50, 0.78, 0.62] };
            let fade = 1.0 - 0.45 * (k - 2) as f32 / days.saturating_sub(2).max(1) as f32;
            tint.map(|v| v * fade)
        }
    };
    Dot { rgb: bytes_of(rgb), rank: (days - k.min(days)) as u8, heavy: k == 0 }
}

/// One day's line as marks, a cell to a column of dots: `pitch` dots to a
/// track, the line's own baseline in the middle of the track whose last
/// dot is `foot`.
fn day_marks(cells: &[Option<f32>], foot: i32, pitch: i32, height: i32, dot: Dot) -> Vec<Mark> {
    let middle = foot as f32 - (pitch as f32 - 1.0) / 2.0;
    cells
        .iter()
        .map(|cell| {
            cell.map(|offset| {
                let y = (middle - offset * pitch as f32 + 0.5).floor() as i32;
                (y.clamp(0, height - 1), dot, 1)
            })
        })
        .collect()
}

/// The temperature as a drum recorder lays it down, in `rows` text rows
/// from `top`: a line a day and twenty-four hours a line, today's at the
/// foot and yesterday's one track above it. Gives back the words for the
/// line under the screen: what one track is worth.
///
/// A TRACK IS AN ORDINARY DAY, by `analysis::temp_drum`, so a day like
/// most days stays in its own track and the lines can be told apart;
/// where one leaves its track and crosses its neighbour the room was not
/// as it usually is, which is what the paper is for. Nothing is hidden
/// behind anything here: a crossing is the news.
///
/// AS MANY DAYS AS THERE ARE ROWS FOR, up to ten: each line has its date
/// at the left and its mean at the right, and those are text, a row each.
/// On twenty-four rows that is nine days. A taller screen gives every
/// track more dots than a row has -- a track need not be a whole number of
/// rows, only its label does -- up to four rows' worth, and the line that
/// much more room to move in.
fn draw_temp_drum(out: &mut String, top: usize, rows: usize, width: usize,
                  samples: &[(f64, f64)], now: f64, depth: Depth) -> String {
    if rows < 2 || width < TEMP_LEFT + 12 {
        return String::new();
    }
    let days = TEMP_DAYS.min(rows - 1);
    let pitch = ((rows - 1) * 4 / days).clamp(4, 16);
    let deep = (days * pitch + 3) / 4;
    // The means at the right give way on a narrow screen; the dates stay.
    let right = if width >= TEMP_LEFT + TEMP_RIGHT + 24 { TEMP_RIGHT } else { 0 };
    let cols = width - 1 - TEMP_LEFT - right;
    let mut paper = Paper::new(cols, deep);
    let drum = analysis::temp_drum(samples, now, paper.width(), days);
    let (pitch, height) = (pitch as i32, paper.height() as i32);
    let foot_of = |k: usize| height - 1 - k as i32 * pitch;
    // Oldest first, so that where two lines share a character the newer
    // is drawn last; which of them colours it is the dot's rank.
    for (k, line) in drum.lines.iter().enumerate().rev() {
        let marks = day_marks(&line.cells, foot_of(k), pitch, height, day_ink(k, days));
        paper.trace(&marks, 0, None);
    }
    paper.render(out, top, TEMP_LEFT, depth, Some(bytes_of(DRUM_GROUND)));
    for (k, line) in drum.lines.iter().enumerate() {
        // The row the line's own baseline is in.
        let row = top + ((foot_of(k) - (pitch - 1) / 2).max(0) / 4) as usize;
        let tint = ink(depth, day_ink(k, days).rgb);
        // Noon, so the date is the day's whatever the clocks did at its
        // midnight.
        out.push_str(&format!(
            "{}{}{}{}{}",
            at(row, 0), if k == 0 { BOLD } else { "" }, tint,
            clock::format(line.day + 43200.0, "%b %d"), OFF
        ));
        if let (Some(mean), true) = (line.mean, right > 0) {
            let (f, c) = both_scales(mean);
            out.push_str(&format!(
                "{}{}{:>7}{} {}{:>6}{}",
                at(row, TEMP_LEFT + cols + 1), tint, f, OFF, DIM, c, OFF
            ));
        }
    }
    // The hours along the foot: every three where they fit, every six
    // where they do not.
    let foot = top + deep;
    let every = if cols >= 48 { 3 } else { 6 };
    out.push_str(&format!("{}{}hour", at(foot, 0), DIM));
    for hour in (0..24).step_by(every) {
        out.push_str(&format!("{}{:02}", at(foot, TEMP_LEFT + hour * cols / 24), hour));
    }
    out.push_str(OFF);
    if right > 0 {
        out.push_str(&format!("{}{}the day's mean{}", at(foot, TEMP_LEFT + cols + 1), DIM, OFF));
    }
    format!(
        "one track is {:.1}\u{b0}F ({:.1}\u{b0}C)   a line a day, today's at the foot",
        drum.span * 9.0 / 5.0, drum.span
    )
}

// ----------------------------------------------------------------- watch ---
/// How long the monitor waits on the feed before it looks at the keyboard,
/// and how long a feed may say nothing before it is taken to have gone.
const KEY_SLICE: Duration = Duration::from_millis(100);
const FEED_SILENCE: Duration = Duration::from_millis(2500);

/// Whether a long paper is drawn in full, folded to one row, or not at all:
/// what the `d` and `m` keys turn through.
#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    Shown,
    Folded,
    Off,
}

impl View {
    fn next(self) -> View {
        match self {
            View::Shown => View::Folded,
            View::Folded => View::Off,
            View::Off => View::Shown,
        }
    }

    fn word(self) -> &'static str {
        match self {
            View::Shown => "shown",
            View::Folded => "folded",
            View::Off => "off",
        }
    }

    /// What a paper is until a key says otherwise, on a screen `h` rows
    /// tall: both in full where there are the rows for three papers, the
    /// month folded on a middling screen, and on a small one the day folded
    /// to its row and the month off, so the live drum keeps its paper.
    fn at_first(h: usize) -> (View, View) {
        if h >= 36 {
            (View::Shown, View::Shown)
        } else if h >= 28 {
            (View::Shown, View::Folded)
        } else {
            (View::Folded, View::Off)
        }
    }
}

/// The window `cpm` reports, and the one the monitor puts in big digits.
const CPM_WINDOW: f64 = 30.0;

/// RadBeeper's own 30-second average, counted here rather than asked for.
///
/// `<GETCPM>>` WAS THE OBVIOUS THING AND IT ANSWERS A DIFFERENT QUESTION. The
/// device keeps an internal rolling SIXTY-second count, and that is a
/// different time constant from anything else this program prints: `watch`
/// puts the 30 s window in the big digits, and `cpm` answering with a 60 s
/// number meant the two disagreed by more than the noise, with neither of
/// them wrong. One number, one time constant, and it is the one the rest of
/// the program already uses.
///
/// THE COST IS THIRTY SECONDS AND IT IS NOT HIDDEN. A window shows nothing
/// until it is full -- the same promise the monitor makes, for the same
/// reason -- so this counts for the whole span before printing anything, and
/// says so on stderr while it waits. stdout carries the answer alone, so it
/// still pipes.
fn cpm_cmd(c: &counter::Counter, cpm_per_usvh: f64) -> i32 {
    let mut w = Windows::new(&[CPM_WINDOW]);
    eprintln!("counting for {}s...", CPM_WINDOW as i64);
    c.heartbeat(true);
    let start = Instant::now();
    while w.average(CPM_WINDOW).is_none() {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        w.add(start.elapsed().as_secs_f64(), counts);
    }
    c.heartbeat(false);
    match w.average(CPM_WINDOW) {
        Some(n) => {
            println!("{:.1} CPM   {:.3} uSv/h", n, n / cpm_per_usvh);
            0
        }
        None => {
            // int(), not round(): the Python prints int(w.elapsed()) and
            // the two messages have to be the same string.
            eprintln!("the counter stopped talking after {}s",
                      w.elapsed() as i64);
            1
        }
    }
}

/// What the monitor writes while it is open. `--no-log` is None, and then it
/// writes nothing at all.
struct WatchLog {
    dir: PathBuf,
    every: f64,
    backfill: Option<(usize, f64)>,
    /// index.html and random.html into `dir`, and how often while open.
    export_every: Option<f64>,
    /// Whether the raw seconds behind each emission are kept beside it.
    ///
    /// ON BY DEFAULT AND TURNED OFF BY A FLAG, because the file is the
    /// evidence for a claim the program makes on its own front page -- that
    /// sixty-four characters came out of decay -- and a default that dropped
    /// the evidence would leave the claim uncheckable unless somebody had
    /// thought to ask for it in advance. It costs about a byte a second.
    frames: bool,
}

/// Write the pages into the log directory, and say so in a few words.
///
/// WHEN, NOT HOW OFTEN. After the backfill, so the page carries the gap it
/// just filled; every `export_every` while open, so a page somebody has up
/// is at most that stale; and on quit, so the last thing the monitor saw is
/// on it. Not on every row: a month of log is tens of thousands of rows and
/// the page is rebuilt from all of them.
fn export_pages(dir: &Path, cpm_per_usvh: f64) -> String {
    match radbeeper::export::export(dir, &dir.join("index.html"), true, cpm_per_usvh,
                                    radbeeper::export::DEFAULT_TITLE, None,
                                    radbeeper::audit::DEFAULT_FRAME_BUDGET) {
        Ok(r) => format!("exported {} rows {}", r.rows,
                         clock::format(clock::now(), "%H:%M")),
        Err(e) => format!("NOT EXPORTED: {}", e),
    }
}

/// How many rows of log the table at the bottom gets on a screen `h` tall.
///
/// The charts come first. Below 36 rows they are drawn compact -- the counts
/// in three rows, the spectrum in one -- and everything under 24 rows goes to
/// the table; from 36 the charts are full size again and the table keeps six
/// rows, and every sixth row after that.
///
/// IT TOOK EVERY ROW AFTER THAT while the charts had a full size to be: five
/// rows of counts and five of spectrum, and nothing to do with a sixth. The
/// drums are better for every row they are given -- there are three papers
/// now, and a row is two days of the day paper -- so of six more rows of
/// screen five are the papers' and one is the table's.
fn table_lines(h: usize) -> usize {
    if h < 26 {
        0
    } else if h < 36 {
        h - 24
    } else {
        6 + (h - 36) / 6
    }
}

/// The table: the log's own header and its newest rows, aligned in columns
/// as wide as their widest cell and cut at the screen's edge, oldest at the
/// top so each new row pushes the rest up like a terminal scrolling.
///
/// The same cells the log is written with, so what is on screen is what is on
/// disk. Rows the flash filled in are dim; a field with no value is a dim
/// `-`, because a window that was not full yet is not a zero.
fn draw_table(out: &mut String, top: usize, lines: usize, width: usize,
              names: &[String], rows: &std::collections::VecDeque<Vec<String>>) {
    if lines == 0 {
        return;
    }
    let shown: Vec<&Vec<String>> = rows.iter().rev().take(lines - 1).rev().collect();
    let src = names.iter().position(|n| n == "src");
    let mut widths: Vec<usize> = names.iter().map(|n| n.len() + 1).collect();
    for r in &shown {
        for (i, cell) in r.iter().enumerate() {
            if let Some(wd) = widths.get_mut(i) {
                *wd = (*wd).max(cell.chars().count().max(1));
            }
        }
    }
    // As many columns as fit, two spaces apart, never wrapping.
    let mut fit = 0;
    let mut used = 0;
    for wd in &widths {
        if used + wd > width.saturating_sub(1) {
            break;
        }
        used += wd + 2;
        fit += 1;
    }
    let mut head = String::new();
    for (i, name) in names.iter().take(fit).enumerate() {
        let label = if i == 0 { format!("#{}", name) } else { name.clone() };
        head.push_str(&format!("{:<w$}  ", label, w = widths[i]));
    }
    let head = head.trim_end().to_string();
    out.push_str(&format!("{}{}{}{}", at(top, 0), DIM, head, OFF));
    for (k, r) in shown.iter().enumerate() {
        let flash = src.and_then(|i| r.get(i)).map(|v| v != log::SRC_LIVE).unwrap_or(false);
        out.push_str(&at(top + 1 + k, 0));
        if flash {
            out.push_str(DIM);
        }
        for i in 0..fit {
            let cell = r.get(i).map(String::as_str).unwrap_or("");
            if cell.is_empty() {
                out.push_str(&format!("{}{:<w$}{}  ", DIM, "-", if flash { "" } else { OFF },
                                      w = widths[i]));
            } else {
                out.push_str(&format!("{:<w$}  ", cell, w = widths[i]));
            }
        }
        out.push_str(OFF);
    }
}

/// Several counters, read at once.
///
/// A THREAD EACH, AND A CHANNEL OUT. Two tubes are two file descriptors that
/// go quiet independently, and reading them in turn on one thread means a
/// counter that has been unplugged costs the other one a 2.5-second timeout
/// every second. A thread per counter costs a few kilobytes of stack and
/// makes the quiet case free; the channel puts the samples back in arrival
/// order, which is exactly the order the display wants them in.
///
/// WHY ARRIVAL ORDER IS THE INTERESTING ORDER. Two counters do not agree on
/// when a second starts -- each has its own clock and its own phase -- so
/// their samples interleave. That interleaving is the whole benefit: the same
/// tube watching the same room twice a second, at some offset, is a finer grid
/// in time than either counter can produce alone.
/// What a reader thread sends back.
enum Tube {
    Sample { who: usize, when: f64, counts: u32 },
    /// This tube stopped answering. Its slot stays reserved for its serial.
    Gone { who: usize },
    /// This tube's own thermometer, in degrees Celsius. Only from a bank
    /// that was told to ask, and a counter that has one. See `Bank::spawn`.
    Temp { who: usize, when: f64, celsius: f64 },
}

/// The spectrum log's windows, in seconds: the drum's own, so a row written
/// here is a row the window would have drawn. The first is the row's own
/// length and how often one is written; the others reach the longer periods.
const SPECTRUM_WINDOWS: [usize; 3] = drum::LADDER;
/// The seconds of the average a row is held against: the drum's.
const SPECTRUM_LEVEL: usize = drum::FALL_LEVEL;

/// How long after a tube starts streaming its temperature is first asked.
const TEMP_FIRST: f64 = 2.0;
/// How soon it is asked again after answering that it has not read yet.
const TEMP_AGAIN: f64 = 2.0;
/// How long an answer is waited for, in milliseconds. The question goes out
/// straight after a second of counts and the next is a second away; the
/// slowest answer seen is a little over 300 ms (+V4 Re 4.83).
const TEMP_WAIT_MS: u64 = 700;
/// How many questions in a row may go unanswered before a tube is left alone.
const TEMP_MISSES: u32 = 3;

struct Bank {
    /// One slot per counter ever seen, `None` once it has gone quiet -- which
    /// drops its `Counter` and with it the file descriptor and the flock, so
    /// the port can be taken again when the thing is plugged back in.
    counters: Vec<Option<std::sync::Arc<counter::Counter>>>,
    /// Who each slot IS, kept whether or not it is answering. A tube that
    /// comes back keeps the index its serial had, so the logs, the colours
    /// and the sample tags all stay pointing at the same instrument.
    ids: Vec<broker::CounterId>,
    /// Held for the lifetime of the bank so the channel never reports itself
    /// disconnected: an empty bank is one waiting for a counter, not one that
    /// is finished.
    tx: std::sync::mpsc::Sender<Tube>,
    rx: std::sync::mpsc::Receiver<Tube>,
    running: bool,
    /// Seconds between temperature questions to each tube that has a
    /// thermometer, or None to ask nothing. The service sets it; the
    /// monitor, holding a port for as long as a window is open, does not.
    temp_every: Option<f64>,
}

impl Bank {
    fn open(found: Vec<counter::Counter>) -> Bank {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut bank = Bank {
            counters: Vec::new(), ids: Vec::new(), tx, rx, running: false,
            temp_every: None,
        };
        for c in found {
            bank.adopt(c);
        }
        bank
    }

    /// Take a counter into the bank, reusing its slot if this serial has been
    /// here before, and return the index it landed in.
    fn adopt(&mut self, c: counter::Counter) -> usize {
        let id = broker::CounterId {
            path: c.path.clone(),
            baud: c.baud,
            version: c.version.clone(),
            serial_no: c.serial_no.clone(),
        };
        // A SERIAL ONLY RECLAIMS ITS SLOT IF THAT SLOT IS EMPTY. Matching on
        // the serial alone let a second counter reporting the same one --
        // which no two real tubes do, but every synthetic counter does, and
        // one relabelled tube would -- take over the live slot and close the
        // port of the counter already in it. A tube that is answering cannot
        // simultaneously be arriving.
        let free = |k: usize| self.counters.get(k).map(|c| c.is_none()).unwrap_or(false);
        let who = match self
            .ids
            .iter()
            .position(|k| k.serial_no == id.serial_no)
            .filter(|k| free(*k))
        {
            Some(i) => {
                self.ids[i] = id;
                self.counters[i] = Some(std::sync::Arc::new(c));
                i
            }
            None => {
                self.ids.push(id);
                self.counters.push(Some(std::sync::Arc::new(c)));
                self.ids.len() - 1
            }
        };
        if self.running {
            self.spawn(who);
        }
        who
    }

    /// The reader for one slot: samples until the counter goes quiet, then a
    /// single Gone and the thread ends.
    ///
    /// AND, WHERE IT WAS ASKED FOR, THE TEMPERATURE, BETWEEN TWO SECONDS. The
    /// counts are what this thread is for and nothing about them changes: a
    /// second is read, stamped and sent exactly as before. Then, if a
    /// temperature is due, the question goes out in the gap -- the next
    /// second is a second away, the answer a twentieth of one -- on this
    /// thread, because this thread is the only reader the port has and a
    /// second one would take a share of the counts. See
    /// `Counter::temp_in_stream` for what is done when a second gets in
    /// ahead of the answer anyway.
    ///
    /// A TUBE THAT DOES NOT ANSWER IS LEFT ALONE. Three questions in a row
    /// with nothing, or nothing sensible, back and it is not asked again
    /// until it is next plugged in: the document promises the command to a
    /// firmware, and a unit that disagrees with the document is believed.
    fn spawn(&self, who: usize) {
        let Some(c) = self.counters[who].clone() else { return };
        let tx = self.tx.clone();
        let mut asking = self
            .temp_every
            .filter(|_| counter::temp_supported(&c.version));
        c.heartbeat(true);
        std::thread::spawn(move || {
            let mut ask_at = clock::now() + TEMP_FIRST;
            let mut misses = 0u32;
            let mut waits = 0u32;
            // Whether every second so far carried the stream's mark, and how
            // many there have been: a counter is asked only once its stream
            // has shown the mark, because the mark is what tells an answer
            // from a count. And when a question went out that nothing has
            // answered yet. See `Counter::temp_in_stream`.
            let mut marked = true;
            let mut seen = 0u32;
            let mut owed: Option<f64> = None;
            loop {
                let Some(raw) = c.next_raw(Duration::from_millis(2500)) else { break };
                if raw & counter::STREAM_FLAG == 0 {
                    // A pair without the mark, from a stream that has always
                    // had it, while a question is unanswered: the answer,
                    // late. It is read as one and it is NOT a second of
                    // counts.
                    if marked && seen > 0 && owed.map(|at| clock::now() - at < 5.0).unwrap_or(false) {
                        owed = None;
                        let now = clock::now();
                        match c.late_answer(raw) {
                            counter::TempAnswer::Degrees(celsius) => {
                                misses = 0;
                                waits = 0;
                                if tx.send(Tube::Temp { who, when: now, celsius }).is_err() {
                                    return;
                                }
                            }
                            counter::TempAnswer::NotYet => {
                                misses = 0;
                                waits += 1;
                                if waits <= 5 {
                                    ask_at = now + TEMP_AGAIN;
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    // A stream without the mark is one this cannot ask
                    // safely, and does not.
                    if marked && asking.is_some() {
                        eprintln!(
                            "radbeeper: {} sends its counts without the mark that tells \
                             them from an answer -- its temperature is not asked",
                            c.path
                        );
                    }
                    marked = false;
                    asking = None;
                }
                seen += 1;
                if tx
                    .send(Tube::Sample { who, when: clock::now(), counts: (raw & counter::COUNT_MASK) as u32 })
                    .is_err()
                {
                    return;
                }
                let Some(every) = asking else { continue };
                if seen < 2 || clock::now() < ask_at {
                    continue;
                }
                let asked = clock::now();
                let got = c.temp_in_stream(Duration::from_millis(TEMP_WAIT_MS));
                let now = clock::now();
                for v in &got.samples {
                    if tx.send(Tube::Sample { who, when: now, counts: *v as u32 }).is_err() {
                        return;
                    }
                }
                ask_at = now + every;
                match got.answer {
                    counter::TempAnswer::Degrees(celsius) => {
                        misses = 0;
                        waits = 0;
                        if tx.send(Tube::Temp { who, when: now, celsius }).is_err() {
                            return;
                        }
                    }
                    // Not read yet: ask again in a moment, a few times, and
                    // then at the ordinary spacing like any other.
                    counter::TempAnswer::NotYet => {
                        misses = 0;
                        waits += 1;
                        if waits <= 5 {
                            ask_at = now + TEMP_AGAIN;
                        }
                    }
                    counter::TempAnswer::Silent | counter::TempAnswer::Garbled => {
                        // It may still be on its way: the reader above takes
                        // it when it comes.
                        if got.answer == counter::TempAnswer::Silent {
                            owed = Some(asked);
                        }
                        misses += 1;
                        if misses >= TEMP_MISSES {
                            asking = None;
                            eprintln!(
                                "radbeeper: {} gave no temperature to {} questions -- \
                                 not asking it again; its counts are unaffected",
                                c.path, TEMP_MISSES
                            );
                        }
                    }
                }
            }
            let _ = tx.send(Tube::Gone { who });
        });
    }

    fn identity(&self, spans: &[f64]) -> broker::Identity {
        broker::Identity { counters: self.ids.clone(), spans: spans.to_vec() }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    /// The ports currently held, for a rescan that wants to skip them.
    fn live_paths(&self) -> Vec<String> {
        self.counters
            .iter()
            .filter_map(|c| c.as_ref().map(|c| c.path.clone()))
            .collect()
    }

    fn live(&self) -> Vec<std::sync::Arc<counter::Counter>> {
        self.counters.iter().flatten().cloned().collect()
    }

    /// Begin reading. Nothing arrives before this, and it must not be called
    /// until every port conversation -- the backfill above all -- is over.
    ///
    /// SEPARATE FROM `open`, AND THE SEPARATION IS LOAD-BEARING. Before the
    /// samples start there is a conversation to have with each counter --
    /// reading the tail of its flash to fill the log's gaps, which is a
    /// request and a reply over the same file descriptor. A reader thread
    /// running during that conversation eats the flash as though it were
    /// counts and hands the backfill the counts as though they were flash:
    /// the monitor showed `16383 counts this second` -- which is the count
    /// mask, every bit set -- and a run of a quarter of a million counts in
    /// one second. Nothing about it looked like a race; it looked like a
    /// broken counter.
    fn start(&mut self) {
        if self.running {
            return;
        }
        self.running = true;
        for who in 0..self.counters.len() {
            self.spawn(who);
        }
    }

    /// None means nothing arrived in `timeout` -- NOT that the bank is over.
    /// An empty bank is one waiting for a counter to be plugged in.
    fn next(&self, timeout: Duration) -> Option<Tube> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Forget a tube that has stopped answering, releasing its port.
    fn retire(&mut self, who: usize) {
        if let Some(slot) = self.counters.get_mut(who) {
            *slot = None;
        }
    }

    fn stop(&self) {
        for c in self.counters.iter().flatten() {
            c.heartbeat(false);
        }
    }
}

/// Looking for a counter, without waiting for the answer.
///
/// IDENTIFYING A COUNTER IS A CONVERSATION ON ITS PORT, and a port whose
/// counter has crashed does not hold up its end: every question waits out
/// its timeout. Asked on the thread that reads the samples, that silence
/// was everybody's -- the service stopped publishing for as long as it
/// took, once a log cycle, and a monitor attached to it took the pause for
/// the counter having gone and closed. The tube that was still working was
/// being switched off, every thirty seconds, by the search for the one that
/// was not.
///
/// So each port is asked on a thread of its own and the answer is collected
/// when there is one. A port already being asked is not asked again.
struct Prober {
    tx: std::sync::mpsc::Sender<(String, Option<counter::Counter>)>,
    rx: std::sync::mpsc::Receiver<(String, Option<counter::Counter>)>,
    asking: Vec<String>,
}

impl Prober {
    fn new() -> Prober {
        let (tx, rx) = std::sync::mpsc::channel();
        Prober { tx, rx, asking: Vec::new() }
    }

    /// Ask every port that is neither held nor already being asked.
    fn sweep(&mut self, ports: Vec<String>, held: &[String], baud: Option<u32>) {
        for port in ports {
            if held.contains(&port) || self.asking.contains(&port) {
                continue;
            }
            self.asking.push(port.clone());
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let found = counter::open_at(&port, baud);
                let _ = tx.send((port, found));
            });
        }
    }

    /// The counters that have answered since this was last asked.
    fn found(&mut self) -> Vec<counter::Counter> {
        let mut out = Vec::new();
        while let Ok((port, c)) = self.rx.try_recv() {
            self.asking.retain(|p| *p != port);
            out.extend(c);
        }
        out
    }
}

/// The mean rate across every tube, over `span` seconds.
///
/// TWO COUNTERS SEE TWICE THE COUNTS AND NOT TWICE THE DOSE. The combined
/// windows hold every sample from every tube, so their sum is `tubes` times
/// the rate the room is actually at; dividing gives the mean of the tubes,
/// which is the same number one tube would report and is measured from twice
/// as many arrivals. That is the entire bargain: identical accuracy, better
/// precision, in proportion to the square root of the counts behind it.
///
/// DIVIDED BY WHAT WAS MEASURED, NOT BY WHAT IS PLUGGED IN. This was the sum
/// over `tubes`, and `tubes` is how many counters there have ever been: when
/// one stopped answering, the sum halved and the divisor did not, and two
/// tubes with one of them silent reported half the room. `Windows::mean`
/// divides by the tube-seconds the windows hold. One tube alone keeps
/// `average`, which is the Python's arithmetic to the digit.
fn mean_cpm(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    if tubes <= 1 { w.average(span) } else { w.mean(span) }
}

/// Where a monitor's samples come from.
///
/// TWO WAYS TO HAVE A COUNTER, AND THE DIFFERENCE IS ONE flock. `Own` holds
/// the port, and therefore owes everybody else a fan-out: it logs, it draws
/// the entropy, it writes the emissions, and it publishes all three. `Attached`
/// holds nothing, opens nothing and writes nothing -- it is handed the same
/// samples a moment later and draws exactly the same screen from them.
///
/// The drawing code cannot tell which of the two it has, and that is the test
/// of whether this abstraction is in the right place. Everything below
/// next_sample() is arithmetic; the port is not part of it.
enum Feed {
    Own {
        bank: Bank,
        /// None when the socket could not be bound -- an unwritable log
        /// directory, or another server already there. The monitor still
        /// draws; it just cannot be attached to, and says so.
        srv: Option<broker::Server>,
        /// Where to look for a tube that went quiet, and when that was last
        /// done. See `rejoin`.
        devices: Vec<String>,
        baud: Option<u32>,
        swept: f64,
        prober: Prober,
    },
    Attached(broker::Client),
}

/// One thing that happened, whichever end it came from.
enum Tick {
    Sample { who: usize, when: f64, counts: u32 },
    /// A row the server wrote. WHICH counter it came from is carried on the
    /// wire and dropped here: this monitor draws one table and the rows are
    /// already stamped with their own serial. The window reads the tag.
    Row { row: String },
    Random { who: usize, hex: String, at: String, suspect: bool },
    /// The replayed history is over. Only an attached feed sends it.
    Live,
    /// A tube joined the stream, or came back to it.
    Joined { who: usize },
    /// A tube stopped answering. The others carry on without it.
    Gone { who: usize },
    /// The server is downloading a flash, or has finished (empty). Only an
    /// attached feed sends it. See broker::Event::Note.
    Note { text: String },
    /// A counter's own thermometer, in degrees Celsius, and when it was
    /// read. The monitor draws it small beside the averages, and by the day
    /// on a key: see `draw_thermometer` and `draw_temp_drum`.
    Temp { who: usize, when: f64, celsius: f64 },
}

/// What one wake-up of `Feed::poll` found.
enum Polled {
    Tick(Tick),
    /// Nothing in this slice of time. The feed is still there.
    Idle,
    /// There is no more counter.
    Over,
}

impl Tick {
    /// What the service said, as the monitor hears it from either end.
    fn of(event: broker::Event) -> Tick {
        match event {
            broker::Event::Sample { who, when, counts } => Tick::Sample { who, when, counts },
            broker::Event::Row { row, .. } => Tick::Row { row },
            broker::Event::Random { who, hex, at, suspect } => {
                Tick::Random { who, hex, at, suspect }
            }
            broker::Event::Live => Tick::Live,
            broker::Event::Note { text } => Tick::Note { text },
            // A tube joining or leaving changes what the room is the
            // mean of, so the monitor is told, as the window is.
            broker::Event::Counter { who, .. } => Tick::Joined { who },
            broker::Event::Gone { who } => Tick::Gone { who },
            broker::Event::Temp { who, when, celsius } => Tick::Temp { who, when, celsius },
        }
    }
}

impl Feed {
    /// Attach to whoever holds the port, or take it.
    ///
    /// THE ORDER IS THE WHOLE FIX. Ask the socket first: if a service is
    /// logging, it is already reading the counter and there is nothing to
    /// negotiate -- attaching costs it one write a second and costs the
    /// person nothing at all. Only when nobody is serving does this open the
    /// port, and then it serves in turn, so the next window attaches to this
    /// one instead of printing `port busy`.
    fn open(dir: &Path, devices: &[String], baud: Option<u32>, spans: &[f64],
            wait: Option<f64>) -> Result<Feed, counter::NotFound> {
        // A NAMED DEVICE MEANS THAT DEVICE, AND THE SOCKET DOES NOT OVERRULE
        // IT. `-d /dev/pts/7` is how the fake counter is driven and how a
        // second counter is picked out on a machine with two; attaching to
        // whatever a service happens to be serving instead would answer a
        // question nobody asked. The differential suite caught this the hour
        // it was written: every `-d <pty> watch` in it silently drew the REAL
        // counter, because a service was running and the socket was tried
        // first. So attach only when the stream carries every counter that
        // was asked for -- which it does, when nothing was asked for.
        if let Some(c) = broker::Client::attach(dir) {
            if devices.iter().all(|d| c.identity().serves(d)) {
                return Ok(Feed::Attached(c));
            }
        }
        let one = devices.len() == 1;
        let found = match (wait, one) {
            (Some(limit), _) => vec![find_waiting(devices.first().map(|s| s.as_str()),
                                                  baud, limit)?],
            _ => {
                let (found, why) = counter::find_all(devices, baud);
                match why {
                    Some(e) => return Err(e),
                    None => found,
                }
            }
        };
        let bank = Bank::open(found);
        let srv = broker::Server::start(dir, &bank.identity(spans)).ok();
        Ok(Feed::Own {
            bank,
            srv,
            devices: devices.to_vec(),
            baud,
            swept: clock::now(),
            prober: Prober::new(),
        })
    }

    fn identity(&self) -> broker::Identity {
        match self {
            // Spans are left empty for an owned feed: the monitor already has
            // its own and uses them, and the table it draws is its own file.
            Feed::Own { bank, .. } => bank.identity(&[]),
            Feed::Attached(c) => c.identity().clone(),
        }
    }

    /// The ports, for the things that genuinely need to talk to a counter --
    /// reading its flash, setting its clock. An attached feed has none, and
    /// the caller must have something sensible to do about that.
    fn counters(&self) -> Vec<std::sync::Arc<counter::Counter>> {
        match self {
            Feed::Own { bank, .. } => bank.live(),
            Feed::Attached(_) => Vec::new(),
        }
    }

    fn owns_the_port(&self) -> bool {
        matches!(self, Feed::Own { .. })
    }

    /// How many other windows are reading this one's stream.
    fn watchers(&self) -> usize {
        match self {
            Feed::Own { srv, .. } => srv.as_ref().map(|s| s.clients()).unwrap_or(0),
            Feed::Attached(_) => 0,
        }
    }

    /// Begin reading. Nothing arrives before this, and it must not be called
    /// until every port conversation -- the backfill above all -- is over.
    fn start(&mut self) {
        if let Feed::Own { bank, .. } = self {
            bank.start();
        }
    }

    /// How many counters are behind this feed.
    fn width(&self) -> usize {
        match self {
            Feed::Own { bank, .. } => bank.len(),
            Feed::Attached(c) => c.identity().len(),
        }
    }

    /// Stop the stream -- but ONLY if this end started it. An attached
    /// monitor closing must not silence the counter for the service that is
    /// still logging it, which is the one way a client could do real damage.
    fn stop(&mut self) {
        if let Feed::Own { bank, .. } = self {
            bank.stop();
        }
    }

    fn next(&mut self, timeout: Duration) -> Option<Tick> {
        match self {
            Feed::Own { bank, srv, devices, baud, swept, prober } => {
                // Let in whoever turned up while we were waiting for this
                // second. Before the publish, so a client that has just been
                // greeted with the history gets this sample live rather than
                // twice.
                if let Some(s) = srv.as_mut() {
                    s.accept_pending();
                }
                if let Some(who) = rejoin(bank, devices, *baud, swept, prober) {
                    if let (Some(s), Some(c)) = (srv.as_mut(), bank.ids.get(who)) {
                        s.announce(who, c);
                    }
                    return Some(Tick::Joined { who });
                }
                // WALL CLOCK, NOT A MONOTONIC STAMP. A sample that is going
                // to be replayed to a process which was not running when it
                // was taken has to mean something to it, and "412.7 seconds
                // after some other program started" does not. It is also the
                // only clock two counters can be placed on together.
                match bank.next(timeout)? {
                    Tube::Sample { who, when, counts } => {
                        if let Some(s) = srv.as_mut() {
                            s.publish_sample(who, when, counts);
                        }
                        Some(Tick::Sample { who, when, counts })
                    }
                    // THE LAST TUBE going quiet in the monitor is the end of
                    // it: `watch` is a session somebody is sitting in front
                    // of, not a service, and it says so and stops rather than
                    // waiting for a counter to come back. ONE OF SEVERAL is
                    // not: the others are still measuring the room, and a
                    // monitor that closed on them because a USB port reset
                    // would be throwing away the tubes that work. Its port is
                    // let go, everybody attached is told, and it is looked
                    // for again; see `rejoin`.
                    Tube::Temp { who, when, celsius } => {
                        if let Some(s) = srv.as_mut() {
                            s.publish_temp(who, when, celsius);
                        }
                        Some(Tick::Temp { who, when, celsius })
                    }
                    Tube::Gone { who } => {
                        bank.retire(who);
                        if let Some(s) = srv.as_mut() {
                            s.publish_gone(who);
                        }
                        if bank.live().is_empty() {
                            None
                        } else {
                            Some(Tick::Gone { who })
                        }
                    }
                }
            }
            Feed::Attached(c) => Some(Tick::of(c.next(timeout)?)),
        }
    }

    /// The next thing that happened, a quiet moment, or the end of the feed:
    /// `next` for a caller that wakes up more often than a counter speaks.
    ///
    /// FOR THE KEYS. The monitor waited up to two and a half seconds in
    /// `next` and looked at the keyboard after it had drawn, so a key was
    /// answered by the next sample, a second later. Waking ten times a
    /// second to look needs a timeout that is not the end of the feed, which
    /// `next` cannot say: its None is both. An attached feed has
    /// `Client::poll` for exactly this -- a short timeout in `Client::next`
    /// can also lose the half of a line it had read. A feed that holds the
    /// ports is over only when the last tube has gone, and the bank knows.
    fn poll(&mut self, slice: Duration) -> Polled {
        if let Feed::Attached(c) = self {
            return match c.poll(slice) {
                broker::Poll::Event(e) => Polled::Tick(Tick::of(e)),
                broker::Poll::Idle => Polled::Idle,
                broker::Poll::Closed => Polled::Over,
            };
        }
        match self.next(slice) {
            Some(tick) => Polled::Tick(tick),
            None => match self {
                Feed::Own { bank, .. } if bank.live().is_empty() => Polled::Over,
                _ => Polled::Idle,
            },
        }
    }

    fn publish_row(&mut self, who: usize, row: &str) {
        if let Feed::Own { srv: Some(s), .. } = self {
            s.publish_row(who, row);
        }
    }

    fn publish_random(&mut self, who: usize, hex: &str, at: &str, suspect: bool) {
        if let Feed::Own { srv: Some(s), .. } = self {
            s.publish_random(who, hex, at, suspect);
        }
    }
}

/// Look for a tube that went quiet, and take it back if it is there.
///
/// ONLY A TUBE THAT WAS HERE. The monitor's screen, its windows and its logs
/// were laid out for the counters it started with, and a stranger plugged in
/// halfway is the service's to adopt, not a session's. So a port is opened
/// only while a slot is empty, and a counter is kept only if its serial is
/// that slot's -- when it returns to its own colour and its own log.
///
/// Once a log cycle, as the service does, and asked the way the service
/// asks: see `Prober`.
fn rejoin(bank: &mut Bank, devices: &[String], baud: Option<u32>, swept: &mut f64,
          prober: &mut Prober) -> Option<usize>
{
    let away: Vec<String> = bank
        .counters
        .iter()
        .zip(bank.ids.iter())
        .filter(|(c, _)| c.is_none())
        .map(|(_, id)| id.serial_no.clone())
        .collect();
    // One that answered an earlier sweep, if it is one of ours. A stranger
    // is dropped, which closes its port and leaves it for the service.
    for c in prober.found() {
        if away.contains(&c.serial_no) {
            return Some(bank.adopt(c));
        }
    }
    if away.is_empty() || clock::now() - *swept < RESCAN {
        return None;
    }
    *swept = clock::now();
    let ports: Vec<String> = if devices.is_empty() {
        counter::candidate_ports()
    } else {
        devices.to_vec()
    };
    prober.sweep(ports, &bank.live_paths(), baud);
    None
}

/// The top row for several tubes: each by its serial, and beside it what it
/// last counted.
///
/// THE READING IS THE ONE THE INTERLEAVE WAS GIVEN: that tube's newest
/// second. Not an average -- the averages are the rows below, and they are
/// of the room. This is what each instrument said most recently, so that two
/// of them can be seen to agree or not, second by second.
///
/// IN COUNTS A SECOND, because that is what it is. It was shown as a rate a
/// minute, the second times sixty, and a figure that can only be 0, 60 or
/// 120 reads as a broken average rather than as one click. As CPS it is the
/// number of clicks, and a number that changes is the data arriving.
///
/// EMPTY, NOT NOUGHT, when a tube is not answering. A nought is a reading:
/// a second in which the tube was listening and nothing arrived. A tube that
/// has stopped has no reading, and printing one would be the instrument
/// claiming to still be measuring. The space is kept, so nothing to its
/// right moves when it comes back.
/// What is written over a tier: `F 16s/bar · 6m`, or as much as fits.
///
/// SHORTENED, NOT CUT. Six tiers and an interleave across eighty columns
/// are twelve columns each, and the whole label is fourteen: cut to fit it
/// read `F 16s/bar ·`, which is a label with its end missing. So the reach
/// goes first, then the hand-over mark and the unit, and what is left is
/// still something: `16s`.
fn tier_label(ti: usize, seconds: f64, columns: usize) -> String {
    let bar = analysis::bar_seconds(seconds);
    let mark = if ti == 0 { "" } else { "F " };
    let room = columns.saturating_sub(1);
    [
        format!("{}{}s/bar \u{b7} {}", mark, bar,
                analysis::span_words(columns as f64 * seconds)),
        format!("{}{}s/bar", mark, bar),
        format!("{}s", bar),
    ]
    .into_iter()
    .find(|l| l.chars().count() <= room)
    .unwrap_or_default()
}

/// A second's counts, as the top row prints them.
fn cps(counts: u32) -> String {
    format!("{:>5} CPS", counts)
}

fn tubes_row(serials: &[String], newest: &[Option<u32>], live: &[bool]) -> String {
    const SHOWN: usize = 4;
    let mut out: Vec<String> = Vec::new();
    for (k, serial) in serials.iter().enumerate().take(SHOWN) {
        let reading = match (live.get(k).copied().unwrap_or(false), newest.get(k).copied().flatten()) {
            (true, Some(c)) => cps(c),
            _ => " ".repeat(9),
        };
        out.push(format!("{} {}", serial, reading));
    }
    if serials.len() > SHOWN {
        out.push(format!("and {} more", serials.len() - SHOWN));
    }
    let answering = live.iter().filter(|l| **l).count();
    out.push(if answering == serials.len() {
        format!("{} tubes averaged", serials.len())
    } else {
        format!("{} of {} tubes averaged", answering, serials.len())
    });
    out.join("   ")
}

fn watch(feed: &mut Feed, spans: &[f64], cpm_per_usvh: f64,
         duration: Option<f64>, logging: Option<WatchLog>, dir: &Path) {
    // So a `kill` stops the stream and puts the terminal back, as q does.
    install_stop_handler();
    let mut id = feed.identity();
    // THE TABLE IS SOMEBODY ELSE'S FILE, SO IT IS DRAWN WITH THEIR COLUMNS.
    // An attached monitor may have been given different --spans on the
    // command line, and its own averages panel honours them -- that is
    // arithmetic over samples it holds, and needs nobody's permission. The
    // rows at the bottom came off the server's disk and their columns are not
    // ours to choose. An owned feed leaves spans empty and uses its own.
    let table_spans: Vec<f64> =
        if id.spans.is_empty() { spans.to_vec() } else { id.spans.clone() };
    let screen = Screen::enter();
    let names = log::columns(&log::header(&table_spans));
    let mut table: std::collections::VecDeque<Vec<String>> = std::collections::VecDeque::new();
    let mut table_note = String::new();
    // A CLIENT WRITES NOTHING, AND THE RULE HAS NO EXCEPTIONS. The process
    // holding the port is already logging, backfilling and exporting this
    // counter; a second writer would race it for the same rows in the same
    // file, and the flock that stops two readers halving a count does nothing
    // at all about two writers interleaving a line. The rows still appear in
    // the table below -- they come down the socket as the server writes them,
    // which is a truer picture than this process's own guess would be.
    // WHETHER A TABLE WAS ASKED FOR, decided before the writing is taken
    // away. An attached monitor still shows the rows -- they arrive down the
    // socket as the server writes them -- and `--no-log` still means a clean
    // screen. Keying the table off `logger.is_some()` instead left an
    // attached window collecting rows it never drew.
    let wants_table = logging.is_some();
    let logging = logging.filter(|_| feed.owns_the_port());
    let mut loggers: Vec<Logger> = Vec::new();
    // None when this monitor is attached to a service: the process holding
    // the ports writes both files, and a client that wrote either would be a
    // second hand on the same record. See broker.rs.
    let mut merged_log: Option<MergedLogger> = None;
    if let Some(wl) = logging.as_ref() {
        let _ = std::fs::create_dir_all(&wl.dir);
        // THE HISTORY FIRST, and it takes a while -- fifteen or twenty seconds
        // of flash over the serial line -- so the screen says what it is
        // waiting for instead of sitting blank.
        // EACH COUNTER'S OWN FLASH, EACH COUNTER'S OWN LOG. They are separate
        // instruments that were in the room at the same time; merging their
        // records would throw away the one thing a second tube is for, which
        // is being able to ask whether the two agree.
        for (k, c) in feed.counters().iter().enumerate() {
            if let Some((bytes, max_gap)) = wl.backfill {
                print!("\x1b[2J{}{}{} @ {} baud   {}   serial {}{}{}reading the counter's history \
                        to fill the log's gaps ({} KiB)...{}",
                       at(0, 0), DIM, c.path, c.baud, c.version, c.serial_no, OFF,
                       at(2, 0), bytes / 1024, at(3, 0));
                let _ = std::io::stdout().flush();
                let note = backfill_at_start(c, &wl.dir, spans, wl.every, bytes, max_gap, true)
                    .replace(" samples, ", " samples ")
                    .replace("backfill -- ", "backfill: ");
                table_note = if k == 0 { note } else { format!("{}   {}", table_note, note) };
            }
            // The table opens on what is already in the log, so it is not
            // empty for the first half-minute -- and what the backfill just
            // wrote is the first thing in it.
            let path = log::path(clock::now(), &wl.dir, Some(&c.serial_no));
            for (_, cells) in
                log::read_table(&path, &names).into_iter().rev().take(TABLE_KEEP).rev()
            {
                table.push_back(cells);
            }
            loggers.push(Logger::new(spans, wl.dir.clone(), &c.serial_no, wl.every));
        }
        // And the room they are all in, beside them. See `MergedLogger`.
        let mut m = MergedLogger::new(spans, wl.dir.clone(), wl.every);
        m.counters(
            &feed.counters().iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
        );
        merged_log = Some(m);
        if wl.export_every.is_some() {
            let done = export_pages(&wl.dir, cpm_per_usvh);
            table_note = if table_note.is_empty() { done } else { format!("{}   {}", table_note, done) };
        }
    }
    let mut exported = Instant::now();
    // HOW MANY TUBES ARE WATCHING THE SAME ROOM. Every combined figure below
    // is divided by it, because two counters see twice the counts and NOT
    // twice the dose -- what doubles is the evidence, not the radiation. The
    // precision that buys is the whole reason for a second tube, and it is
    // reported beside the number rather than left to be inferred.
    let mut tubes = feed.width().max(1);
    // One per counter, and one across all of them. The combined windows hold
    // every sample from every tube, so their sums are `tubes` times the rate:
    // see `mean_cpm`.
    let mut each: Vec<Windows> = (0..tubes).map(|_| Windows::new(spans)).collect();
    let mut w = Windows::new(spans);
    let mut ladder = Ladder::new();
    // THE DRUM, FROM THE SAME SECONDS AND BY THE WINDOW'S OWN NUMBERS: a row
    // of 512 seconds every eight, joined out to the spectrum's longer windows,
    // ninety-six rows kept, each held against the 3000-second average. See
    // radbeeper::drum. `fall` is the rows on show, newest first, worked out
    // again only when a row arrives; `deep` is the three hours-long averages
    // the front row of the paper is joined from.
    let mut trail = analysis::Trail::new(
        &[drum::FALL_WINDOW, drum::LADDER[0], drum::LADDER[1], drum::LADDER[2]],
        drum::FALL_HOP,
        drum::FALL_DEPTH,
        drum::FALL_LEVEL,
    );
    let mut fall: Vec<analysis::TrailRow> = Vec::new();
    let mut fall_end: Option<u64> = None;
    let mut deep: Vec<analysis::Spectrum> =
        drum::LADDER.iter().map(|w| analysis::Spectrum::new(*w)).collect();
    let depth = depth_of(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    // EACH TUBE'S NEWEST TEMPERATURE, and when it was read; None for one
    // that has given none, which is every tube of a monitor that holds the
    // ports itself -- only the service asks. `temp_open` is the key: the
    // ten days of it, where the charts are. `temp_log` is what that is
    // drawn from, for the tube and at the moment `temp_read` says.
    let mut temps: Vec<Option<(f64, f64)>> = vec![None; tubes];
    let mut temp_open = false;
    let (mut day_view, mut month_view) = View::at_first(screen.size().0);
    // THE LONG PAPERS' LINES, newest first, and when each was last read. Read
    // once here whatever the screen's height, so the keys' line can say
    // whether there is a paper to turn on; after that, only while shown.
    let mut day_rows = longdrum::rows(dir, longdrum::Paper::Day, clock::now(), DAY_LINES);
    let mut month_rows = longdrum::rows(dir, longdrum::Paper::Month, clock::now(), MONTH_LINES);
    let mut day_read = Some(Instant::now());
    let mut month_read = Some(Instant::now());
    // How many of the strip's oldest seconds came from the count log and
    // not from the counter: see `prefill_seconds`. None until it is tried.
    let mut prefilled: Option<usize> = None;
    let mut temp_log: Vec<(f64, f64)> = Vec::new();
    let mut temp_read: Option<(Instant, usize)> = None;
    // The tubes with a temperature log already in the directory: a counter
    // that has not answered yet this hour still has its ten days.
    let temp_logged = log::temp_serials(dir);
    let mut pools: Vec<entropy::Entropy> =
        (0..tubes).map(|_| new_pool()).collect();
    // The samples in ARRIVAL order, as rates, which is what the cascade is
    // cut from. With one counter that is one bar a second; with two it is a
    // bar every half second on average, because two tubes on their own clocks
    // interleave. See analysis::tiers_with.
    let mut merged: std::collections::VecDeque<f64> = std::collections::VecDeque::new();
    let mut dropped = 0usize;
    // AND AS THEY ARRIVED, with the tube and the time, for a strip cut by the
    // clock: see analysis::tiers_arrivals. When each tube was last heard,
    // what it said, and whether the reader has given it up.
    let mut arrivals: std::collections::VecDeque<analysis::Arrival> =
        std::collections::VecDeque::new();
    let mut heard: Vec<Option<f64>> = vec![None; tubes];
    let mut newest: Vec<Option<u32>> = vec![None; tubes];
    let mut gone: Vec<bool> = vec![false; tubes];
    // Whole seconds, summed across the tubes, for the spectrum: a period is a
    // property of the room and both counters are looking at it, so adding
    // them is simply twice the signal on one time base.
    let mut sec_bin: Option<i64> = None;
    let mut sec_sum: u32 = 0;
    // The drawn line and the moment it was drawn, kept across frames: a line
    // stays on screen until the pool has earned the next one.
    let mut shown: Option<(String, String)> = None;
    let mut suspect = false;
    // An attached feed opens with everything the server has: up to eight
    // hours of it, as fast as the socket can carry it. Those samples go into
    // the windows and the spectrum but are NOT drawn one frame each -- eight
    // hours of history, drawn a second at a time, takes eight hours.
    let mut replaying = !feed.owns_the_port();
    let mut attached_note = String::new();
    if replaying {
        attached_note = match id.primary() {
            Some(c) if tubes == 1 => format!("attached to the counter on {}", c.path),
            _ => format!("attached to {} counters", tubes),
        };
        table_note = attached_note.clone();
    }
    // Whether a live sample has been drawn yet: a note from the server gets a
    // screen of its own until one has.
    let mut drawn = false;
    // AFTER THE BACKFILL, NEVER BEFORE IT. See Bank::start.
    feed.start();
    let watching = Instant::now();
    // When the feed last said anything, and the sample the screen was last
    // drawn from.
    let mut spoke = Instant::now();
    let mut last: Option<(usize, f64, u32)> = None;
    let mut out = String::with_capacity(16384);
    // Rows written this second, held only until the logger's borrow ends.
    let mut rows: Vec<(usize, String)> = Vec::new();

    loop {
        // A TENTH OF A SECOND AT A TIME, so a key is answered when it is
        // pressed and not when the counter next speaks. A feed that has said
        // nothing at all for two and a half seconds is over, as it always
        // was: see `Feed::poll`.
        let tick = match feed.poll(KEY_SLICE) {
            Polled::Tick(tick) => {
                spoke = Instant::now();
                Some(tick)
            }
            Polled::Idle if spoke.elapsed() < FEED_SILENCE => None,
            Polled::Idle | Polled::Over => break,
        };
        let fresh = match tick {
            Some(Tick::Sample { who, when, counts }) => Some((who.min(tubes - 1), when, counts)),
            None => None,
            // A TEMPERATURE. Kept for the thermometer, and put on the end of
            // the ten days if those are of this tube; the next sample draws
            // it, a second from now at most.
            Some(Tick::Temp { who, when, celsius }) => {
                if temps.len() <= who {
                    temps.resize(who + 1, None);
                }
                temps[who] = Some((when, celsius));
                if temp_read.map_or(false, |(_, k)| k == who) {
                    temp_log.push((when, celsius));
                }
                continue;
            }
            // A row the server has just written. The client shows what went
            // to disk rather than deciding for itself what should have.
            Some(Tick::Row { row }) => {
                table.push_back(row.split('\t').map(str::to_string).collect());
                while table.len() > TABLE_KEEP {
                    table.pop_front();
                }
                continue;
            }
            // The hex on a client's screen is always the server's. Two
            // processes deriving their own from the same counts would print
            // two different lines and only one of them would be in the audit
            // log, which is worse than useless.
            Some(Tick::Random { who, hex, at, suspect: s }) => {
                shown = Some((hex, at));
                suspect = s;
                if let Some(p) = pools.get_mut(who) {
                    p.reset();
                }
                continue;
            }
            Some(Tick::Live) => {
                replaying = false;
                continue;
            }
            // A TUBE STOPPED. It is out of the interleave and out of the
            // mean from here, and its place in the top row is empty. Nothing
            // is redrawn for it: the next sample from a tube that is
            // answering draws the screen, a second from now at most.
            Some(Tick::Gone { who }) => {
                if let Some(g) = gone.get_mut(who) {
                    *g = true;
                }
                continue;
            }
            // AND CAME BACK, or joined. Its windows start again -- what it
            // counted before it left is not the last thirty seconds -- and a
            // tube this monitor has not seen before gets a place made for it.
            Some(Tick::Joined { who }) => {
                id = feed.identity();
                tubes = id.len().max(tubes);
                while each.len() < tubes {
                    each.push(Windows::new(spans));
                    pools.push(new_pool());
                }
                heard.resize(tubes, None);
                newest.resize(tubes, None);
                gone.resize(tubes, false);
                if who < tubes {
                    each[who] = Windows::new(spans);
                    gone[who] = false;
                    heard[who] = None;
                    newest[who] = None;
                }
                continue;
            }
            // THE SERVICE IS READING A FLASH, and no sample will come until it
            // is done -- sixteen minutes, once. Before the first sample there
            // is no screen to put it on, so it gets one of its own, the same
            // one a monitor doing its own backfill shows; after it, it is the
            // note beside the clock.
            Some(Tick::Note { text }) => {
                if text.is_empty() {
                    table_note = attached_note.clone();
                } else {
                    if !drawn {
                        print!("\x1b[2J{}{}{}{}{}{}", at(0, 0), DIM, attached_note, OFF,
                               at(2, 0), text);
                        let _ = std::io::stdout().flush();
                    }
                    table_note = text;
                }
                continue;
            }
        };
        if stopping() {
            break;
        }
        // THE KEYS, BEFORE THE DRAWING, so the frame a key asks for is the
        // next thing on the screen. A key that changes what is shown draws
        // the screen again from the last sample, with nothing counted twice.
        let pressed = screen.keys();
        if pressed.iter().any(|&c| c == b'q' || c == b'Q' || c == 27) {
            break;
        }
        let mut turned = false;
        for key in &pressed {
            match key {
                b't' | b'T' => temp_open = !temp_open,
                b'd' | b'D' => day_view = day_view.next(),
                b'm' | b'M' => month_view = month_view.next(),
                _ => continue,
            }
            turned = true;
        }
        let again = if turned && drawn { last } else { None };
        let Some((who, when, counts)) = fresh.or(again) else { continue };
        if fresh.is_some() {
            last = fresh;
            // THE HOURS BEFORE THE FIRST SAMPLE, FROM THE LOG, once. Only a
            // monitor that is attached does it: one that holds the ports
            // writes its windows into the log, and what it wrote must be
            // what it counted.
            if prefilled.is_none() {
                prefilled = Some(0);
                if !feed.owns_the_port() {
                    let reach = spans.iter().copied().fold(PREFILL_REACH, f64::max);
                    let per: Vec<Vec<u32>> = id
                        .counters
                        .iter()
                        .map(|c| prefill_seconds(&log_rows(dir, &c.serial_no, when, reach), when, reach))
                        .collect();
                    let longest = per.iter().map(|p| p.len()).max().unwrap_or(0);
                    // Oldest first and every tube's second together, as the
                    // windows are given them live.
                    for back in (1..=longest).rev() {
                        for (k, seconds) in per.iter().enumerate().take(each.len()) {
                            if back <= seconds.len() {
                                let (t, c) = (when - back as f64, seconds[seconds.len() - back]);
                                w.add(t, c);
                                each[k].add(t, c);
                            }
                        }
                    }
                    // The strip of one tube is cut by samples and takes
                    // them; several tubes' is cut by arrivals, which a log
                    // row does not have.
                    if tubes == 1 {
                        merged.extend(per[0].iter().map(|c| *c as f64));
                        prefilled = Some(per[0].len());
                    }
                }
            }
            w.add(when, counts);
            each[who].add(when, counts);
            // A sample is the tube answering, whatever was said of it before.
            heard[who] = Some(when);
            newest[who] = Some(counts);
            gone[who] = false;
            if arrivals.len() >= STRIP_KEEP {
                arrivals.pop_front();
            }
            arrivals.push_back(analysis::Arrival { who, when, counts });
            if merged.len() >= STRIP_KEEP {
                merged.pop_front();
                dropped += 1;
            }
            merged.push_back(counts as f64);
            // The spectrum's second closes when the wall clock's does, and takes
            // whatever every tube put in it.
            let this = when.floor() as i64;
            match sec_bin {
                Some(b) if b == this => sec_sum += counts,
                Some(_) => {
                    ladder.add(sec_sum);
                    trail.add(sec_sum);
                    for s in deep.iter_mut() {
                        s.add(sec_sum);
                    }
                    sec_bin = Some(this);
                    sec_sum = counts;
                }
                None => {
                    sec_bin = Some(this);
                    sec_sum = counts;
                }
            }
            // NOT DURING THE REPLAY. The server's pool holds the counts since its
            // last draw; a client that poured eight hours of history into its own
            // would think it had earned a line the moment it opened. Starting at
            // the live edge makes the countdown pessimistic until the first
            // emission arrives, and exact from then on.
            if !replaying {
                if let Some(p) = pools.get_mut(who) {
                    p.add_at(when, counts);
                }
            }
            if replaying {
                continue;
            }
            drawn = true;
            // EACH TUBE'S OWN ROW IN ITS OWN FILE, from its own windows. A row
            // that averaged two counters would be a reading no instrument took.
            if let Some(lg) = loggers.get_mut(who) {
                let averages: Vec<Option<f64>> =
                    spans.iter().map(|s| each[who].average(*s)).collect();
                match lg.add(clock::now(), counts, &averages) {
                    Ok(Some(line)) => {
                        table.push_back(line.split('\t').map(str::to_string).collect());
                        while table.len() > TABLE_KEEP {
                            table.pop_front();
                        }
                        rows.push((who, line));
                    }
                    Ok(None) => {}
                    Err(e) => table_note = format!("NOT LOGGING: {}", e),
                }
            }
            if let Some(m) = merged_log.as_mut() {
                if let Err(e) = m.add(who, when, counts, &w) {
                    table_note = format!("NOT LOGGING THE MERGE: {}", e);
                }
            }
            // Out to whoever is attached, after the borrow of the logger ends.
            for (k, line) in rows.drain(..) {
                feed.publish_row(k, &line);
            }
            if let Some(wl) = logging.as_ref() {
                if let Some(e) = wl.export_every {
                    if exported.elapsed().as_secs_f64() >= e {
                        exported = Instant::now();
                        table_note = export_pages(&wl.dir, cpm_per_usvh);
                    }
                }
            }
        }
        let spec = ladder.best();

        let (h, width) = screen.size();
        // Everything above the table lays out as though the screen ended
        // where the table starts, and the footer stays on the last two rows.
        let tl = if wants_table { table_lines(h) } else { 0 };
        let hv = h - tl;
        out.clear();
        out.push_str("\x1b[2J");
        // The port, the firmware beside it, the serial. The program's own
        // name is not here: it goes in the top right corner, out of the way
        // of the readout, because a header that grows pushes the big number
        // right and the number is what people are looking at.
        let head = match (id.primary(), tubes) {
            // One tube has its reading beside its serial as two do. There
            // is no second tube to be away, so it is never empty: this line
            // is drawn by a sample.
            (Some(c), 1) => format!(
                "{} @ {} baud   {}   serial {} {}",
                c.path, c.baud, c.version, c.serial_no, cps(counts)
            ),
            (Some(_), _) => tubes_row(
                &id.counters.iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
                &newest,
                &analysis::answering(&heard, &gone, when),
            ),
            (None, _) => "no counter".to_string(),
        };
        out.push_str(&format!(
            "{}{}{}{}",
            at(0, 0), DIM, head, OFF
        ));

        let mut row = 2usize;
        for &span in spans {
            match mean_cpm(&w, span, tubes) {
                None => {
                    let left = (span - w.elapsed()).max(0.0).round() as i64;
                    out.push_str(&format!(
                        "{}{}{:>5}s   filling, {}s to go{}",
                        at(row, 0), DIM, span as i64, left, OFF
                    ));
                }
                Some(cpm) => {
                    out.push_str(&format!(
                        "{}{}{:>5}s{}{}{}{:>8.1} CPM{}{}{:>8.3} uSv/h",
                        at(row, 0), DIM, span as i64, OFF,
                        at(row, 7), colour_for(level(cpm)), cpm, OFF,
                        at(row, 23), cpm / cpm_per_usvh
                    ));
                }
            }
            row += 1;
        }
        row += 1;
        out.push_str(&format!(
            "{}{}now{}  {} counts this second{}{}run{}  {} counts in {}s",
            at(row, 0), BOLD, OFF, counts,
            at(row + 1, 0), DIM, OFF, w.total, w.elapsed().round() as i64
        ));
        // THREE, NOT TWO. `row` is still on the "now" line here -- "run" was
        // drawn at row + 1 without moving it -- so clearing the pair and
        // leaving the blank row after it costs three. Two put this monitor's
        // chart one row above the Python's for the whole of the port, which
        // nothing caught because both were only ever read on their own.
        row += 3;

        // The number, big, to the right of everything above.
        let headline = mean_cpm(&w, 30.0, tubes)
            .or_else(|| mean_cpm(&w, *spans.last().unwrap(), tubes));
        let digits = big_number(&match headline {
            Some(v) => format!("{:.0}", v),
            None => "--".to_string(),
        });
        // Clear of the header, which is on the row the digits start on. This
        // was a constant 54 and the header outgrew it: a serial is fourteen
        // characters and the firmware sits beside the port, so the digits
        // were landing on top of the counter's own name. Measured, not
        // guessed.
        let left = digits_left(&head);
        // RESERVED, NOT MEASURED. The block is as wide as 65535 draws even
        // while the reading is 20, so the readout neither shifts under the
        // eye as a digit arrives nor -- which is worse -- gets dropped for
        // want of room at the one moment it is worth reading. Everything
        // optional on this screen is placed around this block, not before it.
        let reserve = readout_width();
        let readout = width >= left + reserve && h > BIG_ROWS;
        if readout {
            let tint = headline.map(|v| colour_for(level(v))).unwrap_or(DIM);
            for (i, d) in digits.iter().enumerate() {
                out.push_str(&format!("{}{}{}{}", at(i, left), tint, d, OFF));
            }
        }
        // The top right corner, which the readout reaches only on a narrow
        // terminal: what program this is and which version drew the screen.
        // It is the last thing placed and the first thing dropped.
        let plate = nameplate();
        let busy = if readout { left + reserve } else { head.chars().count() };
        if let Some(x) = nameplate_left(width, busy, plate.chars().count()) {
            out.push_str(&format!("{}{}{}{}", at(0, x), DIM, plate, OFF));
        }

        // The thermometers, beside the averages and as tall as they and the
        // two lines under them are: only for a counter that has given a
        // temperature, and only where the big digits leave the room.
        let now_wall = clock::now();
        let readings: Vec<(f64, bool)> = temps
            .iter()
            .flatten()
            .take(4)
            .map(|(read, celsius)| (*celsius, now_wall - *read > TEMP_STALE))
            .collect();
        let thermo_right = THERMO_LEFT + thermometer_width(readings.len());
        if !readings.is_empty()
            && hv > row
            && thermo_right <= if readout { left } else { width }
        {
            draw_thermometer(&mut out, 2, THERMO_LEFT, row - 3, &readings);
        }

        // Two rows of air, then the counts. It was three; the third is the
        // clock's, at the bottom, and the chart still clears the digits.
        row += 2;
        // THE DRUM GOES OVER THE COUNTS, WHERE THE WINDOW HAS IT: the paper,
        // the period axis under it, and under that the counts the newest row
        // was made from. It starts on the row the tiers' labels have had and
        // takes what is left when the counts have theirs and, on a screen
        // wide enough for it, the random line and the clock have their four
        // rows. Before the first row -- 512 seconds -- and on a screen with
        // no room for both, there is no paper and everything is where it
        // was: the counts, and the spectrum as bars under them.
        //
        // THE TEN DAYS OF TEMPERATURE TAKE THE SAME ROWS, the counts' too,
        // while the key has them open.
        //
        // A ROW HIGHER WHERE THERE ARE NO BIG DIGITS. The row of air over the
        // charts is the digits' last row; on a screen too narrow for them it
        // is air twice over, and the papers are short of rows.
        let top = if readout { row - 1 } else { row - 2 };
        let below = if width > 84 { 4 } else { 0 };
        let room = hv.saturating_sub(2 + top + 2 + below);
        // Whose temperature the ten days are: the first tube that has given
        // one, or the first with a log of them.
        let temp_tube = temps.iter().position(|t| t.is_some()).or_else(|| {
            id.counters.iter().position(|c| temp_logged.contains(&c.serial_no))
        });
        let showing_temp = temp_open && temp_tube.is_some() && room >= 1 && width >= 32;
        if trail.newest() != fall_end {
            fall = trail.rows();
            fall_end = trail.newest();
        }
        // A PAPER WITH A LINE IS REASON ENOUGH. The live drum has nothing for
        // its first 512 seconds, and the days and months in the log are
        // there from the first frame; so the papers are laid out as soon as
        // any of the three has a line, and the live one says how long it
        // will be.
        let long_shown = (day_view != View::Off && long_lines(&day_rows) > 0)
            || (month_view != View::Off && long_lines(&month_rows) > 0);
        let plan = if showing_temp || (fall.is_empty() && !long_shown) || width < 16 {
            None
        } else {
            drum_rows(room)
        };
        let mut temp_words = String::new();
        if let (true, Some(k)) = (showing_temp, temp_tube) {
            if temp_read.map_or(true, |(read, tube)| {
                tube != k || read.elapsed().as_secs_f64() >= TEMP_REREAD
            }) {
                let serial = id.counters.get(k).map(|c| c.serial_no.as_str()).unwrap_or("");
                temp_log = log::read_temps(
                    dir, serial, now_wall - (TEMP_DAYS as f64 + 1.0) * 86400.0);
                // What came down the socket and may not be on the disk yet.
                temp_log.extend(temps.get(k).copied().flatten());
                temp_read = Some((Instant::now(), k));
            }
            let words = draw_temp_drum(&mut out, top, room + 2, width, &temp_log, now_wall, depth);
            temp_words = format!(
                "temperature   {}   {}",
                words, id.counters.get(k).map(|c| c.serial_no.as_str()).unwrap_or("")
            );
            row = top + room + 2;
        } else if let Some((paper_rows, _)) = plan {
            // THE LONG PAPERS, READ AGAIN WHEN THEY ARE DUE. A day's line and
            // a month's are made from the count log and cost up to a second,
            // so they are read every five minutes and only for a paper that
            // is on the screen in some form; a key that turns one on reads
            // it then.
            for (which, view, lines, read) in [
                (longdrum::Paper::Day, day_view, &mut day_rows, &mut day_read),
                (longdrum::Paper::Month, month_view, &mut month_rows, &mut month_read),
            ] {
                if view != View::Off
                    && read.map_or(true, |r: Instant| r.elapsed().as_secs_f64() >= LONG_REREAD)
                {
                    let asked = if which == longdrum::Paper::Day { DAY_LINES } else { MONTH_LINES };
                    *lines = longdrum::rows(dir, which, now_wall, asked);
                    *read = Some(Instant::now());
                }
            }
            let rooms = drum_rooms(
                paper_rows,
                (day_view, long_lines(&day_rows)),
                (month_view, long_lines(&month_rows)),
            );
            let cols = width - 1;
            let ground = bytes_of(DRUM_GROUND);
            // A row of words on the papers' own ground, so the stack reads
            // as one sheet with quiet rules in it and not as three boxes.
            let ruled = |out: &mut String, y: usize, words: &str, name: &str| {
                let pad = cols.saturating_sub(words.chars().count());
                out.push_str(&format!(
                    "{}{}{}{}{}{}{}{}{}{}{}",
                    at(y, 0), ground_ink(depth, ground), DIM, words, " ".repeat(pad), OFF,
                    at(y, 0), ground_ink(depth, ground),
                    ink(depth, bytes_of(drum::heat(drum::SHADES / 3))), name, OFF
                ));
            };
            let mut y = top;
            for (which, lines, deep_rows, folded) in [
                (longdrum::Paper::Month, &month_rows, rooms.month, rooms.month_folded),
                (longdrum::Paper::Day, &day_rows, rooms.day, rooms.day_folded),
            ] {
                if deep_rows > 0 {
                    let drawn_lines = (deep_rows * 2).min(long_lines(lines));
                    let axis = long_axis(which, lines);
                    let mut paper = Paper::new(cols, deep_rows);
                    draw_long(&mut paper, &lines[..drawn_lines], &axis, long_luck());
                    paper.render(&mut out, y, 0, depth, Some(ground));
                    // The dates, small, over the left edge: the longest
                    // periods are under them, where a line has a point
                    // every few characters and the least to lose.
                    let starts = long_starts(which, &lines[..drawn_lines], now_wall);
                    for (down, date) in long_date_labels(which, &starts, deep_rows) {
                        out.push_str(&format!(
                            "{}{}{}{} {}", at(y + down, 0), ground_ink(depth, ground), DIM, date, OFF
                        ));
                    }
                    y += deep_rows;
                    let words = long_axis_words(which, &axis, cols);
                    ruled(&mut out, y, &words, long_name(which));
                    y += 1;
                } else if folded {
                    let words = folded_words(which, lines, cols, depth);
                    out.push_str(&format!(
                        "{}{}{}{}{}", at(y, 0), ground_ink(depth, ground), " ".repeat(cols), OFF,
                        at(y, 0)
                    ));
                    // Each part of the row ends its own colour, which ends
                    // the ground with it; so the ground is said again
                    // before each.
                    out.push_str(&ground_ink(depth, ground));
                    out.push_str(&words.replace(OFF, &format!("{}{}", OFF, ground_ink(depth, ground))));
                    out.push_str(&format!(
                        "{}{}{}{}{}", OFF, at(y, 0), ground_ink(depth, ground),
                        ink(depth, bytes_of(drum::heat(drum::SHADES / 3))), long_name(which)
                    ));
                    out.push_str(OFF);
                    y += 1;
                }
            }
            let answered: Vec<&analysis::Spectrum> =
                deep.iter().filter(|s| s.runs > 0).collect();
            let longest = answered.iter().map(|s| s.window).max().unwrap_or(drum::FALL_WINDOW);
            let mut paper = Paper::new(cols, rooms.live);
            let floor = drum::period_floor(drum::FALL_WINDOW, longest, paper.width() as f64);
            let axis = drum::stretches(floor, longest);
            let luck = trail.chance_max() as f32;
            draw_drum(&mut paper, &Drum {
                rows: &fall,
                axis: &axis,
                luck,
                slide: trail.age() as f32 / drum::FALL_HOP as f32,
            });
            paper.render(&mut out, y, 0, depth, Some(ground));
            y += rooms.live;
            if fall.is_empty() && rooms.live > 0 {
                let words = format!(
                    "now   the first row in {}s",
                    (drum::FALL_WINDOW as u64).saturating_sub(trail.total)
                );
                ruled(&mut out, y - 1, &words, "now");
            }
            // The hours-long average, on its own strip under the rows and to
            // their scale.
            let mut strip = Paper::new(cols, rooms.average);
            draw_average(&mut strip, &drum::front_row(&answered, floor), &axis,
                         drum::loudest(&fall, &axis, luck), luck);
            strip.render(&mut out, y, 0, depth, Some(ground));
            out.push_str(&format!(
                "{}{}{}{}", at(top + paper_rows, 0), DIM, axis_words(&axis, cols), OFF
            ));
            row = top + paper_rows + 2;
        }
        let counts_rows = match plan {
            Some((_, counts)) => counts,
            None => if hv > row + 12 { 5 } else if hv > row + 8 { 3 } else { 1 },
        };
        // THE COUNTS, COMPRESSING AS THEY AGE. A second a bar on the right,
        // then k seconds, then k*k and k*k*k, reaching back as far as the
        // spectrum's window -- see analysis::tiers. Eleven tiers of equal
        // width, so each one leftwards is another doubling, out to 1024
        // seconds a bar. One scale for all of them, so the same height is
        // the same rate wherever it is drawn.
        let series: Vec<f64> = merged.iter().copied().collect();
        // WHOLE SECONDS, AND ONE TIER BELOW THEM FOR THE INTERLEAVE. One tube
        // keeps the strip it always had; see analysis::tiers_interleaved.
        //
        // SEVERAL TUBES ARE CUT BY THE CLOCK, and only the ones answering
        // are interleaved: see analysis::tiers_arrivals.
        let live = analysis::answering(&heard, &gone, when);
        let mut strip = if tubes > 1 {
            arrivals.make_contiguous();
            analysis::tiers_arrivals(arrivals.as_slices().0, &live, width - 1).tiers
        } else {
            analysis::tiers_with(&series, dropped, width - 1, spec.window, analysis::TIERS, 1.0)
        };
        if let (1, Some(from_log)) = (tubes, prefilled) {
            blank_fine(&mut strip, (dropped + series.len()) as i64, from_log as i64);
        }
        let peak = strip
            .iter()
            .flat_map(|t| t.values.iter().flatten())
            .cloned()
            .fold(0.0f64, f64::max);
        let every = logging.as_ref().map(|wl| wl.every.round().max(1.0) as i64);
        // Where the one-second tier ends, as a count of seconds: of samples
        // with one tube, and of the clock with several -- less the seconds
        // the interleave tier has taken from the end of it.
        let n = if tubes > 1 {
            let taken = if live.iter().filter(|l| **l).count() > 1 {
                analysis::INTERLEAVE_SECONDS as i64
            } else {
                0
            };
            when.floor() as i64 + 1 - taken
        } else {
            (dropped + series.len()) as i64
        };
        let mut x0 = 0;
        // Not under the ten days of temperature, which have these rows.
        for (ti, tier) in strip.iter().enumerate().filter(|_| !showing_temp) {
            for (i, line) in bar_rows_to(&tier.values, counts_rows, peak).iter().enumerate() {
                out.push_str(&at(row + i, x0));
                let mut pen = "";
                for (x, &g) in line.iter().enumerate() {
                    if g == ' ' {
                        out.push(' ');
                        continue;
                    }
                    let rate = tier.values[x].unwrap_or(0.0) * 60.0;
                    let want = colour_for(level(rate));
                    if want != pen {
                        out.push_str(want);
                        pen = want;
                    }
                    out.push(g);
                }
                out.push_str(OFF);
            }
            // Above each tier, where it starts: F for each hand-over from a
            // finer tier, how long a bar is, and how far back the tier reaches.
            if row >= 1 && tier.columns > 2 {
                let label = tier_label(ti, tier.seconds, tier.columns);
                let used = label.chars().count();
                out.push_str(&format!("{}{}{}{}", at(row - 1, x0), DIM, label, OFF));
                // Over the fine tier, a tick where each log row closes: the
                // frames the log is cut into, scrolling left with the counts.
                if let (Some(e), true) = (every, tier.seconds == 1.0) {
                    for j in used + 1..tier.columns {
                        let a = n - tier.columns as i64 + j as i64;
                        if a > 0 && a % e == 0 {
                            out.push_str(&format!("{}{}\u{a6}{}", at(row - 1, x0 + j), DIM, OFF));
                        }
                    }
                }
            }
            x0 += tier.columns;
        }
        // With the paper over them the counts are the last chart, and the
        // random line is a row of air under them; without it the spectrum's
        // bars come next, on the row after the air.
        let papered = showing_temp || plan.is_some();
        if !showing_temp {
            row += counts_rows + if papered { 0 } else { 1 };
        }

        // The spectrum: flat is the good answer.
        let mut footer = String::new();
        let rel = spec.relative();
        if rel.is_empty() {
            footer = format!(
                "spectrum   accumulating, {}s to the first window of {}",
                spec.wait(), spec.window
            );
        } else if papered || hv > row + 4 {
            let cols = spectrum_columns(&rel, width - 1);
            let (top, where_) = spec.loudest();
            let luck = spec.chance_max();
            footer = if top < luck * 1.25 {
                format!(
                    "spectrum   flat -- arrivals look random, as decay should ({} windows)",
                    spec.runs
                )
            } else {
                format!(
                    "spectrum   peak at {:.0}s, {:.1}x the mean (chance gives {:.1}x), {:.1} sigma ({} windows)",
                    spec.period(where_), top, luck, spec.sigma(top), spec.runs
                )
            };
            // THE BARS ONLY WHERE THERE IS NO PAPER. With the drum drawn the
            // spectrum is its front row, and the words under the screen are
            // still the verdict on it.
            let bars = if papered {
                0
            } else if hv > row + 9 {
                5
            } else if hv > row + 7 {
                3
            } else {
                1
            };
            for (i, line) in bar_rows(&cols, cols.len(), bars).iter().enumerate().take(bars) {
                out.push_str(&at(row + i, 0));
                let mut pen = "";
                for (x, &g) in line.iter().enumerate() {
                    if g == ' ' {
                        out.push(' ');
                        continue;
                    }
                    let v = cols[x];
                    let want = if v >= luck * 2.0 { RED } else if v >= luck * 1.25 { YELLOW } else { CYAN };
                    if want != pen {
                        out.push_str(want);
                        pen = want;
                    }
                    out.push(g);
                }
                out.push_str(OFF);
            }
            row += bars;
            if !papered {
                let lo = format!("{}s", spec.window);
                let gap = (width - 1).saturating_sub(lo.len() + 2);
                out.push_str(&format!("{}{}{}{}2s{}", at(row, 0), DIM, lo, " ".repeat(gap), OFF));
            }
        }
        if showing_temp {
            // Cut at the edge: a line that wrapped would land on the keys.
            footer = temp_words.chars().take(width.saturating_sub(1)).collect();
        }

        // The rotating line: 256 bits of decay, when the pool has earned
        // them. See the entropy module for what "earned" is doing there --
        // it is measured from the samples, not modelled from their mean.
        if hv > row + 2 && width > 84 {
            row += 1;
            // ONLY THE PORT-HOLDER DRAWS. A client's pool is here for the
            // countdown and nothing else; its line arrives as an event, from
            // the one process that also wrote it to the emission log.
            let ready = if feed.owns_the_port() {
                pools.iter().position(|p| p.ready())
            } else {
                None
            };
            if let Some(k) = ready {
                let (top, _) = spec.loudest();
                suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
                // Before the draw: see the note in `service`.
                let keep = logging.as_ref().map(|w| w.frames).unwrap_or(false);
                let frame = keep.then(|| pools[k].frame(pools[k].seq, suspect));
                let (text, record) = pools[k].draw();
                let at_time = clock::format(clock::now(), "%H:%M:%S");
                let serial = id.counters.get(k).map(|c| c.serial_no.as_str()).unwrap_or("");
                if let Some(wl) = logging.as_ref() {
                    let _ = entropy::write_record(&wl.dir, &record, serial, suspect);
                    if let Some(f) = frame.as_ref() {
                        let _ = entropy::write_frame(&wl.dir, f, serial);
                    }
                }
                feed.publish_random(k, &text, &at_time, suspect);
                shown = Some((text.clone(), at_time));
            }
            match &shown {
                Some((text, at_time)) => {
                    out.push_str(&format!("{}{}random   {}{}",
                                          at(row, 0), CYAN,
                                          entropy::group_hex(text), OFF));
                    if hv > row + 2 {
                        // THE COUNTDOWN KEEPS RUNNING. The line stayed on
                        // screen with no indication of whether the next one
                        // was a minute away or eight, which is the one thing
                        // somebody watching it wants to know.
                        let note = format!(
                            "{} bits from decay at {}   {}{}",
                            entropy::ENTROPY_BITS as i64, at_time,
                            entropy::pool_status(&pools[0], "next in "),
                            if suspect {
                                "  -- SPECTRUM NOT FLAT, treat as suspect"
                            } else {
                                ""
                            }
                        );
                        out.push_str(&format!("{}{}{}{}", at(row + 1, 9),
                                              if suspect { YELLOW } else { DIM },
                                              note, OFF));
                    }
                }
                None => out.push_str(&format!(
                    "{}{}random   {}{}", at(row, 0), DIM,
                    entropy::pool_status(&pools[0], "next in "), OFF
                )),
            }
            // THE CLOCK SITS ON THE TABLE, when there is one.
            //
            // It used to be pinned two rows under the random line, which is
            // where it belongs when nothing follows it: it does not jump as
            // the first emission arrives, and a screenshot says when it was
            // taken. But the table is anchored to the BOTTOM of the screen,
            // and on a short one the content above ran out first -- leaving a
            // blank row between the clock and the log rows, with everything
            // else pushed tight. Two things anchored to opposite ends of the
            // same gap is how that happens.
            //
            // So when the table is drawn the clock goes directly above it,
            // which is also where it reads best: the clock says when, and the
            // rows under it say what went to disk at that time. `max` keeps
            // it clear of the random line on a screen tall enough for the
            // content to reach down that far.
            let clock_row = if tl > 0 {
                h.saturating_sub(3 + tl).max(row + 2)
            } else {
                row + 2
            };
            if clock_row < hv.saturating_sub(2) {
                out.push_str(&format!("{}{}clock{}    {}", at(clock_row, 0), DIM, OFF,
                                      clock::format(clock::now(), "%Y-%m-%d %H:%M:%S")));
                // Who else is reading this counter through us. Worth a word
                // because the whole point of the socket is invisible
                // otherwise -- a GUI attaching shows up here and nowhere else.
                match feed.watchers() {
                    0 => {}
                    1 => out.push_str(&format!("{}{}1 attached{}", at(clock_row, 24), DIM, OFF)),
                    n => out.push_str(&format!("{}{}{} attached{}", at(clock_row, 24), DIM, n, OFF)),
                }
                // What the log is doing, beside it: what the backfill found,
                // or that rows are not reaching the disk.
                if !table_note.is_empty() && width > 40 + table_note.len() {
                    let tint = if table_note.starts_with("NOT") { YELLOW } else { DIM };
                    out.push_str(&format!("{}{}{}{}", at(clock_row, 32), tint, table_note, OFF));
                }
            }
        }

        if tl > 0 {
            draw_table(&mut out, h - 2 - tl, tl, width, &names, &table);
        }
        if h >= 2 {
            out.push_str(&format!("{}{}{}{}", at(h - 2, 0), DIM, footer, OFF));
            // The keys, and the one for the temperature only where there
            // is a temperature to show.
            let mut keys = String::from("q to quit");
            if temp_tube.is_some() {
                keys.push_str(if showing_temp { "   t counts" } else { "   t temperature" });
            }
            // The papers' keys where there is a line for them to show, and
            // what each paper is now.
            if plan.is_some() || (!showing_temp && !fall.is_empty()) {
                if long_lines(&day_rows) > 0 {
                    keys.push_str(&format!("   d days {}", day_view.word()));
                }
                if long_lines(&month_rows) > 0 {
                    keys.push_str(&format!("   m months {}", month_view.word()));
                }
            }
            let keys: String = keys.chars().take(width.saturating_sub(1)).collect();
            out.push_str(&format!("{}{}{}{}", at(h - 1, 0), DIM, keys, OFF));
        }
        print!("{}", out);
        let _ = std::io::stdout().flush();


        if let Some(d) = duration {
            // HOW LONG THIS HAS BEEN WATCHING, not how much counter it holds.
            // The two were the same number until a monitor could be handed
            // eight hours of history on the way in, at which point
            // `--duration 10` stopped after the replay and before the first
            // live sample.
            if watching.elapsed().as_secs_f64() >= d {
                break;
            }
        }
    }
    for (k, lg) in loggers.iter_mut().enumerate() {
        let averages: Vec<Option<f64>> = spans.iter().map(|s| each[k].average(*s)).collect();
        lg.finish(&averages);
    }
    if let Some(m) = merged_log.as_mut() {
        m.finish(&w);
    }
    if let Some(wl) = logging.as_ref() {
        if wl.export_every.is_some() {
            export_pages(&wl.dir, cpm_per_usvh);
        }
    }
    // Only if this end started it: a monitor closing must not silence the
    // counter for a service that is still logging it.
    feed.stop();
}

/// Rows of log the monitor keeps for its table: more than any screen shows.
const TABLE_KEEP: usize = 200;

// ------------------------------------------------------------------ main ---

/// Log to disk, a row every `every` seconds, until told to stop.
///
/// WAITING IS NOT RETRYING, and what separates them is the device. An absent
/// counter does not become present because a daemon asked again, so that case
/// writes down why and exits 0 -- a stopped service, not a crash loop. A port
/// that is present but LOCKED is the opposite: the counter is right there,
/// somebody is watching it in a monitor, and they will close it. One open()
/// every ten seconds picks the log back up the moment they do.
#[allow(clippy::too_many_arguments)]
fn service(spans: &[f64], every: f64, duration: Option<f64>,
           devices: &[String], baud: Option<u32>,
           logs: Option<std::path::PathBuf>,
           backfill: Option<(usize, f64)>, frames: bool,
           temp_every: Option<f64>, spectrum_log: bool) -> i32 {
    install_stop_handler();

    // WHERE THIS SERVICE'S STATUS BELONGS, settled before anything is written:
    // a service on its own --logs has its own status, and must not report
    // itself into somebody else's.
    let dir = logs.unwrap_or_else(log::state_dir);
    let _ = std::fs::create_dir_all(&dir);

    let started = clock::now();
    let mut waiting = false;
    let found = loop {
        let (found, why) = counter::find_all(devices, baud);
        match why {
            None => break found,
            Some(e) => {
                if !e.busy {
                    let path = log::write_status(&dir, &format!("dormant: {}", e.reason));
                    println!("radbeeper: dormant -- {}", e.reason);
                    for line in e.detail.lines() {
                        println!("    {}", line);
                    }
                    println!("    status: {}", path.display());
                    println!("    it will look again at the next boot, or when you \
                              run: rc-service radbeeper start");
                    return 0;
                }
                if !waiting {
                    waiting = true;
                    log::write_status(&dir, &format!("waiting: {}", e.reason));
                    println!("radbeeper: waiting -- {}", e.reason);
                    println!("    Logging starts by itself when the port is free.");
                }
                let mut slept = 0.0;
                while slept < SERVICE_WAIT {
                    if stopping() {
                        return 0;
                    }
                    if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
                        return 0;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    slept += 0.5;
                }
            }
        }
    };

    // The bank is made here but not started -- no reader runs until every
    // flash has been read. See Bank::start.
    let mut bank = Bank::open(found);
    bank.temp_every = temp_every;
    let mut id = bank.identity(spans);
    let tubes = bank.len();
    // Which tubes have had their first temperature said on stdout, by index.
    let mut temp_said: Vec<bool> = Vec::new();
    // THE FAN-OUT, BOUND BEFORE THE BACKFILL. This process holds the flocks,
    // so it owes the stream to everybody who wants a counter and cannot have
    // the port: a monitor, a second monitor, a GUI. It used to be bound after
    // the backfill, and a backfill once took sixteen minutes: every window
    // opened in that time found the port busy and nothing on the socket, and
    // printed `port busy` with no idea why. Bound first, a window attaches at
    // once and is told what is happening instead. A socket that will not
    // bind is not fatal -- the logging is the job, and it carries on.
    let mut srv = match broker::Server::start(&dir, &id) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("radbeeper: not serving the stream -- {}", e);
            eprintln!("    the log is unaffected; a monitor will need the port itself");
            None
        }
    };

    // EVERY COUNTER BACKFILLED FROM ITS OWN FLASH, into its own file. Two
    // tubes were both in the room while nobody was listening and both wrote
    // down what they saw; the records stay apart, because the only way to ask
    // whether two instruments agree is to have kept both their answers.
    if let Some((bytes, max_gap)) = backfill {
        let counters = bank.live();
        for (k, c) in counters.iter().enumerate() {
            let what = downloading_note(&c.serial_no, k + 1, counters.len());
            log::write_status(&dir, &what);
            println!("radbeeper: {}", what);
            let done = while_serving(srv.as_mut(), &what, || {
                backfill_at_start(c, &dir, spans, every, bytes, max_gap, false)
            });
            println!("radbeeper: {}", done);
        }
    }

    log::write_status(&dir, &format!(
        "monitoring {} ({})",
        id.counters.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", "),
        id.counters.iter().map(|c| c.version.as_str()).collect::<Vec<_>>().join(", ")
    ));
    // ONE SET OF WINDOWS AND ONE LOG PER TUBE. The service records; it does
    // not average. Averaging is a question about a display, and every display
    // that asks it can do so from the stream -- but a row that blended two
    // instruments would be a reading neither of them took, and no later
    // analysis could unpick it.
    let mut each: Vec<Windows> = (0..tubes).map(|_| Windows::new(spans)).collect();
    let mut loggers: Vec<Logger> = id
        .counters
        .iter()
        .map(|c| Logger::new(spans, dir.clone(), &c.serial_no, every))
        .collect();
    // AND ONE SET ACROSS ALL OF THEM, which is not a contradiction of the
    // note above. The per-counter files stay per-counter; this is the second
    // file, beside them, saying what the room did -- see `MergedLogger`, and
    // note that it can be taken apart again because `per_tube` is in it.
    let mut all = Windows::new(spans);
    let mut merged_log = MergedLogger::new(spans, dir.clone(), every);
    merged_log.counters(
        &id.counters.iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
    );
    // THE POOLS BELONG TO WHOEVER HOLDS THE PORTS, and until now nothing held
    // them for long: the pool lived in the monitor, so closing the window
    // threw away however many minutes of measured entropy it had gathered.
    // Here one fills per tube for as long as that tube is plugged in, and the
    // line it draws is written once, by this process, and published to every
    // window watching. Per tube and never merged: an emission is an audit
    // record of ONE source, and `radbeeper random --check` recomputes it from
    // that source's own counts.
    let mut pools: Vec<entropy::Entropy> =
        (0..tubes).map(|_| new_pool()).collect();
    // The ladder is fed the whole-second SUM across the tubes: a period is a
    // property of the room, both counters are looking at the same room, and
    // adding them is twice the signal on one time base. `suspect` is an
    // annotation on the emission record, and one written without it would be
    // quietly claiming a flat spectrum nobody checked.
    let mut ladder = Ladder::new();
    let mut sec_bin: Option<i64> = None;
    let mut sec_sum: u32 = 0;
    // THE DRUM'S ROWS, KEPT. The window draws a spectrum row every eight
    // seconds and forgets it twelve minutes later; this takes the same row
    // -- the same windows, held against the same fifty minutes -- once a
    // window, end to end, and writes it down. See `log::write_spectrum`.
    // Fed where the ladder is fed, with the same whole seconds of the room.
    let mut spec = spectrum_log.then(|| {
        analysis::Trail::new(&SPECTRUM_WINDOWS, SPECTRUM_WINDOWS[0], 1, SPECTRUM_LEVEL)
    });
    let mut spec_seconds: std::collections::VecDeque<u32> =
        std::collections::VecDeque::with_capacity(SPECTRUM_WINDOWS[0] + 1);
    let mut spec_written: Option<u64> = None;
    // THE LONG DRUMS' LINES: a day's, and a month's, written down as each
    // ends, and every one the count log can still give written now. On a
    // thread, because the first time it runs over a year of counts it reads
    // all of them, and the counters do not wait for that. See `longdrum`.
    let keep_lines = |dir: &Path| {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            for paper in [radbeeper::longdrum::Paper::Day, radbeeper::longdrum::Paper::Month] {
                match radbeeper::longdrum::keep(&dir, paper, clock::now()) {
                    Ok(0) => {}
                    Ok(n) => println!("radbeeper: {} line{} of the {} drum written to {}", n,
                                      if n == 1 { "" } else { "s" },
                                      if paper == radbeeper::longdrum::Paper::Day { "day" } else { "month" },
                                      paper.path(&dir).display()),
                    Err(e) => eprintln!("radbeeper: could not write the long drum -- {}", e),
                }
            }
        });
    };
    let mut lines_day = clock::day_start(clock::now());
    if spectrum_log {
        keep_lines(&dir);
    }

    if let Some(s) = srv.as_mut() {
        let names = log::columns(&log::header(spans));
        let mut seed: Vec<(usize, String)> = Vec::new();
        for (k, c) in id.counters.iter().enumerate() {
            let path = log::path(clock::now(), &dir, Some(&c.serial_no));
            for (_, cells) in
                log::read_table(&path, &names).into_iter().rev().take(TABLE_KEEP).rev()
            {
                seed.push((k, cells.join("\t")));
            }
        }
        s.seed_rows(seed.into_iter());
    }

    let here = log::site_at(
        id.counters.first().map(|c| c.serial_no.as_str()).unwrap_or(""),
        clock::now(),
        &loggers.first().map(|l| l.sites.clone()).unwrap_or_default(),
    );
    for c in &id.counters {
        println!("radbeeper: monitoring {} -- {}", c.path, c.version);
        println!("radbeeper: counter {} logging to {}", c.serial_no,
                 log::path(clock::now(), &dir, Some(&c.serial_no)).display());
    }
    if id.counters.len() > 1 {
        println!("radbeeper: the room, merged, to {}",
                 log::path(clock::now(), &dir, Some(log::MERGED)).display());
    }
    println!("radbeeper: {} at {}", if tubes == 1 { "counter" } else { "counters" },
             here.unwrap_or_else(|| "an unrecorded place".to_string()));
    println!("radbeeper: a row every {}s", log::g(every));
    match temp_every {
        Some(t) => {
            let with: Vec<&str> = id.counters.iter()
                .filter(|c| counter::temp_supported(&c.version))
                .map(|c| c.serial_no.as_str())
                .collect();
            if with.is_empty() {
                println!("radbeeper: no counter here has a thermometer this program knows how to ask");
            } else {
                println!("radbeeper: temperature of {} every {}s, between two seconds of counts",
                         with.join(", "), log::g(t));
            }
        }
        None => println!("radbeeper: temperature not asked (--no-temp)"),
    }
    if spectrum_log {
        println!("radbeeper: the room's spectrum, a row every {}s, to {}",
                 SPECTRUM_WINDOWS[0], log::spectrum_path(clock::now(), &dir).display());
    }
    if let Some(s) = srv.as_ref() {
        println!("radbeeper: serving the stream on {}", s.socket().display());
        println!("radbeeper: a monitor attaches to it -- the ports stay here");
    }

    // Every port conversation is over -- the backfill above ran before the
    // bank existed -- so the counters can start streaming.
    bank.start();
    let mut swept = clock::now();
    // Counters plugged in while this runs, handed back by the thread that
    // read their flash; and the serials still being read, for the note.
    let (joined_tx, joined) = std::sync::mpsc::channel::<counter::Counter>();
    let mut prober = Prober::new();
    let mut downloading: Vec<String> = Vec::new();
    let mut noted = clock::now();
    loop {
        // Whoever turned up while we were waiting for this second. Before the
        // read, so a window that has just opened is greeted within a second
        // rather than after the next sample.
        if let Some(s) = srv.as_mut() {
            s.accept_pending();
        }
        if stopping() {
            break;
        }
        if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
            break;
        }
        // ---- A COUNTER PLUGGED INTO A RUNNING SERVICE ------------------
        //
        // Once a log cycle, look for one. Plugging a second tube in used to
        // mean restarting the service, which means a hole in every tube's
        // record to pick up one of them -- and nothing about the fan-out
        // needed it: a counter is a thread, a logger and a line on the wire,
        // and all three can be made while the others are running.
        //
        // The flock does the filtering. A port this process already holds
        // fails to open exactly as another process's would, so every
        // candidate can be tried and the ones already held fall out on their
        // own, with no list to keep and no list to go stale.
        if clock::now() - swept >= RESCAN {
            swept = clock::now();
            // A day has ended since the long drums' lines were last written.
            if spectrum_log && clock::day_start(swept) != lines_day {
                lines_day = clock::day_start(swept);
                keep_lines(&dir);
            }
            let ports: Vec<String> = if devices.is_empty() {
                counter::candidate_ports()
            } else {
                devices.to_vec()
            };
            // ASKED ON THREADS OF THEIR OWN, and collected below when they
            // have answered: see `Prober`. A port that has crashed takes
            // its timeouts there, and nobody here waits for them.
            prober.sweep(ports, &bank.live_paths(), baud);
        }
        {
            for c in prober.found() {
                // ITS OWN FLASH FIRST, exactly as at start: this tube was
                // recording while nobody was listening to it. And exactly as
                // at start, BEFORE IT IS ADOPTED -- adopting a counter into a
                // running bank starts its reader, and a reader running during
                // the download eats the flash as counts. That is what this
                // used to do, and it is what put 16383 counts a second (the
                // count mask, every bit set) and a second of 4.5 million into
                // the log on 2026-09-23. See Bank::start.
                //
                // ON A THREAD OF ITS OWN, so the tubes already here go on
                // being logged and served while it reads. The thread holds
                // the flock, so the next sweep finds this port busy and
                // leaves it alone, and it hands the counter back when done.
                let Some((bytes, max_gap)) = backfill else {
                    let _ = joined_tx.send(c);
                    continue;
                };
                let what = downloading_note(&c.serial_no, 1, 1);
                println!("radbeeper: {} on {}", what, c.path);
                log::write_status(&dir, &what);
                downloading.push(c.serial_no.clone());
                if let Some(s) = srv.as_mut() {
                    s.note(&what);
                }
                let (tx, dir, spans) = (joined_tx.clone(), dir.clone(), spans.to_vec());
                std::thread::spawn(move || {
                    println!("radbeeper: {}",
                             backfill_at_start(&c, &dir, &spans, every, bytes, max_gap, false));
                    let _ = tx.send(c);
                });
            }
        }
        while let Ok(c) = joined.try_recv() {
            downloading.retain(|s| *s != c.serial_no);
            let (path, version, serial) =
                (c.path.clone(), c.version.clone(), c.serial_no.clone());
            let who = bank.adopt(c);
            if who == each.len() {
                each.push(Windows::new(spans));
                pools.push(new_pool());
                loggers.push(Logger::new(spans, dir.clone(), &serial, every));
            } else {
                // A tube that has come back: its windows start again, its
                // log does not.
                each[who] = Windows::new(spans);
            }
            // THE ONE LIST, REFRESHED, NOT A SECOND ONE IN ITS SHADOW. This
            // used to be `let id`, a new list that lived to the end of this
            // block; the writer of the random records further down kept
            // indexing the list taken at start, and a tube that joined by
            // hot-plug had no entry in it. Seven minutes after the second
            // counter joined -- when its pool first had a record to draw --
            // the service died of an index out of bounds (2026-09-30).
            id = bank.identity(spans);
            merged_log.counters(
                &id.counters.iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
            );
            if let (Some(s), Some(c)) = (srv.as_mut(), id.counters.get(who)) {
                s.announce(who, c);
            }
            // EVERY LIVE COUNTER, NOT THE ONE THAT JOINED. The line at start
            // names them all, and `cat status` after a rescan adopted the
            // second tube read "monitoring /dev/ttyUSB1" as though the first
            // had gone (2026-09-30).
            log::write_status(&dir, &format!(
                "monitoring {} ({})",
                id.counters.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", "),
                id.counters.iter().map(|c| c.version.as_str()).collect::<Vec<_>>().join(", ")
            ));
            println!("radbeeper: {} joined -- {} ({})", path, version, serial);
            if downloading.is_empty() {
                if let Some(s) = srv.as_mut() {
                    s.note("");
                }
            }
        }
        // Still reading a flash: say so again, so a monitor attached to a
        // service whose only tube is the one being read does not take the
        // silence for the service having gone.
        if !downloading.is_empty() && clock::now() - noted >= 1.0 {
            noted = clock::now();
            if let Some(s) = srv.as_mut() {
                s.accept_pending();
                s.note(&downloading_note(&downloading.join(", "), 1, 1));
            }
        }
        let wait = if downloading.is_empty() { 2500 } else { 500 };
        let (who, when, counts) = match bank.next(Duration::from_millis(wait)) {
            Some(Tube::Sample { who, when, counts }) => (who, when, counts),
            // A TUBE GOING QUIET IS NOT THE END OF THE SERVICE any more. Its
            // port is released so it can be taken again, everybody watching
            // is told, and the sweep above will pick it back up when it is
            // plugged in. Nothing is exited: a service that stopped logging
            // the counters still present because one was unplugged would be
            // the worst possible reading of "one counter went away".
            Some(Tube::Gone { who }) => {
                bank.retire(who);
                if let Some(s) = srv.as_mut() {
                    s.publish_gone(who);
                }
                let name = bank.ids.get(who).map(|c| c.path.clone()).unwrap_or_default();
                println!("radbeeper: {} stopped answering -- waiting for it", name);
                log::write_status(&dir, &format!("waiting: {} stopped answering", name));
                continue;
            }
            // A TEMPERATURE IS NOT A SECOND OF COUNTS and goes nowhere a
            // second of counts goes: not into a window, a pool, the spectrum
            // or a row. It is told to whoever is attached and written to the
            // counter's own temperature file, and a failure to write it is a
            // line on stderr -- the counts are the record.
            Some(Tube::Temp { who, when, celsius }) => {
                let serial = id.counters.get(who).map(|c| c.serial_no.clone()).unwrap_or_default();
                if let Some(s) = srv.as_mut() {
                    s.publish_temp(who, when, celsius);
                }
                if temp_said.len() <= who {
                    temp_said.resize(who + 1, false);
                }
                if !temp_said[who] {
                    temp_said[who] = true;
                    println!("radbeeper: counter {} reads {:.1} C, logging to {}", serial, celsius,
                             log::temp_path(when, &dir, &serial).display());
                }
                if let Err(e) = log::write_temp(&dir, &serial, when, celsius) {
                    eprintln!("radbeeper: could not write the temperature -- {}", e);
                }
                continue;
            }
            None => continue,
        };
        each[who].add(when, counts);
        all.add(when, counts);
        if let Some(s) = srv.as_mut() {
            s.publish_sample(who, when, counts);
        }
        // The spectrum's second closes when the wall clock's does, and takes
        // whatever every tube put in it.
        let this = when.floor() as i64;
        match sec_bin {
            Some(b) if b == this => sec_sum += counts,
            Some(closed) => {
                ladder.add(sec_sum);
                if let Some(trail) = spec.as_mut() {
                    trail.add(sec_sum);
                    spec_seconds.push_back(sec_sum);
                    if spec_seconds.len() > SPECTRUM_WINDOWS[0] {
                        spec_seconds.pop_front();
                    }
                    let newest = trail.newest();
                    if newest.is_some() && newest != spec_written {
                        spec_written = newest;
                        if let Some(row) = trail.rows().into_iter().next() {
                            let cps = spec_seconds.iter().map(|c| *c as f64).sum::<f64>()
                                / spec_seconds.len().max(1) as f64;
                            // The second that closed the window, by the wall.
                            if let Err(e) = log::write_spectrum(
                                &dir, (closed + 1) as f64, spec_seconds.len(),
                                bank.live().len(), cps, row.level, &row.bins,
                            ) {
                                eprintln!("radbeeper: could not write the spectrum -- {}", e);
                            }
                        }
                    }
                }
                sec_bin = Some(this);
                sec_sum = counts;
            }
            None => {
                sec_bin = Some(this);
                sec_sum = counts;
            }
        }
        pools[who].add_at(when, counts);
        // The line, when a pool has earned it: written once, here, against the
        // tube that earned it, and sent to every window so they all show the
        // same hex.
        if pools[who].ready() {
            let spec = ladder.best();
            let (top, _) = spec.loudest();
            let suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
            // THE FRAME IS TAKEN BEFORE THE DRAW, because drawing empties the
            // pool. It is the raw material the key came out of -- every
            // second, unclamped, with the gaps where the counter was away.
            let frame = frames.then(|| pools[who].frame(pools[who].seq, suspect));
            let (text, record) = pools[who].draw();
            let at_time = clock::format(clock::now(), "%H:%M:%S");
            // BY LOOKUP, NOT BY INDEX: see the note where a tube joins.
            let serial = id.counters.get(who).map(|c| c.serial_no.as_str()).unwrap_or("");
            let _ = entropy::write_record(&dir, &record, serial, suspect);
            if let Some(f) = frame.as_ref() {
                if let Err(e) = entropy::write_frame(&dir, f, serial) {
                    eprintln!("radbeeper: could not write the frame -- {}", e);
                }
            }
            if let Some(s) = srv.as_mut() {
                s.publish_random(who, &text, &at_time, suspect);
            }
        }
        let averages: Vec<Option<f64>> =
            spans.iter().map(|s| each[who].average(*s)).collect();
        match loggers[who].add(when, counts, &averages) {
            Ok(Some(line)) => {
                if let Some(s) = srv.as_mut() {
                    s.publish_row(who, &line);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("radbeeper: could not write the log -- {}", e);
                break;
            }
        }
        // AND THE MERGE, WHOSE FAILURE IS NOT FATAL. The per-counter files
        // are the record and a service that cannot write them should stop;
        // this one is the second view of the same seconds, and losing it is
        // worth a line on stderr and nothing else.
        if let Err(e) = merged_log.add(who, when, counts, &all) {
            eprintln!("radbeeper: could not write the merged log -- {}", e);
        }
    }
    for (k, lg) in loggers.iter_mut().enumerate() {
        let averages: Vec<Option<f64>> = spans.iter().map(|s| each[k].average(*s)).collect();
        lg.finish(&averages);
    }
    merged_log.finish(&all);
    bank.stop();
    log::write_status(&dir, "stopped");
    0
}

/// What the service says while it reads a flash: in its status file, on the
/// socket, and on its own stdout.
fn downloading_note(serial: &str, k: usize, n: usize) -> String {
    let of = if n > 1 { format!(" ({} of {})", k, n) } else { String::new() };
    format!("downloading history from {}{} -- live logging starts when it is done", serial, of)
}

/// Do `work` -- a flash download, which has taken sixteen minutes -- while a
/// second thread keeps the socket answering: letting in whoever attaches and
/// telling everyone, once a second, what is going on and for how long.
///
/// THE SECOND THREAD TOUCHES THE SOCKET AND NOTHING ELSE. The port belongs to
/// `work` for as long as it runs; nothing here reads a sample or writes a log.
fn while_serving<T>(srv: Option<&mut broker::Server>, what: &str,
                    work: impl FnOnce() -> T) -> T {
    let Some(s) = srv else { return work() };
    let done = std::sync::atomic::AtomicBool::new(false);
    let done = &done;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let started = clock::now();
            let mut said = 0.0f64;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                s.accept_pending();
                let t = clock::now() - started;
                if t - said >= 1.0 || said == 0.0 {
                    said = t.max(f64::MIN_POSITIVE);
                    let t = t as u64;
                    s.note(&format!("{} ({}m {:02}s)", what, t / 60, t % 60));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            s.note("");
        });
        let r = work();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        r
    })
}

/// Read the tail of the counter's flash into the log, before anything live is
/// appended: backfilled rows belong in the past and the merge rewrites the
/// file. The service starts when a counter is plugged in or the machine comes
/// up, and the monitor when somebody sits down at it -- which is exactly when
/// the counter has been recording somewhere this log was not. The flash is a
/// ring, and the gaps are wherever nothing was listening: `history::backfill`
/// finds the newest end of the ring and fills only the slots that are empty.
///
/// AND THE COUNTER'S CLOCK IS SET, AFTER THE DOWNLOAD, when it is out by
/// more than `CLOCK_SLACK`. The flash is stamped by the counter's own clock,
/// so the offset that places its rows must be the one the rows were recorded
/// under: measure, download, and only then correct. A counter whose battery
/// was pulled comes back reading the year 2000, and every start is the one
/// moment something with the right time is holding its port. It is set to
/// THIS machine's clock, synchronised or not, because this machine's clock
/// is the one the log is written in -- see `clock_cmd` for the warning a
/// person gets when that clock is nothing to copy.
///
/// One line saying what happened, for whoever is showing it.
fn backfill_at_start(c: &counter::Counter, dir: &Path, spans: &[f64], every: f64,
                     bytes: usize, max_gap: f64, quiet: bool) -> String {
    let before = c.clock_ahead();
    let offset = before.map(|o| -o.ahead).unwrap_or(0.0);
    let blob = read_history_tail(c, bytes, quiet);
    let mut note = if blob.is_empty() {
        "backfill skipped -- the counter returned no history".to_string()
    } else {
        let sites = log::read_sites(dir);
        let r = history::backfill(&blob, spans, every, max_gap, offset, dir,
                                  Some(c.serial_no.as_str()), &sites, None);
        format!("backfill -- {} samples, {} rows, {} added, {} already logged",
                r.samples, r.rows, r.added, r.clashed)
    };
    match before {
        Some(o) if o.ahead.abs() > CLOCK_SLACK => {
            note.push_str(&match aim_clock(c) {
                Ok(after) => format!("; clock set -- was {} at {}, now {}",
                                     describe_drift(o.ahead),
                                     clock::format(o.theirs, "%Y-%m-%d %H:%M:%S"),
                                     describe_offset(&after)),
                Err(e) => format!("; clock NOT set -- was {}, and {}",
                                  describe_drift(o.ahead), e),
            });
        }
        Some(_) => {}
        None => note.push_str("; clock not read -- the counter did not answer <GETDATETIME>>"),
    }
    log::note_start(dir, &c.serial_no, &note);
    note
}

/// `radbeeper probe`: every counter on the machine, in one listing.
///
/// TWO PLACES A COUNTER CAN BE, AND BOTH ARE ASKED. A service holding a
/// port introduces its counters over the socket, and interrupting its
/// stream to ask one a question would cost the log a sample -- so those are
/// listed from what the service says, without the battery and the clock.
/// Whatever is on a free port is opened and asked everything. `-d` narrows
/// either list. With the service holding one tube and the other on a port
/// it has not swept yet, the old `probe` showed one and said nothing of the
/// other, and which one depended on the moment (2026-09-30).
/// One counter as `now` reports it.
struct NowRow {
    id: broker::CounterId,
    live: bool,
    /// (when, counts) for every second the service replayed.
    seconds: Vec<(f64, u32)>,
    temp: Option<(f64, f64)>,
}

impl NowRow {
    /// Counts a minute over the newest `span` seconds this counter gave, and
    /// how many seconds that was. None before it has given any.
    fn cpm(&self, span: f64) -> Option<(f64, usize)> {
        let newest = self.seconds.last()?.0;
        let (sum, n) = self
            .seconds
            .iter()
            .rev()
            .take_while(|(w, _)| newest - *w < span)
            .fold((0u64, 0usize), |(s, n), (_, c)| (s + *c as u64, n + 1));
        (n > 0).then(|| (sum as f64 * 60.0 / n as f64, n))
    }
}

/// `radbeeper now`: what the running service has for each counter, asked of
/// the service and never of the counter.
///
/// THIS IS THE QUESTION ANOTHER PROGRAM ASKS. The port belongs to the
/// service, and a second program that wants a counter's rate or its
/// temperature must not open it -- two readers on one tty each get a share
/// of the bytes. Everything it could want is already on the socket: the
/// service replays its last seconds and its newest temperatures to whoever
/// attaches, then says `live`. This attaches, reads to that line, does the
/// arithmetic and leaves. It costs the service one write.
///
/// CPM HERE IS OURS, over the newest minute and the newest five, from the
/// seconds themselves -- not the counter's own <GETCPM>>, for the reasons
/// `cpm_cmd` gives. The time beside each is how old the newest second is, so
/// a reader can tell a counter that is counting from one that stopped.
///
/// Exit 0 with something to say, 1 when no service is serving here.
fn now_cmd(dir: &Path, cpm_per_usvh: f64, json: bool) -> i32 {
    let Some(mut client) = broker::Client::attach(dir) else {
        eprintln!("radbeeper: nothing is serving {}", broker::socket_path(dir).display());
        eprintln!("    `now` reads the running service; start one, or ask the counter");
        eprintln!("    itself with: radbeeper probe");
        return 1;
    };
    let mut rows: Vec<NowRow> = client
        .identity()
        .counters
        .iter()
        .map(|c| NowRow { id: c.clone(), live: true, seconds: Vec::new(), temp: None })
        .collect();
    fn slot(rows: &mut Vec<NowRow>, who: usize) -> Option<&mut NowRow> {
        rows.get_mut(who)
    }
    let mut note = String::new();
    loop {
        match client.next(Duration::from_secs(5)) {
            Some(broker::Event::Sample { who, when, counts }) => {
                if let Some(r) = slot(&mut rows, who) {
                    r.seconds.push((when, counts));
                }
            }
            Some(broker::Event::Temp { who, when, celsius }) => {
                if let Some(r) = slot(&mut rows, who) {
                    r.temp = Some((when, celsius));
                }
            }
            Some(broker::Event::Counter { who, id }) => {
                if who < rows.len() {
                    rows[who].id = id;
                    rows[who].live = true;
                } else {
                    rows.push(NowRow { id, live: true, seconds: Vec::new(), temp: None });
                }
            }
            Some(broker::Event::Gone { who }) => {
                if let Some(r) = slot(&mut rows, who) {
                    r.live = false;
                }
            }
            Some(broker::Event::Note { text }) => note = text,
            Some(broker::Event::Live) | None => break,
            Some(_) => {}
        }
    }
    let now = clock::now();
    // A counter whose newest second is old has stopped, whatever the
    // greeting said: the replay does not carry who has gone.
    for r in rows.iter_mut() {
        if r.seconds.last().map(|(w, _)| now - *w > 10.0).unwrap_or(true) {
            r.live = false;
        }
    }
    let num = |v: Option<f64>, places: usize| match v {
        Some(x) => format!("{:.*}", places, x),
        None => if json { "null".to_string() } else { String::new() },
    };
    if json {
        let mut out = format!("{{\"time\":{:.3},\"cpm_per_usvh\":{},\"note\":\"{}\",\"counters\":[",
                              now, cpm_per_usvh, note.replace(['"', '\\'], "'"));
        for (k, r) in rows.iter().enumerate() {
            let m1 = r.cpm(60.0);
            let m5 = r.cpm(300.0);
            if k > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"serial\":\"{}\",\"version\":\"{}\",\"port\":\"{}\",\"live\":{},\
                 \"cpm\":{},\"cpm_seconds\":{},\"cpm_300\":{},\"cpm_300_seconds\":{},\
                 \"usvh\":{},\"age\":{},\"temp_c\":{},\"temp_age\":{}}}",
                r.id.serial_no, r.id.version, r.id.path, r.live,
                num(m1.map(|m| m.0), 1), m1.map(|m| m.1).unwrap_or(0),
                num(m5.map(|m| m.0), 1), m5.map(|m| m.1).unwrap_or(0),
                num(m5.map(|m| m.0 / cpm_per_usvh), 4),
                num(r.seconds.last().map(|(w, _)| (now - *w).max(0.0)), 1),
                num(r.temp.map(|t| t.1), 1),
                num(r.temp.map(|t| (now - t.0).max(0.0)), 1),
            ));
        }
        out.push_str("]}");
        println!("{}", out);
        return 0;
    }
    if !note.is_empty() {
        println!("# {}", note);
    }
    println!("#serial\tcpm\tcpm_300\tusvh\tage\ttemp_c\ttemp_age\tlive\tport\tversion");
    for r in &rows {
        let m1 = r.cpm(60.0);
        let m5 = r.cpm(300.0);
        println!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                 r.id.serial_no,
                 num(m1.map(|m| m.0), 1),
                 num(m5.map(|m| m.0), 1),
                 num(m5.map(|m| m.0 / cpm_per_usvh), 4),
                 num(r.seconds.last().map(|(w, _)| (now - *w).max(0.0)), 1),
                 num(r.temp.map(|t| t.1), 1),
                 num(r.temp.map(|t| (now - t.0).max(0.0)), 1),
                 if r.live { "yes" } else { "no" },
                 r.id.path, r.id.version);
    }
    0
}

fn probe_cmd(devices: &[String], baud: Option<u32>, dir: &Path, cpm_per_usvh: f64) -> i32 {
    let wanted = |path: &str| devices.is_empty() || devices.iter().any(|d| d == path);
    let mut shown = 0usize;
    if let Some(client) = broker::Client::attach(dir) {
        let id = client.identity();
        for c in id.counters.iter().filter(|c| wanted(&c.path)) {
            if shown > 0 {
                println!();
            }
            println!("counter    {}", c.version);
            println!("model      {}", counter::model_of(&c.version));
            println!("serial     {}", c.serial_no);
            println!("port       {} @ {} baud", c.path, c.baud);
            println!("held by    the radbeeper serving {}",
                     broker::socket_path(dir).display());
            print_last_start(dir, &c.serial_no);
            shown += 1;
        }
    }
    // The ports the service holds fail at the flock and drop out here, so
    // this is exactly the counters nobody is reading.
    let (free, why) = counter::find_all(devices, baud);
    for c in &free {
        if shown > 0 {
            println!();
        }
        println!("counter    {}", c.version);
        println!("model      {}", c.model());
        println!("serial     {}", c.serial_no);
        println!("port       {} @ {} baud", c.path, c.baud);
        if let Some(v) = c.voltage() {
            println!("battery    {:.1} V", v);
        }
        if let Some(o) = c.clock_ahead() {
            println!("its clock  {}   {}",
                     clock::format(o.theirs, "%Y-%m-%d %H:%M:%S"), describe_offset(&o));
            if o.ahead.abs() > 1.0 {
                println!("           radbeeper clock --set  corrects it from this machine");
            }
        }
        if let Some(n) = c.cpm() {
            println!("reading    {} CPM  ({:.3} uSv/h at {:.1} CPM per uSv/h)",
                     n, n as f64 / cpm_per_usvh, cpm_per_usvh);
        }
        print_last_start(dir, &c.serial_no);
        shown += 1;
    }
    if shown == 0 {
        if let Some(e) = why {
            eprintln!("{}: {}", if e.busy { "port busy" } else { "no counter" }, e.reason);
            for line in e.detail.lines() {
                eprintln!("    {}", line);
            }
        }
        return 1;
    }
    if shown > free.len() {
        println!();
        println!("battery and clock of a served counter need the port itself:");
        println!("    doas rc-service radbeeper stop");
        println!("radbeeper watch attaches to the service -- no handover needed");
    }
    0
}

/// `probe`'s lines for what the last start did to this counter, from the
/// `starts` journal: the backfill on one line and the clock on the next.
fn print_last_start(dir: &Path, serial: &str) {
    let Some((stamp, what)) = log::last_start(dir, serial) else { return };
    for (k, part) in what.split("; ").enumerate() {
        if k == 0 {
            println!("last start {}  {}", stamp, part);
        } else {
            println!("           {}", part);
        }
    }
}

/// Rows to the dated log, one every `every` seconds.
///
/// What `service` writes, shared, so the monitor writing the log while it is
/// open cannot become a second dialect of the same file.
struct Logger {
    dir: PathBuf,
    serial: String,
    every: f64,
    out: log::Writer,
    iv: log::Interval,
    due: Option<f64>,
    sites: Vec<(String, f64, String)>,
    sites_mtime: Option<u64>,
}

impl Logger {
    fn new(spans: &[f64], dir: PathBuf, serial: &str, every: f64) -> Logger {
        // The site is re-read whenever sites.tsv changes, so `radbeeper site`
        // takes effect on the next row rather than at the next restart.
        let sites = log::read_sites(&dir);
        let sites_mtime = mtime_of(&dir.join("sites.tsv"));
        Logger {
            out: log::Writer::new(spans, dir.clone(), Some(serial.to_string()), every),
            iv: log::Interval::new(spans.len()),
            dir,
            serial: serial.to_string(),
            every,
            due: None,
            sites,
            sites_mtime,
        }
    }

    /// One second. Returns the row when this second completed an interval
    /// and the row was written -- not when its slot was already taken.
    fn add(&mut self, when: f64, counts: u32, averages: &[Option<f64>])
        -> std::io::Result<Option<String>>
    {
        self.iv.add(counts, averages, 1.0);
        let due = *self.due.get_or_insert(when + self.every);
        // One write and one flush per interval instead of per second: at the
        // default that is two syscalls a minute rather than a hundred and
        // twenty, which is the whole difference on a Pi Zero logging to an SD
        // card. Nothing is buffered up to pay for it.
        if when < due {
            return Ok(None);
        }
        let line = self.row(averages);
        let wrote = self.out.write(clock::now(), &line)?;
        self.iv.reset();
        self.due = Some(when + self.every);
        Ok(wrote.then_some(line))
    }

    /// Whatever the last interval collected is worth keeping: stopped four
    /// seconds after a spike, the spike should still be on disk, and the
    /// seconds column says the row is short.
    fn finish(&mut self, averages: &[Option<f64>]) {
        if self.iv.seconds > 0.0 {
            let line = self.row(averages);
            let _ = self.out.write(clock::now(), &line);
        }
        self.out.close();
    }

    fn row(&mut self, averages: &[Option<f64>]) -> String {
        let now = clock::now();
        let m = mtime_of(&self.dir.join("sites.tsv"));
        if m != self.sites_mtime {
            self.sites_mtime = m;
            self.sites = log::read_sites(&self.dir);
        }
        let site = log::site_at(&self.serial, now, &self.sites).unwrap_or_default();
        log::row(now, self.iv.cps(), self.iv.counts, self.iv.seconds,
                 averages, &self.iv.peaks, log::SRC_LIVE, &site)
    }
}

/// Rows to the merged log: the room, rather than any one instrument.
///
/// THE SECOND FILE, AND WHY THERE IS A SECOND FILE. `Logger` above writes the
/// format of record -- one file per counter, the Python's characters to the
/// byte, and a row in it is a reading ONE tube took. Nothing about two tubes
/// belongs in it: a row that blended them could never be taken apart again,
/// and the whole reason to run a second instrument is to be able to ask
/// whether the two agree.
///
/// So the merge is its own file, `cpm-merged-YYYY-MM.tsv`, carrying what the
/// per-counter files cannot say between them -- the combined rate, its error
/// bar, the interleave, and the raw arrivals that produced all three -- at
/// the precision that reads back bit-identical. See `log::merged_header`.
///
/// ONE TUBE WRITES NO MERGED FILE. There is nothing to merge and nothing to
/// interleave: `per_tube` would have a single entry, `interleave` would be
/// empty, and `cps` would be the counter's own rate restated to more places.
/// A second file that only ever repeats the first is a second file to explain,
/// to back up and to get out of step. So the merge begins when there is
/// something to merge -- including when a second counter is plugged into a
/// running service, which is exactly when a merged record starts being worth
/// keeping.
struct MergedLogger {
    dir: PathBuf,
    spans: Vec<f64>,
    every: f64,
    out: log::Writer,
    serials: Vec<String>,
    /// The interval: every arrival off every tube, and the tube-time behind
    /// them.
    counts: u64,
    per_tube: Vec<u64>,
    tube_seconds: f64,
    /// WHICH WALL SECONDS THIS ROW COVERS, as a set of them.
    ///
    /// NOT A COUNT OF SAMPLES, which with n tubes is n per second, and not
    /// the span from first to last, which is short by however long the last
    /// sample lasted. Two tubes both reporting second 1,203 make that one
    /// second of room however many samples it took, and the set says so
    /// exactly however the samples happen to arrive.
    wall: BTreeSet<i64>,
    /// WHICH TUBES ACTUALLY REPORTED IN THIS INTERVAL.
    ///
    /// NOT THE NUMBER OF SLOTS, which only ever grows: a tube that is
    /// unplugged keeps its slot -- deliberately, so that plugging it back in
    /// returns it to its own colour and its own log -- and a divisor taken
    /// from the slot count would go on dividing the room by a counter that
    /// left the building. The set is emptied with the rest of the interval,
    /// so it answers for the thirty seconds the row is about.
    seen: BTreeSet<usize>,
    peaks: Vec<Option<f64>>,
    /// The interleave, measured over this interval only: the mean gap between
    /// one tube's sample and the NEXT TUBE'S. Same-tube gaps are a second by
    /// definition and would drown it.
    prev: Option<(usize, f64)>,
    gap: (f64, u32),
    due: Option<f64>,
    sites: Vec<(String, f64, String)>,
    sites_mtime: Option<u64>,
}

impl MergedLogger {
    fn new(spans: &[f64], dir: PathBuf, every: f64) -> MergedLogger {
        let sites = log::read_sites(&dir);
        let sites_mtime = mtime_of(&dir.join("sites.tsv"));
        MergedLogger {
            out: log::Writer::merged(spans, dir.clone(), every),
            spans: spans.to_vec(),
            dir,
            every,
            serials: Vec::new(),
            counts: 0,
            per_tube: Vec::new(),
            tube_seconds: 0.0,
            wall: BTreeSet::new(),
            seen: BTreeSet::new(),
            peaks: vec![None; spans.len()],
            prev: None,
            gap: (0.0, 0),
            due: None,
            sites,
            sites_mtime,
        }
    }

    /// The tubes as they stand, so a counter plugged into a running service
    /// gets a column in the next row rather than at the next restart.
    fn counters(&mut self, serials: &[String]) {
        self.serials = serials.to_vec();
        self.per_tube.resize(serials.len(), 0);
    }

    /// One sample, and the combined windows as they stand after it.
    ///
    /// Returns the row when this sample completed an interval, exactly as
    /// `Logger::add` does.
    fn add(&mut self, who: usize, when: f64, counts: u32, all: &Windows)
        -> std::io::Result<Option<String>>
    {
        if !self.worth_keeping() {
            // Whatever one tube put in the interval is not the start of a
            // merged row. Dropped rather than carried, so that the first row
            // after a second counter joins is a row about two counters.
            self.reset();
            self.due = None;
            return Ok(None);
        }
        self.seen.insert(who);
        self.counts += counts as u64;
        if self.per_tube.len() <= who {
            self.per_tube.resize(who + 1, 0);
        }
        self.per_tube[who] += counts as u64;
        self.tube_seconds += 1.0;
        self.wall.insert(when.floor() as i64);
        if let Some((prev, t)) = self.prev {
            if prev != who && when > t {
                self.gap.0 += when - t;
                self.gap.1 += 1;
            }
        }
        self.prev = Some((who, when));
        for (i, span) in self.spans.iter().enumerate() {
            if let Some(v) = all.mean(*span) {
                let slot = &mut self.peaks[i];
                if slot.is_none() || v > slot.unwrap() {
                    *slot = Some(v);
                }
            }
        }
        let due = *self.due.get_or_insert(when + self.every);
        if when < due {
            return Ok(None);
        }
        let line = self.row(all);
        let wrote = self.out.write(clock::now(), &line)?;
        self.reset();
        self.due = Some(when + self.every);
        Ok(wrote.then_some(line))
    }

    /// Whatever the last interval collected, as `Logger::finish` does: a
    /// service stopped four seconds after a spike leaves the spike on disk.
    /// Whether there is a room to write down, rather than one instrument.
    fn worth_keeping(&self) -> bool {
        self.serials.len() > 1
    }

    fn finish(&mut self, all: &Windows) {
        if self.worth_keeping() && !self.wall.is_empty() {
            let line = self.row(all);
            let _ = self.out.write(clock::now(), &line);
        }
        self.out.close();
    }

    fn reset(&mut self) {
        self.counts = 0;
        for c in self.per_tube.iter_mut() {
            *c = 0;
        }
        self.tube_seconds = 0.0;
        self.wall.clear();
        self.seen.clear();
        for p in self.peaks.iter_mut() {
            *p = None;
        }
        self.gap = (0.0, 0);
    }

    /// How many tubes reported in this interval.
    ///
    /// WHAT THE `tubes` COLUMN SAYS, AND NO LONGER WHAT ANYTHING IS DIVIDED
    /// BY. The averages were the windows' sums over this number, and this
    /// number is the tubes reporting NOW while the windows hold the last
    /// thirty seconds: when a tube stopped, thirty seconds of two tubes were
    /// divided by one and the room read 1108 where it was 600, and when it
    /// came back thirty seconds of one were divided by two and it read 364.
    /// The averages are `Windows::mean`, counts over the tube-seconds that
    /// took them, which needs to be told nothing.
    fn tubes(&self) -> usize {
        if self.seen.is_empty() {
            // Nothing has reported yet, so the plugged-in count is all there
            // is to go on.
            self.serials.len().max(1)
        } else {
            self.seen.len()
        }
    }

    fn row(&mut self, all: &Windows) -> String {
        let now = clock::now();
        let tubes = self.tubes();
        let m = mtime_of(&self.dir.join("sites.tsv"));
        if m != self.sites_mtime {
            self.sites_mtime = m;
            self.sites = log::read_sites(&self.dir);
        }
        // THE SITE OF THE MERGE IS THE SITE OF THE FIRST TUBE. They are in one
        // room -- that is the premise the merge rests on -- and if they are
        // not, `together` in the report is the thing that says so.
        let site = self
            .serials
            .first()
            .and_then(|s| log::site_at(s, now, &self.sites))
            .unwrap_or_default();
        let averages: Vec<Option<f64>> =
            self.spans.iter().map(|s| all.mean(*s)).collect();
        let sigma = all.sigma(HEADLINE_SPAN);
        let per_tube: Vec<(String, u64)> = self
            .per_tube
            .iter()
            .enumerate()
            .map(|(k, n)| {
                let name = self
                    .serials
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| format!("tube{}", k));
                (name, *n)
            })
            .collect();
        log::merged_row(
            now,
            tubes,
            (self.gap.1 > 0).then(|| self.gap.0 / self.gap.1 as f64),
            self.counts,
            self.wall.len() as f64,
            self.tube_seconds,
            &averages,
            sigma,
            &self.peaks,
            &per_tube,
            log::SRC_LIVE,
            &site,
        )
    }
}

/// The window the merged log's error bar is quoted over.
///
/// The same thirty seconds the panels put in their big number, so the figure
/// on disk and the figure on the screen are the same figure.
const HEADLINE_SPAN: f64 = 30.0;

fn mtime_of(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.mtime() as u64)
}

/// How many bits an emission is worth, when it is not the usual 256.
///
/// A ONCE-SET GLOBAL, read where a pool is built. The pools are created in
/// three places -- the service, the monitor, and the sweep that adopts a tube
/// plugged in while either is running -- and threading a float through all of
/// them to serve one flag nobody sets in ordinary use is more moving parts
/// than the flag is worth. `--entropy-bits` was in the reference program and
/// in this program's own `--help` for a year without being implemented here,
/// which is the other half of why it is being added rather than deleted.
static WANT_BITS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

fn new_pool() -> entropy::Entropy {
    entropy::Entropy::new(*WANT_BITS.get().unwrap_or(&entropy::ENTROPY_BITS))
}

static STOP: AtomicBool = AtomicBool::new(false);

/// SIGTERM and SIGINT set a flag; the loop notices between samples.
///
/// A signal handler must not allocate, take a lock or call anything that
/// might, so it does the one thing that is safe: stores to an atomic the loop
/// already reads. Stopping between samples rather than mid-write is also what
/// keeps a half-written row off the disk.
fn install_stop_handler() {
    extern "C" fn handler(_sig: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }
    let f: extern "C" fn(libc::c_int) = handler;
    unsafe {
        libc::signal(libc::SIGTERM, f as usize as libc::sighandler_t);
        libc::signal(libc::SIGINT, f as usize as libc::sighandler_t);
    }
}

fn stopping() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// What a failed find means to `--wait`.
enum Verdict {
    /// Look again. The port is busy, and busy is the whole point of waiting.
    Retry,
    /// Stop, and let the find's own message stand -- this is not a wait
    /// problem and a wait will not fix it.
    Give,
    /// Stop, with a line to add: the wait itself ran out.
    Expired(String),
}

/// Whether a busy port is worth asking about again.
///
/// WHY ONLY BUSY IS WAITED ON. "No counter" is not a condition that clears by
/// standing still: an unplugged counter, a kernel with no ch341, a typo in
/// --device -- none of them are fixed by asking a second time, and a `watch`
/// that sat there on a misspelt device would be indistinguishable from one
/// that had hung. A locked port is the one failure that somebody in another
/// window is about to fix, so it is the one that is waited on.
///
/// Split from the loop because a decision is testable and a loop with a sleep
/// in it is not: this is what says the timeout message gets written.
fn wait_verdict(e: &counter::NotFound, waited: f64, limit: f64) -> Verdict {
    if !e.busy {
        return Verdict::Give;
    }
    if waited >= limit {
        return Verdict::Expired(format!(
            "Waited {}s for the port and it never came free.",
            log::g(limit)));
    }
    Verdict::Retry
}

/// Put SIGINT and SIGTERM back the way they were found.
///
/// `--wait` installs the stop handler so that Ctrl-C answers the wait. The
/// commands the wait runs *before* are not all written to read that flag --
/// `cpm` sits on the counter for thirty seconds and never looks at it -- and a
/// Ctrl-C swallowed for thirty seconds is worse than no handler at all. So the
/// handler lives exactly as long as the waiting does, and whatever runs next
/// installs its own if it wants one. `watch` does.
fn restore_stop_default() {
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

/// `counter::find`, but a busy port is waited on instead of refused.
///
/// WHY THIS IS NOT THE DEFAULT. The error it replaces names the command that
/// hands the counter over, and a person who has not read that yet is better
/// served by reading it than by watching a program sit still. Waiting is what
/// you want in the one case where you already know -- the service is logging,
/// you are about to stop it, and you would rather not race it to the port.
///
/// The race is real, and it is not with the service: `service` polls the same
/// port every SERVICE_WAIT seconds and takes it back the moment it is free. A
/// stopped service is not in the running, but a RESTARTED one is, and then the
/// two are simply both asking. Whoever asks first wins.
fn find_waiting(device: Option<&str>, baud: Option<u32>, limit: f64)
                -> Result<counter::Counter, counter::NotFound> {
    // So Ctrl-C during the wait is an answer and not a kill: the flag is read
    // between polls, which is also where the process is doing nothing.
    install_stop_handler();
    let started = clock::now();
    let mut said = false;
    loop {
        let e = match counter::find(device, baud) {
            Ok(c) => {
                restore_stop_default();
                // A Ctrl-C that landed between the last check and the port
                // coming free is still a Ctrl-C, and it still means stop.
                if stopping() {
                    println!("radbeeper: stopped waiting.");
                    std::process::exit(1);
                }
                if said {
                    println!("radbeeper: the port came free.");
                }
                return Ok(c);
            }
            Err(e) => e,
        };
        match wait_verdict(&e, clock::now() - started, limit) {
            Verdict::Give => {
                restore_stop_default();
                return Err(e);
            }
            Verdict::Expired(note) => {
                // Appended rather than substituted: the detail is the part
                // that names `doas rc-service radbeeper stop`, and a wait that
                // timed out is exactly when that is worth reading.
                restore_stop_default();
                let mut e = e;
                e.detail = format!("{}\n{}", e.detail, note);
                return Err(e);
            }
            Verdict::Retry => {}
        }
        if !said {
            said = true;
            let how_long = if limit.is_finite() {
                format!(" up to {}s", log::g(limit))
            } else {
                String::new()
            };
            println!("radbeeper: the port is busy -- waiting{} for it. \
                      Ctrl-C to stop.", how_long);
        }
        if stopping() {
            println!("radbeeper: stopped waiting -- the port is still busy.");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_secs_f64(WAIT_POLL));
    }
}


/// Collect until the pool has earned the bits, then print one line.
fn random(spans: &[f64], duration: Option<f64>, device: Option<&str>,
          baud: Option<u32>, logs: Option<std::path::PathBuf>) -> i32 {
    install_stop_handler();
    let c = match counter::find(device, baud) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {}",
                      if e.busy { "port busy" } else { "no counter" }, e.reason);
            return 1;
        }
    };
    let want = entropy::ENTROPY_BITS;
    let mut pool = entropy::Entropy::new(want);
    let mut ladder = Ladder::new();
    println!("radbeeper: collecting {} bits from {}", want as i64, c.path);
    c.heartbeat(true);
    let started = clock::now();
    while !pool.ready() {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        pool.add(counts);
        ladder.add(counts);
        if stopping() {
            break;
        }
        if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
            break;
        }
    }
    c.heartbeat(false);
    if !pool.ready() {
        eprintln!("radbeeper: only {:.0} of {} bits -- {}",
                  pool.bits(), want as i64,
                  if duration.is_some() { "stopped early" } else { "interrupted" });
        return 1;
    }
    let spec = ladder.best();
    let (top, _) = spec.loudest();
    let suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
    let modelled = pool.model_bits();
    let seconds = pool.counts.len();
    let rate = pool.rate();
    let (text, record) = pool.draw();
    println!();
    println!("{}", entropy::group_hex(&text));
    println!();
    println!("  {} bits, min-entropy {:.0} measured, from {} seconds at {:.2} \
              counts/s", want as i64, record.bits, seconds, rate);
    // The number the old Poisson model would have printed, beside the one the
    // samples actually support. Over-dispersed arrivals make the gap large.
    println!("  {:.0} bits is what a Poisson model would have claimed for the \
              same {} seconds", modelled, seconds);
    println!("  spectrum {}", if suspect {
        "NOT FLAT -- something periodic; treat as suspect"
    } else {
        "flat -- the source looks like decay"
    });
    let dir = logs.unwrap_or_else(log::state_dir);
    match entropy::write_record(&dir, &record, &c.serial_no, suspect) {
        Ok(p) => println!("  recorded in {}", p.display()),
        Err(e) => println!("  not recorded: {}", e),
    }
    let _ = spans;
    0
}

/// Recompute every emission from the counts written beside it.
///
/// This is the whole of the audit trail's promise: a line that cannot be
/// recomputed from its own counts was invented, and one that can was not. It
/// proves nothing about the NEXT line, which is the point -- that one comes
/// from decays that have not happened.
/// `random --frames FILE`: what is in a frame file, and does it hold up.
///
/// THE FRAME IS THE EVIDENCE AND THIS IS HOW YOU LOOK AT IT. The `.tsv`
/// carries the counts clamped to one hex digit a second, which is what the
/// key is derived from and is not what the tube did; the `.bin` carries every
/// second exactly, with the gaps where the counter was away. Each frame
/// recomputes its own key from its own counts, so a file from somebody else's
/// machine can be audited without trusting anything but the arithmetic.
fn frames_report(path: &std::path::Path) -> i32 {
    let frames = entropy::read_frames(path);
    if frames.is_empty() {
        eprintln!("radbeeper: no frames in {}", path.display());
        return 1;
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut bad = 0;
    let mut samples = 0usize;
    for f in &frames {
        let counts = f.counts();
        let ok = f.verifies();
        if !ok {
            bad += 1;
        }
        samples += counts.len();
        // The gaps are the thing the counts alone cannot say: a pool that
        // spanned an unplugged counter looks exactly like one that did not.
        let holes = f.samples.iter().skip(1).filter(|(g, _)| *g != 1).count();
        println!(
            "  seq {:<4} {:<20} {:>5}s  {:>5} samples  peak {:>5}  {:.3} bits/s  {}{}",
            f.seq,
            clock::stamp(f.started as f64),
            f.seconds(),
            counts.len(),
            counts.iter().copied().max().unwrap_or(0),
            entropy::mcv_min_entropy(&counts),
            if ok { "recomputes" } else { "DOES NOT RECOMPUTE" },
            if holes == 0 {
                String::new()
            } else {
                format!("  ({} gap{})", holes, if holes == 1 { "" } else { "s" })
            }
        );
        if f.suspect {
            println!("       spectrum was not flat when this was drawn -- suspect");
        }
    }
    println!(
        "radbeeper: {} of {} frames recompute from their own seconds",
        frames.len() - bad,
        frames.len()
    );
    println!(
        "radbeeper: {} seconds of counts in {} bytes -- {:.2} bytes a second",
        samples,
        bytes,
        if samples > 0 { bytes as f64 / samples as f64 } else { 0.0 }
    );
    if bad > 0 { 1 } else { 0 }
}

/// `frames <verb>`: the raw record, from the command line.
///
/// THE SAME ANSWERS THE PAGE GIVES, WITHOUT A BROWSER. `frames.html` is the
/// comfortable way to read this and a terminal is the one that is always
/// there -- over ssh, in a service's log, on a machine with no display at
/// all -- so every view the page has is a verb here as well, and both of them
/// read the files through `radbeeper::frames` rather than each other.
fn frames_cmd(action: &str, dir: &std::path::Path, serial: Option<&str>,
              month: Option<&str>, seq: Option<u64>, form: &str,
              output: Option<&str>, file: Option<&std::path::Path>) -> i32 {
    use radbeeper::frames as api;

    // WHICH COUNTER, AND SAYING SO RATHER THAN GUESSING. One counter is the
    // ordinary case and naming it every time would be noise; two is the case
    // this machine is actually in, and picking one of them silently is how a
    // person ends up auditing the wrong tube.
    let found = api::counters(dir);
    let chosen: Vec<String> = match serial {
        Some(s) => vec![s.to_string()],
        None if found.len() == 1 => found.clone(),
        None if found.is_empty() => {
            eprintln!("radbeeper: no frame files in {}", dir.display());
            eprintln!("    frames are written by `radbeeper service` and `radbeeper watch`,");
            eprintln!("    as random-<serial>-YYYY-MM.bin, unless --no-frames is given.");
            return 1;
        }
        None if action == "list" || action == "verify" => found.clone(),
        None => {
            eprintln!("radbeeper: {} counters here -- name one with --serial:", found.len());
            for s in &found {
                eprintln!("    {}", s);
            }
            return 1;
        }
    };

    match action {
        "list" => {
            for s in &chosen {
                let series = api::Series::open(dir, s);
                println!("counter {}", s);
                if series.months().is_empty() {
                    println!("  no frames");
                    continue;
                }
                let mut samples = 0usize;
                for m in series.months() {
                    let frames = series.read(&m.label);
                    let secs: usize = frames.iter().map(|f| f.samples.len()).sum();
                    samples += secs;
                    println!(
                        "  {:<9} {:>6} frames  {:>9}  {:>9} seconds  {} .. {}",
                        if m.label.is_empty() { "undated" } else { &m.label },
                        m.frames,
                        bytes_text(m.bytes),
                        secs,
                        clock::format(m.first as f64, "%Y-%m-%d"),
                        clock::format(m.last as f64, "%Y-%m-%d"),
                    );
                }
                let bytes = series.bytes();
                println!(
                    "  {} frames, {} seconds, {} -- {:.2} bytes a second",
                    series.frames(), samples, bytes_text(bytes),
                    if samples > 0 { bytes as f64 / samples as f64 } else { 0.0 }
                );
                println!("  chain head {}", api::head(dir, s));
            }
            0
        }
        "verify" => {
            let mut bad = 0;
            for s in &chosen {
                let series = api::Series::open(dir, s);
                let frames = series.read_all();
                if frames.is_empty() {
                    println!("counter {}: no frames", s);
                    continue;
                }
                let v = api::verify(&frames, entropy::GENESIS_LINK);
                println!(
                    "counter {}: {} of {} recompute, {} of {} link{}",
                    s, v.keys_ok, v.frames, v.links_ok,
                    v.frames - v.unlinked,
                    if v.unlinked > 0 {
                        format!(", {} unlinked (written before the chain)", v.unlinked)
                    } else {
                        String::new()
                    }
                );
                for seq in &v.broken {
                    println!("  seq {} does not hold up", seq);
                }
                // THE HEAD IS CHECKED AGAINST THE DISK, not just recomputed.
                // A chain that verifies internally but does not end where the
                // next frame will be linked from is a fork waiting to happen.
                let disk = api::head(dir, s);
                if v.head != disk {
                    println!("  chain head disagrees with the file:");
                    println!("    recomputed {}", v.head);
                    println!("    on disk    {}", disk);
                    bad += 1;
                }
                if !v.broken.is_empty() {
                    bad += 1;
                }
            }
            if bad > 0 { 1 } else { 0 }
        }
        "show" => {
            let s = &chosen[0];
            let series = api::Series::open(dir, s);
            let frames = match month {
                Some(m) => series.read(m),
                None => series.read_all(),
            };
            if frames.is_empty() {
                eprintln!("radbeeper: no frames for {}{}", s,
                          month.map(|m| format!(" in {}", m)).unwrap_or_default());
                return 1;
            }
            let picked: Vec<&entropy::Frame> = match seq {
                Some(n) => frames.iter().filter(|f| f.seq == n).collect(),
                None => frames.last().into_iter().collect(),
            };
            if picked.is_empty() {
                eprintln!("radbeeper: no frame with seq {} for {}", seq.unwrap_or(0), s);
                eprintln!("    `seq` belongs to the pool and restarts; \
                           `radbeeper frames list` has the months.");
                return 1;
            }
            // seq RESTARTS, so asking for one can legitimately find several.
            // Showing the first and saying nothing would be answering a
            // different question than the one asked.
            if picked.len() > 1 {
                eprintln!("radbeeper: {} frames carry seq {} -- narrow it with --month:",
                          picked.len(), seq.unwrap_or(0));
                for f in &picked {
                    eprintln!("    {}  {}", clock::stamp(f.started as f64),
                              clock::format(f.started as f64, "%Y-%m"));
                }
                return 1;
            }
            inspect(picked[0], s);
            0
        }
        "export" => {
            let s = &chosen[0];
            let series = api::Series::open(dir, s);
            let frames = match month {
                Some(m) => series.read(m),
                None => series.read_all(),
            };
            if frames.is_empty() {
                eprintln!("radbeeper: no frames to export for {}", s);
                return 1;
            }
            let head = vec![
                format!("radbeeper {} -- counter {}", VERSION, s),
                format!("frames {}{}", frames.len(),
                        month.map(|m| format!(" from {}", m)).unwrap_or_default()),
            ];
            let (bytes, what): (Vec<u8>, &str) = match form {
                "bin" => (api::to_bytes(&frames), "bytes"),
                "tsv" => (api::to_tsv(&frames, &head).into_bytes(), "rows"),
                _ => (api::to_json(&frames).into_bytes(), "json"),
            };
            match output {
                Some(path) => {
                    if let Err(e) = std::fs::write(path, &bytes) {
                        eprintln!("radbeeper: {}: {}", path, e);
                        return 1;
                    }
                    println!("radbeeper: {} -- {} frames, {} of {}",
                             path, frames.len(), bytes_text(bytes.len() as u64), what);
                }
                None if form == "bin" => {
                    // Frame bytes contain every byte value there is, and a
                    // terminal is not a file. `-o` is not a convenience here.
                    eprintln!("radbeeper: --bin writes bytes -- give it -o FILE");
                    return 2;
                }
                None => {
                    print!("{}", String::from_utf8_lossy(&bytes));
                }
            }
            0
        }
        "import" => {
            let s = &chosen[0];
            let Some(path) = file else {
                eprintln!("radbeeper: frames import needs a file (.bin or .json)");
                return 2;
            };
            let frames = match api::read_any(path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("radbeeper: {}", e);
                    return 1;
                }
            };
            match api::import(dir, s, &frames) {
                Ok(done) => {
                    println!("radbeeper: {} of {} frames written to {}",
                             done.written, done.offered, dir.display());
                    for p in &done.files {
                        println!("  {}", p.display());
                    }
                    if done.duplicates > 0 {
                        println!("  {} already here, by key -- not written again",
                                 done.duplicates);
                    }
                    for seq in &done.refused {
                        println!("  seq {} does not produce its own key -- REFUSED", seq);
                    }
                    println!("  chain head {}", api::head(dir, s));
                    if done.refused.is_empty() { 0 } else { 1 }
                }
                Err(e) => {
                    eprintln!("radbeeper: import: {}", e);
                    1
                }
            }
        }
        other => {
            eprintln!("radbeeper: frames {}? -- list, show, export, import, verify", other);
            2
        }
    }
}

/// One frame, in full: what it is, what it sounds like, and every second.
///
/// THE INSPECTOR'S JOB IS TO SHOW THE EVIDENCE, NOT A SUMMARY OF IT. The key
/// is a claim about a particular stretch of decay; the seconds below are that
/// stretch, unclamped, with the gaps where the counter was away marked -- so
/// the line that says `recomputes` can be believed by someone who does the
/// arithmetic themselves.
fn inspect(f: &entropy::Frame, serial: &str) {
    use radbeeper::frames as api;
    let counts = f.counts();
    let total: u64 = counts.iter().map(|c| *c as u64).sum();
    let secs = f.seconds().max(1);
    println!("counter    {}", serial);
    println!("seq        {}", f.seq);
    println!("started    {}", clock::stamp(f.started as f64));
    println!("covers     {} seconds in {} samples", f.seconds(), counts.len());
    println!("counts     {}, {:.2} a second, peak {}",
             total, total as f64 / secs as f64, counts.iter().copied().max().unwrap_or(0));
    println!("min-entropy {:.3} bits a sample (most-common-value)",
             entropy::mcv_min_entropy(&counts));
    println!("key        {}", f.key);
    println!("link       {}", if f.link.is_empty() { "-- (written before the chain)" } else { &f.link });
    println!("recomputes {}", if f.verifies() { "yes" } else { "NO -- this frame does not produce its key" });

    // The gaps, because the counts alone cannot say the counter was away.
    //
    // A GAP AND A DOUBLED SECOND ARE NOT THE SAME DEPARTURE. Anything that
    // is not one second is irregular, but a gap of four means the counter
    // was away for three seconds and a gap of zero means two samples landed
    // inside one second -- the opposite complaint. Calling both "the counter
    // was away" is the kind of wrong label somebody reasons from later.
    let holes: Vec<(usize, u32)> = f.samples.iter().enumerate().skip(1)
        .filter(|(_, (g, _))| *g > 1).map(|(i, (g, _))| (i, *g)).collect();
    let doubled = f.samples.iter().skip(1).filter(|(g, _)| *g == 0).count();
    if holes.is_empty() {
        println!("gaps       none -- every sample is one second after the last");
    } else {
        println!("gaps       {}", holes.len());
        for (i, g) in holes.iter().take(8) {
            println!("             at sample {}: {} seconds away", i, g);
        }
        if holes.len() > 8 {
            println!("             ... and {} more", holes.len() - 8);
        }
    }
    if doubled > 0 {
        println!("doubled    {} sample{} landed inside a second already sampled",
                 doubled, if doubled == 1 { "" } else { "s" });
    }

    match api::spectrum(&counts) {
        None => println!("\nspectrum   too few seconds to say anything"),
        Some(spec) => {
            println!("\nspectrum   {} second window, {} averaged, flat is 1.0",
                     spec.window, spec.runs);
            let width = 60usize;
            let cols = analysis::spectrum_columns(&spec.relative, width);
            for row in analysis::bar_rows(&cols, width, 6) {
                println!("  {}", row.into_iter().collect::<String>());
            }
            println!("  loudest {:.2} at a period of {:.0}s -- luck alone reaches {:.2}",
                     spec.loudest.0, spec.period, spec.chance_max);
            println!("  {}", if spec.suspect {
                "NOT FLAT: something periodic is in this frame -- treat it as suspect"
            } else {
                "flat: this looks like decay"
            });
            if spec.suspect != f.suspect {
                // NOT A CONTRADICTION. The flag came off the monitor's
                // running ladder -- every second that process had seen, and
                // the sum across both tubes when two were plugged in. This
                // is the ladder over this frame alone. A period longer than
                // the frame cannot appear here and is plain there.
                println!("  the recorder wrote suspect={}, and this frame on its own \
                          recomputes to {}", f.suspect, spec.suspect);
                println!("  (the flag is drawn from the whole run, not from one frame)");
            }
        }
    }

    // Every second, as the digits the record keeps them in. Sixty to a line,
    // so a minute is a line and a pattern has somewhere to show itself.
    //
    // ONE CHARACTER A SECOND STOPS MEANING ANYTHING ABOVE 35, which is a
    // counter on a real source rather than on a desk. Rendering every one of
    // those seconds as the same `+` is a blank reading, not a compressed
    // one, so past that the numbers are written out. The page does the same.
    let peak = counts.iter().copied().max().unwrap_or(0);
    if peak > 35 {
        println!("\nseconds    (counts, ten to a line, ... is a gap)");
        let mut row: Vec<String> = Vec::new();
        for (i, (gap, count)) in f.samples.iter().enumerate() {
            if i > 0 && *gap > 1 {
                row.push("...".to_string());
            }
            row.push(format!("{:>4}", count));
            if row.len() >= 10 {
                println!("  {}", row.join(" "));
                row.clear();
            }
        }
        if !row.is_empty() {
            println!("  {}", row.join(" "));
        }
        return;
    }
    println!("\nseconds    (counts, one character a second, . is a gap)");
    let mut line = String::new();
    let mut at = 0usize;
    for (i, (gap, count)) in f.samples.iter().enumerate() {
        if i > 0 && *gap > 1 {
            for _ in 0..(*gap).min(6).saturating_sub(1) {
                line.push('.');
                at += 1;
                if at % 60 == 0 {
                    println!("  {}", line);
                    line.clear();
                }
            }
        }
        line.push(match count {
            0..=9 => char::from(b'0' + *count as u8),
            10..=35 => char::from(b'a' + (*count - 10) as u8),
            _ => '+',
        });
        at += 1;
        if at % 60 == 0 {
            println!("  {}", line);
            line.clear();
        }
    }
    if !line.is_empty() {
        println!("  {}", line);
    }
}

fn check_random(path: &std::path::Path) -> i32 {
    let pools = entropy::read_emissions(path);
    if pools.is_empty() {
        eprintln!("radbeeper: no emissions in {}", path.display());
        return 1;
    }
    let mut bad = 0;
    for p in &pools {
        let started = clock::parse_stamp(&p.time).unwrap_or(0.0);
        let ok = entropy::check_record(p.seq, started, &p.counts, &p.hex);
        if !ok {
            bad += 1;
        }
        println!("  seq {:<4} {:<20} {:>4}s  {:.3} bits/s  {}",
                 p.seq, p.time, p.seconds,
                 entropy::mcv_min_entropy(&p.counts),
                 if ok { "recomputes" } else { "DOES NOT RECOMPUTE" });
    }
    println!("radbeeper: {} of {} emissions recompute from their own counts",
             pools.len() - bad, pools.len());
    if bad > 0 { 1 } else { 0 }
}


/// The time of the first timestamp in a probe read at `address`.
fn first_mark_time(c: &counter::Counter, address: usize, probe: usize)
    -> Option<f64>
{
    let chunk = c.read_history(address, probe);
    history::raw(&chunk).find_map(|r| match r {
        history::Raw::Mark { when, .. } => Some(when),
        _ => None,
    })
}

/// (newest byte address, whether the flash has wrapped).
///
/// Pulling a full megabyte over 115200 baud takes ten minutes and the rows a
/// backfill wants are the NEWEST, so the first job is to find where they are.
/// Two shapes of flash, two searches.
///
/// NOT YET FULL: writing runs forward from zero and the rest is 0xFF, so
/// eleven probes bisect for where the 0xFF starts.
///
/// ALREADY WRAPPED, which is what the counter on the bench does -- a megabyte
/// of ring with no unwritten byte in it. The newest sample is immediately
/// before the write pointer and the oldest immediately after, so the flash
/// reads as one long climb in time with exactly one step backwards in it, and
/// that step bisects. Getting this wrong is not a small error: reading the
/// physical tail of a wrapped ring hands back the OLDEST hours while claiming
/// they are the newest, and a backfill would file last week under this
/// afternoon.
fn find_history_end(c: &counter::Counter, size: usize) -> (usize, bool) {
    const PROBE: usize = 2048;
    let tail = c.read_history(size.saturating_sub(PROBE), PROBE);
    if !tail.iter().any(|b| *b != 0xFF) {
        if !c.read_history(0, PROBE).iter().any(|b| *b != 0xFF) {
            return (0, false);
        }
        let (mut lo, mut hi) = (0usize, size);
        while hi - lo > PROBE {
            let mid = (lo + hi) / 2;
            if c.read_history(mid, PROBE).iter().any(|b| *b != 0xFF) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // Bisection lands within a probe of the truth; the last step reads
        // that window and takes the exact byte, so the tail read does not
        // carry a kilobyte of 0xFF with it.
        let window = c.read_history(lo, PROBE * 2);
        let last = window.iter().rposition(|b| *b != 0xFF);
        return (last.map(|j| lo + j + 1).unwrap_or(lo), false);
    }
    let start = match first_mark_time(c, 0, PROBE) {
        Some(t) => t,
        None => return (size, true), // no marks to steer by; take it as it lies
    };
    let (mut lo, mut hi) = (0usize, size);
    while hi - lo > PROBE {
        let mid = (lo + hi) / 2;
        match first_mark_time(c, mid, PROBE) {
            Some(seen) if seen < start => hi = mid,
            _ => lo = mid,
        }
    }
    let window = c.read_history(lo, PROBE * 2);
    for r in history::raw(&window) {
        if let history::Raw::Mark { off, when } = r {
            if when < start {
                return (lo + off, true);
            }
        }
    }
    (lo + PROBE, true)
}

/// The newest `want` bytes of the flash, in the order they were recorded.
///
/// In a wrapped ring the newest bytes end at the write pointer and, if more
/// are asked for than lie before it, continue from the physical end of the
/// chip. What comes back is always chronological, so nothing downstream has
/// to know the flash is a circle.
fn read_history_tail(c: &counter::Counter, want: usize, quiet: bool) -> Vec<u8> {
    let size = c.flash_size();
    if size == 0 {
        return Vec::new();
    }
    let (end, wrapped) = find_history_end(c, size);
    if end == 0 {
        return Vec::new();
    }
    if !quiet {
        println!("radbeeper: newest history at {} KiB of {} KiB{}; reading {} KiB",
                 end / 1024, size / 1024,
                 if wrapped { " (wrapped)" } else { "" },
                 want.min(size) / 1024);
    }
    if want <= end {
        return c.read_history(end - want, want);
    }
    let head = c.read_history(0, end);
    if !wrapped {
        return head;
    }
    let over = (want - end).min(size - end);
    let mut out = c.read_history(size - over, over);
    out.extend_from_slice(&head);
    out
}

/// Seconds to add to the counter's timestamps to land on this clock.
///
/// The counter's RTC is not this machine's. On the unit here it reads about
/// half an hour behind, which is six hundred slots at the default row
/// spacing: backfilling without this correction would file four hours of
/// recording under the wrong four hours. Timed to the counter's tick rather
/// than read once, because one reading is only good to the whole second it
/// names -- see `Counter::clock_ahead`.
fn measure_clock_offset(c: &counter::Counter) -> Option<f64> {
    c.clock_ahead().map(|o| -o.ahead)
}

/// "111.4 s ahead of this machine (±0.02 s)", or "matches" when it does.
fn describe_offset(o: &counter::ClockOffset) -> String {
    let within = if o.within < 0.1 {
        format!("\u{b1}{:.2} s", o.within)
    } else {
        format!("\u{b1}{:.1} s", o.within)
    };
    if o.ahead.abs() <= o.within.max(0.05) {
        return format!("matches this machine ({})", within);
    }
    let (n, way) = if o.ahead > 0.0 { (o.ahead, "ahead of") } else { (-o.ahead, "behind") };
    let size = if n >= 10.0 { format!("{:.1} s", n) } else { format!("{:.2} s", n) };
    format!("{} {} this machine ({})", size, way, within)
}

/// How far out a counter's clock has to be before a start corrects it.
///
/// A second, which is where `clock` starts suggesting `--set`: the set is
/// aimed at a whole second and measured to a few hundredths, so anything
/// under this is inside what one set would land on anyway.
const CLOCK_SLACK: f64 = 1.0;

/// "26.7 years behind", "3.2 s ahead": how far a clock was out, in the unit
/// a person would pick. For the note a start leaves, where "8.4e8 s" says
/// nothing and a full `describe_offset` is a mouthful twice over.
fn describe_drift(ahead: f64) -> String {
    let n = ahead.abs();
    let size = if n < 60.0 {
        format!("{:.1} s", n)
    } else if n < 3600.0 {
        format!("{:.1} min", n / 60.0)
    } else if n < 86400.0 {
        format!("{:.1} h", n / 3600.0)
    } else if n < 365.25 * 86400.0 {
        format!("{:.1} days", n / 86400.0)
    } else {
        format!("{:.1} years", n / (365.25 * 86400.0))
    };
    format!("{} {}", size, if ahead > 0.0 { "ahead" } else { "behind" })
}

/// Set the counter's clock from this machine's, and measure where it landed.
///
/// THE SET IS AIMED AT A WHOLE SECOND. <SETDATETIME>> carries whole seconds,
/// so it is sent as this machine's clock reaches the second it names, and
/// the result is measured the same way `probe` measures. If it landed off --
/// the counter's own latency, or a firmware that keeps its sub-second phase
/// -- the send is moved by what was measured and tried once more, and
/// whatever the second measurement says is what comes back. Costs two to
/// four seconds, mostly waiting for the second to come round.
fn aim_clock(c: &counter::Counter) -> Result<counter::ClockOffset, &'static str> {
    let mut lead = 0.0f64;
    let mut after = None;
    for _ in 0..2 {
        let now = clock::now();
        let target = now.floor() + 2.0;
        let wait = target - lead - clock::now();
        if wait > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(wait));
        }
        if !c.set_clock(target) {
            return Err("the counter did not acknowledge <SETDATETIME>>");
        }
        after = c.clock_ahead();
        match after {
            Some(o) if o.ahead.abs() > o.within.max(0.05) => {
                lead = (lead - o.ahead).clamp(-0.9, 0.9);
            }
            _ => break,
        }
    }
    after.ok_or("set, but the counter did not answer the check")
}

/// Whether the kernel believes this machine's clock is disciplined by NTP.
///
/// Setting the counter from this clock copies its error, so a clock nothing
/// is steering is worth a line of warning. adjtimex with no modes changes
/// nothing; it reports TIME_ERROR while the kernel's clock is unsynchronised.
#[cfg(target_os = "linux")]
fn system_clock_synced() -> bool {
    let mut t: libc::timex = unsafe { std::mem::zeroed() };
    unsafe { libc::adjtimex(&mut t) != libc::TIME_ERROR }
}

/// Off Linux there is no adjtimex to ask. A Mac keeps its clock by the
/// network unless somebody has switched that off, and there is no call here
/// that says which; it is taken to be steered, and no warning is printed
/// that could not be backed.
#[cfg(not(target_os = "linux"))]
fn system_clock_synced() -> bool {
    true
}

/// `radbeeper clock`, and `--set` to correct it from this machine. The set
/// itself is `aim_clock`, which every start of `service` and `watch` also
/// runs when the counter is out by more than `CLOCK_SLACK`; this is the
/// command for doing it by hand, and for seeing the figures.
fn clock_cmd(c: &counter::Counter, set: bool) -> i32 {
    let before = match c.clock_ahead() {
        Some(o) => o,
        None => {
            eprintln!("radbeeper: the counter did not answer <GETDATETIME>>");
            return 1;
        }
    };
    let synced = system_clock_synced();
    println!("its clock      {}", clock::format(before.theirs, "%Y-%m-%d %H:%M:%S"));
    println!("               {}", describe_offset(&before));
    println!("this machine   {}", if synced {
        "synchronised (the kernel says NTP is steering it)"
    } else {
        "NOT synchronised -- nothing is steering this clock"
    });
    if !set {
        if before.ahead.abs() > 1.0 {
            println!("               radbeeper clock --set  corrects it from this machine");
        }
        return 0;
    }
    if !synced {
        println!("               setting anyway: the counter will be as right as this machine is");
    }

    let after = match aim_clock(c) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("radbeeper: {}", e);
            return 1;
        }
    };
    println!("set            {}", clock::format(after.theirs, "%Y-%m-%d %H:%M:%S"));
    println!("               {}", describe_offset(&after));
    if before.ahead.abs() >= 1.0 {
        // The flash does not rewrite itself. A backfill measures one offset
        // and applies it to the whole tail it reads, so a tail that spans
        // this moment has two clocks in it and the older part is out by
        // what was just corrected.
        println!();
        println!("  history the counter recorded before now carries the old clock, {:.0} s",
                 before.ahead.abs());
        println!("  {}. A backfill applies one offset to everything it reads, so rows",
                 if before.ahead > 0.0 { "ahead" } else { "behind" });
        println!("  it rebuilds from before this moment will be out by that much.");
    }
    0
}

#[allow(clippy::too_many_arguments)]
fn backfill_cmd(spans: &[f64], every: f64, max_gap: f64, bytes: usize,
                device: Option<&str>, baud: Option<u32>,
                logs: Option<std::path::PathBuf>, image: Option<&std::path::Path>,
                serial_arg: Option<&str>, one_file: Option<&str>)
    -> i32
{
    let dir = logs.unwrap_or_else(log::state_dir);
    let _ = std::fs::create_dir_all(&dir);
    let (blob, offset, serial) = match image {
        // An image on disk needs no counter, which is what makes a decoder
        // fix testable against last week's dump.
        Some(p) => match std::fs::read(p) {
            // A dumped image does not carry its own serial, and rows that
            // cannot say which counter they came from are half a measurement.
            Ok(b) => match serial_arg {
                Some(sn) => (b, 0.0, sn.to_string()),
                None => {
                    eprintln!("radbeeper: --serial is how a dumped image says \
                               which counter it came from");
                    return 1;
                }
            },
            Err(e) => {
                eprintln!("radbeeper: cannot read {} -- {}", p.display(), e);
                return 1;
            }
        },
        None => {
            let c = match counter::find(device, baud) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("no counter: {}", e.reason);
                    return 1;
                }
            };
            let off = measure_clock_offset(&c).unwrap_or(0.0);
            let b = read_history_tail(&c, bytes, false);
            (b, off, c.serial_no.clone())
        }
    };
    if blob.is_empty() {
        eprintln!("radbeeper: nothing to read");
        return 1;
    }
    let sites = log::read_sites(&dir);
    let serial_opt = if serial.is_empty() { None } else { Some(serial.as_str()) };
    let r = history::backfill(&blob, spans, every, max_gap, offset, &dir,
                              serial_opt, &sites, one_file.map(std::path::Path::new));
    if r.samples == 0 {
        println!("radbeeper: no placeable samples in the history read");
        return 0;
    }
    println!("radbeeper: {} samples, {} rows, {} added, {} already logged",
             r.samples, r.rows, r.added, r.clashed);
    if let (Some(a), Some(b)) = (r.first, r.last) {
        println!("           {} .. {}", clock::stamp(a), clock::stamp(b));
    }
    println!("           clock offset {:+.0}s, {} hole{}", offset, r.holes,
             if r.holes == 1 { "" } else { "s" });
    for f in &r.files {
        println!("           {}", f.display());
    }
    0
}

fn log_cmd(action: &str, bytes: Option<usize>, out_stem: Option<&str>,
           device: Option<&str>, baud: Option<u32>) -> i32 {
    let c = match counter::find(device, baud) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no counter: {}", e.reason);
            return 1;
        }
    };
    if action == "info" {
        println!("model     {}", c.model());
        println!("flash     {} KiB", c.flash_size() / 1024);
        println!("pull it   radbeeper log pull -o FILE");
        return 0;
    }
    let size = bytes.unwrap_or_else(|| c.flash_size());
    let stem = match out_stem {
        Some(s) => s.to_string(),
        None => log::state_dir()
            .join(format!("history-{}", clock::format(clock::now(), "%Y%m%d-%H%M%S")))
            .display()
            .to_string(),
    };
    println!("reading {} KiB of history from {}", size / 1024, c.path);
    let got = c.read_history(0, size);
    if got.is_empty() {
        eprintln!("the counter returned nothing");
        return 1;
    }
    let raw_path = format!("{}.bin", stem);
    let csv_path = format!("{}.csv", stem);
    // THE RAW BYTES FIRST, ALWAYS. A decoder that mis-reads a marker costs a
    // bad CSV and never the data, and a better decoder can be run over the
    // same image later. That rule is what found both corrections to GQ's
    // published format.
    if let Err(e) = std::fs::write(&raw_path, &got) {
        eprintln!("radbeeper: cannot write {} -- {}", raw_path, e);
        return 1;
    }
    println!("raw    {}  ({} KiB)", raw_path, got.len() / 1024);
    let mut text = String::from("offset,time,interval_s,count,note\n");
    let mut rows = 0usize;
    for r in history::records(&got) {
        text.push_str(&format!(
            "{},{},{},{},{}\n",
            r.off,
            r.when.map(clock::stamp).unwrap_or_default(),
            r.dt.map(|d| format!("{:.4}", d)).unwrap_or_default(),
            r.count.map(|c| c.to_string()).unwrap_or_default(),
            r.note.replace(',', " ")
        ));
        rows += 1;
    }
    if let Err(e) = std::fs::write(&csv_path, text) {
        eprintln!("radbeeper: cannot write {} -- {}", csv_path, e);
        return 1;
    }
    println!("csv    {}  ({} rows)", csv_path, rows);
    println!("       times are the COUNTER's clock; interval_s is the measured");
    println!("       spacing of its samples. radbeeper backfill corrects both.");
    if rows == 0 {
        println!("nothing was recorded -- the counter's history may be empty.");
    }
    0
}

fn parse_spans(text: &str) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let v: f64 = part.trim().parse().ok()?;
        if v <= 0.0 {
            return None;
        }
        out.push(v);
    }
    (!out.is_empty()).then_some(out)
}

/// A size with an optional K/M/G suffix, as `--frame-budget` takes it.
///
/// `0` is meaningful and is not an error: it asks for a page that embeds no
/// frames at all, which is what somebody publishing a very long record and
/// linking the files instead actually wants.
fn parse_bytes(text: &str) -> Option<u64> {
    let t = text.trim();
    let (digits, scale) = match t.chars().last() {
        Some('k') | Some('K') => (&t[..t.len() - 1], 1024u64),
        Some('m') | Some('M') => (&t[..t.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    digits.trim().parse::<u64>().ok().map(|n| n.saturating_mul(scale))
}

fn usage() {
    println!("radbeeper {} -- a GQ GMC counter on the desk (Rust build)", VERSION);
    println!();
    println!("  radbeeper probe            find the counter and say what it is");
    println!("  radbeeper cpm              the counter's own CPM, once");
    println!("  radbeeper now [--json]     what the service reads this minute: CPM and temperature");
    println!("  radbeeper clock [--set]    its clock against this machine's, or correct it");
    println!("  radbeeper watch            the monitor, logging while it is open");
    println!("  radbeeper service          log every counter found, and serve the stream");
    println!("  radbeeper random           256 bits of hex, out of decay timing");
    println!("  radbeeper random --check F recompute every line in an emission log");
    println!("  radbeeper random --frames F  the raw seconds behind those lines");
    println!("  radbeeper backfill         fill the log's gaps from the counter's flash");
    println!("  radbeeper log info|pull    what history it holds, or download it");
    println!("  radbeeper frames list      the raw frames on disk, by month");
    println!("  radbeeper frames show      one frame: its seconds, its spectrum, its key");
    println!("  radbeeper frames export    those frames out, as bytes, a table or json");
    println!("  radbeeper frames import F  frames in, recomputed before they are written");
    println!("  radbeeper frames verify    every key and every chain link, from genesis");
    println!("  radbeeper export           index.html and random.html, from the logs");
    println!("  radbeeper pages            the landing page and lab reports, from the documents");
    println!("  radbeeper hotplug          sit in the session, open the monitor on plug-in");
    println!("  radbeeper preview          serve a generated site on loopback, so the viewer can fetch");
    println!();
    println!("  -d, --device PATH          serial port; repeat it for a second counter");
    println!("  -b, --baud RATE            baud (default: try 115200 then 57600)");
    println!("      --spans 3,30,300,3000,30000  averaging windows, seconds");
    println!("      --cpm-per-usvh N       tube factor (default {})", counter::DEFAULT_CPM_PER_USVH);
    println!("      --duration SECONDS     stop after this long");
    println!("      --wait [SECONDS]       a busy port: wait for it, not give up");
    println!("      --log-every SECONDS    row spacing for service (default {})",
             log::g(log::DEFAULT_LOG_EVERY));
    println!("      --logs DIR             where service, watch and backfill write, and export reads");
    println!("      --no-log               watch without writing anything");
    println!("      --no-export            watch without writing index.html (hourly, and on quit)");
    println!("      --no-frames            do not keep the raw seconds behind each random line");
    println!("      --no-backfill          skip reading the counter's history at start");
    println!("      --no-spectrum          service: do not keep the room's spectrum, a row a window");
    println!("      --no-temp              service: do not ask a counter its temperature");
    println!("      --temp-every SECONDS   service: how often it is asked (default: the row spacing)");
    println!("      --image FILE           backfill from a saved .bin, no counter");
    println!("      --serial SERIAL        which counter an image came from");
    println!("      --bytes N              how much flash to read");
    println!("      --max-gap SECONDS      a longer hole ends the averages");
    println!("      --entropy-bits N       bits a random line is worth (default 256)");
    println!("      --poll SECONDS         hotplug: how often /dev is read (default 4)");
    println!("      --settle SECONDS       hotplug: grace before a new node is opened (default 2)");
    println!("      --port N               preview: the port it listens on (default 8765)");
    println!("      --bind ADDR            preview: the address (default 127.0.0.1)");
    println!("      --tries N              hotplug: attempts per plug event (default 3)");
    println!("      --tui                  hotplug: a terminal, even where radbeeper-gui is installed");
    println!("  -o, --output STEM          where log pull writes .bin and .csv");
    println!("  -o, --output FILE          export: where the page goes (default index.html)");
    println!("      --title TEXT           export: the page's heading");
    println!("      --random-output FILE   export: the audit page (default random.html beside it)");
    println!("      --frame-budget SIZE    export: raw frame bytes carried in the page (default 2M, 0 for none)");
    println!("      --frames-page          export: also write frames.html, the frame browser (Rust only)");
    println!("      --frames-output FILE   export: where the frame browser goes (default frames.html beside it)");
    println!("      --month YYYY-MM        frames: one month rather than all of them");
    println!("      --seq N                frames show: which frame (default the newest)");
    println!("      --json | --tsv | --bin frames export: which form (default json)");
    println!("      --no-random-page       export: do not write the audit page");
    println!();
    println!("site and recompute are in the");
    println!("Python program in the same repository. They are being ported; the");
    println!("log format is here already, and tests/test_differential.py is what");
    println!("says it is the same format and not a second dialect of it.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = String::new();
    // REPEATABLE, BECAUSE TWO TUBES ARE A SUPPORTED ARRANGEMENT. `-d A -d B`
    // names both; no `-d` at all searches /dev and takes everything that
    // answers. One `-d` behaves exactly as it always did.
    let mut devices: Vec<String> = Vec::new();
    let mut baud: Option<u32> = None;
    let mut spans = vec![3.0, 30.0, 300.0, 3000.0, 30000.0];
    let mut cpm_per_usvh = counter::DEFAULT_CPM_PER_USVH;
    let mut duration: Option<f64> = None;
    let mut log_every = log::DEFAULT_LOG_EVERY;
    let mut logs: Option<std::path::PathBuf> = None;
    let mut check: Option<std::path::PathBuf> = None;
    let mut image: Option<std::path::PathBuf> = None;
    let mut bytes: Option<usize> = None;
    let mut max_gap = 300.0f64;
    let mut no_backfill = false;
    let mut no_log = false;
    let mut no_export = false;
    let mut no_frames = false;
    let mut frames_file: Option<std::path::PathBuf> = None;
    let mut wait: Option<f64> = None;
    let mut set_clock = false;
    let mut output: Option<String> = None;
    // hotplug's three. Four seconds is a read of /dev fifteen times a minute,
    // which costs nothing; settle is the grace a node gets between appearing
    // and being opened, and tries is how many times one plug event is worth
    // retrying before it is written off.
    let mut poll: f64 = 4.0;
    let mut settle: f64 = 2.0;
    let mut tries: u32 = 3;
    let mut tui = false;
    // `preview`'s two. Loopback by default: a log directory is not something
    // to put on the network by accident, so a wider bind has to be typed.
    let mut port: u16 = 8765;
    let mut bind = "127.0.0.1".to_string();
    let mut log_action = "info".to_string();
    // `frames` and its verb, which is a noun-then-verb command like `log`.
    let mut frames_action = "list".to_string();
    let mut frames_month: Option<String> = None;
    let mut frames_seq: Option<u64> = None;
    let mut frames_form = "json".to_string();
    let mut frames_in: Option<PathBuf> = None;
    let mut frames_page = false;
    let mut frames_output: Option<PathBuf> = None;
    let mut serial: Option<String> = None;
    let mut title = radbeeper::export::DEFAULT_TITLE.to_string();
    let mut random_output: Option<PathBuf> = None;
    let mut no_random_page = false;
    let mut frame_budget = radbeeper::audit::DEFAULT_FRAME_BUDGET;
    // The service asks each counter that has a thermometer for it, once a
    // log interval unless told another spacing, and not at all on --no-temp.
    let mut temp_every: Option<f64> = None;
    let mut no_temp = false;
    let mut no_spectrum = false;
    // `now --json`. `frames export` has its own reading of the same flag.
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a {
            "-h" | "--help" => {
                usage();
                return;
            }
            "--version" => {
                println!("radbeeper {}", VERSION);
                return;
            }
            "-d" | "--device" => devices.extend(next(&mut i)),
            "-b" | "--baud" => baud = next(&mut i).and_then(|v| v.parse().ok()),
            "--spans" => {
                spans = match next(&mut i).as_deref().and_then(parse_spans) {
                    Some(s) => s,
                    None => {
                        eprintln!("radbeeper: --spans wants whole seconds, comma separated");
                        std::process::exit(2);
                    }
                }
            }
            "--cpm-per-usvh" => {
                cpm_per_usvh = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(cpm_per_usvh)
            }
            "--duration" => duration = next(&mut i).and_then(|v| v.parse().ok()),
            "--wait" => {
                // AN OPTIONAL VALUE, which is a thing this parser does
                // nowhere else: `--wait` on its own waits for as long as it
                // takes, `--wait 30` gives up after thirty seconds. Peeking at
                // the next argument is unambiguous only because no command
                // this program has is a number, and none can become one --
                // `--wait watch` and `--wait 30 watch` both read correctly.
                wait = Some(match args.get(i + 1).and_then(|v| v.parse::<f64>().ok()) {
                    Some(secs) => {
                        i += 1;
                        secs
                    }
                    None => f64::INFINITY,
                });
            }
            "--logs" => logs = next(&mut i).map(std::path::PathBuf::from),
            "--port" => port = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(port),
            "--bind" => bind = next(&mut i).unwrap_or(bind),
            "--log-every" => {
                log_every = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(log_every)
            }
            "--check" => check = next(&mut i).map(std::path::PathBuf::from),
            "--frames" => frames_file = next(&mut i).map(std::path::PathBuf::from),
            "--image" => image = next(&mut i).map(std::path::PathBuf::from),
            "--serial" => serial = next(&mut i),
            "--bytes" | "--backfill-bytes" => bytes = next(&mut i).and_then(|v| v.parse().ok()),
            "--no-backfill" => no_backfill = true,
            "--no-log" => no_log = true,
            "--no-export" => no_export = true,
            "--no-frames" => no_frames = true,
            "--entropy-bits" => {
                if let Some(v) = next(&mut i).and_then(|v| v.parse::<f64>().ok()) {
                    if v > 0.0 {
                        let _ = WANT_BITS.set(v);
                    }
                }
            }
            "--set" => set_clock = true,
            "--max-gap" => {
                max_gap = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(max_gap)
            }
            "-o" | "--output" => output = next(&mut i),
            "--poll" => poll = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(poll),
            "--settle" => settle = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(settle),
            "--tries" => tries = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(tries),
            "--tui" => tui = true,
            "--title" => title = next(&mut i).unwrap_or(title),
            "--random-output" => random_output = next(&mut i).map(PathBuf::from),
            "--no-random-page" => no_random_page = true,
            "--frame-budget" => {
                frame_budget = next(&mut i)
                    .and_then(|v| parse_bytes(&v))
                    .unwrap_or(frame_budget)
            }
            "info" | "pull" if command == "log" => log_action = a.to_string(),
            // A VERB IS ONLY A VERB AFTER ITS NOUN. `export` is a command in
            // its own right; `frames export` is a different thing entirely,
            // and the guard is what keeps the two apart.
            "list" | "show" | "export" | "import" | "verify" if command == "frames" => {
                frames_action = a.to_string()
            }
            "--month" => frames_month = next(&mut i),
            "--seq" => frames_seq = next(&mut i).and_then(|v| v.parse().ok()),
            "--json" => {
                frames_form = "json".to_string();
                json = true;
            }
            "--no-temp" => no_temp = true,
            "--no-spectrum" => no_spectrum = true,
            "--temp-every" => {
                temp_every = next(&mut i).and_then(|v| v.parse().ok()).filter(|v: &f64| *v >= 1.0)
            }
            "--tsv" => frames_form = "tsv".to_string(),
            "--bin" => frames_form = "bin".to_string(),
            "--frames-page" => frames_page = true,
            "--frames-output" => frames_output = next(&mut i).map(PathBuf::from),
            _ if command == "frames"
                && frames_action == "import"
                && frames_in.is_none()
                && !a.starts_with('-') =>
            {
                frames_in = Some(PathBuf::from(a))
            }
            "site" => {
                eprintln!(
                    "radbeeper: `{}` is not in the Rust build -- it writes the log\n\
                     format, which the Python program owns. Use that one:\n\
                     \n    https://github.com/vonglurt/radbeeper",
                    a
                );
                std::process::exit(2);
            }
            _ if a.starts_with('-') => {
                eprintln!("radbeeper: unknown option {}", a);
                std::process::exit(2);
            }
            _ => command = a.to_string(),
        }
        i += 1;
    }
    if command.is_empty() {
        usage();
        return;
    }

    if command == "backfill" {
        std::process::exit(backfill_cmd(
            &spans, log_every, max_gap, bytes.unwrap_or(64 * 1024),
            devices.first().map(|s| s.as_str()), baud, logs, image.as_deref(),
            serial.as_deref(), output.as_deref(),
        ));
    }
    if command == "frames" {
        std::process::exit(frames_cmd(
            &frames_action,
            &logs.clone().unwrap_or_else(log::state_dir),
            serial.as_deref(),
            frames_month.as_deref(),
            frames_seq,
            &frames_form,
            output.as_deref(),
            frames_in.as_deref(),
        ));
    }
    if command == "export" {
        let dir = logs.unwrap_or_else(log::state_dir);
        let out = PathBuf::from(output.as_deref().unwrap_or("index.html"));
        let code = radbeeper::export::run(
            &dir, &out, !no_random_page, cpm_per_usvh, &title,
            random_output.as_deref(), frame_budget,
        );
        // THE FRAME PAGE IS ASKED FOR, NEVER ASSUMED. Without the flag this
        // command prints and writes exactly what the Python prints and
        // writes for the same arguments, which is what the differential
        // suite compares -- and what a workflow with no Rust toolchain has
        // to be able to reproduce. See the head of src/browser.rs.
        if code == 0 && frames_page {
            match radbeeper::browser::write_pages(
                &dir, &out, &title, frame_budget, frames_output.as_deref(),
            ) {
                Ok(pages) if pages.is_empty() => {
                    println!("radbeeper: no frames in {} -- no frame page written",
                             dir.display());
                    println!("    frames are the .bin files `service` and `watch` \
                              write beside the log.");
                }
                Ok(pages) => {
                    for p in pages {
                        println!("radbeeper: {} -- {} frames in {} month{}",
                                 p.path.display(), p.frames, p.months,
                                 if p.months == 1 { "" } else { "s" });
                    }
                }
                Err(e) => {
                    eprintln!("radbeeper: frames page: {}", e);
                    std::process::exit(1);
                }
            }
        }
        std::process::exit(code);
    }
    if command == "log" {
        std::process::exit(log_cmd(&log_action, bytes, output.as_deref(),
                                   devices.first().map(|s| s.as_str()), baud));
    }
    if command == "random" {
        // --check needs no hardware, so it runs before anything looks for a
        // counter: an audit of a file from another machine is the ordinary
        // case, not an odd one.
        if let Some(p) = frames_file {
            std::process::exit(frames_report(&p));
        }
        if let Some(p) = check {
            std::process::exit(check_random(&p));
        }
        std::process::exit(random(&spans, duration, devices.first().map(|s| s.as_str()), baud, logs));
    }
    // `pages`, NOT `site`. `radbeeper site` is already a command and it means
    // where the COUNTER is -- a place name against a serial over time. This
    // one builds the GitHub Pages site, and two commands one letter apart
    // meaning entirely different things is how somebody ends up publishing a
    // web page when they meant to record a garage.
    if command == "pages" {
        // Built from the documents where they are, so the root is the
        // checkout rather than a log directory: `-o DIR` moves it.
        let root = output.as_deref().map(std::path::Path::new);
        std::process::exit(radbeeper::site::run(root));
    }
    if command == "preview" {
        // `-o DIR` names what to serve, the same way `pages` uses it to name
        // where to write. The default is here, because that is where `make
        // site` has just put a page.
        let root = output.as_deref().map(std::path::Path::new)
            .unwrap_or_else(|| std::path::Path::new("."));
        std::process::exit(radbeeper::serve::run(root, &bind, port));
    }
    if command == "hotplug" {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        std::process::exit(hotplug(&dir, devices.first().map(|s| s.as_str()), baud, poll, settle, tries,
                                   duration, tui));
    }
    if command == "service" {
        let backfill = (!no_backfill).then(|| (bytes.unwrap_or(64 * 1024), max_gap));
        let temp = (!no_temp).then(|| temp_every.unwrap_or(log_every.max(1.0)));
        std::process::exit(service(
            &spans, log_every, duration, &devices, baud, logs, backfill,
            !no_frames, temp, !no_spectrum,
        ));
    }
    if command == "now" {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        std::process::exit(now_cmd(&dir, cpm_per_usvh, json));
    }

    // WATCH ASKS THE SOCKET BEFORE IT ASKS /dev, so it is settled here rather
    // than after a port has been opened. If a service is logging this counter
    // the monitor attaches to it and the port is never touched; if nobody is,
    // the monitor takes the port and serves it in turn, and the next window
    // attaches to this one.
    if command == "watch" {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        let logging = (!no_log).then(|| WatchLog {
            dir: dir.clone(),
            every: log_every,
            backfill: (!no_backfill).then(|| (bytes.unwrap_or(64 * 1024), max_gap)),
            export_every: (!no_export).then_some(3600.0),
            frames: !no_frames,
        });
        let mut feed = match Feed::open(&dir, &devices, baud, &spans, wait) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{}: {}", if e.busy { "port busy" } else { "no counter" }, e.reason);
                for line in e.detail.lines() {
                    eprintln!("    {}", line);
                }
                std::process::exit(1);
            }
        };
        watch(&mut feed, &spans, cpm_per_usvh, duration, logging, &dir);
        return;
    }

    // `probe` lists everything, served or free; only `--wait` still goes
    // the one-counter way, because waiting is for one port to turn up.
    if command == "probe" && wait.is_none() {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        std::process::exit(probe_cmd(&devices, baud, &dir, cpm_per_usvh));
    }
    let found = match wait {
        Some(limit) => find_waiting(devices.first().map(|s| s.as_str()), baud, limit),
        None => counter::find(devices.first().map(|s| s.as_str()), baud),
    };
    let c = match found {
        Ok(c) => c,
        Err(e) => {
            // A BUSY PORT IS NOT A DEAD END FOR `probe` ANY MORE. The service
            // holding the counter introduced itself the moment we connected,
            // and its greeting is most of what probe prints. What is missing
            // is the battery and the clock, which are questions for the
            // counter -- and interrupting a stream mid-second to ask one
            // costs the log a sample, which a status command has no business
            // doing to a logger.
            if e.busy && command == "probe" {
                let dir = logs.clone().unwrap_or_else(log::state_dir);
                if let Some(client) = broker::Client::attach(&dir)
                    .filter(|c| devices.iter().all(|d| c.identity().serves(d)))
                {
                    let id = client.identity();
                    for (k, c) in id.counters.iter().enumerate() {
                        if k > 0 {
                            println!();
                        }
                        println!("counter    {}", c.version);
                        println!("model      {}", counter::model_of(&c.version));
                        println!("serial     {}", c.serial_no);
                        println!("port       {} @ {} baud", c.path, c.baud);
                        print_last_start(&dir, &c.serial_no);
                    }
                    println!("held by    the radbeeper serving {}",
                             broker::socket_path(&dir).display());
                    println!("           battery and clock need the port itself:");
                    println!("           doas rc-service radbeeper stop");
                    println!("reading    radbeeper watch  attaches to it -- no handover needed");
                    return;
                }
            }
            eprintln!("{}: {}", if e.busy { "port busy" } else { "no counter" }, e.reason);
            for line in e.detail.lines() {
                eprintln!("    {}", line);
            }
            std::process::exit(1);
        }
    };

    match command.as_str() {
        "probe" => {
            println!("counter    {}", c.version);
            println!("model      {}", c.model());
            println!("serial     {}", c.serial_no);
            println!("port       {} @ {} baud", c.path, c.baud);
            if let Some(v) = c.voltage() {
                println!("battery    {:.1} V", v);
            }
            if let Some(o) = c.clock_ahead() {
                println!("its clock  {}   {}",
                         clock::format(o.theirs, "%Y-%m-%d %H:%M:%S"), describe_offset(&o));
                if o.ahead.abs() > 1.0 {
                    println!("           radbeeper clock --set  corrects it from this machine");
                }
            }
            if let Some(n) = c.cpm() {
                println!(
                    "reading    {} CPM  ({:.3} uSv/h at {:.1} CPM per uSv/h)",
                    n, n as f64 / cpm_per_usvh, cpm_per_usvh
                );
            }
            print_last_start(&logs.clone().unwrap_or_else(log::state_dir), &c.serial_no);
        }
        "cpm" => std::process::exit(cpm_cmd(&c, cpm_per_usvh)),
        "clock" => std::process::exit(clock_cmd(&c, set_clock)),
        other => {
            eprintln!("radbeeper: unknown command {}", other);
            std::process::exit(2);
        }
    }
}

// ----------------------------------------------------------------- hotplug ---
//
// WHY THE SESSION WATCHES AND udev DOES NOT. A udev rule fires as root, in
// whatever environment udev happens to have: no WAYLAND_DISPLAY, no session
// bus, and no idea which of several logged-in people a window would belong to.
// Starting a background LOGGER from udev is right, and the rule Copal installs
// does exactly that. Opening a WINDOW from udev is guesswork. So the two
// halves are split at the line where the guessing starts: the rule starts the
// service, and this -- one process inside the session, on the desktop's
// autostart line -- opens the monitor.
//
// WHAT IT POLLS, AND WHAT IT DOES NOT. The names in /dev, never the serial
// port. See counter::candidate_ports.

/// Terminal emulators the autostart will open the monitor in, best first.
/// foot is the Wayland session's; the rest are what an X11 one is likely to
/// have. The first that exists wins.
const TERMINALS: [(&str, &str); 5] = [
    ("foot", "-e"),
    ("alacritty", "-e"),
    ("urxvt", "-e"),
    ("xterm", "-e"),
    ("st", "-e"),
];

/// The first terminal emulator on PATH, as (path, exec flag).
fn find_terminal() -> Option<(PathBuf, &'static str)> {
    find_terminal_in(&std::env::var_os("PATH")?)
}

/// The search itself, given the PATH to search.
///
/// Taking the value rather than reading the environment is what makes this
/// testable: a test can lay out a directory of its own and ask which terminal
/// would be picked, without setting a variable the whole process shares.
fn find_terminal_in(path: &std::ffi::OsStr) -> Option<(PathBuf, &'static str)> {
    // BEST FIRST, AND THE ORDER IS THE POINT: the whole of PATH is searched
    // for foot before anything is searched for alacritty. A session that has
    // both wants the one its desktop installed, not the one that happens to
    // sit in an earlier directory.
    for (term, flag) in TERMINALS {
        for dir in std::env::split_paths(path) {
            let p = dir.join(term);
            if p.is_file() && is_executable(&p) {
                return Some((p, flag));
            }
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// When a window is wanted, with no `/dev` and no processes in it.
///
/// This is the whole of `hotplug` that can be wrong, so it is the whole of
/// `hotplug` that is tested. The loop around it does two things a test cannot
/// usefully do: read `/dev`, and start a terminal.
struct Watcher {
    tries: u32,
    max_tries: u32,
    settle: f64,
    due: f64,
}

impl Watcher {
    /// A counter already plugged in at login is the same event as one plugged
    /// in later, and gets the same retries rather than a special case. That is
    /// the whole reason the autostart line runs this and not a one-shot.
    fn new(max_tries: u32, settle: f64, anything_plugged_in: bool, now: f64) -> Watcher {
        Watcher {
            tries: if anything_plugged_in { max_tries } else { 0 },
            max_tries,
            settle,
            due: now,
        }
    }

    /// Should a window be opened at `now`? `window_open` is whether the last
    /// one is still up.
    fn should_open(&mut self, now: f64, window_open: bool) -> bool {
        if window_open {
            // It took: this plug event is dealt with, and no later tick is to
            // open a second window for it.
            self.tries = 0;
            return false;
        }
        if self.tries > 0 && now >= self.due {
            // THE RETRIES ARE FOR THE GAP between a node appearing and udev
            // giving it its group: the first open can be EACCES on a node that
            // is perfectly good a second later. A few attempts, then the event
            // is written off -- a cable that is not a counter must not be
            // opened again every four seconds for the rest of the session.
            self.tries -= 1;
            self.due = now + self.settle;
            return true;
        }
        false
    }

    /// The attempt found the port held and nothing serving yet. That is not a
    /// failed try -- the holder is starting up, and a backfill can take many
    /// minutes -- so the try is given back and the port is asked again after
    /// `hold`, for as long as it stays that way. A busy open is refused at the
    /// flock, before the line is configured or written, so asking costs the
    /// holder nothing.
    fn not_yet(&mut self, now: f64, hold: f64) {
        self.tries = (self.tries + 1).min(self.max_tries);
        self.due = now + hold;
    }

    /// Nodes that were not there before are there now.
    fn plugged(&mut self, now: f64) {
        self.tries = self.max_tries;
        self.due = now + self.settle;
    }
}

/// Open the monitor in a terminal, if there is something to watch.
///
/// DELIBERATELY SILENT WHEN THERE IS NO COUNTER. A window that opens at every
/// login to say "nothing is plugged in" gets closed at every login and then
/// gets deleted. A busy port is silent too: it means the counter is already
/// being read, by the service or by a monitor this session opened earlier, and
/// neither wants a second window -- the status file belongs to whoever holds
/// the port.
/// Is there a counter here worth opening a window for?
///
/// A COUNTER THAT IS BEING SERVED COUNTS, and this is the line that used to
/// make the two halves of the design deadlock. udev starts the service, the
/// service takes the port, and this then asked the PORT whether there was a
/// counter -- got `busy`, and gave up. On a machine where the logger works,
/// which is every machine it was designed for, the window could therefore
/// never open at all.
///
/// The existence of the socket is enough and a connection is not attempted:
/// greeting a client costs the server its whole history, which is not a price
/// to pay fifteen times a minute for a yes/no. A STALE node gives a false yes,
/// and that is harmless -- the monitor it opens tries the socket, finds
/// nothing behind it and takes the port itself, which is what should happen.
///
/// A BUSY PORT WITH NO SOCKET IS `NotYet`, not no. Whoever holds the port has
/// not started serving: the service backfills from both flashes before it
/// binds the socket, and that took sixteen minutes on the morning this was
/// written. Three tries a few seconds apart all landed inside it, the plug
/// event was written off, and the login that should have opened the monitor
/// opened nothing.
#[derive(Debug, PartialEq)]
enum Worth {
    Yes,
    No,
    NotYet,
}

fn worth_a_window(dir: &Path, device: Option<&str>, baud: Option<u32>) -> Worth {
    if broker::socket_path(dir).exists() {
        return Worth::Yes;
    }
    match counter::find(device, baud) {
        Ok(_) => Worth::Yes,
        Err(e) if e.busy => Worth::NotYet,
        Err(e) => {
            log::write_status(dir, &format!("dormant: {}", e.reason));
            Worth::No
        }
    }
}

/// What one attempt to open the monitor came to.
enum Opened {
    Window(Child),
    Nothing,
    NotYet,
}

fn open_window(dir: &Path, device: Option<&str>, baud: Option<u32>, tui: bool) -> Opened {
    match worth_a_window(dir, device, baud) {
        Worth::Yes => {}
        Worth::No => return Opened::Nothing,
        Worth::NotYet => return Opened::NotYet,
    }
    // THE WINDOW A PERSON WOULD HAVE OPENED THEMSELVES. If radbeeper-gui is
    // installed, that is a window on its own and needs no terminal wrapped
    // round it; if it is not, a terminal running the monitor is the window,
    // and that is what this did before the GUI existed. Both attach to
    // whatever is serving the stream, so the choice is cosmetic and `--tui`
    // takes it back.
    if !tui {
        if let Some(g) = find_gui() {
            return spawn_window(Command::new(g)).map_or(Opened::Nothing, Opened::Window);
        }
    }
    let (term, flag) = match find_terminal() {
        Some(t) => t,
        None => {
            eprintln!("no terminal emulator found; run: radbeeper watch");
            return Opened::Nothing;
        }
    };
    // AN ABSOLUTE PATH TO OURSELVES. The terminal inherits whatever directory
    // the caller had, and hotplug is started from a session whose directory is
    // not this checkout. A relative argv[0] works from the Makefile and
    // nowhere else, which is the worst way for it to be wrong.
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("radbeeper: cannot find my own path: {}", e);
            return Opened::Nothing;
        }
    };
    let mut cmd = Command::new(&term);
    cmd.arg(flag).arg(&me);
    if let Some(d) = device {
        cmd.arg("--device").arg(d);
    }
    if let Some(b) = baud {
        cmd.arg("--baud").arg(b.to_string());
    }
    cmd.arg("watch");
    spawn_window(cmd).map_or(Opened::Nothing, Opened::Window)
}

/// Start a window and let go of it.
fn spawn_window(mut cmd: Command) -> Option<Child> {
    // setsid, so the window outlives the watcher that opened it.
    //
    // SAFETY: setsid is async-signal-safe and touches nothing this process
    // shares with the child between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("radbeeper: could not open a window: {}", e);
            None
        }
    }
}

/// radbeeper-gui, if it is installed.
///
/// BESIDE US FIRST, then PATH. The two are built from one checkout and land in
/// the same directory; a session that has an old radbeeper-gui somewhere on
/// PATH and a new pair in ~/.local/bin should get the pair.
fn find_gui() -> Option<PathBuf> {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let beside = dir.join("radbeeper-gui");
            if beside.is_file() && is_executable(&beside) {
                return Some(beside);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("radbeeper-gui"))
        .find(|p| p.is_file() && is_executable(p))
}

/// `radbeeper hotplug`: open the monitor when a counter appears -- now, or in
/// an hour's time.
fn hotplug(
    dir: &Path,
    device: Option<&str>,
    baud: Option<u32>,
    poll: f64,
    settle: f64,
    tries: u32,
    duration: Option<f64>,
    tui: bool,
) -> i32 {
    let mut seen: BTreeSet<String> = counter::candidate_ports().into_iter().collect();
    let started = Instant::now();
    let now = || started.elapsed().as_secs_f64();
    let mut w = Watcher::new(tries, settle, !seen.is_empty(), now());
    let mut child: Option<Child> = None;

    loop {
        // Is the last window still up? A window that was closed, or that never
        // opened at all, leaves this None.
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(_)) | Err(_) => child = None,
                Ok(None) => {}
            }
        }
        if w.should_open(now(), child.is_some()) {
            child = match open_window(dir, device, baud, tui) {
                Opened::Window(c) => Some(c),
                Opened::Nothing => None,
                Opened::NotYet => {
                    w.not_yet(now(), SERVICE_WAIT);
                    None
                }
            };
        }
        if duration.map_or(false, |d| now() >= d) {
            return 0;
        }
        std::thread::sleep(Duration::from_secs_f64(poll));
        let ports: BTreeSet<String> = counter::candidate_ports().into_iter().collect();
        if ports.difference(&seen).next().is_some() {
            w.plugged(now());
        }
        seen = ports;
    }
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_gets_the_colours_it_has_said_it_has() {
        assert_eq!(depth_of(Some("truecolor"), Some("xterm-256color")), Depth::True);
        assert_eq!(depth_of(Some("24bit"), None), Depth::True);
        assert_eq!(depth_of(None, Some("xterm-direct")), Depth::True);
        assert_eq!(depth_of(None, Some("xterm-256color")), Depth::Cube);
        assert_eq!(depth_of(Some(""), Some("vt100")), Depth::Eight);
        assert_eq!(depth_of(None, None), Depth::Eight);
        assert_eq!(ink(Depth::True, [1, 2, 3]), "\x1b[38;2;1;2;3m");
        assert_eq!(ground_ink(Depth::Eight, [1, 2, 3]), "");
        // The corners of the cube, and a grey on the grey ramp.
        assert_eq!(cube_index([0, 0, 0]), 16);
        assert_eq!(cube_index([255, 255, 255]), 231);
        assert_eq!(cube_index([255, 0, 0]), 196);
        assert_eq!(cube_index([128, 128, 128]), 244);
    }

    #[test]
    fn the_pen_is_still_hot_in_eight_colours() {
        use drum::{heat, shade_of, SHADES};
        let code = |shade: usize| basic_ink(bytes_of(heat(shade)));
        // The floor is a faint cyan, what is flat is green, what luck
        // reaches is yellow, and the top is white.
        assert_eq!(code(0), "\x1b[2;36m");
        assert_eq!(code(shade_of(1.0, 8.0)), "\x1b[32m");
        assert_eq!(code(shade_of(8.0, 8.0) - 6), "\x1b[33m");
        assert_eq!(code(SHADES - 4), "\x1b[31m");
        assert_eq!(code(SHADES - 1), "\x1b[1;37m");
    }

    fn plain(rank: u8) -> Dot {
        Dot { rgb: [rank, rank, rank], rank, heavy: false }
    }

    #[test]
    fn a_character_is_eight_dots_and_the_colour_of_the_one_that_matters() {
        let mut paper = Paper::new(2, 1);
        assert_eq!((paper.width(), paper.height()), (4, 4));
        assert_eq!(paper.cell(0, 0), None);
        paper.set(0, 0, plain(1));
        assert_eq!(paper.cell(0, 0).map(|c| c.0), Some('\u{2801}'));
        paper.set(1, 3, Dot { heavy: true, ..plain(9) });
        paper.set(0, 3, plain(4));
        // Dots one, seven and eight; the ink of the highest rank, and heavy
        // because one of them was.
        let (glyph, dot) = paper.cell(0, 0).unwrap();
        assert_eq!(glyph, '\u{28c1}');
        assert_eq!((dot.rgb, dot.heavy), ([9, 9, 9], true));
        for y in 0..4 {
            paper.set(2, y, plain(1));
            paper.set(3, y, plain(1));
        }
        assert_eq!(paper.cell(1, 0).map(|c| c.0), Some('\u{28ff}'));
        // Off the paper is nowhere, and is not an error.
        paper.set(9, 9, plain(1));
        paper.set(-1, 0, plain(1));
        assert_eq!(paper.get(9, 9), None);
    }

    #[test]
    fn a_line_is_joined_and_hides_what_is_behind_it() {
        let mut paper = Paper::new(3, 3);
        // An older line, flat, six dots down.
        let old: Vec<Mark> = (0..6).map(|_| Some((6, plain(1), 1))).collect();
        paper.trace(&old, 0, Some(8));
        // A newer one from its baseline at ten, climbing to four and back,
        // with a gap in it.
        let new: Vec<Mark> = vec![
            Some((10, plain(2), 1)),
            Some((4, plain(2), 1)),
            Some((9, plain(2), 1)),
            None,
            Some((10, plain(2), 2)),
        ];
        paper.trace(&new, 0, Some(10));
        let column = |paper: &Paper, x: i32| -> Vec<u8> {
            (0..12).map(|y| paper.get(x, y).map_or(0, |d| d.rank)).collect()
        };
        // Column one is inked from the peak down to one short of where the
        // line was, and the older line under the peak is gone.
        assert_eq!(column(&paper, 1), vec![0, 0, 0, 0, 2, 2, 2, 2, 2, 2, 0, 0]);
        // Coming down, the column is inked from one under the peak.
        assert_eq!(column(&paper, 2), vec![0, 0, 0, 0, 0, 2, 2, 2, 2, 2, 0, 0]);
        // Where the newer line has nothing, the older one shows.
        assert_eq!(column(&paper, 3), vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
        // A pen two dots wide is the dot over its line as well; the older
        // line is over that, and still there.
        assert_eq!(column(&paper, 4), vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 2, 2, 0]);
        // Without a baseline nothing is rubbed out: lines only cross.
        let across: Vec<Mark> = (0..6).map(|_| Some((8, plain(3), 1))).collect();
        paper.trace(&across, 0, None);
        assert_eq!(column(&paper, 1), vec![0, 0, 0, 0, 2, 2, 2, 2, 3, 2, 0, 0]);
    }

    #[test]
    fn a_row_rises_where_there_is_power_and_the_pen_is_wider_for_it() {
        let axis = drum::stretches(8.0, 512);
        let luck = 8.0f32;
        // Flat, with one bin at 64 seconds three times what luck reaches.
        let bins: Vec<(f32, f32)> = (1..=64)
            .map(|k| (512.0 / k as f32, if k == 8 { 24.0 } else { 1.0 }))
            .collect();
        let marks = row_marks(&bins, &axis, 160, 24.0, 24, 40, luck, 1.0);
        let peak = (drum::place(&axis, 64.0).unwrap() * 159.0).round() as usize;
        let (y, dot, wide) = marks[peak].unwrap();
        // The loudest thing on the paper rises the whole reach, in a heavy
        // pen three dots wide, in the hottest colour there is.
        assert_eq!((y, wide, dot.heavy), (16, 3, true));
        assert_eq!(dot.rgb, bytes_of(drum::heat(drum::SHADES - 1)));
        // What is flat is a fifth of the way up, in one dot, and green.
        let (y, dot, wide) = marks[159].unwrap();
        assert_eq!((y, wide, dot.heavy), (35, 1, false));
        assert!(dot.rgb[1] > dot.rgb[0] && dot.rgb[1] > dot.rgb[2]);
        // Every column from the first bin to the last has a mark.
        assert!(marks.iter().all(|m| m.is_some()));
        // A fainter row is the same line nearer the ground's colour.
        let faint = row_marks(&bins, &axis, 160, 24.0, 24, 40, luck, 0.3);
        let (fy, fdot, _) = faint[159].unwrap();
        assert_eq!(fy, 35);
        assert!(fdot.rgb[1] < dot.rgb[1]);
        // Periods off the axis are not drawn, and nothing is not a line.
        assert!(row_marks(&[(4.0, 9.0), (2.0, 9.0)], &axis, 160, 24.0, 24, 40, luck, 1.0)
            .iter()
            .all(|m| m.is_none()));
    }

    #[test]
    fn the_drum_is_the_newest_row_at_the_foot_and_nothing_hides_it() {
        let axis = drum::stretches(8.0, 512);
        let flat = |power: f32| -> Vec<(f32, f32)> {
            (1..=64).map(|k| (512.0 / k as f32, power)).collect()
        };
        let mut loud = flat(1.0);
        loud[7].1 = 30.0;
        let rows = vec![
            analysis::TrailRow { level: 1.0, bins: flat(1.0) },
            analysis::TrailRow { level: 1.0, bins: loud },
            analysis::TrailRow { level: 1.0, bins: flat(1.0) },
        ];
        let mut paper = Paper::new(80, 8);
        draw_drum(&mut paper, &Drum { rows: &rows, axis: &axis, luck: 8.0, slide: 0.0 });
        let (base, reach) = (31, drum_reach(32));
        let flat_rise = (analysis::gain(1.0, 30.0) as f32 * reach as f32).round() as i32;
        let inked = |paper: &Paper, x: i32| -> Vec<i32> {
            (0..32).filter(|y| paper.get(x, *y).is_some()).collect()
        };
        // Away from the peak: the newest row on the last line of dots and
        // the two before it two dots apart over it.
        assert_eq!(
            inked(&paper, 159),
            vec![base - 4 - flat_rise, base - 2 - flat_rise, base - flat_rise]
        );
        // At the peak the middle row stands the whole reach over its own
        // line, and the one loudest thing on the paper is white.
        let peak = (drum::place(&axis, 64.0).unwrap() * 159.0).round() as i32;
        let top = base - 2 - reach;
        assert_eq!(paper.get(peak, top).map(|d| d.rgb), Some([255, 255, 255]));
        // The paper slides a dot up half way to the next row.
        let mut later = Paper::new(80, 8);
        draw_drum(&mut later, &Drum { rows: &rows, axis: &axis, luck: 8.0, slide: 0.5 });
        assert_eq!(
            inked(&later, 159),
            vec![base - 5 - flat_rise, base - 3 - flat_rise, base - 1 - flat_rise]
        );
        // One row of characters is still a drum.
        let mut small = Paper::new(80, 1);
        draw_drum(&mut small, &Drum { rows: &rows, axis: &axis, luck: 8.0, slide: 0.0 });
        assert!((0..4).any(|y| small.get(159, y).is_some()));
    }

    #[test]
    fn the_average_has_a_strip_of_its_own_to_the_papers_scale() {
        let axis = drum::stretches(8.0, 512);
        let mut front: Vec<(f32, f32)> = (1..=64).map(|k| (512.0 / k as f32, 1.0)).collect();
        front[7].1 = 30.0;
        let mut strip = Paper::new(80, 1);
        draw_average(&mut strip, &front, &axis, 30.0, 8.0);
        let peak = (drum::place(&axis, 64.0).unwrap() * 159.0).round() as i32;
        // What is as loud as the loudest thing on the paper is at the top
        // of the strip and white hot; what is flat is a dot up from the foot.
        assert_eq!(strip.get(peak, 0).map(|d| d.rgb), Some(bytes_of(drum::heat(drum::SHADES - 1))));
        assert!(strip.get(159, 2).is_some() && strip.get(159, 0).is_none());
        // Louder than the paper's top is held at the top, not off it.
        let mut over = Paper::new(80, 2);
        draw_average(&mut over, &front, &axis, 10.0, 8.0);
        assert!(over.get(peak, 0).is_some());
        // No room, no strip, and no fault.
        draw_average(&mut Paper::new(80, 0), &front, &axis, 30.0, 8.0);
    }

    fn day_line(start: f64, whole: bool, loud: Option<f32>) -> longdrum::LongRow {
        // Thirty-two bands from eight hours down to two minutes, or from
        // forty minutes down for a line that is not finished.
        let long = if whole { 28800.0f32 } else { 2400.0 };
        let bands = (0..32)
            .map(|k| {
                let period = long * (120.0 / long).powf(k as f32 / 31.0);
                (period, if loud.map_or(false, |p| (period / p).ln().abs() < 0.08) { 40.0 } else { 1.0 })
            })
            .collect();
        longdrum::LongRow {
            start,
            seconds: if whole { 86400.0 } else { 7200.0 },
            covered: 1.0,
            whole,
            cps: 0.7,
            bands,
        }
    }

    #[test]
    fn a_long_paper_takes_its_axis_from_its_lines() {
        let day = longdrum::Paper::Day;
        let today = clock::from_parts(2026, 11, 16, 0, 0, 0).unwrap();
        let rows = vec![
            Some(day_line(today, false, None)),
            None,
            Some(day_line(day.before(day.before(today)), true, Some(1200.0))),
        ];
        // The finished line sets the long end; the unfinished one does not.
        let axis = long_axis(day, &rows);
        assert_eq!((axis[0].long.round(), axis[0].short.round()), (28800.0, 120.0));
        // With only the unfinished line, that is the axis there is.
        assert_eq!(long_axis(day, &rows[..1])[0].long.round(), 2400.0);
        // With no line at all, what a whole line would have.
        assert_eq!(long_axis(day, &[])[0].long, 28800.0);
        assert_eq!(long_axis(day, &[])[0].short, 120.0);
        assert_eq!(long_axis(longdrum::Paper::Month, &[None])[0].short, 1200.0);
        let words = long_axis_words(day, &axis, 79);
        assert!(words.starts_with("day 8h"), "{:?}", words);
        assert!(words.ends_with("2m"), "{:?}", words);
        let col = |label: &str| words.find(label).unwrap_or(usize::MAX);
        assert!(col(" 3h") < col(" 1h") && col(" 1h") < col(" 20m") && col(" 20m") < col(" 5m"));
        assert!(!words.contains("12h") && !words.contains("1d"), "{:?}", words);
        assert_eq!(long_words(10.3 * 86400.0), "10d");
        assert_eq!(long_words(86400.0), "1d");
        assert_eq!(long_words(43200.0), "12h");
        assert_eq!(long_words(1200.0), "20m");
        assert_eq!(long_words(45.0), "45s");
        // The lines there are to show run to the oldest with a record.
        assert_eq!(long_lines(&rows), 3);
        assert_eq!(long_lines(&[None, None]), 0);
        // The unfinished line alone is not yet a paper.
        assert_eq!(long_lines(&rows[..2]), 0);
    }

    #[test]
    fn a_long_paper_is_dated_down_its_left_edge() {
        let day = longdrum::Paper::Day;
        let today = clock::from_parts(2026, 11, 16, 0, 0, 0).unwrap();
        let rows: Vec<Option<longdrum::LongRow>> = vec![
            None,
            Some(day_line(day.before(today), true, None)),
            None,
            Some(day_line(day.before(day.before(day.before(today))), true, None)),
            None,
            None,
        ];
        // Worked from a line that is there, forwards and back.
        let starts = long_starts(day, &rows, today + 7.0 * 86400.0);
        assert_eq!(starts[0], today);
        assert_eq!(clock::format(starts[5], "%d %b"), "11 Nov");
        // Three rows of characters hold six lines: a date a row, of the
        // line at the row's foot, the newest at the bottom.
        assert_eq!(
            long_date_labels(day, &starts, 3),
            vec![(2, "16 Nov".to_string()), (1, "14 Nov".to_string()), (0, "12 Nov".to_string())]
        );
        // A taller paper is dated every other row, and only where it has a line.
        let labels = long_date_labels(day, &starts, 6);
        assert_eq!(labels, vec![(5, "16 Nov".to_string()), (3, "12 Nov".to_string())]);
        let month = longdrum::Paper::Month;
        let starts = long_starts(month, &[None, None], today);
        assert_eq!(long_date_labels(month, &starts, 1), vec![(0, "Nov 2026".to_string())]);
    }

    #[test]
    fn a_long_paper_keeps_an_empty_track_and_draws_a_partial_line_where_it_reaches() {
        let day = longdrum::Paper::Day;
        let today = clock::from_parts(2026, 11, 16, 0, 0, 0).unwrap();
        let rows = vec![
            Some(day_line(today, false, None)),
            None,
            Some(day_line(day.before(day.before(today)), true, Some(1200.0))),
        ];
        let axis = long_axis(day, &rows);
        let mut paper = Paper::new(60, 3);
        draw_long(&mut paper, &rows, &axis, long_luck());
        let inked = |x: i32| -> Vec<i32> { (0..12).filter(|y| paper.get(x, *y).is_some()).collect() };
        // At the right, today's line at the foot and the day before
        // yesterday's four dots over it: yesterday's track is ground.
        let right = inked(119);
        assert_eq!(right.len(), 2, "{:?}", right);
        assert_eq!(right[1] - right[0], 4);
        // At the left only the finished line: today has not had the time
        // for an eight-hour period.
        assert_eq!(inked(0).len(), 1);
        // The twenty-minute rhythm stands the whole reach over its line,
        // in the hottest ink.
        let x = (drum::place(&axis, 1200.0).unwrap() * 119.0).round() as i32;
        let top = 11 - 4 - drum_reach(12);
        assert!((x - 2..=x + 2).any(|c| paper.get(c, top).map_or(false, |d| d.heavy)));
    }

    #[test]
    fn a_folded_paper_is_one_row_that_names_its_loudest_period() {
        let day = longdrum::Paper::Day;
        let today = clock::from_parts(2026, 11, 16, 0, 0, 0).unwrap();
        let rows = vec![None, Some(day_line(day.before(today), true, Some(1200.0)))];
        let words = folded_words(day, &rows, 79, Depth::Eight);
        // The newest line there is, by its date; its loudest period and how
        // many times chance.
        assert!(words.starts_with(&format!("{}day 15 Nov {}", DIM, OFF)), "{:?}", words);
        assert!(words.ends_with(&format!("{} 20m \u{d7}40{}", DIM, OFF)), "{:?}", words);
        // A strip of blocks: the full one where the rhythm is, low ones
        // where there is nothing, and seventy-nine characters in all.
        assert_eq!(words.matches('\u{2588}').count(), 1, "{:?}", words);
        assert!(words.matches('\u{2582}').count() + words.matches('\u{2583}').count() > 20);
        let plain: String = {
            let mut out = String::new();
            let mut skip = false;
            for ch in words.chars() {
                match (skip, ch) {
                    (false, '\x1b') => skip = true,
                    (true, 'm') => skip = false,
                    (false, c) => out.push(c),
                    _ => {}
                }
            }
            out
        };
        assert_eq!(plain.chars().count(), 79, "{:?}", plain);
        // A line with nothing over chance says so.
        let quiet = folded_words(day, &[Some(day_line(today, true, None))], 79, Depth::Eight);
        assert!(quiet.ends_with(&format!("{} flat{}", DIM, OFF)), "{:?}", quiet);
        assert_eq!(folded_words(day, &[None, None], 79, Depth::Eight), "");
    }

    #[test]
    fn the_papers_share_the_room_and_the_live_drum_is_never_squeezed_out() {
        use View::{Folded, Off, Shown};
        // No long paper: the live drum and the average's strip.
        let alone = drum_rooms(6, (Off, 30), (Off, 3));
        assert_eq!(alone, Rooms { live: 5, average: 1, ..Rooms::default() });
        // Twenty-four rows by eighty: the day folded to its row.
        let small = drum_rooms(6, (Folded, 30), (Off, 3));
        assert_eq!(small, Rooms { live: 4, average: 1, day_folded: true, ..Rooms::default() });
        // The same screen with the day paper asked for: a row of it, two
        // lines, and its labels.
        let asked = drum_rooms(6, (Shown, 30), (Off, 3));
        assert_eq!(asked, Rooms { live: 3, average: 1, day: 1, ..Rooms::default() });
        // Forty rows: all three.
        let middling = drum_rooms(11, (Shown, 30), (Shown, 3));
        assert_eq!(middling, Rooms { month: 1, day: 3, live: 4, average: 1, ..Rooms::default() });
        // A tall screen gives the average two rows and every paper more.
        let tall = drum_rooms(24, (Shown, 30), (Shown, 3));
        assert_eq!(tall.average, 2);
        assert!(tall.day >= 6 && tall.month == 2 && tall.live >= 8, "{:?}", tall);
        // A paper with no line has no rows, nor a folded row.
        assert_eq!(drum_rooms(11, (Shown, 0), (Folded, 0)), drum_rooms(11, (Off, 0), (Off, 0)));
        // One line is one row and its labels.
        assert_eq!(drum_rooms(11, (Shown, 1), (Off, 0)).day, 1);
        // Whatever is asked, every row is somebody's and the live drum has one.
        for rows in 1..40 {
            for day in [(Shown, 0), (Shown, 1), (Shown, 40), (Folded, 9), (Off, 9)] {
                for month in [(Shown, 0), (Shown, 2), (Shown, 12), (Folded, 2), (Off, 2)] {
                    let r = drum_rooms(rows, day, month);
                    assert_eq!(r.rows(), rows, "{} {:?} {:?} {:?}", rows, day, month, r);
                    assert!(r.live >= 1 || rows < 2, "{} {:?} {:?} {:?}", rows, day, month, r);
                }
            }
        }
    }

    #[test]
    fn the_keys_turn_a_paper_through_shown_folded_and_off() {
        assert_eq!(View::Shown.next(), View::Folded);
        assert_eq!(View::Folded.next(), View::Off);
        assert_eq!(View::Off.next(), View::Shown);
        assert_eq!(View::Folded.word(), "folded");
        assert_eq!(View::at_first(24), (View::Folded, View::Off));
        assert_eq!(View::at_first(30), (View::Shown, View::Folded));
        assert_eq!(View::at_first(40), (View::Shown, View::Shown));
    }

    #[test]
    fn the_log_fills_what_the_replay_did_not_reach_and_stops_at_a_break() {
        let until = 1_000_000.0;
        // Rows every thirty seconds up to twenty seconds before the first
        // sample; a hole of five minutes before the last four of them.
        let mut rows: Vec<(f64, u32, u32)> = Vec::new();
        for k in 0..10 {
            rows.push((until - 20.0 - 30.0 * 3.0 - 300.0 - 30.0 * (9 - k) as f64 - 30.0, 21, 30));
        }
        for k in 0..4 {
            rows.push((until - 20.0 - 30.0 * (3 - k) as f64, 20 + k as u32, 30));
        }
        let seconds = prefill_seconds(&rows, until, 28800.0);
        // The four rows after the hole, laid end to end: every count, in
        // whole counts a second, the newest row last.
        assert_eq!(seconds.len(), 120);
        assert_eq!(seconds.iter().sum::<u32>(), 20 + 21 + 22 + 23);
        assert_eq!(seconds[90..].iter().sum::<u32>(), 23);
        assert!(seconds.iter().all(|c| *c <= 1));
        // A row that counted more than its seconds is spread, not lost.
        assert_eq!(prefill_seconds(&[(until - 1.0, 100, 30)], until, 28800.0).iter().sum::<u32>(), 100);
        // No further back than was asked.
        assert_eq!(prefill_seconds(&rows, until, 60.0).len(), 60);
        // A log that stopped more than ten minutes ago is another sitting.
        assert!(prefill_seconds(&rows, until + 700.0, 28800.0).is_empty());
        // A row written after the first sample is the replay's, not ours.
        assert!(prefill_seconds(&[(until + 5.0, 20, 30)], until, 28800.0).is_empty());
        assert!(prefill_seconds(&[], until, 28800.0).is_empty());
    }

    #[test]
    fn the_fine_tiers_are_empty_over_seconds_that_came_from_the_log() {
        // Two thousand seconds from the log and a hundred counted.
        let series: Vec<f64> = (0..2100).map(|_| 1.0).collect();
        let mut strip = analysis::tiers_with(&series, 0, 110, 512, analysis::TIERS, 1.0);
        blank_fine(&mut strip, 2100, 2000);
        let spans = analysis::bar_spans(&strip, 2100);
        for (tier, spans) in strip.iter().zip(&spans) {
            for (value, (begins, ends)) in tier.values.iter().zip(spans) {
                if tier.seconds >= 32.0 {
                    // Coarse as a log row or coarser: drawn wherever there
                    // are seconds at all.
                    assert_eq!(value.is_some(), *ends > 0 && *begins < 2100, "{} {}", tier.seconds, begins);
                } else if *begins < 2000 {
                    assert_eq!(*value, None, "{} {}", tier.seconds, begins);
                } else {
                    assert_eq!(*value, Some(1.0), "{} {}", tier.seconds, begins);
                }
            }
        }
        // The last hundred seconds are all there in the one-second tier.
        let fine = strip.last().unwrap();
        assert_eq!(fine.values.iter().flatten().count(), 20.min(fine.values.len()).max(fine.values.len()));
    }

    #[test]
    fn the_axis_is_labelled_where_it_is_cut() {
        // Nine hours of it: three stretches, and the doublings under 512.
        let axis = drum::stretches(12.0, 32768);
        let words = axis_words(&axis, 79);
        assert!(words.starts_with("9h 6m"), "{:?}", words);
        assert!(words.ends_with("12s"), "{:?}", words);
        let col = |label: &str| words.find(label).unwrap_or(usize::MAX);
        // The cuts are at a tenth and at a tenth and a seventh of the width.
        assert_eq!(col("1h 8m"), 7);
        assert_eq!(col("8m period"), 18);
        assert!(col("4m") > col("8m period") && col("64s") > col("2m") && col("16s") > col("32s"));
        assert_eq!(words.chars().count(), 79);
        // Only the first eight minutes: one stretch, and what the figures are.
        let short = axis_words(&drum::stretches(23.0, 512), 79);
        assert!(short.starts_with("8m period"), "{:?}", short);
        assert!(short.ends_with("23s"), "{:?}", short);
        // Too narrow for any of it, it is what fits and never past the edge.
        assert!(axis_words(&axis, 6).chars().count() <= 6);
        assert_eq!(axis_words(&axis, 0), "");
    }

    #[test]
    fn the_drum_has_what_the_counts_leave() {
        assert_eq!(drum_rows(0), None);
        assert_eq!(drum_rows(1), None);
        assert_eq!(drum_rows(2), Some((1, 1)));
        assert_eq!(drum_rows(6), Some((5, 1)));
        // Twenty-four rows by eighty.
        assert_eq!(drum_rows(8), Some((5, 3)));
        assert_eq!(drum_rows(15), Some((10, 5)));
        assert_eq!(drum_rows(40), Some((35, 5)));
    }

    #[test]
    fn a_tall_screen_is_shared_between_the_table_and_the_paper() {
        assert_eq!(table_lines(24), 0);
        assert_eq!(table_lines(30), 6);
        assert_eq!(table_lines(35), 11);
        assert_eq!(table_lines(36), 6);
        assert_eq!(table_lines(46), 7);
        assert_eq!(table_lines(60), 10);
    }

    #[test]
    fn the_mercury_stands_between_fifty_and_a_hundred_fahrenheit() {
        let tube = |f: f64, height: usize| -> String {
            mercury((f - 32.0) * 5.0 / 9.0, height).into_iter().collect()
        };
        assert_eq!(tube(100.0, 4), "\u{2588}\u{2588}\u{2588}\u{2588}");
        assert_eq!(tube(120.0, 4), "\u{2588}\u{2588}\u{2588}\u{2588}");
        assert_eq!(tube(75.0, 4), "  \u{2588}\u{2588}");
        // 62.5 F is a quarter of the way, 56.25 F an eighth: half a character.
        assert_eq!(tube(62.5, 4), "   \u{2588}");
        assert_eq!(tube(56.25, 4), "   \u{2584}");
        // A cold room still has a reading in the tube.
        assert_eq!(tube(50.0, 4), "   \u{2581}");
        assert_eq!(tube(20.0, 4), "   \u{2581}");
        assert_eq!(both_scales(26.1), ("79.0\u{b0}F".to_string(), "26.1\u{b0}C".to_string()));
        assert_eq!(both_scales(-5.0).0, "23.0\u{b0}F");
        // Wide enough for the scale, the tubes and `100.0 F`.
        assert_eq!(thermometer_width(1), 16);
    }

    #[test]
    fn each_counter_has_a_tube_and_a_reading_in_both_scales() {
        let mut out = String::new();
        // Two counters, eight rows from the second, at the forty-second
        // column; the second reading is an old one.
        draw_thermometer(&mut out, 2, 42, 8, &[(26.1, false), (10.0, true)]);
        assert!(out.contains(&format!("{}{}100\u{b0}F", at(2, 42), DIM)));
        assert!(out.contains(&format!("{}{} 50\u{b0}F", at(9, 42), DIM)));
        // The tubes are two columns apart and the readings are beside them,
        // two rows each: 79 F is green, 50 F is cold and, being old, dim.
        assert!(out.contains(&format!("{}{}{}79.0\u{b0}F{}", at(4, 52), BOLD, GREEN, OFF)));
        assert!(out.contains(&format!("{}{}26.1\u{b0}C{}", at(5, 52), DIM, OFF)));
        assert!(out.contains(&format!("{}{}{}50.0\u{b0}F{}", at(6, 52), DIM, CYAN, OFF)));
        assert!(out.contains(&format!("{}{}{}\u{2588}{}", at(9, 48), "", GREEN, OFF)));
        assert!(out.contains(&format!("{}{}{}\u{2581}{}", at(9, 50), DIM, CYAN, OFF)));
        // The empty part of a tube is its glass.
        assert!(out.contains(&format!("{}{}\u{2502}{}", at(2, 48), DIM, OFF)));
        // No reading, no thermometer; and none in a single row.
        let mut none = String::new();
        draw_thermometer(&mut none, 2, 42, 8, &[]);
        draw_thermometer(&mut none, 2, 42, 1, &[(26.1, false)]);
        assert_eq!(none, "");
    }

    #[test]
    fn a_day_stays_in_its_track_unless_it_was_not_an_ordinary_day() {
        let dot = day_ink(0, 10);
        // A track of four dots whose last is the thirty-ninth: an ordinary
        // day, half a track either way, is those four dots and no others.
        let cells = [Some(0.0), Some(0.49), Some(-0.49), None, Some(1.0), Some(-30.0), Some(30.0)];
        let ys: Vec<Option<i32>> =
            day_marks(&cells, 39, 4, 40, dot).into_iter().map(|m| m.map(|m| m.0)).collect();
        assert_eq!(ys[..4], [Some(38), Some(36), Some(39), None]);
        // A whole track over is the same place in the line above's track.
        assert_eq!(ys[4], Some(34));
        // And off the paper is held at its edge.
        assert_eq!(ys[5..], [Some(39), Some(0)]);
        // A taller track gives the same day more dots.
        let ys: Vec<i32> = day_marks(&[Some(0.49), Some(-0.49)], 39, 8, 40, dot)
            .into_iter()
            .flatten()
            .map(|m| m.0)
            .collect();
        assert_eq!(ys, vec![32, 39]);
        // Today's line is the one that matters most, and the heavy one.
        assert!(day_ink(0, 10).rank > day_ink(1, 10).rank);
        assert!(day_ink(1, 10).rank > day_ink(9, 10).rank);
        assert!(day_ink(0, 10).heavy && !day_ink(1, 10).heavy);
    }

    #[test]
    fn ten_days_of_temperature_are_drawn_with_their_dates_and_means() {
        // Three days of a steady room, a reading every ten minutes.
        let now = clock::from_parts(2026, 11, 16, 12, 0, 0).unwrap();
        let samples: Vec<(f64, f64)> = (0..3 * 144)
            .map(|k| (now - k as f64 * 600.0, 22.0 + ((k / 72) % 2) as f64 * 0.1))
            .collect();
        let mut out = String::new();
        let words = draw_temp_drum(&mut out, 12, 11, 80, &samples, now, Depth::Eight);
        assert!(words.starts_with("one track is "), "{}", words);
        assert!(words.contains("\u{b0}F (") && words.contains("\u{b0}C)"), "{}", words);
        // Ten lines, today's on the row over the hours and the oldest on
        // the first; the mean beside each day that has one.
        assert!(out.contains(&format!("{}{}", at(21, 0), BOLD)), "today is at the foot");
        assert!(out.contains("Nov 16") && out.contains("Nov 15") && out.contains("Nov 07"));
        assert!(out.contains(&format!("{}{}hour", at(22, 0), DIM)));
        assert_eq!(out.matches("\u{b0}F").count(), 4, "a mean for each day that was read");
        assert!(out.contains("71.") && out.contains("22."));
        // On twenty-four rows there are ten rows for it and nine days.
        let mut out = String::new();
        draw_temp_drum(&mut out, 12, 10, 80, &samples, now, Depth::Eight);
        assert!(out.contains("Nov 08") && !out.contains("Nov 07"));
        // And with no room there is nothing.
        let mut out = String::new();
        assert_eq!(draw_temp_drum(&mut out, 12, 1, 80, &samples, now, Depth::Eight), "");
        assert_eq!(out, "");
    }

    #[test]
    fn a_clock_that_lost_its_battery_is_years_behind_and_not_a_count_of_seconds() {
        assert_eq!(describe_drift(-26.7 * 365.25 * 86400.0), "26.7 years behind");
        assert_eq!(describe_drift(3.2), "3.2 s ahead");
        assert_eq!(describe_drift(-1830.0), "30.5 min behind");
        assert_eq!(describe_drift(4.0 * 3600.0), "4.0 h ahead");
        assert_eq!(describe_drift(-3.5 * 86400.0), "3.5 days behind");
    }

    #[test]
    fn the_top_row_says_what_each_tube_last_counted() {
        let serials = vec!["F7F4CA7F05C2EA".to_string(), "F48824B8207F7E".to_string()];
        assert_eq!(
            tubes_row(&serials, &[Some(2), Some(0)], &[true, true]),
            "F7F4CA7F05C2EA     2 CPS   F48824B8207F7E     0 CPS   2 tubes averaged"
        );
    }

    #[test]
    fn a_tube_that_is_not_answering_is_empty_and_not_nought() {
        let serials = vec!["F7F4CA7F05C2EA".to_string(), "F48824B8207F7E".to_string()];
        let row = tubes_row(&serials, &[Some(2), Some(3)], &[true, false]);
        assert_eq!(
            row,
            "F7F4CA7F05C2EA     2 CPS   F48824B8207F7E             1 of 2 tubes averaged"
        );
        // Nothing to its right moves when it stops, or when it comes back.
        let both = tubes_row(&serials, &[Some(2), Some(3)], &[true, true]);
        assert_eq!(row.find("1 of 2"), both.find("2 tubes"));
        // And one that has never spoken has nothing to show either.
        assert!(tubes_row(&serials, &[Some(2), None], &[true, true])
            .contains("F48824B8207F7E             "));
    }

    #[test]
    fn a_tier_is_labelled_with_what_fits_over_it() {
        assert_eq!(tier_label(0, 32.0, 26), "32s/bar \u{b7} 13m");
        assert_eq!(tier_label(1, 16.0, 26), "F 16s/bar \u{b7} 6m");
        // Eighty columns, six tiers and an interleave: twelve each.
        assert_eq!(tier_label(1, 16.0, 12), "F 16s/bar");
        // Eleven tiers and an interleave across eighty columns: six each.
        assert_eq!(tier_label(1, 1024.0, 6), "1024s");
        assert_eq!(tier_label(6, 0.5, 8), "1/2s");
        assert_eq!(tier_label(1, 16.0, 3), "");
    }

    #[test]
    fn a_long_rig_is_named_by_its_first_four() {
        let serials: Vec<String> = (0..9).map(|k| format!("SERIAL{:08}", k)).collect();
        let row = tubes_row(&serials, &[Some(1); 9], &[true; 9]);
        assert!(row.contains("SERIAL00000003") && !row.contains("SERIAL00000004"));
        assert!(row.ends_with("and 5 more   9 tubes averaged"), "{row}");
    }

    #[test]
    fn a_port_that_does_not_answer_holds_nobody_up() {
        // Nothing is listening at either path, so both are asked and both
        // come back empty -- on their own threads, while this one goes on.
        let mut p = Prober::new();
        let asked = Instant::now();
        p.sweep(vec!["/nonexistent/a".into(), "/nonexistent/b".into()], &[], None);
        assert!(asked.elapsed() < Duration::from_millis(200));
        assert_eq!(p.asking.len(), 2);
        // Asked again before they have answered: not asked twice.
        p.sweep(vec!["/nonexistent/a".into()], &[], None);
        assert_eq!(p.asking.len(), 2);
        // A port that is held is not asked at all.
        p.sweep(vec!["/nonexistent/c".into()], &["/nonexistent/c".to_string()], None);
        assert_eq!(p.asking.len(), 2);
        let until = Instant::now() + Duration::from_secs(10);
        let mut found = Vec::new();
        while !p.asking.is_empty() && Instant::now() < until {
            found.extend(p.found());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(p.asking.is_empty() && found.is_empty());
    }


    /// ONE TUBE WRITES NO MERGED FILE, and two do.
    ///
    /// With one counter there is nothing to merge and nothing to interleave,
    /// so the merged row would restate that counter's own log to more decimal
    /// places -- a second file to explain, to back up and to get out of step.
    /// The differential suite is what caught this: it asserts that a
    /// single-counter `watch` leaves exactly one `cpm-*.tsv` behind, which is
    /// the same rule stated from the outside.
    #[test]
    fn the_merge_begins_when_there_is_something_to_merge() {
        let spans = [3.0, 30.0];
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-mergedlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = log::path(clock::now(), &dir, Some(log::MERGED));

        // One tube, for long enough that several intervals have gone by.
        let mut lg = MergedLogger::new(&spans, dir.clone(), 2.0);
        lg.counters(&["F48824B8207F7E".to_string()]);
        let mut all = Windows::new(&spans);
        for i in 0..20 {
            let when = 1_700_000_000.0 + i as f64;
            all.add(when, 2);
            assert_eq!(lg.add(0, when, 2, &all).unwrap(), None);
        }
        lg.finish(&all);
        assert!(!path.exists(), "one counter wrote a merged log");

        // A second joins. From here there IS a room to write down.
        lg.counters(&["F48824B8207F7E".to_string(), "F7F4CA7F05C2EA".to_string()]);
        let mut wrote = None;
        for i in 20..40 {
            let when = 1_700_000_000.0 + i as f64;
            for who in 0..2 {
                all.add(when + who as f64 * 0.5, 2);
                if let Some(line) = lg.add(who, when + who as f64 * 0.5, 2, &all).unwrap() {
                    wrote = Some(line);
                }
            }
        }
        let line = wrote.expect("two counters wrote a merged row");
        assert!(path.exists(), "and it went to the merged log");

        let names = log::columns(&log::merged_header(&spans));
        let cells: Vec<&str> = line.split('\t').collect();
        let at = |n: &str| cells[names.iter().position(|c| c == n).unwrap()];
        assert_eq!(at("tubes"), "2");
        // Both tubes in the breakdown, and an interleave now that there are
        // two clocks to measure between.
        assert!(at("per_tube").contains("F48824B8207F7E="), "{}", at("per_tube"));
        assert!(at("per_tube").contains("F7F4CA7F05C2EA="), "{}", at("per_tube"));
        assert!(!at("interleave").is_empty(), "no interleave was measured");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The header the monitor draws on row 0, at its real length: a path, a
    /// baud rate, a firmware string and a fourteen-character serial.
    const HEAD: &str = "/dev/ttyUSB0 @ 115200 baud   GMC-320Re 4.26   serial F48824B8207F7E";

    /// A counter already plugged in at login gets a window, and gets it at
    /// once: the autostart line runs hotplug precisely so that "already there"
    /// and "plugged in later" are one case.
    #[test]
    fn a_counter_already_there_at_login_opens_a_window() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false), "no waiting for a plug that already happened");
    }

    #[test]
    fn an_empty_dev_opens_nothing_and_waits() {
        let mut w = Watcher::new(3, 2.0, false, 0.0);
        assert!(!w.should_open(0.0, false));
        assert!(!w.should_open(100.0, false), "and goes on not opening one");
        // Until something appears.
        w.plugged(100.0);
        assert!(!w.should_open(101.0, false), "the node gets its settling time");
        assert!(w.should_open(102.0, false));
    }

    /// THE RETRIES ARE FOR THE GAP between a node appearing and udev giving it
    /// its group: the first open can be EACCES on a node that is good a second
    /// later. Three attempts, then the event is written off -- a cable that is
    /// not a counter must not be opened again every four seconds all session.
    #[test]
    fn a_plug_event_is_retried_and_then_written_off() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        let mut opens = 0;
        let mut t = 0.0;
        while t < 60.0 {
            if w.should_open(t, false) {
                opens += 1;
            }
            t += 1.0;
        }
        assert_eq!(opens, 3, "three tries for one plug event, and no more");
    }

    /// THE LOGIN THAT OPENED NOTHING, pinned: the service held both ports for
    /// a sixteen-minute backfill, every try came back busy, and the event was
    /// written off before the socket appeared. Busy-and-not-serving spends no
    /// tries, however long it lasts, and the window opens once it is over.
    #[test]
    fn a_port_held_while_the_service_starts_up_spends_no_tries() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        let mut t = 0.0;
        while t < 1000.0 {
            if w.should_open(t, false) {
                w.not_yet(t, 10.0);
            }
            t += 1.0;
        }
        // The backfill is over: the next look is still a try, and so are the
        // two after it if the window fails to open.
        let mut opens = 0;
        while t < 1100.0 {
            if w.should_open(t, false) {
                opens += 1;
            }
            t += 1.0;
        }
        assert_eq!(opens, 3, "all three tries were still there");
    }

    #[test]
    fn a_window_that_took_stops_the_retries() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false), "the first attempt");
        // It opened and is still up.
        assert!(!w.should_open(4.0, true));
        // And now it is closed again -- but the event was dealt with, so this
        // does not open a second window behind the person who closed the first.
        assert!(!w.should_open(8.0, false), "closing the window is not a plug event");
        assert!(!w.should_open(400.0, false));
    }

    #[test]
    fn a_second_counter_is_a_second_event() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false));
        assert!(!w.should_open(4.0, true), "the first window took");
        // Another node appears while that window is up.
        w.plugged(10.0);
        assert!(!w.should_open(11.0, false), "settling");
        assert!(w.should_open(12.0, false), "and the new one gets its window");
    }

    /// The terminals are tried in order, and the list is the one the desktop
    /// actually installs. foot is the Wayland session's.
    /// THE BUG THIS RELEASE EXISTS FOR, pinned: a counter held by a service
    /// that is serving the stream is worth a window. The port is busy, there
    /// is no device to find -- and it must still say yes, because the monitor
    /// it opens will attach rather than ask for the port.
    #[test]
    fn a_counter_being_served_is_worth_a_window_though_the_port_is_busy() {
        let dir = std::env::temp_dir().join(format!("radbeeper-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = broker::socket_path(&dir);
        let _ = std::fs::remove_file(&sock);
        // No socket and a device that cannot exist: nothing to open.
        assert_eq!(worth_a_window(&dir, Some("/dev/null-no-counter-here"), None), Worth::No);
        // A server on the socket, and the same impossible device: still yes.
        std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert_eq!(worth_a_window(&dir, Some("/dev/null-no-counter-here"), None), Worth::Yes);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn the_terminals_are_tried_best_first() {
        assert_eq!(TERMINALS[0].0, "foot");
        assert!(TERMINALS.iter().all(|(_, flag)| *flag == "-e"));
        assert!(TERMINALS.iter().any(|(t, _)| *t == "xterm"), "an X11 session has one of these");
    }

    fn a_terminal_called(dir: &Path, name: &str, executable: bool) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn the_terminal_search_prefers_the_desktops_over_the_directory_order() {
        let root = std::env::temp_dir().join(format!("radbeeper-term-{}", std::process::id()));
        let (first, second) = (root.join("first"), root.join("second"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        // xterm sits in the EARLIER directory and foot in the later one. foot
        // still wins: the list is ordered by what the desktop installs, not by
        // what PATH happens to reach first.
        a_terminal_called(&first, "xterm", true);
        a_terminal_called(&second, "foot", true);
        let path = std::env::join_paths([&first, &second]).unwrap();
        let (found, flag) = find_terminal_in(&path).expect("one of them");
        assert_eq!(found, second.join("foot"));
        assert_eq!(flag, "-e");

        // A file that is not executable is not a terminal.
        let root2 = root.join("third");
        std::fs::create_dir_all(&root2).unwrap();
        a_terminal_called(&root2, "foot", false);
        a_terminal_called(&root2, "st", true);
        let path = std::env::join_paths([&root2]).unwrap();
        assert_eq!(find_terminal_in(&path).unwrap().0, root2.join("st"));

        // And a PATH with none of them opens nothing rather than guessing.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let path = std::env::join_paths([&empty]).unwrap();
        assert!(find_terminal_in(&path).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_big_digits_start_clear_of_the_header() {
        // THE REGRESSION. This was a constant 54 while the header was 67
        // columns wide, so the readout was drawn straight through the
        // counter's own serial number -- at every terminal width, on both
        // implementations, and nowhere near where anyone was looking for it.
        assert!(HEAD.chars().count() > 54, "the header outgrew the old constant");
        assert!(digits_left(HEAD) > HEAD.chars().count());
    }

    #[test]
    fn the_readout_has_room_for_everything_the_counter_can_say() {
        // Two bytes of CPM is 0..=65535, and every one of them has to fit in
        // the block the readout reserves -- at the reference width of 160
        // columns, beside the longest header a 320 produces.
        let reserve = readout_width();
        for v in [0u16, 7, 42, 999, 9999, u16::MAX] {
            let drawn = big_number(&v.to_string())
                .iter()
                .map(|r| r.chars().count())
                .max()
                .unwrap_or(0);
            assert!(drawn <= reserve, "{} needs {} of {}", v, drawn, reserve);
        }
        assert!(digits_left(HEAD) + reserve <= 160,
                "the readout does not fit the terminal the shots are taken in");
    }

    #[test]
    fn the_nameplate_names_the_program_and_its_version() {
        // Out of the manifest, not a string somebody remembers to bump.
        let plate = nameplate();
        assert!(plate.starts_with(env!("CARGO_PKG_NAME")));
        assert!(plate.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_nameplate_yields_the_corner_to_the_readout() {
        let plate = nameplate().chars().count();
        let left = digits_left(HEAD);
        let busy = left + readout_width();
        // 160 columns: the corner is free and the plate sits in it.
        let x = nameplate_left(160, busy, plate).expect("no corner at 160");
        assert_eq!(x + plate, 160, "not against the right edge");
        assert!(x > busy, "drawn over the readout");
        // Narrow enough that the readout wants those columns: nothing.
        assert_eq!(nameplate_left(busy + plate, busy, plate), None);
        assert_eq!(nameplate_left(40, busy, plate), None);
    }

    #[test]
    fn a_shorter_header_lets_the_digits_come_left() {
        // Measured, not a new constant: a counter on a short path with a
        // short serial should not push the number needlessly right.
        assert!(digits_left("/dev/ttyUSB0 @ 57600 baud   GMC-300  1.0   serial A1")
                < digits_left(HEAD));
    }

    #[test]
    fn every_character_the_readout_can_contain_has_a_glyph() {
        // The readout is a formatted f64, or "--" before a window is full.
        for ch in "0123456789.-".chars() {
            assert!(glyph(ch).is_some(), "no glyph for {:?}", ch);
        }
        assert!(glyph('x').is_none());
    }

    #[test]
    fn a_big_number_is_twelve_rows_of_equal_width() {
        let rows = big_number("40.5");
        assert_eq!(rows.len(), BIG_ROWS);
        let w = rows[0].chars().count();
        assert!(w > 0);
        for r in &rows {
            assert_eq!(r.chars().count(), w, "ragged row: {:?}", r);
        }
    }

    #[test]
    fn a_number_with_no_reading_yet_still_draws() {
        let rows = big_number("--");
        assert_eq!(rows.len(), BIG_ROWS);
        assert!(rows.iter().any(|r| r.contains('█')));
    }

    #[test]
    fn an_unknown_character_is_skipped_rather_than_drawn_ragged() {
        // Whatever happens, the twelve rows stay the same width as each
        // other -- a ragged block is worse than a missing character.
        let rows = big_number("4x0");
        let w = rows[0].chars().count();
        assert!(rows.iter().all(|r| r.chars().count() == w));
    }

    #[test]
    fn spans_are_a_comma_list_and_must_be_positive() {
        assert_eq!(parse_spans("3,30,300,3000"),
                   Some(vec![3.0, 30.0, 300.0, 3000.0]));
        assert_eq!(parse_spans(" 1 , 10 "), Some(vec![1.0, 10.0]));
        assert_eq!(parse_spans("3,0"), None, "a zero-second window is not one");
        assert_eq!(parse_spans("3,-1"), None);
        assert_eq!(parse_spans("3,x"), None);
        assert_eq!(parse_spans(""), None);
    }

    #[test]
    fn a_cursor_move_is_one_based_on_the_wire_and_zero_based_here() {
        // at(0, 0) has to be the top left corner, not one row down from it.
        assert_eq!(at(0, 0), "\x1b[1;1H");
        assert_eq!(at(11, 53), "\x1b[12;54H");
    }

    #[test]
    fn the_bands_colour_the_number_the_same_way_they_colour_the_bars() {
        assert_ne!(colour_for(Level::Calm), colour_for(Level::Raised));
        assert_ne!(colour_for(Level::Raised), colour_for(Level::High));
    }

    /// `--wait`, which is one decision taken over and over: is this failure
    /// one that standing still will fix?
    fn failure(busy: bool) -> counter::NotFound {
        counter::NotFound {
            reason: "r".into(),
            detail: "d".into(),
            busy,
        }
    }

    #[test]
    fn a_busy_port_is_waited_on() {
        assert!(matches!(wait_verdict(&failure(true), 0.0, 30.0), Verdict::Retry));
        assert!(matches!(wait_verdict(&failure(true), 29.9, 30.0), Verdict::Retry));
    }

    /// The case that would otherwise turn a typo into a hang: nothing about an
    /// absent counter changes because a program asked twice.
    #[test]
    fn a_missing_counter_is_not_waited_on_at_all() {
        assert!(matches!(wait_verdict(&failure(false), 0.0, 30.0), Verdict::Give));
        assert!(matches!(wait_verdict(&failure(false), 0.0, f64::INFINITY), Verdict::Give));
    }

    #[test]
    fn the_limit_ends_the_wait_and_says_how_long_it_was() {
        match wait_verdict(&failure(true), 30.0, 30.0) {
            Verdict::Expired(note) => assert!(note.contains("30s"), "got {}", note),
            _ => panic!("30s of a 30s wait is the end of it"),
        }
    }

    /// `--wait` with no number is a wait with no end, and an hour in it is
    /// still waiting -- that is the whole difference from `--wait 3600`.
    #[test]
    fn an_unbounded_wait_does_not_expire() {
        assert!(matches!(wait_verdict(&failure(true), 3600.0, f64::INFINITY),
                         Verdict::Retry));
    }
}
