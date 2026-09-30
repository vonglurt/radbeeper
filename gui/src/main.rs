// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// radbeeper-gui -- a window onto the counters, and nothing more than a window.
//
// WHAT THIS PROGRAM CANNOT DO, BY CONSTRUCTION: open a serial port. It has no
// code for one. Every number it shows arrives down the socket that whichever
// radbeeper holds the ports is serving, which is what makes it safe to open
// and close all day while the logging carries on underneath. Closing this
// window costs the log nothing, because this window was never writing it.
//
// WHERE THE ARITHMETIC HAPPENS, AND WHY IT IS NOT HERE. Attaching to a service
// that has been up since breakfast delivers hours of history as fast as a unix
// socket can carry it -- tens of thousands of samples, in a few milliseconds.
// Turning those into as many UI messages would wedge the event loop for as
// long as it took to drain them, and every frame drawn on the way would be
// thrown away unseen. So the feed thread owns the windows, the ladders and the
// pools, folds the history in as it arrives, and hands the interface ONE
// snapshot when it is done and one a second after that.
//
// TWO TUBES ARE TWO MEASUREMENTS OF ONE NUMBER. They do not double the dose;
// they double the evidence. Everything combined below is the MEAN across the
// tubes -- the same number one of them would report, measured from twice as
// many arrivals -- and the uncertainty printed beside it is what actually
// improves. See `mean_of` and `sigma_of`.
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::{canvas, column, container, row, stack, text, Space};
use iced::{Color, Element, Fill, Font, Length, Rectangle, Renderer, Subscription, Theme};
use radbeeper::analysis::{
    self, answering, band, bar_seconds, interleave_quality, span_words, tiers_arrivals,
    tiers_with, Arrival, Band, Spectrum, Tier, Trail, Windows, TIERS,
};
use radbeeper::broker::{self, Client, CounterId, Event, Poll};
use radbeeper::{clock, entropy, log};

/// A NAMED MONOSPACE, NOT `Font::MONOSPACE`. The generic family resolves
/// through fontconfig to whatever the machine calls "monospace", and on a
/// desktop with a big font collection that came out as a DOS codepage bitmap
/// face: no lowercase, no solidus, no hyphen. The port line read
/// "<box>DEV<box>TTYUSB0" and the dose read "USV<box>H". DejaVu Sans Mono is
/// on every Linux with fontconfig and covers what this window draws.
const MONO: Font = Font::with_name("DejaVu Sans Mono");

/// What this program is, for anything that outlives the window.
///
/// ON THE PANEL AND NOT ONLY IN `--help`, because a screenshot is the form
/// this program is most often seen in: the README's clip, a bug report, a
/// message to somebody asking what changed. A picture of an instrument that
/// does not say which build it is cannot be dated, and every one of those
/// questions starts with which build it is. The terminal monitor has put its
/// name in a corner since the first version; this is the same idea with the
/// version beside it.
const NAMEPLATE: &str = concat!("radbeeper ", env!("CARGO_PKG_VERSION"));

/// Samples kept for the cascade strip.
const STRIP_KEEP: usize = 32768;

/// Rows of log kept under everything else.
const ROWS: usize = 6;

/// Columns the cascade is cut into, whatever the window's width.
///
/// Fixed, so the captions above the strip -- which are text WIDGETS, laid out
/// by the same engine as everything else -- can be given the tiers' widths as
/// fill portions and land over the right bars. The canvas stretches these
/// columns to the width it is handed.
const STRIP_COLS: usize = 240;

/// How long the cascade's vertical scale takes to forget a spike.
///
/// THE SCALE WAS BREATHING. Taking the tallest bar in view as the top means
/// every burst rescales the whole strip and every quiet second rescales it
/// back, so the bars a person is trying to read never stand still. The top
/// now rises the instant something exceeds it and sinks over half a minute,
/// which is long enough that ordinary Poisson lumpiness does not move it at
/// all.
const PEAK_TAU: f64 = 30.0;

/// How tall each band of the instrument panel is inside the one canvas.
///
/// THE WHOLE PANEL IS ONE CANVAS, dials and charts together, because only the
/// last canvas widget in a view is drawn on this renderer. Everything with
/// letters in it is a widget stacked over the top -- see `view`.
/// The dials take a fixed band at the top; the two charts share whatever the
/// window has left, three parts to two. A fixed height for those would leave a
/// band of empty grey under the table on a tall window and clip them on a
/// short one.
const CLUSTER_H: f32 = 156.0;

/// How tall the dial band actually is, given how many rows of per-tube
/// readings have to sit beside it.
///
/// THE BAND CANNOT BE A CONSTANT ONCE THE TUBE COUNT IS NOT. Nine counters is
/// two more rows of numbers than one counter, and a fixed band let them
/// overflow into the cascade's captions -- two pieces of text on top of each
/// other, which is the one thing a dense panel must never do.
fn cluster_h(tubes: usize, scale: f32) -> f32 {
    let rows = if tubes > 1 { tubes.div_ceil(5) } else { 0 };
    // THE ROWS OF PER-TUBE READINGS DO NOT SCALE. They are text at a fixed
    // size, beside the dials rather than on them, so they take the same
    // twelve pixels a row whatever the faces are doing.
    CLUSTER_H * scale + rows as f32 * 12.0
}
/// The cascade's share of the space under the dials.
const CASCADE_SHARE: f32 = 0.62;

/// The trail: seconds in a row, seconds between rows, rows kept.
///
/// THREE HUNDRED SECONDS, because five minutes is the stretch somebody
/// asks about; it is not a power of two, and `analysis::powers` sums it as
/// it is. A row every ten is twenty-nine in thirty of its seconds shared
/// with the row before, so a ridge is a line and not a row of dots.
const FALL_WINDOW: usize = 300;
const FALL_HOP: usize = 10;
const FALL_DEPTH: usize = 48;
/// The seconds of the average a row is held against: the fourth of the
/// panel's five averages. See `Waterfall::leveled`.
const FALL_LEVEL: usize = 3000;
/// The shortest window the trail is drawn in. Under it the counts and the
/// spectrum have the panel to themselves, as they had.
const FALL_ROOM: f32 = 640.0;
/// How far below a row the next is drawn, and how far the loudest thing
/// in view rises from its own line: six rows. What is flat rises about
/// one, by `analysis::gain`.
const TRAIL_STEP: f32 = 4.0;
const TRAIL_REACH: f32 = 24.0;
/// The counts' share of the space under the dials when the trail has the
/// rest, and how much of the rest the spectrum's own bars stand in.
const CASCADE_WITH_TRAIL: f32 = 0.45;
const SPECTRUM_BARS: f32 = 0.22;
/// Where each chart is on the canvas: its top and its height.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Regions {
    cascade: (f32, f32),
    spectrum: (f32, f32),
}

/// The canvas under the dials, cut in two: the counts, and under them the
/// spectrum with its trail when there is one.
fn regions(height: f32, cluster: f32, trail: bool) -> Regions {
    let rest = (height - cluster - 4.0).max(40.0);
    let share = if trail { CASCADE_WITH_TRAIL } else { CASCADE_SHARE };
    // A pixel under the counts' floor, so the two do not touch; and a
    // margin at the foot, where the readouts sit under the canvas.
    let top = cluster + rest * share + 1.0;
    Regions {
        cascade: (cluster, rest * share),
        spectrum: (top, (height - top - 6.0).max(1.0)),
    }
}

/// How much of the panel's width each stretch of long periods has.
///
/// THE LEFT OF THE AXIS IS COMPRESSED, because there is next to nothing in
/// it. On an axis that gave every doubling of the period the same width,
/// the periods over five minutes had the left two fifths of the panel and
/// fifteen bins to put there -- a window holds one bin at its own length,
/// one at half, one at a third -- while the right-hand fifth held a
/// hundred. So the axis is cut where the windows are, as the counts' strip
/// is cut into tiers, and each stretch is a logarithm of its own: five
/// minutes and under has most of the width, and what is longer has these.
const STRETCH_LONG: f32 = 0.10;
const STRETCH_MIDDLE: f32 = 0.14;
/// And when the longest window to have answered is the shortest there is.
const STRETCH_FIRST: f32 = 0.06;

/// A stretch of the period axis: the periods at its two ends, and where it
/// begins and ends across the panel, from nought to one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stretch {
    long: f64,
    short: f64,
    left: f32,
    right: f32,
}

/// The axis the spectrum and its trail are both drawn on, from the longest
/// window that has answered down to `floor`.
fn stretches(floor: f64, longest: usize) -> Vec<Stretch> {
    let base = FALL_WINDOW as f64;
    let cuts: Vec<(f64, f32)> = LADDER
        .iter()
        .rev()
        .filter(|w| **w <= longest && **w as f64 > base)
        .map(|w| (*w as f64, *w))
        .map(|(long, w)| {
            let share = match (w == LADDER[0], w == LADDER[LADDER.len() - 1]) {
                (true, _) => STRETCH_FIRST,
                (_, true) => STRETCH_LONG,
                _ => STRETCH_MIDDLE,
            };
            (long, share)
        })
        // The shortest window is a stretch of its own only while it is the
        // longest that has answered: after that the next one holds its
        // periods and more.
        .filter(|(long, _)| *long as usize != LADDER[0] || longest == LADDER[0])
        .collect();
    let mut out = Vec::new();
    let mut left = 0.0f32;
    for (k, (long, share)) in cuts.iter().enumerate() {
        let short = cuts.get(k + 1).map(|c| c.0).unwrap_or(base);
        out.push(Stretch { long: *long, short, left, right: left + share });
        left += share;
    }
    out.push(Stretch { long: base.min(longest as f64), short: floor.min(base), left, right: 1.0 });
    out
}

/// Where a period is across the panel, from nought to one; None where it
/// is longer than the axis goes, or shorter.
fn place(axis: &[Stretch], period: f64) -> Option<f32> {
    let s = axis.iter().find(|s| period <= s.long && period >= s.short)?;
    let span = (s.long / s.short).ln();
    let t = if span > 0.0 { ((s.long / period).ln() / span) as f32 } else { 0.0 };
    Some(s.left + t.clamp(0.0, 1.0) * (s.right - s.left))
}

/// How many shades a trace is drawn in.
const SHADES: usize = 16;

/// Which shade a power is: the first for nothing, and the last for half
/// as much again as luck reaches, and over.
///
/// BY ITS LOGARITHM, as the heights are. In proportion, everything under
/// three times the mean -- which is nearly everything -- was the first
/// two shades of ten, and the pen had one colour for the floor and nine
/// for what is hardly ever there. By `analysis::gain` what is flat is the
/// fifth shade of sixteen and what luck reaches is the fourteenth.
fn shade_of(power: f32, luck: f32) -> usize {
    let t = analysis::gain(power as f64, 1.5 * luck.max(1e-6) as f64) as f32;
    ((t * SHADES as f32) as usize).min(SHADES - 1)
}

/// The colour of a shade: the pen's temperature.
///
/// MOST OF IT IS GREEN, because most of what is drawn is the floor and the
/// eye tells more greens apart than it does any other colour. From a teal
/// that is hardly there, through sea green and green to lime, is the run
/// from nothing up to what luck reaches: eleven shades of the sixteen, for
/// the part of the paper where the detail is. Then it is hot. Yellow is
/// the luck line; over it orange and red, which no floor ever is; and the
/// last is white, for what is half as much again as luck could do.
///
/// The same in both skins, because the ground is: see `shaded`.
fn heat(shade: usize) -> Color {
    const STOPS: [(f32, [f32; 3]); 8] = [
        (0.00, [0.16, 0.38, 0.40]),
        (0.22, [0.14, 0.60, 0.46]),
        (0.45, [0.27, 0.82, 0.38]),
        (0.70, [0.66, 0.93, 0.30]),
        (0.84, [1.00, 0.86, 0.24]),
        (0.91, [1.00, 0.55, 0.14]),
        (0.96, [1.00, 0.27, 0.20]),
        (1.00, [1.00, 0.96, 0.90]),
    ];
    let t = shade.min(SHADES - 1) as f32 / (SHADES - 1) as f32;
    let k = STOPS.iter().rposition(|s| s.0 <= t).unwrap_or(0).min(STOPS.len() - 2);
    let ((t0, a), (t1, b)) = (STOPS[k], STOPS[k + 1]);
    let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
    Color {
        r: a[0] + (b[0] - a[0]) * f,
        g: a[1] + (b[1] - a[1]) * f,
        b: a[2] + (b[2] - a[2]) * f,
        a: 1.0,
    }
}

/// How wide the pen is for a shade, in pixels: under one for the floor, and
/// three for the hottest. The shade is a logarithm, so the width is.
fn pen(shade: usize) -> f32 {
    0.7 + 2.3 * shade.min(SHADES - 1) as f32 / (SHADES - 1) as f32
}

/// The ground the trail is drawn on: the panel's, half way to black.
fn shaded(ground: Color) -> Color {
    Color { r: ground.r * 0.5, g: ground.g * 0.5, b: ground.b * 0.5, a: 1.0 }
}

/// Where the bar under the pointer is: which tier, and which bar of it.
///
/// `x` is from the left of the strip and `width` is the strip's. The bars
/// are as wide as each other whatever tier they are in, so this is a
/// count of bars and then a walk along the tiers.
fn bar_at(strip: &[Tier], x: f32, width: f32) -> Option<(usize, usize)> {
    let total: usize = strip.iter().map(|t| t.columns).sum();
    if total == 0 || width <= 0.0 || x < 0.0 || x >= width {
        return None;
    }
    let mut bar = ((x / width * total as f32) as usize).min(total - 1);
    for (k, t) in strip.iter().enumerate() {
        if bar < t.columns {
            return Some((k, bar));
        }
        bar -= t.columns;
    }
    None
}

/// What the pointer is over, in words: when the bar was, how long it is,
/// what the trend reads there and what the bar does.
///
/// IN CPM, as every other rate on the panel is. The bar is what was
/// counted in its own seconds; the trend is that bar with its neighbours,
/// and is the one the circle is drawn on.
fn hover_words(strip: &[Tier], end: i64, shift: i64, at: (usize, usize)) -> Option<String> {
    let tier = strip.get(at.0)?;
    let bar = tier.values.get(at.1).copied().flatten();
    if tier.seconds < 1.0 {
        // The interleave: one tube's one reading, and no stretch of time.
        return Some(format!(
            "one tube's second \u{b7} {}",
            bar.map(|v| format!("{:.0} counts", v)).unwrap_or_else(|| "nothing".into())
        ));
    }
    let (from, to) = *analysis::bar_spans(strip, end).get(at.0)?.get(at.1)?;
    let trend = analysis::trend(strip).get(at.0)?.get(at.1).copied().flatten();
    let cpm = |v: Option<f64>| match v {
        Some(v) => format!("{:.1} CPM", v * 60.0),
        None => "nothing measured".to_string(),
    };
    Some(format!(
        "{} \u{b7} {}s \u{b7} trend {} \u{b7} bar {}",
        clock::format((from + shift) as f64, "%H:%M:%S"),
        (to - from).max(1),
        cpm(trend),
        cpm(bar)
    ))
}

/// One dial at its smallest: the face, the space it is given.
///
/// THE SMALLEST, NOT THE SIZE. These were fixed, and on a maximised window --
/// which is what a hero clip and a spare monitor both are -- two 126-pixel
/// dials sat in the corner of twelve hundred pixels with the whole top right
/// of the panel empty beside them. An instrument cluster that does not use
/// the glass it is given looks like a mistake, and it is one: the dials are
/// the thing being read at a glance, and at a glance is exactly when size is
/// what makes a needle legible. See `dial_scale`.
const DIAL_R: f32 = 56.0;
const DIAL_W: f32 = 126.0;
/// Measured from the top of the panel, so the readouts stacked over the face
/// land in the same place whatever the tube count. See `Chart::cluster`.
const DIAL_CY: f32 = DIAL_R + 8.0;

/// The largest the cluster is allowed to grow to.
///
/// A DIAL CAN BE TOO BIG. Past about half as much again the face stops
/// reading as an instrument on a panel and starts reading as a logo, and the
/// numbers beside it -- which are a fixed size, because they are text -- look
/// stranded next to it.
const DIAL_MAX_SCALE: f32 = 1.75;

/// The most of a window's height the instrument cluster may take.
///
/// RAISED WHEN THE VERDICT LINES WENT. Four rows came out of the bottom of
/// the panel and the dials were the thing most short of room, so they took
/// the larger share of what was freed -- a bigger face is a needle that can
/// be read at a glance across a desk, which is the entire argument for
/// drawing a dial instead of printing the number again.
const CLUSTER_SHARE: f32 = 0.34;

/// How much bigger than its minimum to draw the cluster, in this window.
///
/// TWO BUDGETS, AND THE SMALLER WINS. Width, because the dials share the top
/// band with the readout beside them and that block does not shrink: whatever
/// is left over after it has its room is what the dials may grow into.
/// Height, because a cluster that took a third of a tall window would be
/// taking it from the charts, and the charts are the instrument -- the same
/// rule that decides what goes first when there is no room at all.
///
/// Never below 1.0. The small-window layout is the one that was measured
/// against a 560x400 tile, and nothing here may make that worse.
fn dial_scale(size: iced::Size, tubes: usize) -> f32 {
    // What the readout beside the dials needs: the big number and the five
    // window rows, at the sizes they are drawn.
    const READOUT_W: f32 = 320.0;
    let faces = 2.0;
    let spare = (size.width - 22.0 - READOUT_W) / (faces * DIAL_W);
    // Rather less than a third of the height. The charts are the instrument
    // and this is taken from them, so the share is the smallest one that
    // still leaves the cluster looking like it belongs on the panel rather
    // than parked in the corner of it.
    let tall = size.height * CLUSTER_SHARE / cluster_h(tubes, 1.0);
    spare.min(tall).clamp(1.0, DIAL_MAX_SCALE)
}

/// Where the digits on the face start.
///
/// UNDER THE HUB, NOT OVER IT. The reading goes on the black face below the
/// spindle, as it does on the instruments this borrows from -- over the hub
/// it is a number with a needle rotating through it.
const DIAL_TEXT_TOP: f32 = DIAL_CY + 4.0;

/// Where the two lines naming the face go: under the bezel, not on it.
///
/// A FACE IS ROUND AND A LINE OF TEXT IS NOT. Everything drawn on the face
/// has to fit the CHORD at its own height, and the chord runs out fast below
/// the middle: forty pixels under the centre of a 56-pixel face there are 78
/// left -- and the band arcs are inside that, at 0.82 of the radius, so a
/// line only has to be 65 pixels wide before its ENDS cross them. "1s 60 30s
/// 85" did, and "3s held . 41-117" went past the bezel and out onto the
/// panel, which reads as a mistake rather than as a label.
///
/// So only the reading stays on the face, which is what a gauge face is for,
/// and everything supporting it moves to a nameplate under the bezel where
/// the width available is the whole dial's share of the row and does not
/// depend on how far down the line sits.
const DIAL_PLATE_TOP: f32 = DIAL_CY + DIAL_R + 7.0;

/// How heavy each face's needle is drawn, as a half-width in pixels.
///
/// THE TWO FACES ARE NOT THE SAME INSTRUMENT AND SHOULD NOT LOOK IT. The raw
/// face carries a needle per tube and they have to be told apart, so they are
/// drawn thin and in their own colours -- nine of them at the collected
/// needle's weight would be a black disc. The collected face carries exactly
/// one needle and it is the panel's headline: it gets a proper tapered
/// pointer, wide at the hub and sharp at the tip, which is both easier to
/// read at a glance and what the instruments this borrows from actually have.
const NEEDLE_RAW: f32 = 0.0;
const NEEDLE_HELD: f32 = 3.4;

/// The dial's scale: three decades, fixed, from 3 CPM to 3000.
///
/// A GAUGE WITH A MOVING SCALE IS NOT A GAUGE, and this one used to have one.
/// The range was picked from a list of full-scale marks by a function of the
/// CURRENT READING with no memory of the last -- so a reading sitting near a
/// boundary flipped the whole face back and forth, every band and every tick
/// jumping with it, several times a minute. The doc comment claimed the range
/// "only ever moves up a step"; nothing in the code did that, and nothing
/// could, because there was nowhere to keep what the last step had been.
///
/// A PEAK HOLD WOULD HAVE FIXED THE FLICKER AND KEPT THE PROBLEM. The needle
/// would still mean something different at two o'clock than it did at one,
/// and a dial whose angles have to be re-read after every glance is a chart
/// with extra steps.
///
/// SO THE SCALE IS FIXED, AND LOGARITHMIC BECAUSE THE QUANTITY IS. Background
/// is tens of counts a minute and a source is thousands; on a linear face the
/// only reading anybody ever takes sits in the bottom tenth. Three decades put
/// 3, 30, 300 and 3000 at nought, a third, two thirds and full -- and those
/// are the numbers the bands are already named after, so the tick marks and
/// the colours are the same statement made twice.
///
/// The floor is `BAND_ATTENUATED`: under 3 CPM the counter is reporting
/// itself rather than the room, and pegging at the bottom stop is the honest
/// picture of that.
const DIAL_FLOOR: f64 = analysis::BAND_ATTENUATED;
const DIAL_CEIL: f64 = 3000.0;

/// The window the big number is an average over, when it has one.
const HEADLINE: f64 = 30.0;

/// The window the collecting needle sits at.
///
/// THREE SECONDS, NOT ONE. One second of one tube is a handful of arrivals --
/// at background, two or three -- so a one-second needle is mostly Poisson
/// noise, and what it draws is the counting statistics rather than the room.
/// Three seconds is the shortest window the rest of the program already keeps,
/// it is what the panel's top line of figures reports, and at three times the
/// counts it is root-three steadier: a needle somebody can actually read a
/// value off, still quick enough to show a source passing under the tube.
const NEEDLE_SPAN: f64 = 3.0;

/// The window the slow pointer sits at -- the same half minute as the big
/// number, so the dial and the headline agree by construction.
const POINTER_SPAN: f64 = 30.0;

/// How long the collecting meter's three-second window is kept in arrivals.
///
/// A little more than the longest thing computed from it, so the half-minute
/// pointer has a full half minute behind it and the trim never eats a sample
/// the next tick wanted.
const RECENT_KEEP: f64 = POINTER_SPAN + 2.0;

/// How often the meters are recomputed and sent, at most.
///
/// THE SNAPSHOT IS A SECOND'S WORK AND THE NEEDLE IS NOT. Everything in a
/// Snapshot -- the cascade, three spectra, the table -- changes once a second
/// and costs a clone of all of it to send. The needle wants to MOVE, which is
/// a handful of floats, so it travels separately and twelve times as often.
///
/// And only when it has moved: see `Meters::worth_sending`. A steady reading
/// sends nothing between snapshots, which matters because this program's
/// usual home is a VM with no GPU, where every message is a full software
/// re-render of the panel.
const METER_MS: u64 = 80;

/// How long a silence means the server has gone rather than gone quiet.
///
/// Ten seconds, where a sample is due every one. It was the read timeout
/// itself until the meters wanted a twelfth of a second; now the poll is
/// short and this is counted, which is the same rule stated once instead of
/// hidden in an argument.
const ADRIFT_AFTER: Duration = Duration::from_secs(10);

/// THE OVERLAY, AND WHY THESE THREE. A spectrum resolves periods up to its own
/// window and no further: you cannot see an hourly rhythm in ten minutes of
/// listening. One window is therefore always the wrong question for somebody
/// hunting an unknown period, so three are run at once and drawn on top of one
/// another against a shared logarithmic period axis -- a real line stands in
/// the same place in all three, which is most of what tells it from a fluke.
///
/// Powers of two because the transform is radix-2. 512 s is eight minutes and
/// answers quickly; 4096 s is an hour and change; 32768 s is nine hours, which
/// is the working day the longest averaging window already watches.
const LADDER: [usize; 3] = [512, 4096, 32768];

/// The width the cutoff is worked out against.
///
/// A nominal figure rather than the real pixel width, so the text under the
/// panel and the bars in it agree about what is being shown whatever size the
/// window happens to be. Being a little conservative on a wide screen costs
/// nothing; disagreeing with the caption would cost the panel its credibility.
const SPECTRUM_COLS: f64 = 600.0;

/// The shortest period the overlay will draw, worked out rather than picked.
///
/// NOT TWO SECONDS, WHICH IS ONLY WHERE THE TRANSFORM STOPS. A window of W
/// seconds has a bin at every W/n, so the bins crowd towards the short-period
/// end: by two seconds a nine-hour window has sixteen thousand of them and the
/// axis has a few hundred columns to put them in. Folding twenty-seven bins
/// into one column and taking the loudest does not draw a line, it draws the
/// largest of twenty-seven draws -- high by construction and high EVERYWHERE,
/// a wall of noise across the short end that hides the thing the panel exists
/// to find.
///
/// AND NOT A ROUND NUMBER EITHER. The honest floor is wherever the SHORTEST
/// window's bins are still a bar apart, which depends on that window, on the
/// longest one (because the two set the span of the axis) and on how many
/// columns there are. It is implicit -- the floor sets the span and the span
/// sets the floor -- so it is solved by iterating, which converges in three or
/// four passes from any starting guess. A hand-picked 30s threw away a whole
/// octave the 512-second window could have shown.
fn period_floor(shortest: usize, longest: usize) -> f64 {
    let mut p = 2.0f64;
    for _ in 0..8 {
        let span = (longest as f64 / p).ln().max(1e-9);
        // Two seconds is the Nyquist limit of a one-second sample and nothing
        // is resolvable below it at any width.
        p = (shortest as f64 * span / SPECTRUM_COLS).max(2.0);
    }
    p
}

/// The shortest period THIS window can put a bar's width between.
///
/// ONE BIN, ONE BAR, which is the whole of the rule. Bins are spaced by `P/W`
/// of an e-fold at period P, so over an axis of `span` e-folds and
/// `SPECTRUM_COLS` columns that is one column when `P = W * span / columns`.
/// Shorter than that and the layer is drawing several bins per column, so it
/// stops. A long window therefore covers the long end of the axis and a short
/// one the short end -- each over the band it is actually good for, and
/// overlapping in the middle where both are.
fn layer_floor(window: usize, shortest: usize, longest: usize) -> f64 {
    let span = (longest as f64 / period_floor(shortest, longest)).ln().max(1e-9);
    (window as f64 * span / SPECTRUM_COLS).max(2.0)
}

/// The ladder's two ends, which everything above is worked out from.
///
/// OF THE WINDOWS THAT HAVE ANSWERED. The longest is nine hours, and until
/// nine hours have been counted it has nothing to draw: an axis that ran out
/// to it anyway gave the left third of the panel to a window that was not
/// there, and squeezed the two that were into what was left. So the axis
/// ends at the longest window with a spectrum in it, and grows a step when
/// the next one has its first -- to 8m 32s, then 1h 8m, then 9h 6m.
///
/// Before any has answered there is nothing drawn, and the ends are the
/// shortest window's, which is the one that will.
fn ladder_ends(layers: &[Layer]) -> (usize, usize) {
    let answered = || layers.iter().filter(|l| !l.rel.is_empty()).map(|l| l.window);
    let shortest = layers.iter().map(|l| l.window).min().unwrap_or(2);
    (
        answered().min().unwrap_or(shortest),
        answered().max().unwrap_or(shortest),
    )
}

/// Where the socket is, when it is not where it usually is.
///
/// A ONCE-SET GLOBAL, because the subscription is built from a plain function
/// pointer with nowhere to hang a captured path. One flag, read at startup,
/// never written again.
static LOGS: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The skin, chosen once at startup: `--theme`, or what the desktop is
/// wearing. Beside LOGS and for the same reason -- there is nowhere to hang a
/// captured value on a plain function pointer.
static SKIN: std::sync::OnceLock<Skin> = std::sync::OnceLock::new();

/// Ask for the renderer that can actually work here, before anything probes.
///
/// WHAT THIS IS FIXING: eight lines of somebody else's diagnostics on every
/// start.
///
///     virtio_gpu: driver missing
///     libEGL warning: egl: failed to create dri2 screen
///
/// four times over. None of it is wrong and none of it is ours: it is Mesa's
/// gallium loader and libEGL reporting, at warning level, that this machine
/// has no GPU they can drive -- after which iced falls back to tiny-skia by
/// itself and the window comes up perfectly. But a program that prints four
/// warnings and then works is a program that looks broken, and the only way
/// to find out otherwise is to read the README.
///
/// SO DO NOT ASK THE QUESTION THAT PRODUCES THE ANSWER. The messages come out
/// of probing wgpu; iced probes wgpu because nothing told it not to; and
/// `ICED_BACKEND` is how you tell it. Setting the environment variable rather
/// than hard-coding the choice leaves `ICED_BACKEND=wgpu` working for anyone
/// who wants the GPU path back, which is the whole point of compiling both
/// renderers in.
///
/// THE TEST IS THE DRM DRIVER, not a feature probe, because every feature
/// probe is the thing that prints. A virtio GPU is this distribution's usual
/// home -- Alpine under UTM or QEMU -- and on one without virgl the host has
/// no 3D to lend: Mesa drops to llvmpipe, where wgpu either fails to start or
/// crawls. tiny-skia is both quieter and faster there. Anything else keeps
/// wgpu, so a real card is still a real card.
///
/// And the two log levels, for the case where wgpu IS tried and fails anyway:
/// then the fallback is real and still nobody needs four copies of it.
fn pick_renderer(asked: Option<&str>) {
    if std::env::var_os("EGL_LOG_LEVEL").is_none() {
        std::env::set_var("EGL_LOG_LEVEL", "fatal");
    }
    if std::env::var_os("MESA_LOG_FILE").is_none() {
        std::env::set_var("MESA_LOG_FILE", "/dev/null");
    }
    if let Some(name) = asked {
        std::env::set_var("ICED_BACKEND", name);
        return;
    }
    // Already asked for, by whoever launched us. Theirs, not ours.
    if std::env::var_os("ICED_BACKEND").is_some() {
        return;
    }
    if software_only() {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }
}

/// Whether this machine's graphics are software whatever we ask for.
fn software_only() -> bool {
    let Ok(cards) = std::fs::read_dir("/sys/class/drm") else {
        // No DRM at all: there is nothing for wgpu to find.
        return true;
    };
    let mut saw_card = false;
    for card in cards.flatten() {
        let name = card.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("card") {
            continue;
        }
        saw_card = true;
        let driver = card.path().join("device/driver");
        let Ok(target) = std::fs::read_link(&driver) else { continue };
        let driver = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // `virtio-pci` is what the guest kernel binds; `virtio_gpu` is what
        // Mesa then says it cannot load. Either spelling is this case.
        if !driver.starts_with("virtio") {
            return false;
        }
    }
    saw_card
}

fn logs_dir() -> std::path::PathBuf {
    LOGS.get().cloned().unwrap_or_else(log::state_dir)
}

fn main() -> iced::Result {
    let mut renderer: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--logs" => {
                if let Some(d) = args.next() {
                    let _ = LOGS.set(std::path::PathBuf::from(d));
                }
            }
            "--theme" => {
                if let Some(t) = args.next() {
                    let _ = SKIN.set(Skin::named(&t));
                }
            }
            "--renderer" => {
                renderer = args.next();
            }
            "-h" | "--help" => {
                println!("radbeeper-gui -- a window onto the counters");
                println!();
                println!("  --logs DIR    where the serving radbeeper keeps its socket");
                println!("  --theme T     dark, antiquity, or auto (the default:");
                println!("                whatever ~/.config/copal/current says)");
                println!("  --renderer R  wgpu or tiny-skia (the default: wgpu,");
                println!("                except on a virtio GPU, which has none)");
                println!();
                println!("It opens no serial port. Start `radbeeper service` first, or");
                println!("`radbeeper watch`, and this attaches to whichever is serving.");
                return Ok(());
            }
            _ => {}
        }
    }
    let _ = SKIN.set(Skin::detect());
    // BEFORE THE RUNTIME STARTS, because it is the runtime's first act that
    // prints. See `pick_renderer`.
    pick_renderer(renderer.as_deref());
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        // AN APPLICATION ID, so a compositor can say anything at all about
        // this window. Without one Hyprland reports its class as the empty
        // string, and a `windowrule` has nothing to match on -- no float, no
        // size, no workspace, no way to pick it out of a tiling layout. The
        // convention is the basename of the .desktop file.
        .window(iced::window::Settings {
            size: iced::Size::new(760.0, 900.0),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "radbeeper-gui".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

// ---------------------------------------------------------------- messages ---

/// A held range that snaps outward and drifts back like molasses.
///
/// A PEAK HOLD THAT FORGETS SLOWLY. The needle bounces around several times a
/// second and the eye cannot integrate that; what a person actually wants off
/// a dial is *where has it been lately*. So each extreme moves out instantly
/// when the reading pushes past it -- the needle shoves the bug -- and creeps
/// back toward the reading with a time constant of the window it stands for.
/// A true windowed min and max would be exact and would also lurch every time
/// an old extreme fell out of the window, which on a dial reads as a fault.
#[derive(Debug, Clone, Copy)]
struct Drift {
    lo: f64,
    hi: f64,
    tau: f64,
    seeded: bool,
}

impl Drift {
    fn new(tau: f64) -> Drift {
        Drift { lo: 0.0, hi: 0.0, tau, seeded: false }
    }

    fn push(&mut self, v: f64, dt: f64) {
        if !self.seeded {
            self.lo = v;
            self.hi = v;
            self.seeded = true;
            return;
        }
        let creep = 1.0 - (-dt / self.tau).exp();
        self.hi = if v > self.hi { v } else { self.hi + (v - self.hi) * creep };
        self.lo = if v < self.lo { v } else { self.lo + (v - self.lo) * creep };
    }

    fn range(&self) -> Option<(f64, f64)> {
        self.seeded.then_some((self.lo, self.hi))
    }
}

/// A needle with mass, which is the other half of how the bugs move.
///
/// THE BUGS SNAP OUT AND CREEP BACK; THE NEEDLE LEANS BOTH WAYS. They are the
/// same idea -- a reading is not a number, it is a number with a history, and
/// an instrument that jumps between them throws the history away -- but a
/// range mark and a pointer want different asymmetries. A bug must not miss
/// an excursion, so it moves out instantly. A needle must be readable, so it
/// moves out FAST and settles back slower: the quarter second is quick enough
/// that a source passing under the tube is not smoothed away, and the
/// nine-tenths coming back is what stops a lone Poisson lump from reading as
/// a spike.
///
/// This is what every moving-coil meter does mechanically and what every VU
/// meter specifies deliberately. Without it the needle teleports once a
/// second and the eye cannot follow which way it went -- which is the whole
/// reason for drawing a dial rather than printing the number.
#[derive(Debug, Clone, Copy)]
struct Ballistic {
    at: f64,
    rise: f64,
    fall: f64,
    seeded: bool,
}

impl Ballistic {
    fn new(rise: f64, fall: f64) -> Ballistic {
        Ballistic { at: 0.0, rise, fall, seeded: false }
    }

    /// Move `dt` seconds toward `target`, and say where that leaves it.
    fn push(&mut self, target: f64, dt: f64) -> f64 {
        if !self.seeded {
            // A NEEDLE THAT HAS NEVER READ ANYTHING STARTS WHERE IT IS, not
            // at zero. Attaching to a service that has been up since
            // breakfast would otherwise sweep the needle up from the stop
            // over the first second, which looks like an event and is not one.
            self.at = target;
            self.seeded = true;
            return self.at;
        }
        let tau = if target > self.at { self.rise } else { self.fall };
        self.at += (target - self.at) * (1.0 - (-dt / tau.max(1e-3)).exp());
        self.at
    }

    fn at(&self) -> Option<f64> {
        self.seeded.then_some(self.at)
    }
}

/// The arrivals of the last half minute, for the meters that cannot wait.
///
/// WHY NOT `Windows`, WHICH ALREADY DOES THIS. Because `Windows::average`
/// answers None until the window is full and is keyed by the spans the server
/// declared, and because what the meters need is not the same question: this
/// is asked between samples, at whatever moment the needle is being drawn,
/// over a window that ends NOW rather than at the last sample.
///
/// THE DIVISOR IS THE SAMPLES, NOT THE SECONDS. Every sample covers one
/// second of one tube, so `sum * 60 / samples` is counts per minute per tube
/// -- the mean the tubes agree on -- whatever the tube count is and whatever
/// happens to a tube mid-window. Dividing by `span * tubes` instead reports a
/// tube that has stopped answering as the room having gone quiet.
#[derive(Debug, Default)]
struct Recent {
    q: VecDeque<(f64, u32)>,
}

impl Recent {
    fn push(&mut self, when: f64, counts: u32) {
        self.q.push_back((when, counts));
        let cutoff = when - RECENT_KEEP;
        while self.q.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            self.q.pop_front();
        }
    }

    /// CPM per tube over the `span` seconds ending at `now`, or None if
    /// nothing was heard in them.
    fn rate(&self, now: f64, span: f64) -> Option<f64> {
        let cutoff = now - span;
        let mut sum = 0u64;
        let mut n = 0u32;
        // EVERY SAMPLE IN THE WINDOW, NOT EVERY SAMPLE UNTIL THE FIRST OLD
        // ONE. Arrival order is not quite time order: the tubes are read on
        // separate threads and a sample stamped 9.95 can be delivered after
        // one stamped 10.00. Scanning backwards and stopping at the first
        // sample outside the window would then throw away everything before
        // the inversion -- which, for a one-second window holding two
        // samples, is the whole reading. The queue is half a minute of
        // arrivals, so looking at all of it costs nothing worth saving.
        for (t, c) in self.q.iter() {
            if *t > cutoff {
                sum += *c as u64;
                n += 1;
            }
        }
        (n > 0).then(|| sum as f64 * 60.0 / n as f64)
    }

    /// The newest sample's time, which is not necessarily the last one
    /// pushed; see `rate`.
    fn newest(&self) -> Option<f64> {
        self.q.iter().map(|(t, _)| *t).fold(None, |best: Option<f64>, t| {
            Some(best.map_or(t, |b| if t > b { t } else { b }))
        })
    }
}

/// The collecting meter, and only it: the numbers that have to move.
///
/// SENT APART FROM THE SNAPSHOT, AND OFTEN. See METER_MS.
#[derive(Debug, Clone, Copy, Default)]
struct Meters {
    /// The newest whole second, as soon as a second of arrivals has gone by
    /// rather than when the next second's first sample turns up.
    live: Option<f64>,
    /// Three seconds, which is what the needle is pointing at.
    live3: Option<f64>,
    /// Half a minute, which is where the slow pointer sits.
    live30: Option<f64>,
    /// Where the needle actually IS, which is not the same thing: see
    /// `Ballistic`.
    needle: Option<f64>,
    /// The half-minute and minute ranges the reading has been bouncing
    /// between, drifting inward.
    range30: Option<(f64, f64)>,
    range60: Option<(f64, f64)>,
}

impl Meters {
    /// Whether this is different enough from what was last sent to be worth a
    /// re-render.
    ///
    /// A PANEL THAT REDRAWS TWELVE TIMES A SECOND FOR NOTHING is a quarter of
    /// a core on the software renderer this usually runs on, and a steady
    /// reading is the ordinary case. So the needle's motion is what buys the
    /// frame: when it has settled, the once-a-second snapshot is the only
    /// thing redrawing the window, exactly as before any of this.
    fn worth_sending(&self, last: &Meters) -> bool {
        let moved = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() > 0.05 + 0.002 * y.abs(),
            (x, y) => x.is_some() != y.is_some(),
        };
        moved(self.needle, last.needle)
            || moved(self.live, last.live)
            || moved(self.live30, last.live30)
            || self.range30 != last.range30
            || self.range60 != last.range60
    }
}

/// One spectrum of the overlay, as the interface needs it.
#[derive(Debug, Clone)]
struct Layer {
    window: usize,
    rel: Vec<f64>,
    /// The height a peak has to clear before it means anything, which is
    /// what the luck line across the overlay is drawn at.
    luck: f64,
}

/// Everything the interface knows, computed by the feed thread.
#[derive(Debug, Clone)]
struct Snapshot {
    counters: Vec<CounterId>,
    /// span, the mean CPM across the tubes, and its one-sigma.
    averages: Vec<(f64, Option<f64>, Option<f64>)>,
    /// Each tube on its own, over the headline window.
    per: Vec<Option<f64>>,
    /// What each tube counted in its newest second, as the interleave was
    /// given it: None for a tube that is not answering, which is drawn as
    /// nothing and never as nought.
    latest: Vec<Option<u32>>,
    /// How many tubes are answering, and so how many the room is the mean of.
    live: usize,
    elapsed: f64,
    headline: Option<f64>,
    headline_sigma: Option<f64>,
    now: u32,
    total: u64,
    /// The cascade's vertical scale, held so it does not breathe. See
    /// PEAK_TAU.
    peak: f64,
    /// Mean gap between one tube's sample and the next tube's, in seconds.
    /// Half a second is the ideal interleave; see `phase_note`.
    phase: Option<f64>,
    strip: Vec<Tier>,
    /// Which tube each of the newest samples came from, newest last, only as
    /// far back as the finest tier can show. See `Chart::cascade`.
    sources: Vec<u8>,
    samples: i64,
    every: i64,
    layers: Vec<Layer>,
    /// The trail's rows, newest first; how many have ever been taken;
    /// the seconds since the newest, and until the first.
    fall: Arc<Vec<analysis::TrailRow>>,
    fall_made: u64,
    fall_age: usize,
    fall_luck: f32,
    /// Where the strip's seconds end, as the strip counts them, and what to
    /// add to one of its seconds to have the clock's. See
    /// `analysis::bar_spans`.
    strip_end: i64,
    strip_shift: i64,
    /// Which tube drew it, the line, when, and whether its spectrum was
    /// flat at the time. An emission is an audit record of ONE source.
    random: Option<(usize, String, String, bool)>,
    pool: String,
    rows: Vec<Vec<String>>,
    columns: Vec<String>,
}

impl Snapshot {
    fn tubes(&self) -> usize {
        self.counters.len().max(1)
    }
}

#[derive(Debug, Clone)]
enum Message {
    Update(Box<Snapshot>),
    /// The collecting meter, on its own beat. See METER_MS.
    Moved(Meters),
    Adrift(String),
    /// The window changed size. See `App::view`: what gets dropped first.
    Resized(iced::Size),
    /// Where the pointer is on the chart, or that it has left it.
    Hover(Option<iced::Point>),
}

// ------------------------------------------------------------------- state ---

struct App {
    skin: Skin,
    shot: Option<Snapshot>,
    /// The collecting meter, which arrives between snapshots and more often
    /// than them.
    meters: Meters,
    adrift: String,
    cpm_per_usvh: f64,
    /// What the compositor has actually given us, which on a tiling desktop
    /// is whatever is left after every other window has had its share.
    size: iced::Size,
    /// Where the pointer is on the chart. See `Chart::update`.
    hover: Option<iced::Point>,
}

impl App {
    fn new() -> (App, iced::Task<Message>) {
        (
            App {
                skin: SKIN.get().cloned().unwrap_or_else(Skin::dark),
                shot: None,
                meters: Meters::default(),
                adrift: "looking for the counters...".into(),
                cpm_per_usvh: 153.8,
                size: iced::Size::new(760.0, 900.0),
                hover: None,
            },
            iced::Task::none(),
        )
    }

    fn title(&self) -> String {
        match self.shot.as_ref().and_then(|s| s.headline) {
            Some(cpm) => format!("{:.0} CPM -- radbeeper", cpm),
            None => "radbeeper".into(),
        }
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Update(s) => self.shot = Some(*s),
            Message::Moved(m) => self.meters = m,
            Message::Adrift(why) => {
                self.shot = None;
                self.meters = Meters::default();
                self.adrift = why;
            }
            Message::Resized(size) => self.size = size,
            Message::Hover(at) => self.hover = at,
        }
    }

    /// A NAMED FUNCTION, NOT A CLOSURE. `|_| Theme::CatppuccinMocha` looks
    /// like the obvious spelling and does not compile: the builder wants a
    /// function that works for any lifetime of the borrow, and inference will
    /// not generalise a closure that far.
    fn theme(&self) -> Theme {
        self.skin.theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        // ONE SOURCE OF DATA, AND NO TIMER BESIDE IT. A snapshot lands every
        // second while anything is attached, which is the same beat a clock
        // tick would have had -- and `iced::time::every` needs a tokio or
        // smol backend this crate deliberately does not carry. The other
        // subscription is not data; it is how much room there is to put it in.
        Subscription::batch([
            Subscription::run(feed),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        let Some(s) = self.shot.as_ref() else {
            return container(
                column![
                    text("no counter").size(30).color(self.skin.dim),
                    text(self.adrift.clone()).size(13).color(self.skin.dim),
                ]
                .spacing(6)
                .align_x(iced::Center),
            )
            .center(Fill)
            .into();
        };

        let tubes = s.tubes();
        // Whether there is room for the trail under the spectrum.
        let falls = self.size.height >= FALL_ROOM;
        // How big the instruments are in this window. Everything about the
        // cluster is measured from it, on the canvas and in the widgets
        // alike, so the two cannot drift apart.
        let scale = dial_scale(self.size, tubes);
        let dial_w = DIAL_W * scale;
        let band_h = cluster_h(tubes, scale);
        let tint = s.headline.map(|c| self.skin.colour(band(c))).unwrap_or(self.skin.dim);

        // ---- who is on the other end ------------------------------------
        //
        // ONE LINE WHATEVER THE COUNT. Nine counters listed one per line is
        // nine lines of a panel that is trying to be dense; the per-tube
        // readings below already carry the letters and the colours, so this
        // only has to say what they are and where.
        let firmwares: std::collections::BTreeSet<&str> =
            s.counters.iter().map(|c| c.version.as_str()).collect();
        let inner: Element<Message> = if tubes <= 2 {
            let mut col = column![].spacing(0);
            for (k, c) in s.counters.iter().enumerate() {
                col = col.push(
                    mono(format!(
                        "{} {} \u{b7} {} \u{b7} {}{}",
                        tube_name(k), c.path, c.version, c.serial_no,
                        reading(s.latest.get(k).copied().flatten())
                    ))
                    .size(10)
                    .color(if tubes > 1 { self.skin.tube(k) } else { self.skin.dim }),
                );
            }
            col.into()
        } else {
            // The summary, and under it EVERY TUBE'S READING: a rig of nine
            // is where one going quiet is least likely to be noticed.
            column![
                mono(format!(
                    "{} tubes \u{b7} {} \u{b7} {} \u{2026} {}",
                    tubes,
                    firmwares.into_iter().collect::<Vec<_>>().join(", "),
                    s.counters.first().map(|c| c.path.as_str()).unwrap_or(""),
                    s.counters.last().map(|c| c.path.as_str()).unwrap_or("")
                ))
                .size(10)
                .color(self.skin.dim),
                mono(readings_line(&s.latest, tubes)).size(10).color(self.skin.dim),
            ]
            .spacing(0)
            .into()
        };
        // THE NAMEPLATE GOES IN THE CORNER, out of the way of everything that
        // changes. It is the one line on the panel that never does.
        let who: Element<Message> = row![
            inner,
            Space::new().width(Fill),
            column![
                Space::new().height(2.0),
                mono(NAMEPLATE).size(9).color(self.skin.faint),
            ],
        ]
        .into();

        // ---- the dials --------------------------------------------------
        //
        // EVENS ON THE LEFT FACE, ODDS ON THE RIGHT. With one counter there
        // is one face and one needle; with two, a face each; with nine, five
        // needles on the left and four on the right. Needles on one face
        // share a scale, because the only question several tubes in one room
        // raise is whether they agree, and two angles can only be compared
        // when they mean the same thing.
        // ONE METER RAW, ONE METER COLLECTED. The left face carries a needle
        // per tube -- every input, in its own colour -- so a tube that has
        // wandered off is a needle that has wandered off. The right face
        // carries ONE needle and answers a different question, and neither
        // answers the other: the first is "do they agree?", the second is
        // "what is the room doing?".
        //
        // THE COLLECTED FACE IS A REAL INSTRUMENT AND CARRIES THREE TIME
        // CONSTANTS, which is what a dial can show and a number cannot:
        //
        //   the needle    three seconds, with mass -- see `Ballistic`
        //   the pointer   half a minute, the same window as the big number
        //   the arcs      where the needle has been over the last half
        //                 minute and minute -- see `Drift`
        //
        // Three seconds and not one because one second of one tube is a
        // handful of arrivals, and a needle drawn from it is drawing the
        // counting statistics rather than the room. The one-second figure has
        // not gone anywhere -- it is the caption under the face, where a
        // jumpy number belongs.
        //
        // They share a scale, because two dials that do not are two dials
        // that cannot be compared, which is the only reason to draw them side
        // by side.
        let m = &self.meters;
        let faces: Vec<Face> = vec![
            Face {
                needles: (0..tubes).map(|k| (k, s.per.get(k).copied().flatten())).collect(),
                range30: None,
                range60: None,
                pointer: None,
                weight: NEEDLE_RAW,
            },
            Face {
                // Where the needle IS, not where it is heading. Drawing the
                // target instead would make the mass invisible, which is the
                // whole of what it is for.
                needles: vec![(usize::MAX, m.needle)],
                range30: m.range30,
                range60: m.range60,
                pointer: m.live30,
                weight: NEEDLE_HELD,
            },
        ];

        let chart = canvas(Chart {
            skin: self.skin.clone(),
            cluster: band_h,
            scale,
            peak: s.peak,
            dials: faces.clone(),
            strip: s.strip.clone(),
            sources: s.sources.clone(),
            // The interleave is of the tubes that are answering: with one
            // left there is no such tier, and the last is a tier of seconds.
            tubes: s.live,
            every: s.every,
            n: s.samples,
            layers: s.layers.clone(),
            falls,
            fall: s.fall.clone(),
            fall_made: s.fall_made,
            fall_age: s.fall_age,
            fall_luck: s.fall_luck,
            hover: self.hover,
        })
        .width(Fill)
        .height(Fill);

        // The numerals that belong on the faces: what the face reads in the
        // lower half of it, and which tubes are on it under that.
        let mut dial_faces = row![].spacing(0);
        for (fi, face) in faces.iter().enumerate() {
            let live: Vec<f64> = face.needles.iter().filter_map(|(_, v)| *v).collect();
            let mean = (!live.is_empty())
                .then(|| live.iter().sum::<f64>() / live.len() as f64);
            // WHAT THE FACE READS, IN DIGITS, under the needle. The raw
            // face reads the mean of its needles; the collected face reads
            // the three seconds the needle is pointing at -- the number the
            // needle is FOR, so that the angle and the digits can never
            // disagree.
            let reads = if fi == 0 { mean } else { m.live3 };
            let caption: Element<Message> = if fi == 0 {
                let mut letters = row![].spacing(3);
                for (k, _) in face.needles.iter().take(10) {
                    letters = letters.push(
                        mono(tube_name(*k)).size(9).color(self.skin.tube(*k)),
                    );
                }
                letters.into()
            } else {
                // THE TWO FIGURES THE NEEDLE IS NOT. The one-second reading
                // is the fastest thing the panel knows and it is no longer on
                // the needle -- printed, which is where a number that jumps
                // several times a second belongs -- and the half minute is
                // where the chrome pointer is sitting. Labelled, because
                // three bare numbers under a dial are a puzzle.
                row![
                    mono("1s").size(9).color(self.skin.faint),
                    mono(match m.live {
                        Some(v) => format!("{:.0}", v),
                        None => "--".into(),
                    })
                    .size(9)
                    .color(Color { a: 0.9, ..self.skin.dim }),
                    mono("\u{b7}").size(9).color(self.skin.faint),
                    mono(format!("{}s", POINTER_SPAN as i64))
                        .size(9)
                        .color(self.skin.faint),
                    mono(match m.live30 {
                        Some(v) => format!("{:.0}", v),
                        None => "--".into(),
                    })
                    .size(9)
                    .color(Color { a: 0.95, ..self.skin.bezel }),
                ]
                .spacing(3)
                .into()
            };
            dial_faces = dial_faces.push(
                container(
                    column![
                        // ON THE FACE: the reading, and nothing else. A gauge
                        // face carries the number the needle is pointing at;
                        // everything that explains it goes on the plate.
                        Space::new().height(DIAL_TEXT_TOP * scale),
                        mono(match reads {
                            Some(v) => format!("{:.0}", v),
                            None => "--".into(),
                        })
                        .size(18.0 * scale)
                        .color(reads.map(|v| self.skin.colour(band(v))).unwrap_or(self.skin.dim)),
                        // Down to the nameplate, clear of the bezel.
                        Space::new().height(
                            ((DIAL_PLATE_TOP - DIAL_TEXT_TOP) * scale - 22.0 * scale)
                                .max(0.0),
                        ),
                        caption,
                        // What this face is reading, and over what. The
                        // collected one adds where it has been, which is what
                        // the two arcs outside the bands are drawing.
                        mono(if fi == 0 {
                            format!("raw \u{b7} {}s", HEADLINE as i64)
                        } else {
                            match m.range30 {
                                Some((l, h)) => format!(
                                    "{}s held \u{b7} {:.0}\u{2013}{:.0}",
                                    NEEDLE_SPAN as i64, l, h
                                ),
                                None => format!("{}s held", NEEDLE_SPAN as i64),
                            }
                        })
                        .size(9)
                        .color(self.skin.faint),
                    ]
                    .spacing(0)
                    .align_x(iced::Center),
                )
                .width(Length::Fixed(dial_w))
                .align_x(iced::Center),
            );
        }

        // ---- the dense numeric block, beside the dials -----------------
        let mut windows = column![].spacing(0);
        for (span, avg, sd) in &s.averages {
            let label = mono(format!("{:>6}s", *span as i64)).size(10).color(self.skin.dim);
            let body: Element<Message> = match avg {
                Some(cpm) => row![
                    mono(format!("{:>8.1}", cpm)).size(10).color(self.skin.colour(band(*cpm))),
                    mono(format!("{:>8.3}", cpm / self.cpm_per_usvh)).size(10).color(self.skin.dim),
                    mono(match sd {
                        // A WINDOW THAT IS FULL SAYS HOW WELL IT KNOWS ITS
                        // NUMBER. The long ones are the precise ones, and
                        // without this the only visible difference between
                        // the 3-second figure and the working-day figure is
                        // that one of them jumps about.
                        Some(sd) if *cpm > 0.0 => format!("{:>6.1}%", 100.0 * sd / cpm),
                        _ => String::new(),
                    })
                    .size(10)
                    .color(self.skin.faint),
                ]
                .spacing(5)
                .into(),
                None => mono(format!(
                    "  filling {:>6}s",
                    (span - s.elapsed).max(0.0).round() as i64
                ))
                .size(10)
                .color(self.skin.faint)
                .into(),
            };
            windows = windows.push(row![label, body].spacing(6));
        }

        // EVERY TUBE'S OWN NUMBER, in its own colour, chunked so that nine of
        // them are three short rows and not one that runs off the panel. The
        // needles say the same thing at a glance; this is for reading off.
        let mut per_grid = column![].spacing(0);
        if tubes > 1 {
            for chunk in (0..tubes).collect::<Vec<_>>().chunks(5) {
                let mut line = row![].spacing(7);
                for k in chunk {
                    line = line.push(
                        mono(match s.per.get(*k).copied().flatten() {
                            Some(v) => format!("{} {:>5.0}", tube_name(*k), v),
                            None => format!("{}    --", tube_name(*k)),
                        })
                        .size(10)
                        .color(self.skin.tube(*k)),
                    );
                }
                per_grid = per_grid.push(line);
            }
        }

        let numbers = column![
            row![
                text(match s.headline {
                    Some(v) => format!("{:.0}", v),
                    None => "--".into(),
                })
                .size(42)
                .font(MONO)
                .color(tint),
                column![
                    Space::new().height(12.0),
                    mono(match (s.headline, s.headline_sigma) {
                        (Some(v), Some(sd)) if v > 0.0 =>
                            format!("CPM \u{b1}{:.0} ({:.1}%)", sd, 100.0 * sd / v),
                        _ => "CPM".into(),
                    })
                    .size(11)
                    .color(self.skin.dim),
                    mono(match s.headline {
                        Some(v) => format!("{:.3} uSv/h", v / self.cpm_per_usvh),
                        None => "-- uSv/h".into(),
                    })
                    .size(11)
                    .color(self.skin.dim),
                ]
                .spacing(0),
            ]
            .spacing(8),
            windows,
            per_grid,
            mono(format!(
                "now {:<4} run {} in {}s{}",
                s.now,
                s.total,
                s.elapsed.round() as i64,
                match (tubes, s.live, s.phase) {
                    (1, _, _) => String::new(),
                    (n, live, _) if live < n => format!("  {} of {} tubes averaged", live, n),
                    (n, _, Some(p)) => format!("  {} tubes {}", n, phase_note(p, n)),
                    (n, _, None) => format!("  {} tubes averaged", n),
                }
            ))
            .size(10)
            .color(self.skin.faint),
        ]
        .spacing(1);

        // THE KEY TO THE COLOURS IS NOT PRINTED HERE ANY MORE. It was a line
        // under the dials -- the range, and each band's name and floor in
        // its own colour -- and in half a screen it cost the strip a row to
        // say what the README says once. The bands are where they were, on
        // every dial and every bar; `Band` has their names.

        // ---- the cascade captions, over the tiers they describe ---------
        //
        // A CAPTION HAS TO FIT ITS OWN TIER, and the tiers are no longer the
        // same width as each other. It used to be a count -- more than five
        // tiers and every caption went terse -- which was right while every
        // tier was an equal share and became wrong the moment the interleave
        // was capped to four seconds of it: five tiers, so every caption took
        // the long form, and the narrow one wrapped "F 1/2s/bar . 4s" over
        // three lines onto the bars it was labelling.
        //
        // So each caption is measured against the pixels ITS tier has, and
        // takes the longest of three forms that fits. The tier that gets a
        // twentieth of the width says the one thing that cannot be inferred
        // -- how much time a bar holds -- and the wide ones still say the
        // rest.
        let total: usize = s.strip.iter().map(|t| t.columns).sum::<usize>().max(1);
        // The canvas's width, which is the window's less the padding either
        // side. A caption that is a little conservative costs nothing; one
        // that wraps lands on the chart.
        let panel_w = (self.size.width - 22.0).max(80.0);
        let mut caps = row![].spacing(0);
        for (ti, t) in s.strip.iter().enumerate() {
            let room = panel_w * t.columns as f32 / total as f32;
            let bar = bar_seconds(t.seconds);
            let long = format!(
                "{}{}s/bar \u{b7} {}",
                if ti == 0 { "" } else { "F " },
                bar,
                span_words(t.columns as f64 * t.seconds)
            );
            let mid = format!("{}s/bar", bar);
            let short = format!("{}s", bar);
            // DejaVu Sans Mono advances 0.602 em, and a little over that
            // here so a caption never sits flush against its neighbour.
            let fits = |text: &str, size: f32| text.chars().count() as f32 * size * 0.63 <= room;
            let (text, size) = if fits(&long, 10.0) {
                (long, 10)
            } else if fits(&mid, 10.0) {
                (mid, 10)
            } else if fits(&short, 9.0) {
                (short, 9)
            } else {
                (String::new(), 9)
            };
            caps = caps.push(
                container(mono(text).size(size).color(self.skin.dim))
                    .width(Length::FillPortion(t.columns as u16)),
            );
        }

        // EVERYTHING WITH LETTERS IN IT GOES OVER THE TOP. The panel is one
        // canvas because only the last canvas in a view is drawn here, and a
        // canvas cannot contain text at all, so the readouts and the captions
        // are widgets in a stack above it -- laid out by the same engine as
        // the rest, at the same sizes, in the same font.
        // FILL, SAID OUT LOUD. A `stack` is Shrink by default and its base
        // layer here is a canvas that is Fill, so the column above measured
        // this as a shrinking child, handed it the whole of the remaining
        // height to measure itself against, and got back a panel as tall as
        // everything left -- which then drew over the emission line and the
        // clock under it. Visible only on a short window, because a tall one
        // has height to spare either way; the committed 560x400 screenshot
        // has the cascade running behind both lines.
        //
        // Declaring the height makes it a filling child of a filling column,
        // which is what it always was, and the space is shared instead of
        // taken.
        // ---- the spectrum's axis and its verdict ------------------------
        //
        // A CAPTION AT THE LEFT OF EVERY STRETCH, which is the period that
        // stretch begins at, and one at the far right for where the axis
        // ends. The stretches are as wide here as they are on the canvas,
        // by the same shares, so a caption stands under the cut it names.
        let (shortest, longest) = ladder_ends(&s.layers);
        let cut = stretches(period_floor(shortest, longest), longest);
        let mut axis = row![].spacing(0);
        for (k, st) in cut.iter().enumerate() {
            let share = (((st.right - st.left) * 1000.0).round() as u16).max(1);
            let begins = mono(span_words(st.long))
                .size(10)
                .wrapping(text::Wrapping::None)
                .color(self.skin.dim);
            axis = axis.push(if k + 1 < cut.len() {
                container(begins).width(Length::FillPortion(share))
            } else {
                container(row![
                    begins,
                    Space::new().width(Fill),
                    mono("period \u{b7} log").size(10).color(self.skin.faint),
                    Space::new().width(Fill),
                    mono(span_words(st.short)).size(10).color(self.skin.dim),
                ])
                .width(Length::FillPortion(share))
            });
        }

        // ---- what the pointer is over ----------------------------------
        //
        // A LINE OF WORDS AT THE HEAD OF THE STRIP, beside the bar they are
        // about, and a circle on the trend line there, which the canvas
        // draws. Words cannot go in the canvas, so they are a third layer
        // over it: a space as tall as the dials and the captions, then a
        // space as wide as the pointer is far across, then the words. On
        // the right of the panel they are set to the left of the pointer,
        // so the edge does not cut them off.
        let panel_w = (self.size.width - 22.0).max(80.0);
        let hover: Element<Message> = match self
            .hover
            .filter(|p| p.y >= band_h)
            .and_then(|p| Some((p, bar_at(&s.strip, p.x, panel_w)?)))
            .and_then(|(p, at)| Some((p, hover_words(&s.strip, s.strip_end, s.strip_shift, at)?)))
        {
            Some((p, words)) => {
                let wide = words.chars().count() as f32 * 10.0 * ADVANCE + 10.0;
                let left = if p.x + 10.0 + wide <= panel_w {
                    p.x + 10.0
                } else {
                    (p.x - 10.0 - wide).max(0.0)
                };
                let (ground, edge) = (self.skin.bg, self.skin.faint);
                column![
                    Space::new().height(Length::Fixed(band_h + 14.0)),
                    row![
                        Space::new().width(Length::Fixed(left)),
                        container(
                            mono(words)
                                .size(10)
                                .wrapping(text::Wrapping::None)
                                .color(self.skin.fg)
                        )
                        .padding([1, 4])
                        .style(move |_| container::Style {
                            background: Some(Color { a: 0.92, ..ground }.into()),
                            border: iced::Border {
                                color: edge,
                                width: 1.0,
                                radius: 3.0.into(),
                            },
                            ..container::Style::default()
                        }),
                    ],
                ]
                .into()
            }
            None => Space::new().into(),
        };

        let over = column![
            container(row![dial_faces, Space::new().width(6.0), numbers].spacing(0))
                .height(Length::Fixed(band_h)),
            caps,
            Space::new().height(Fill),
        ]
        .spacing(0);
        let panel = stack![chart, over, hover].height(Fill);
        // ---- the clock, the emission and its countdown, on ONE line -----
        //
        // FOUR LINES BECAME ONE, and the panel is denser for it. It used to
        // run: a verdict per spectrum layer, then the hex, then a sentence
        // saying which counter and when and how long until the next one, then
        // the clock on a line of its own. Between them they spent six rows
        // restating things the reader can already see -- the layer colours
        // are on the chart, the serial is at the top of the panel, and "next
        // in 168s" needs neither the word "random" nor a row to itself.
        //
        // LEFT, CENTRE, RIGHT. The clock anchors the left because it is the
        // one thing on the panel that is not about the counter; the hex takes
        // the middle because it is the widest and the thing being read; the
        // countdown sits hard right where a number that only ever decreases
        // belongs. Two `Fill` spacers do the justifying, so it holds at any
        // width.
        //
        // AND NOTHING SAYS "FLAT". The spectrum's own verdict lines are gone
        // and so is the suspect note: flatness is a health check on the
        // source, not a headline, and a panel that spent three rows a second
        // saying a counter was behaving normally was spending them on the
        // least surprising fact it knows. A source that stops looking like
        // decay still marks the emission -- the hex turns warning-coloured --
        // and the audit page and `random --frames` carry the detail.
        //
        // THE KEY IS NEVER BROKEN ACROSS TWO LINES. Side by side with another
        // window this one is half a screen wide, and date, key and countdown
        // together are wider than that: the key wrapped, and took a row from
        // the charts to do it. So the others give way, in the order they can
        // be spared -- see `foot_for` -- and nothing on this line wraps.
        //
        // THE LETTER IS WHOSE KEY IT IS. Every tube earns its own, from its
        // own counts, when its own pool holds 256 bits: a tube that counts
        // three times as fast earns three times as many. The newest is what
        // is shown, so B here is not A left out -- it is B having been the
        // last to finish. They cannot be spaced evenly without holding a
        // key back or drawing one early, and the log has every one of both.
        let countdown = match &s.random {
            // After a line has been drawn, the pool status IS the countdown
            // and needs no label in front of it.
            Some(_) => s.pool.clone(),
            None => format!("random \u{b7} {}", s.pool),
        };
        let foot = foot_for(
            self.size.width - 22.0,
            s.random.as_ref().map(|(_, hex, _, _)| hex.len()),
            countdown.chars().count(),
        );
        let stamp = mono(clock::format(clock::now(), foot.stamp))
            .size(FOOT_SIZE)
            .wrapping(text::Wrapping::None)
            .color(self.skin.faint);
        let countdown = mono(if foot.countdown { countdown } else { String::new() })
            .size(FOOT_SIZE)
            .wrapping(text::Wrapping::None)
            .color(self.skin.faint);
        let emission: Element<Message> = match &s.random {
            Some((who, hex, _at, suspect)) => row![
                mono(format!("{} ", tube_name(*who)))
                    .size(KEY_SIZE)
                    .wrapping(text::Wrapping::None)
                    .color(self.skin.tube(*who)),
                mono(if foot.grouped { entropy::group_hex(hex) } else { hex.clone() })
                    .size(KEY_SIZE)
                    .wrapping(text::Wrapping::None)
                    .color(if *suspect { self.skin.warn } else { self.skin.cyan }),
            ]
            .into(),
            None => Space::new().into(),
        };
        let mut footline = row![].spacing(FOOT_GAP).align_y(iced::Center);
        if !foot.stamp.is_empty() {
            footline = footline.push(stamp).push(Space::new().width(Fill));
        }
        footline = footline.push(emission);
        if foot.countdown {
            footline = footline.push(Space::new().width(Fill)).push(countdown);
        }

        // WHAT GOES FIRST WHEN THERE IS NO ROOM. A tiling compositor will
        // hand this window a quarter of a screen without asking, and
        // everything above was sized as though it would not: the charts are
        // Fill and everything else is fixed, so at 373 pixels the fixed
        // content took the lot and the cascade collapsed to nothing. The
        // charts ARE the instrument -- they are the last thing to go, not the
        // first. So the log table goes, then the axis, in that order, and
        // what is left keeps its shape.
        //
        // THE VERDICT LINES USED TO BE IN THIS LADDER and are not in the
        // panel at all now, which is why there are two thresholds here
        // instead of three. Four rows of the shortest window's content went
        // with them, and the charts have it.
        let room = self.size.height;
        let (want_table, want_axis) = (room >= 560.0, room >= 400.0);
        let table: Element<Message> = if s.rows.is_empty() || !want_table {
            Space::new().into()
        } else {
            // Fewer rows on a shorter window, rather than none.
            // 700 and not 760: a full-height tile on a 800-pixel screen is
            // 756 after the bar and the gaps, and that is the commonest
            // window this will ever be in. A threshold above it would give
            // the short table to the ordinary case.
            let keep = if room >= 700.0 { ROWS } else { 3 };
            let mut t = column![mono(cells(&s.columns)).size(9).color(self.skin.faint)].spacing(0);
            for r in s.rows.iter().rev().take(keep).rev() {
                t = t.push(mono(cells(r)).size(9).color(self.skin.dim));
            }
            t.into()
        };

        container(
            column![
                who,
                panel,
                if want_axis { axis.into() } else { Element::from(Space::new()) },
                footline,
                table,
            ]
            .spacing(2),
        )
        .padding(10)
        .into()
    }
}

/// The line under the charts: the sizes it is set in, the gap between its
/// parts, and a character's width as a share of its size in MONO.
const FOOT_SIZE: f32 = 10.0;
const KEY_SIZE: f32 = 11.0;
const FOOT_GAP: f32 = 10.0;
const ADVANCE: f32 = 0.61;

/// What that line has room for.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Foot {
    /// The clock's format: the date and the time, the time, or nothing.
    stamp: &'static str,
    countdown: bool,
    /// The key in eights, with a space between them.
    grouped: bool,
}

/// What fits in `width` beside a key of `key` hex digits, if there is one.
///
/// THE KEY IS WHAT THE LINE IS FOR, so it is the last to be touched. The
/// countdown goes first: it says when this key will be replaced, which
/// nobody copying it needs. Then the date, which the log table has; then the
/// clock; and only then the spaces in the key itself. Narrower than that it
/// runs off the edge, which is still one line.
fn foot_for(width: f32, key: Option<usize>, countdown: usize) -> Foot {
    const DATED: &str = "%Y-%m-%d %H:%M:%S";
    const TIMED: &str = "%H:%M:%S";
    let chars = |n: usize, size: f32| n as f32 * size * ADVANCE;
    let Some(digits) = key else {
        // No key yet: a date and a countdown, which fit in anything.
        return Foot { stamp: DATED, countdown: true, grouped: true };
    };
    let eights = digits + digits.saturating_sub(1) / 8;
    let ladder = [
        Foot { stamp: DATED, countdown: true, grouped: true },
        Foot { stamp: DATED, countdown: false, grouped: true },
        Foot { stamp: TIMED, countdown: false, grouped: true },
        Foot { stamp: "", countdown: false, grouped: true },
        Foot { stamp: "", countdown: false, grouped: false },
    ];
    let needs = |f: &Foot| {
        // The letter and its space, then the key.
        let mut w = chars(2 + if f.grouped { eights } else { digits }, KEY_SIZE);
        if !f.stamp.is_empty() {
            // `%Y-%m-%d %H:%M:%S` prints nineteen characters, `%H:%M:%S` eight.
            let n = if f.stamp == DATED { 19 } else { 8 };
            w += chars(n, FOOT_SIZE) + 2.0 * FOOT_GAP;
        }
        if f.countdown {
            w += chars(countdown, FOOT_SIZE) + 2.0 * FOOT_GAP;
        }
        w
    };
    *ladder.iter().find(|f| needs(f) <= width).unwrap_or(&ladder[4])
}

fn mono(s: impl text::IntoFragment<'static>) -> text::Text<'static> {
    text(s).font(MONO)
}

/// A, B, C: short enough to sit beside a number without crowding it.
fn tube_name(k: usize) -> String {
    format!("{}", (b'A' + (k as u8 % 26)) as char)
}

/// A tube's newest second beside its serial, in counts.
///
/// CPS AND NOT CPM: the second times sixty can only be 0, 60 or 120, which
/// reads as a broken average. The count is the clicks, and it changing is
/// the data arriving.
///
/// THE READING THE INTERLEAVE WAS GIVEN, not an average: what that tube said
/// most recently, so two of them can be seen to agree or not. EMPTY, NOT
/// NOUGHT, for a tube that is not answering -- a nought is a second in which
/// the tube listened and nothing came, and a tube that has stopped has no
/// reading at all. The space is kept, so the line does not move.
fn reading(counts: Option<u32>) -> String {
    match counts {
        Some(c) => format!(" \u{b7} {:>5} CPS", c),
        None => " ".repeat(12),
    }
}

/// Every tube's reading on one line, by its letter: `A     2 CPS   B ...`.
fn readings_line(latest: &[Option<u32>], tubes: usize) -> String {
    (0..tubes)
        .map(|k| format!("{}{}", tube_name(k), reading(latest.get(k).copied().flatten())))
        .collect::<Vec<_>>()
        .join("  ")
}

/// The measured interleave, as the panel says it.
///
/// The arithmetic is `analysis::interleave_quality`, and deliberately not
/// here: the terminal and the exported page report the same number, and a
/// figure that disagreed between them would be the exact class of bug the
/// differential suite exists to catch. This is only the wording.
fn phase_note(gap: f64, tubes: usize) -> String {
    format!(
        "\u{b7} interleave {:.2}s ({:.0}%)",
        gap,
        100.0 * interleave_quality(gap, tubes)
    )
}

/// A log row as one monospace line.
fn cells(cells: &[String]) -> String {
    cells
        .iter()
        .take(9)
        .map(|c| {
            let c = if c.is_empty() { "-" } else { c.as_str() };
            format!("{:<10}", c.chars().take(9).collect::<String>())
        })
        .collect::<Vec<_>>()
        .concat()
}


/// Where a dial's needle sits for a reading, as a fraction of its sweep.
///
/// Logarithmic, over a scale that never moves. See DIAL_FLOOR.
fn dial_fraction(value: f64) -> f64 {
    let span = (DIAL_CEIL / DIAL_FLOOR).log10();
    ((value.max(1e-9) / DIAL_FLOOR).log10() / span).clamp(0.0, 1.0)
}

// ------------------------------------------------------------------- chart ---

/// The cascade, and the spectrum overlay under it.
///
/// ONE CANVAS FOR BOTH, AND IT IS NOT A TIDINESS DECISION. On this renderer
/// only the LAST canvas widget in a view is drawn: with the cascade above and
/// the spectrum below, as two widgets, the spectrum appeared and the cascade
/// was an empty box -- a solid fill over its whole area vanished just the
/// same, which is what finally pinned it after an afternoon of blaming the bar
/// arithmetic. Delete the spectrum widget and the cascade came back.
///
/// BARS ONLY, AND NOT ONE CHARACTER OF TEXT, for the same family of reason: a
/// `fill_text` in a canvas program takes that frame's geometry with it. Every
/// caption around these charts is an ordinary text widget.
/// A dial face: every needle on it, and the scale they share.
#[derive(Debug, Clone)]
struct Face {
    /// (tube index, its reading), or (usize::MAX, reading) for the collected
    /// one, which belongs to no single tube.
    needles: Vec<(usize, Option<f64>)>,
    /// The half-minute and minute ranges, on the collecting meter only. The
    /// raw meter has none: a range needs one needle to be the range OF.
    range30: Option<(f64, f64)>,
    range60: Option<(f64, f64)>,
    /// The slow pointer: the half-minute mean, which is the window the big
    /// number is quoted over. Where the needle is going if nothing changes.
    pointer: Option<f64>,
    /// How heavy the needle is. Zero draws the thin stroked kind, which is
    /// what several needles on one face need. See NEEDLE_RAW.
    weight: f32,
}

struct Chart {
    skin: Skin,
    /// The dial band's height, which depends on the tube count and on how
    /// much room the window has. See cluster_h and dial_scale.
    cluster: f32,
    /// How big the faces are drawn, relative to their smallest. The widgets
    /// stacked over them are laid out from the same number.
    scale: f32,
    /// The cascade's vertical scale, held steady by the feed. See PEAK_TAU.
    peak: f64,
    dials: Vec<Face>,
    strip: Vec<Tier>,
    /// Which tube the newest samples came from, for the finest tier.
    sources: Vec<u8>,
    tubes: usize,
    every: i64,
    /// Absolute index of the newest sample, for the log-row ticks.
    n: i64,
    layers: Vec<Layer>,
    /// Whether the trail is drawn, and what it is drawn from.
    falls: bool,
    fall: Arc<Vec<analysis::TrailRow>>,
    fall_made: u64,
    fall_age: usize,
    fall_luck: f32,
    /// Where the pointer is on the canvas, when it is on it.
    hover: Option<iced::Point>,
}

/// What the canvas keeps between one drawing and the next.
///
/// THE TRAIL IS FIVE THOUSAND LINES AND CHANGES ONCE A SECOND; the needles
/// are a dozen and change twelve times. Drawn together the five thousand
/// were drawn twelve times a second to move a needle, which a graphics
/// card does not notice and the software renderer in a virtual machine
/// does. So the trail is kept, and drawn again only when a second has
/// passed over it or the axis it is drawn on has grown.
#[derive(Default)]
struct Kept {
    fall: canvas::Cache,
    /// Which row was newest, how old it was, and how far the axis went,
    /// when that was drawn.
    of: std::cell::Cell<(u64, usize, usize)>,
    /// Where the pointer was last said to be.
    hover: Option<iced::Point>,
}

impl canvas::Program<Message> for Chart {
    type State = Kept;

    /// The pointer, said to the window when it moves: the words about
    /// what it is over are a widget, and the window lays those out.
    fn update(
        &self,
        kept: &mut Kept,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if !matches!(event, iced::Event::Mouse(_)) {
            return None;
        }
        let now = cursor.position_in(bounds);
        if now == kept.hover {
            return None;
        }
        kept.hover = now;
        Some(canvas::Action::publish(Message::Hover(now)))
    }

    fn draw(
        &self,
        kept: &Kept,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if bounds.width < 40.0 {
            return vec![frame.into_geometry()];
        }
        let at = regions(bounds.height, self.cluster, self.falls);
        self.cluster(&mut frame);
        self.cascade(&mut frame, bounds, at.cascade);
        if !self.falls {
            self.spectrum(&mut frame, bounds, at.spectrum);
            return vec![frame.into_geometry()];
        }
        // The trail first and the spectrum over it: the spectrum is the
        // front of the stack.
        let of = (self.fall_made, self.fall_age, ladder_ends(&self.layers).1);
        if kept.of.get() != of {
            kept.of.set(of);
            kept.fall.clear();
        }
        // The spectrum's bars stand in a band at the head of the ground,
        // and the newest row is drawn on the line they stand on.
        let bars = (at.spectrum.1 * SPECTRUM_BARS).clamp(24.0, 90.0);
        let trail = kept.fall.draw(renderer, bounds.size(), |frame| {
            self.trail(frame, bounds.width, at.spectrum, bars);
        });
        self.spectrum(&mut frame, bounds, (at.spectrum.0, bars));
        vec![trail, frame.into_geometry()]
    }
}

impl Chart {

    /// The dials, in a row from the left, as an instrument cluster.
    ///
    /// A 270-DEGREE SWEEP FROM DOWN-LEFT TO DOWN-RIGHT, which is what every
    /// speedometer and tachometer does, because it is the arc a needle can
    /// cross without the eye losing it. The bands are painted onto the face
    /// at the thresholds the rest of the program already uses, so the colour
    /// under the needle and the colour of the number above agree by
    /// construction rather than by being kept in step by hand.
    fn cluster(&self, frame: &mut canvas::Frame) {
        use iced::widget::canvas::{Path, Stroke};
        use iced::Radians;

        let start = std::f32::consts::PI * 0.75; // down-left
        let sweep = std::f32::consts::PI * 1.5; // three quarters of a turn

        for (k, face) in self.dials.iter().enumerate() {
            let (dial_r, dial_w) = (DIAL_R * self.scale, DIAL_W * self.scale);
            let cx = dial_w * (k as f32 + 0.5);
            // A FIXED HEIGHT, NOT THE MIDDLE OF THE BAND. The band grows by a
            // row for every five tubes, and a face that followed it slid down
            // away from the numerals stacked over it -- which are laid out by
            // the widget engine against the top of the panel and cannot
            // follow. Pinning the face means the digits sit in the same place
            // on it with one tube or with nine.
            let cy = DIAL_CY * self.scale;
            let c = iced::Point::new(cx, cy);

            // The face, and the bezel around it.
            frame.fill(&Path::circle(c, dial_r), self.skin.face);
            frame.stroke(
                &Path::circle(c, dial_r),
                Stroke::default().with_width(2.0).with_color(Color { a: 0.5, ..self.skin.bezel }),
            );

            // An arc of the scale, at whatever radius, in whatever colour.
            let arc_at = |frame: &mut canvas::Frame, from: f64, to: f64,
                          radius: f32, width: f32, tint: Color| {
                let a0 = start + sweep * dial_fraction(from) as f32;
                let a1 = start + sweep * dial_fraction(to) as f32;
                if a1 <= a0 + 0.004 {
                    return;
                }
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: c,
                        radius: dial_r * radius,
                        start_angle: Radians(a0),
                        end_angle: Radians(a1),
                    });
                });
                frame.stroke(&arc, Stroke::default().with_width(width).with_color(tint));
            };

            // THE FIVE BANDS, PAINTED ON THE self.skin.face. Each runs from its own
            // floor to the next one's, so the colour under the needle is the
            // colour of the word for where the needle is -- attenuated,
            // nominal, advisory, warning, deadly -- and the dial agrees with
            // every number on the panel by construction.
            let bands = Band::all();
            for (i, b) in bands.iter().enumerate() {
                let to = bands.get(i + 1).map(|n| n.floor()).unwrap_or(DIAL_CEIL);
                arc_at(frame, b.floor(), to.min(DIAL_CEIL), 0.82, 5.0,
                       Color { a: 0.75, ..self.skin.face_colour(*b) });
            }

            // TICKS AT THE NUMBERS, NOT AT EQUAL ANGLES. An evenly spaced
            // ring told you where half of full scale was, which on a
            // logarithmic face is not a number anybody is looking for. These
            // are the decades -- 3, 30, 300, 3000 -- with the 2..9 of each
            // one between them, which is what every log scale ever printed
            // does and what makes the spacing legible AS logarithmic rather
            // than as a dial with uneven ticks.
            //
            // And a heavy mark at every band floor, so the place the colour
            // changes is a place the eye can find without reading the colour.
            let mut tick = |value: f64, weight: f32, alpha: f32| {
                let a = start + sweep * dial_fraction(value) as f32;
                let r1 = dial_r * if weight > 1.5 { 0.66 } else { 0.73 };
                let r2 = dial_r * 0.79;
                frame.stroke(
                    &Path::line(
                        iced::Point::new(cx + r1 * a.cos(), cy + r1 * a.sin()),
                        iced::Point::new(cx + r2 * a.cos(), cy + r2 * a.sin()),
                    ),
                    Stroke::default()
                        .with_width(weight)
                        // THE BEZEL'S COLOUR, NOT THE PAGE'S. `fg` is the
                        // colour of text on paper, and the dial face is black
                        // in BOTH skins -- so under Antiquity the ticks were
                        // dark navy on near-black and simply were not there.
                        // This is the same trap the band colours have their
                        // own `face_bands` set to avoid, and the ticks are
                        // instrument furniture like the hub and the pointer,
                        // so they take the chrome.
                        .with_color(Color { a: alpha, ..self.skin.bezel }),
                );
            };
            let mut decade = DIAL_FLOOR;
            while decade <= DIAL_CEIL * 1.0001 {
                tick(decade, 2.0, 0.75);
                for step in 2..10 {
                    let v = decade * step as f64;
                    if v < DIAL_CEIL {
                        tick(v, 1.0, 0.30);
                    }
                }
                decade *= 10.0;
            }
            for b in Band::all() {
                if b.floor() > DIAL_FLOOR && b.floor() < DIAL_CEIL {
                    tick(b.floor(), 2.0, 0.6);
                }
            }

            // THE RANGES, AS ARCS OUTSIDE THE BANDS -- the half minute and the
            // minute the needle has been bouncing between. A bug at each end,
            // coloured by the band that end is in, so an excursion into
            // warning leaves an orange mark sitting there for half a minute
            // after the needle has come back. This is a head-up display's
            // heading bug doing a VU meter's job: the arc is where it has
            // been, the needle is where it is.
            // Takes the frame rather than capturing it: `arc_at` already
            // holds a unique borrow, and two closures cannot both have one.
            let bug = |frame: &mut canvas::Frame, value: f64, radius: f32, width: f32| {
                let a = start + sweep * dial_fraction(value) as f32;
                let (r0, r1) = (dial_r * (radius - 0.05), dial_r * (radius + 0.05));
                frame.stroke(
                    &Path::line(
                        iced::Point::new(cx + r0 * a.cos(), cy + r0 * a.sin()),
                        iced::Point::new(cx + r1 * a.cos(), cy + r1 * a.sin()),
                    ),
                    Stroke::default().with_width(width).with_color(self.skin.face_colour(band(value))),
                );
            };
            for (range, radius, width, alpha) in [
                (face.range60, 0.99f32, 2.0f32, 0.30f32),
                (face.range30, 0.91f32, 3.0f32, 0.55f32),
            ] {
                let Some((lo, hi)) = range else { continue };
                arc_at(frame, lo, hi, radius, width, Color { a: alpha, ..self.skin.hud });
                bug(frame, lo, radius, width + 0.5);
                bug(frame, hi, radius, width + 0.5);
            }

            // THE SLOW POINTER: the half minute, under the needle.
            //
            // A DIAL CAN SHOW TWO TIME CONSTANTS AT ONCE AND A NUMBER CANNOT,
            // which is most of the argument for drawing a dial at all. The
            // needle is three seconds and moves; this is thirty and barely
            // does, so the gap between them IS the trend -- needle above
            // pointer is a rising room, and the two together are the reading
            // and its context in one glance.
            //
            // DRAWN AS A REFERENCE AND NOT AS A SECOND NEEDLE: no
            // counterweight, chrome rather than a band colour, and stopping
            // short of the hub. It must never be mistaken for the reading.
            if let Some(v) = face.pointer {
                let a = start + sweep * dial_fraction(v) as f32;
                let (r0, r1) = (dial_r * 0.30, dial_r * 0.795);
                let (ca, sa) = (a.cos(), a.sin());
                frame.stroke(
                    &Path::line(
                        iced::Point::new(cx + r0 * ca, cy + r0 * sa),
                        iced::Point::new(cx + r1 * ca, cy + r1 * sa),
                    ),
                    Stroke::default()
                        .with_width(1.4)
                        .with_color(Color { a: 0.85, ..self.skin.bezel }),
                );
                // A diamond at the tip, so which end is pointing is not a
                // question the eye has to work out from the hub.
                let (px, py) = (-sa, ca);
                let mid = dial_r * 0.70;
                let w = 2.6f32;
                let kite = Path::new(|b| {
                    b.move_to(iced::Point::new(cx + r1 * ca, cy + r1 * sa));
                    b.line_to(iced::Point::new(cx + mid * ca + px * w, cy + mid * sa + py * w));
                    b.line_to(iced::Point::new(
                        cx + (mid - dial_r * 0.08) * ca,
                        cy + (mid - dial_r * 0.08) * sa,
                    ));
                    b.line_to(iced::Point::new(cx + mid * ca - px * w, cy + mid * sa - py * w));
                    b.close();
                });
                frame.fill(&kite, Color { a: 0.9, ..self.skin.bezel });
            }

            // A NEEDLE PER TUBE ON THIS FACE, each in its own colour and each
            // a little shorter than the one before, so two tubes reading the
            // same number are two needles that can still be told apart
            // instead of one that has swallowed the other.
            let n = face.needles.len().max(1);
            for (j, (tube, value)) in face.needles.iter().enumerate() {
                let Some(v) = value else { continue };
                let a = start + sweep * dial_fraction(*v) as f32;
                let reach = dial_r * (0.74 - 0.05 * (j as f32).min(4.0));
                let (ca, sa) = (a.cos(), a.sin());
                let tint = if *tube == usize::MAX {
                    self.skin.face_colour(band(*v))
                } else {
                    self.skin.face_tube(*tube)
                };
                // A counterweight the other side of the hub, as a real needle
                // has: it is what stops the dial looking like a clock hand.
                let back = dial_r * 0.17;
                if face.weight <= 0.0 {
                    frame.stroke(
                        &Path::line(
                            iced::Point::new(cx - back * ca, cy - back * sa),
                            iced::Point::new(cx + reach * ca, cy + reach * sa),
                        ),
                        Stroke::default()
                            .with_width(if n > 3 { 1.8 } else { 2.5 })
                            .with_color(tint),
                    );
                    continue;
                }
                // THE HEAVY KIND: a filled taper rather than a stroke, which
                // is the only way to be wide at the hub and sharp at the tip.
                // A stroke of this width would be a rectangle with a blunt
                // end, and a blunt end on a dial is a reading you cannot take
                // to better than a couple of degrees.
                let (px, py) = (-sa, ca);
                let half = face.weight;
                let tail = half * 0.55;
                let needle = Path::new(|b| {
                    b.move_to(iced::Point::new(cx + reach * ca, cy + reach * sa));
                    b.line_to(iced::Point::new(cx + px * half, cy + py * half));
                    b.line_to(iced::Point::new(
                        cx - back * ca + px * tail,
                        cy - back * sa + py * tail,
                    ));
                    b.line_to(iced::Point::new(
                        cx - back * ca - px * tail,
                        cy - back * sa - py * tail,
                    ));
                    b.line_to(iced::Point::new(cx - px * half, cy - py * half));
                    b.close();
                });
                frame.fill(&needle, tint);
            }
            // The hub last, over every needle's root, as a real one is.
            frame.fill(&Path::circle(c, 5.0), self.skin.bezel);
            frame.fill(&Path::circle(c, 2.4), Color { a: 0.9, ..self.skin.face });
        }
    }

    fn cascade(&self, frame: &mut canvas::Frame, bounds: Rectangle, room: (f32, f32)) {
        let total: usize = self.strip.iter().map(|t| t.columns).sum();
        if total == 0 {
            return;
        }
        let bar = bounds.width / total as f32;
        // THE HELD SCALE, not the tallest bar in view. Taking the maximum
        // afresh each frame made the strip breathe -- every burst rescaled it
        // and every quiet second rescaled it back, so nothing on it stood
        // still long enough to be read. The feed holds the top and lets it
        // sink slowly; see PEAK_TAU.
        let peak = self.peak.max(1.0);
        let tick = 3.0f32;
        // WHATEVER IS LEFT, SPLIT THREE TO TWO. The panel grows with the
        // window rather than leaving a band of empty grey under the table,
        // and the cascade gets the larger share because it is the one people
        // watch second by second. See `regions`, which does the splitting.
        let top = room.0;
        let height = (room.1 - tick).max(1.0);
        let mut x0 = 0.0f32;

        for (ti, tier) in self.strip.iter().enumerate() {
            let w = tier.columns as f32 * bar;
            // A tier's own patch of background, a shade apart from its
            // neighbours, so the hand-over from one resolution to the next is
            // visible rather than something you have to be told about.
            let interleave = ti + 1 == self.strip.len() && self.tubes > 1;
            if ti % 2 == 1 || interleave {
                frame.fill_rectangle(
                    iced::Point::new(x0, top + tick),
                    iced::Size::new(w, height),
                    self.skin.tier_wash(),
                );
            }
            // AND A LINE AT THE ONE BOUNDARY THAT IS NOT A DOUBLING. Every
            // hand-over to the left of it is the same measurement over twice
            // as long; this one is where the strip stops measuring TIME and
            // starts measuring ARRIVAL -- a bar to the right of it is one
            // tube's one-second reading placed where it landed, and 1/n is
            // its spacing and not its window. The alternating wash says
            // "another tier" and cannot say that.
            //
            // It is a narrow tier now that it is capped to four seconds of
            // the rota, so the line also stops it reading as a stray gap at
            // the edge of the strip.
            if interleave {
                frame.fill_rectangle(
                    iced::Point::new(x0 - 1.0, top + tick),
                    iced::Size::new(1.0, height),
                    Color { a: 0.45, ..self.skin.hud },
                );
            }
            // THE FINEST TIER IS THE ONE THAT KNOWS WHO SAID WHAT. Every bar
            // in it is one tube's reading, so with a pair it is drawn in that
            // tube's colour and the two interleave visibly -- which is the
            // only place the extra time resolution can be SEEN. Every tier
            // left of it is a mean over both and takes the level colours,
            // because from there on the two have merged into one number.
            let finest = ti + 1 == self.strip.len() && self.tubes > 1;
            for (i, v) in tier.values.iter().enumerate() {
                let Some(v) = v else { continue };
                if *v <= 0.0 {
                    continue;
                }
                let h = (*v / peak) as f32 * height;
                let tint = if finest {
                    // The sources run to the newest sample, as the tier does.
                    let back = tier.columns - i;
                    match self.sources.len().checked_sub(back).and_then(|j| self.sources.get(j)) {
                        Some(t) => self.skin.tube(*t as usize),
                        None => self.skin.dim,
                    }
                } else {
                    // Coloured by the rate a whole minute at that height
                    // would be, so the strip and the number above it agree
                    // about what "raised" means.
                    self.skin.colour(band(*v * 60.0))
                };
                frame.fill_rectangle(
                    iced::Point::new(x0 + i as f32 * bar, top + tick + height - h),
                    iced::Size::new((bar - 0.6).max(1.0), h),
                    tint,
                );
            }
            // Over the fine tier, a tick where each log row closes: the frames
            // the log is cut into, scrolling left with the counts.
            if (tier.seconds - 1.0).abs() < 1e-9 && self.every > 0 {
                for j in 0..tier.columns {
                    let a = self.n - tier.columns as i64 + j as i64;
                    if a > 0 && a % self.every == 0 {
                        frame.fill_rectangle(
                            iced::Point::new(x0 + j as f32 * bar, top),
                            iced::Size::new(1.0, tick),
                            self.skin.dim,
                        );
                    }
                }
            }
            x0 += w;
        }

        // THE TREND, OVER THE BARS IT IS THE MEAN OF. A line from the middle
        // of each bar's top to the next, at the height of that bar taken
        // with its neighbours -- see analysis::trend. One line across every
        // tier, because the strip is one stretch of time; broken where
        // nothing was measured, and not drawn over the interleave, whose
        // bars are not a rate over time.
        //
        // DRAWN LAST AND IN THE COLOUR OF THE TEXT, which no bar has: the
        // bars take the level colours and the tubes', and a line in any of
        // those -- or in the readout's teal, which was tried -- is lost in
        // the green it spends most of its time crossing. Under it a wider
        // one in the panel's ground, so it can be followed across a bar.
        let mut x0 = 0.0f32;
        let mut runs: Vec<Vec<iced::Point>> = vec![Vec::new()];
        for (tier, line) in self.strip.iter().zip(analysis::trend(&self.strip)) {
            for (i, v) in line.iter().enumerate() {
                match v {
                    Some(v) => {
                        let h = ((*v / peak) as f32).min(1.0) * height;
                        runs.last_mut().unwrap().push(iced::Point::new(
                            x0 + (i as f32 + 0.5) * bar,
                            top + tick + height - h,
                        ));
                    }
                    None if !runs.last().unwrap().is_empty() => runs.push(Vec::new()),
                    None => {}
                }
            }
            x0 += tier.columns as f32 * bar;
        }
        for run in runs.iter().filter(|r| r.len() > 1) {
            let path = canvas::Path::new(|b| {
                b.move_to(run[0]);
                for p in &run[1..] {
                    b.line_to(*p);
                }
            });
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_width(3.0)
                    .with_color(Color { a: 0.55, ..self.skin.bg }),
            );
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_width(1.3)
                    .with_color(Color { a: 0.9, ..self.skin.fg }),
            );
        }

        // WHERE THE POINTER IS, ON THE LINE. A circle on the trend at the
        // bar the pointer is over -- or on the bar's own top, over the
        // interleave, which has no trend -- and a hair down to the floor,
        // so which bar it is can be seen. The words are the window's.
        let Some(p) = self.hover.filter(|p| p.y >= top && p.y <= top + tick + height) else {
            return;
        };
        let Some((k, i)) = bar_at(&self.strip, p.x, bounds.width) else { return };
        let value = analysis::trend(&self.strip)
            .get(k)
            .and_then(|l| l.get(i).copied().flatten())
            .or_else(|| self.strip[k].values.get(i).copied().flatten());
        let Some(value) = value else { return };
        let before: usize = self.strip[..k].iter().map(|t| t.columns).sum();
        let x = (before + i) as f32 * bar + bar / 2.0;
        let y = top + tick + height - ((value / peak) as f32).min(1.0) * height;
        frame.fill_rectangle(
            iced::Point::new(x, y),
            iced::Size::new(1.0, top + tick + height - y),
            Color { a: 0.35, ..self.skin.fg },
        );
        let at = iced::Point::new(x, y);
        frame.fill(&canvas::Path::circle(at, 4.5), self.skin.bg);
        frame.stroke(
            &canvas::Path::circle(at, 4.5),
            canvas::Stroke::default().with_width(1.6).with_color(self.skin.fg),
        );
        frame.fill(&canvas::Path::circle(at, 1.6), self.skin.warn);
    }

    /// The trail: a spectrum every ten seconds, as a line, one under
    /// another, the way a drum recorder lays an earthquake down.
    ///
    /// IT IS THE SPECTRUM ABOVE IT, COMING DOWN THE PAPER. The newest row
    /// is drawn on the line the spectrum's bars stand on, and every older
    /// one four pixels further down; a row arrives at the top and the rest
    /// move down to let it in, a tenth of a row each second. There is no
    /// box, no second axis and no words: the axis is the spectrum's, under
    /// the panel, and a period is where it is in both -- `axis`.
    ///
    /// UP IS MORE. A trace rises from its line for what is louder, as a
    /// bar of the spectrum does and as every other chart on the panel
    /// does. It hung for an afternoon, under a spectrum that hung, and a
    /// peak that points at the floor is read as a dip.
    ///
    /// THE GROUND IS SHADED, half way to black, so that a bright line is
    /// bright against something. On paper the pen could only be darker than
    /// its ground, and the loudest thing on it was the least like light.
    ///
    /// THE PEN IS A HOT ONE, and says how loud in three ways at once. Its
    /// COLOUR is a temperature: `heat`. Its WIDTH grows with the same
    /// figure, from under a pixel to three. And over the line luck reaches
    /// it has a GLOW, a wider stroke of the same colour under it. How loud
    /// is against the luck line and not against the loudest in view, so a
    /// colour and a width mean the same on every paper.
    ///
    /// THE WHOLE TRACE IS AS BRIGHT AS THE ROOM COUNTED. A row has a level
    /// -- what was counted in its own five minutes, against the long
    /// average -- and that is the trace's opacity: a row from a room
    /// counting twice its usual is the brightest there is, and one from a
    /// room at half is a ghost. So the paper says when the rate rose as
    /// well as what it rose at.
    ///
    /// A PEAK IS MARKED, where a row stands over both its neighbours and
    /// over the luck line: a dot, in the pen's colour. And THE ONE LOUDEST
    /// THING ON THE PAPER has a ring, which is the only white on it.
    ///
    /// HOW FAR IT RISES is `analysis::gain`. AND THE ROW IN FRONT HIDES
    /// WHAT IS BEHIND IT, where in front is further down the paper: a
    /// trace rises over the lines above its own, so under each trace the
    /// ground is painted back in, newest row first and oldest last, and
    /// the lines do not run through each other. That is all the depth
    /// there is.
    fn trail(&self, frame: &mut canvas::Frame, width: f32, room: (f32, f32), bars: f32) {
        use canvas::{path::Builder, Path, Stroke};
        let (top, height) = room;
        let ground = shaded(self.skin.bg);
        frame.fill_rectangle(
            iced::Point::new(0.0, top),
            iced::Size::new(width, height),
            ground,
        );
        let Some(x_of) = self.axis(width) else { return };
        let luck = self.fall_luck;
        let slide = (self.fall_age as f32 / FALL_HOP as f32).min(1.0);
        let foot = top + height;
        // The line the spectrum stands on, which is the newest row's.
        let base = top + bars;
        let shown = (((foot - base) / TRAIL_STEP) as usize + 1).min(self.fall.len());
        // The loudest thing on the paper rises the whole reach, and never
        // less than luck does: see `analysis::gain`. And the row that
        // counted most is as bright as ink is, and never for less than
        // twice the usual.
        let on_axis = |row: &analysis::TrailRow| -> Vec<(usize, f32)> {
            row.bins
                .iter()
                .enumerate()
                .filter_map(|(i, (period, _))| Some((i, x_of(*period as f64)?)))
                .collect()
        };
        let rows: Vec<(usize, &analysis::TrailRow, Vec<(usize, f32)>)> = self
            .fall
            .iter()
            .enumerate()
            .take(shown)
            .map(|(k, row)| (k, row, on_axis(row)))
            .filter(|(_, _, across)| across.len() >= 2)
            .collect();
        let loudest = rows
            .iter()
            .flat_map(|(_, row, across)| across.iter().map(move |(i, _)| row.bins[*i].1))
            .fold(luck, f32::max) as f64;
        let busiest = rows.iter().map(|(_, row, _)| row.level).fold(2.0f32, f32::max) as f64;
        let mut extreme: Option<(f32, iced::Point)> = None;

        for (k, row, across) in rows.iter() {
            let line = base + (*k as f32 + slide) * TRAIL_STEP;
            if line > foot {
                continue;
            }
            let power = |i: usize| row.bins[i].1;
            let at = |i: usize| {
                (line - analysis::gain(power(i) as f64, loudest) as f32 * TRAIL_REACH).max(top)
            };
            // The ground, put back under the trace.
            let mut under = Builder::new();
            under.move_to(iced::Point::new(across[0].1, line));
            for (i, x) in across {
                under.line_to(iced::Point::new(*x, at(*i)));
            }
            under.line_to(iced::Point::new(across[across.len() - 1].1, line));
            under.close();
            frame.fill(&under.build(), ground);

            // As bright as the room counted, and a little fainter for
            // being newer: further up the paper is further away.
            let bright = 0.30 + 0.70 * analysis::gain(row.level as f64, busiest) as f32;
            let away = 1.0 - (*k as f32 + slide) / FALL_DEPTH as f32;
            let alpha = bright * (1.0 - 0.35 * away.clamp(0.0, 1.0));
            let mut shades: Vec<Option<Builder>> = (0..SHADES).map(|_| None).collect();
            for pair in across.windows(2) {
                let ((i, x0), (j, x1)) = (pair[0], pair[1]);
                let b = shades[shade_of(power(i).max(power(j)), luck)]
                    .get_or_insert_with(Builder::new);
                b.move_to(iced::Point::new(x0, at(i)));
                b.line_to(iced::Point::new(x1, at(j)));
            }
            for (shade, b) in shades.into_iter().enumerate() {
                let Some(b) = b else { continue };
                let (path, ink, wide) = (b.build(), heat(shade), pen(shade));
                if shade >= shade_of(luck, luck) {
                    frame.stroke(
                        &path,
                        Stroke::default()
                            .with_width(wide + 4.0)
                            .with_color(Color { a: 0.22 * alpha, ..ink }),
                    );
                }
                frame.stroke(
                    &path,
                    Stroke::default().with_width(wide).with_color(Color { a: alpha, ..ink }),
                );
            }
            for i in row.peaks(luck) {
                let Some((_, x)) = across.iter().find(|(j, _)| *j == i) else { continue };
                let here = iced::Point::new(*x, at(i));
                frame.fill(
                    &Path::circle(here, 2.2),
                    Color { a: alpha.max(0.6), ..heat(shade_of(power(i), luck)) },
                );
                if extreme.map_or(true, |(p, _)| power(i) > p) {
                    extreme = Some((power(i), here));
                }
            }
        }
        if let Some((_, here)) = extreme {
            frame.stroke(
                &Path::circle(here, 5.0),
                Stroke::default().with_width(1.4).with_color(Color::WHITE),
            );
        }
    }

    /// Where a period is across the panel, for the spectrum and for the
    /// trail under it: the one arithmetic both are drawn by, which is
    /// `stretches` and `place`. None where nothing has answered yet; and,
    /// of a period, None where it is off the axis.
    fn axis(&self, width: f32) -> Option<impl Fn(f64) -> Option<f32>> {
        if self.layers.iter().all(|l| l.rel.is_empty()) {
            return None;
        }
        let (shortest, longest) = ladder_ends(&self.layers);
        let axis = stretches(period_floor(shortest, longest), longest);
        // LONG PERIODS ON THE LEFT, as the terminal monitor has always drawn
        // them and as the captions under this panel say. It ran the other way
        // for exactly as long as it took somebody to read the axis.
        Some(move |period: f64| place(&axis, period).map(|x| x * width))
    }

    /// The overlay: three spectra on one logarithmic period axis.
    ///
    /// LOG PERIOD, NOT BIN INDEX. Three windows have three different bin
    /// spacings and three different longest periods, so plotting them by bin
    /// would put the same physical period in three different places and the
    /// overlay would say nothing. On a log-period axis a real line stands at
    /// the same x in every layer that can see it, and the layers that cannot
    /// simply stop short -- which is itself the useful picture, because a
    /// period only one long window reaches is exactly the one worth doubting.
    ///
    /// TRANSPARENT AND ADDITIVE-LOOKING ON PURPOSE. Each layer is drawn at a
    /// third alpha in one primary, so two layers agreeing read as a blend and
    /// a lone spike keeps its own colour and announces which window it came
    /// from.
    ///
    /// STANDING, bars upward, as it first was. It hung from the floor of the
    /// counts for an afternoon, to close the band of nothing between the
    /// two charts, and a peak that points at the floor was read as a dip.
    /// Up is more on every chart of the panel. With the trail, the bars
    /// stand in a band at the head of its ground and the newest row is
    /// drawn on the line they stand on; without it, they stand on the foot
    /// of the canvas.
    fn spectrum(&self, frame: &mut canvas::Frame, bounds: Rectangle, room: (f32, f32)) {
        // UP IS MORE: the bars stand on the foot of their room.
        let (top, h) = room;
        let floor = top + h;
        let live: Vec<&Layer> = self.layers.iter().filter(|l| !l.rel.is_empty()).collect();
        let Some(x_of) = self.axis(bounds.width) else { return };
        let (shortest, longest) = ladder_ends(&self.layers);
        // ONE SCALE FOR EVERY LAYER, so the taller of two peaks is the
        // louder; ending at the loudest bar in view, and never under a
        // little more than luck reaches, so a flat spectrum does not fill
        // the panel with its own noise. The heights are `analysis::gain`.
        let peak = live
            .iter()
            .flat_map(|l| {
                let floor = layer_floor(l.window, shortest, longest);
                l.rel
                    .iter()
                    .enumerate()
                    .filter(move |(i, _)| l.window as f64 / (*i + 1) as f64 >= floor)
                    .map(|(_, v)| *v)
            })
            .fold(0.0f64, f64::max)
            .max(live.iter().map(|l| l.luck).fold(0.0, f64::max) * 1.6)
            .max(1e-9);

        // The luck line of the tightest layer: the height a peak has to clear
        // before it means anything at all.
        let luck = live.iter().map(|l| l.luck).fold(f64::INFINITY, f64::min);
        if luck.is_finite() {
            frame.fill_rectangle(
                iced::Point::new(0.0, floor - analysis::gain(luck, peak) as f32 * h),
                iced::Size::new(bounds.width, 1.0),
                Color { a: 0.30, ..self.skin.dim },
            );
        }

        for l in &live {
            let idx = self
                .layers
                .iter()
                .position(|x| x.window == l.window)
                .unwrap_or(0);
            let tint = self.skin.layers[idx.min(self.skin.layers.len() - 1)];
            // Bins are dense at the short-period end of a log axis, so fold
            // them onto columns and keep the LOUDEST in each: a single sharp
            // line is what is being looked for, and averaging it with its
            // quiet neighbours is how it disappears.
            // ONLY THE BAND THIS WINDOW CAN RESOLVE. Below its floor the
            // bins are closer together than a bar is wide, and folding them
            // would draw the largest of a crowd rather than a line.
            let shortest_drawn = layer_floor(l.window, shortest, longest);
            let mut col: Vec<f64> = vec![0.0; bounds.width.max(1.0) as usize];
            for (i, v) in l.rel.iter().enumerate() {
                let period = l.window as f64 / (i + 1) as f64;
                if period < shortest_drawn {
                    continue;
                }
                let Some(x) = x_of(period) else { continue };
                let x = x as usize;
                if x < col.len() && *v > col[x] {
                    col[x] = *v;
                }
            }
            for (x, v) in col.iter().enumerate() {
                if *v <= 0.0 {
                    continue;
                }
                let bh = analysis::gain(*v, peak) as f32 * h;
                frame.fill_rectangle(
                    iced::Point::new(x as f32, floor - bh),
                    iced::Size::new(1.0, bh),
                    Color { a: self.skin.layer_alpha, ..tint },
                );
            }
        }
    }
}

// -------------------------------------------------------------------- feed ---

/// The mean rate across the tubes, from windows holding every tube's samples.
///
/// OVER THE TUBE-SECONDS THAT WERE MEASURED, not over the tubes plugged in:
/// a tube that has stopped answering is not in the division. See
/// `Windows::mean`, and `mean_cpm` in the monitor, which is the same rule.
fn mean_of(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    if tubes <= 1 { w.average(span) } else { w.mean(span) }
}

/// One sigma on that mean, in CPM.
///
/// Arrivals are Poisson, so the whole of the uncertainty is the count behind
/// the number: N arrivals give a relative error of 1/sqrt(N), and the counts
/// behind a mean of `tubes` tubes over `span` seconds is `cpm * span * tubes /
/// 60`. THIS is what a second counter buys -- the same figure, known to within
/// a factor of root two better -- and printing it is the only way the benefit
/// is visible at all.
///
/// THE COUNTS THE WINDOW HOLDS, which is that product only while every tube
/// answers every second. See `Windows::sigma`.
fn sigma_of(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    if tubes > 1 {
        return w.sigma(span);
    }
    let cpm = w.average(span)?;
    let n = cpm * span / 60.0;
    (n > 0.0).then(|| cpm / n.sqrt())
}

/// The stream of snapshots, and the thread that makes them.
///
/// RECONNECTING IS THE NORMAL CASE, not an error path. The service is
/// restarted when the machine is, when a counter is unplugged, and whenever
/// somebody upgrades it; a window left open across any of those should pick
/// the counters back up by itself rather than have to be closed and reopened.
fn feed() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(32, async |mut out| {
        std::thread::spawn(move || {
            let dir = logs_dir();
            loop {
                let Some(mut client) = Client::attach(&dir) else {
                    let _ = out.try_send(Message::Adrift(format!(
                        "nothing is serving {}",
                        broker::socket_path(&dir).display()
                    )));
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                // BOTH CHANGE WHILE THE WINDOW IS OPEN. A counter plugged
                // into the running service arrives as an event, not a
                // reconnection, so everything sized by the tube count has to
                // be able to grow: the windows, the pools, the colours, the
                // divisor under every combined figure and the unit of the
                // cascade's finest tier.
                let mut id = client.identity().clone();
                let mut tubes = id.len().max(1);
                let mut present: Vec<bool> = vec![true; tubes];
                let columns = log::columns(&log::header(&id.spans));
                // One set of windows per tube, and one across all of them.
                let mut each: Vec<Windows> =
                    (0..tubes).map(|_| Windows::new(&id.spans)).collect();
                let mut all = Windows::new(&id.spans);
                let mut ladder: Vec<Spectrum> =
                    LADDER.iter().map(|w| Spectrum::new(*w)).collect();
                // The same seconds, a row at a time. The rows are shared
                // with the interface and made again only when one arrives.
                // The shortest window and the spectrum's own three, so a
                // row reaches as far left as the axis does: see `Trail`.
                let mut fall = Trail::new(
                    &[FALL_WINDOW, LADDER[0], LADDER[1], LADDER[2]],
                    FALL_HOP,
                    FALL_DEPTH,
                    FALL_LEVEL,
                );
                let mut fall_rows: Arc<Vec<analysis::TrailRow>> = Arc::new(Vec::new());
                let mut fall_drawn: Option<u64> = None;
                let mut pool = entropy::Entropy::default();
                // The samples in ARRIVAL order, as rates, which is what the
                // cascade is cut from. One counter is one bar a second; two
                // is a bar every half second on average, because two tubes on
                // their own clocks interleave.
                let mut merged: VecDeque<f64> = VecDeque::with_capacity(STRIP_KEEP);
                // AND AS THEY ARRIVED, with the tube and the time, for a
                // strip cut by the clock; when each tube last spoke and
                // what it said. See analysis::tiers_arrivals.
                let mut arrivals: VecDeque<Arrival> = VecDeque::with_capacity(STRIP_KEEP);
                let mut spoke: Vec<Option<f64>> = vec![None; tubes];
                let mut newest: Vec<Option<u32>> = vec![None; tubes];
                let mut dropped = 0usize;
                let mut rows: VecDeque<Vec<String>> = VecDeque::with_capacity(ROWS);
                let mut random: Option<(usize, String, String, bool)> = None;
                let mut now = 0u32;
                // Whole seconds, summed across the tubes, for the spectra: a
                // period is a property of the room and every tube is looking
                // at the same room, so adding them is simply more signal on
                // one time base.
                let mut sec_bin: Option<i64> = None;
                let mut sec_sum: u32 = 0;
                // WHAT THE COLLECTING METER SHOWS, AND WHERE IT HAS BEEN.
                //
                // THE ARRIVALS, NOT THE CLOSED SECONDS. This waited for a
                // second to CLOSE before it had a reading for it -- which
                // means waiting for the first sample of the NEXT second, so
                // the needle was always showing a second that had already
                // finished, and with several tubes it was whichever tube
                // happened to tick over first that ended the wait. A rolling
                // window over the arrivals themselves has a reading for the
                // second just gone the moment it is gone. See `Recent`.
                let mut recent = Recent::default();
                let mut d30 = Drift::new(30.0);
                let mut d60 = Drift::new(60.0);
                // The needle, which has mass. See `Ballistic`.
                let mut needle = Ballistic::new(0.25, 0.9);
                let mut meters = Meters::default();
                let mut was = Meters::default();
                let mut metered = Instant::now();
                let mut heard = Instant::now();
                let mut peak_hold = 0.0f64;
                // For the interleave measurement: when the previous sample
                // landed, and which tube it came from.
                let mut last: Option<(usize, f64)> = None;
                let mut gaps = (0.0f64, 0u32);
                let mut replaying = true;
                let mut sent = Instant::now() - Duration::from_secs(1);

                // A SHORT POLL AND A COUNTED SILENCE, rather than a long
                // blocking read. The needle has to move between samples and
                // the snapshot is too heavy to send at that rate, so this
                // wakes twelve times a second whatever the counters are
                // doing. `Poll` is what keeps "nothing yet" apart from "the
                // server has gone" -- with `next` they are the same `None`,
                // and every one of these wake-ups would have read as the
                // counter stopping.
                loop {
                    let event = match client.poll(Duration::from_millis(METER_MS)) {
                        Poll::Event(e) => {
                            heard = Instant::now();
                            Some(e)
                        }
                        // Nothing on the wire. Still a chance to move the
                        // needle -- and, after long enough, the end.
                        Poll::Idle => {
                            if heard.elapsed() >= ADRIFT_AFTER {
                                break;
                            }
                            None
                        }
                        Poll::Closed => break,
                    };
                    if let Some(event) = event {
                        match event {
                            Event::Sample { who, when, counts } => {
                                let who = who.min(tubes - 1);
                                all.add(when, counts);
                                each[who].add(when, counts);
                                if let Some((prev, t)) = last {
                                    if prev != who && when > t {
                                        gaps.0 += when - t;
                                        gaps.1 += 1;
                                    }
                                }
                                last = Some((who, when));
                                // A sample is the tube answering, whatever
                                // was said of it before.
                                spoke[who] = Some(when);
                                newest[who] = Some(counts);
                                present[who] = true;
                                if arrivals.len() == STRIP_KEEP {
                                    arrivals.pop_front();
                                }
                                arrivals.push_back(Arrival { who, when, counts });
                                if merged.len() == STRIP_KEEP {
                                    merged.pop_front();
                                    dropped += 1;
                                }
                                merged.push_back(counts as f64);
                                recent.push(when, counts);
                                now = counts;
                                let this = when.floor() as i64;
                                match sec_bin {
                                    Some(b) if b == this => sec_sum += counts,
                                    Some(_) => {
                                        // The SPECTRA still want whole seconds --
                                        // a period is a property of the room on
                                        // one time base -- and only they do. The
                                        // meters no longer wait for this.
                                        for r in ladder.iter_mut() {
                                            r.add(sec_sum);
                                        }
                                        fall.add(sec_sum);
                                        sec_bin = Some(this);
                                        sec_sum = counts;
                                    }
                                    None => {
                                        sec_bin = Some(this);
                                        sec_sum = counts;
                                    }
                                }
                                // NOT DURING THE REPLAY. The server's pool holds
                                // the counts since its last draw; pouring hours of
                                // history into this one would have it claim a line
                                // the moment the window opened.
                                if !replaying {
                                    pool.add(counts);
                                }
                            }
                            Event::Live => replaying = false,
                            // A tube joined, or came back. Its slot is its
                            // serial's, so a counter that was unplugged and put
                            // back resumes its own colour and its own windows
                            // rather than appearing as a stranger.
                            Event::Counter { who, .. } => {
                                id = client.identity().clone();
                                tubes = id.len().max(1);
                                while each.len() < tubes {
                                    each.push(Windows::new(&id.spans));
                                }
                                present.resize(tubes, false);
                                spoke.resize(tubes, None);
                                newest.resize(tubes, None);
                                if let Some(p) = present.get_mut(who) {
                                    *p = true;
                                }
                                each[who] = Windows::new(&id.spans);
                                spoke[who] = None;
                                newest[who] = None;
                                // The gap is measured afresh: what it was
                                // before the tube left is not what it is now.
                                gaps = (0.0, 0);
                                last = None;
                            }
                            Event::Gone { who } => {
                                if let Some(p) = present.get_mut(who) {
                                    *p = false;
                                }
                                gaps = (0.0, 0);
                                last = None;
                            }
                            // The service is reading a flash and nothing will
                            // be counted until it is done. Until the first
                            // sample there is no panel to spoil, so the note
                            // is the window; after it, the panel carries on.
                            Event::Note { text } => {
                                if !text.is_empty() && recent.newest().is_none() {
                                    let _ = out.try_send(Message::Adrift(text));
                                }
                            }
                            Event::Random { who, hex, at, suspect } => {
                                random = Some((who.min(tubes - 1), hex, at, suspect));
                                pool.reset();
                            }
                            Event::Row { row, .. } => {
                                if rows.len() == ROWS {
                                    rows.pop_front();
                                }
                                rows.push_back(row.split('\t').map(str::to_string).collect());
                            }
                        }
                    }
                    if replaying {
                        continue;
                    }
                    // ---- the meters, on their own beat ------------------
                    //
                    // BEFORE THE SNAPSHOT AND INDEPENDENT OF IT. This is the
                    // part that has to be quick: a rolling read of the last
                    // one, three and thirty seconds of arrivals, and one step
                    // of the needle toward the three-second one.
                    let dt = metered.elapsed().as_secs_f64();
                    if dt >= METER_MS as f64 / 1000.0 {
                        metered = Instant::now();
                        // The window ends NOW, not at the last sample: a tube
                        // that has gone quiet has to show as the room going
                        // quiet, and it cannot if the window follows it.
                        // Never ahead of the newest sample, though, or a
                        // replay would read its own history as stale.
                        let at = recent
                            .newest()
                            .map(|t| clock::now().max(t))
                            .unwrap_or_else(clock::now);
                        meters.live = recent.rate(at, 1.0);
                        meters.live3 = recent.rate(at, NEEDLE_SPAN);
                        meters.live30 = recent.rate(at, POINTER_SPAN);
                        // THE BUGS FOLLOW THE NEEDLE, not the raw second.
                        // They are the range the needle has been bouncing
                        // between, and a range drawn around a number the
                        // needle never showed is a range of nothing.
                        if let Some(v) = meters.live3 {
                            meters.needle = Some(needle.push(v, dt));
                            d30.push(v, dt);
                            d60.push(v, dt);
                        } else {
                            meters.needle = needle.at();
                        }
                        meters.range30 = d30.range();
                        meters.range60 = d60.range();
                        if meters.worth_sending(&was) {
                            was = meters;
                            let _ = out.try_send(Message::Moved(meters));
                        }
                    }
                    if sent.elapsed() < Duration::from_millis(900) {
                        continue;
                    }
                    sent = Instant::now();
                    let series: Vec<f64> = merged.iter().copied().collect();
                    // WHICH TUBES ARE ANSWERING, at the moment being drawn: not
                    // given up by the service, and heard from within
                    // QUIET_AFTER. Never ahead of the newest sample, for the
                    // reason the meters are not.
                    let at = recent
                        .newest()
                        .map(|t| clock::now().max(t))
                        .unwrap_or_else(clock::now);
                    let gone: Vec<bool> = present.iter().map(|p| !p).collect();
                    let live = answering(&spoke, &gone, at);
                    let answering_now = live.iter().filter(|l| **l).count();
                    // Whole seconds BY THE CLOCK, and one tier below them for
                    // the tubes that are answering; see
                    // analysis::tiers_arrivals.
                    // The second after the newest, by the clock.
                    let clock_end = arrivals
                        .iter()
                        .map(|a| a.when)
                        .fold(f64::MIN, f64::max)
                        .floor() as i64
                        + 1;
                    let (strip, sources, strip_end, strip_shift) = if tubes > 1 {
                        arrivals.make_contiguous();
                        let s = tiers_arrivals(arrivals.as_slices().0, &live, STRIP_COLS);
                        // Cut by the clock; the interleave, when there is
                        // one, has the last seconds to itself.
                        let held = if s.live > 1 { analysis::INTERLEAVE_SECONDS as i64 } else { 0 };
                        (s.tiers,
                         s.sources.iter().map(|k| k.unwrap_or(u8::MAX)).collect(),
                         clock_end - held, 0)
                    } else {
                        // Cut in samples, the newest of which is this second.
                        let n = (dropped + series.len()) as i64;
                        (tiers_with(&series, dropped, STRIP_COLS, LADDER[0], TIERS, 1.0),
                         Vec::new(), n, clock_end - n)
                    };
                    // The scale rises at once and sinks slowly: see PEAK_TAU.
                    let tallest = strip
                        .iter()
                        .flat_map(|t| t.values.iter().flatten())
                        .cloned()
                        .fold(0.0f64, f64::max);
                    peak_hold = if tallest > peak_hold {
                        tallest
                    } else {
                        peak_hold + (tallest - peak_hold) * (1.0 - (-1.0f64 / PEAK_TAU).exp())
                    };
                    let headline = mean_of(&all, HEADLINE, tubes)
                        .or_else(|| id.spans.last().and_then(|s| mean_of(&all, *s, tubes)));
                    // The rows, worked out when there is a new one to show.
                    if fall.newest() != fall_drawn {
                        fall_drawn = fall.newest();
                        fall_rows = Arc::new(fall.rows());
                    }
                    let shot = Snapshot {
                        counters: id.counters.clone(),
                        averages: id
                            .spans
                            .iter()
                            .map(|s| {
                                (*s, mean_of(&all, *s, tubes), sigma_of(&all, *s, tubes))
                            })
                            .collect(),
                        // A TUBE THAT HAS STOPPED ANSWERING READS `--`, not
                        // its last number. A stale reading on a dial is worse
                        // than no reading: it is the instrument claiming to
                        // still be measuring.
                        per: (0..tubes)
                            .map(|k| {
                                if !live.get(k).copied().unwrap_or(false) {
                                    return None;
                                }
                                each[k].average(HEADLINE).or_else(|| {
                                    id.spans.last().and_then(|s| each[k].average(*s))
                                })
                            })
                            .collect(),
                        latest: (0..tubes)
                            .map(|k| {
                                newest.get(k).copied().flatten()
                                    .filter(|_| live.get(k).copied().unwrap_or(false))
                            })
                            .collect(),
                        live: answering_now,
                        elapsed: all.elapsed(),
                        headline,
                        headline_sigma: headline.and_then(|_| sigma_of(&all, HEADLINE, tubes)),
                        now,
                        total: all.total,
                        // An interleave is between tubes that are answering.
                        phase: (gaps.1 > 0 && answering_now > 1)
                            .then(|| gaps.0 / gaps.1 as f64),
                        strip,
                        peak: peak_hold,
                        sources,
                        samples: (dropped + merged.len()) as i64,
                        every: log::DEFAULT_LOG_EVERY.round() as i64,
                        layers: ladder
                            .iter()
                            .map(|r| {
                                let rel = r.relative();
                                Layer {
                                    window: r.window,
                                    rel,
                                    luck: r.chance_max(),
                                }
                            })
                            .collect(),
                        fall: fall_rows.clone(),
                        fall_made: fall.newest().unwrap_or(0),
                        fall_age: fall.age(),
                        fall_luck: fall.chance_max() as f32,
                        strip_end,
                        strip_shift,
                        random: random.clone(),
                        pool: entropy::pool_status(&pool, "next in "),
                        rows: rows.iter().cloned().collect(),
                        columns: columns.clone(),
                    };
                    if out.try_send(Message::Update(Box::new(shot))).is_err() {
                        // The interface is gone, or a second behind and not
                        // catching up. The next second's snapshot supersedes
                        // this one, so there is nothing to retry.
                        continue;
                    }
                }
                let _ = out.try_send(Message::Adrift(
                    "the counter stopped -- waiting for it to come back".into(),
                ));
            }
        });
    })
}


// ------------------------------------------------------------------- skin ---

/// The panel's colours, which follow the desktop's.
///
/// COPAL WRITES DOWN WHICH THEME IS ON, so this asks rather than guesses:
/// `~/.config/copal/current/theme/theme.conf` is the active theme's own file,
/// carrying its NAME and whether it is a light or a dark one. A desktop that
/// does not have it falls back to dark, which is what an instrument panel is
/// by default.
///
/// THE DIAL FACES STAY DARK IN BOTH. Antiquity's own note about itself is that
/// it is "dark chrome around light paper", and a black-faced gauge on paper is
/// what the instruments this borrows from actually look like. Inverting the
/// faces to match the page would make them worse, not more consistent.
#[derive(Debug, Clone)]
struct Skin {
    name: String,
    dark: bool,
    bg: Color,
    fg: Color,
    dim: Color,
    faint: Color,
    face: Color,
    bezel: Color,
    hud: Color,
    bands: [Color; 5],
    tubes: [Color; 10],
    layers: [Color; 3],
    cyan: Color,
    warn: Color,
    /// THE MARKS ON A DIAL FACE ARE NOT THE MARKS ON THE PAGE. The face is
    /// dark in both skins -- a black-faced gauge is what the instruments this
    /// borrows from look like, and Antiquity's own note about itself is that
    /// it is dark chrome around light paper. So a light skin needs two sets:
    /// dark colours for text on paper, and bright ones for needles and bands
    /// on the face. A single set cannot serve both; the first attempt drew
    /// near-black bands on a near-black face.
    face_bands: [Color; 5],
    face_tubes: [Color; 10],
    /// How solidly the spectrum layers are laid over one another. Paper takes
    /// more than a dark panel does before a colour reads as a colour.
    layer_alpha: f32,
}

fn rgb(hex: u32) -> Color {
    Color::from_rgb8(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

impl Skin {
    /// The instrument at night: what this panel has always looked like.
    fn dark() -> Skin {
        Skin {
            name: "dark".into(),
            dark: true,
            bg: rgb(0x1e1e2e),
            fg: rgb(0xccd6eb),
            dim: rgb(0x9ea3b8),
            faint: rgb(0x73788c),
            face: rgb(0x0a0d14),
            bezel: rgb(0x9ea8bd),
            hud: rgb(0x8cf2e6),
            bands: [
                rgb(0x739ed9), // attenuated -- cold, and deliberately not green
                rgb(0x66d98c), // nominal
                rgb(0xf2d859), // advisory
                rgb(0xfa9e40), // warning
                rgb(0xf75257), // deadly
            ],
            tubes: [
                rgb(0x8ccbfa), rgb(0xd9b3ff), rgb(0x8ceda6), rgb(0xfcbf73),
                rgb(0x73ebeb), rgb(0xfc9ec7), rgb(0xd1e680), rgb(0xfa8c85),
                rgb(0x80c7b8), rgb(0xc2c2f5),
            ],
            layers: [rgb(0x66bfff), rgb(0x73f28c), rgb(0xff7373)],
            cyan: rgb(0x73d1eb),
            warn: rgb(0xfac959),
            face_bands: [
                rgb(0x739ed9), rgb(0x66d98c), rgb(0xf2d859),
                rgb(0xfa9e40), rgb(0xf75257),
            ],
            face_tubes: [
                rgb(0x8ccbfa), rgb(0xd9b3ff), rgb(0x8ceda6), rgb(0xfcbf73),
                rgb(0x73ebeb), rgb(0xfc9ec7), rgb(0xd1e680), rgb(0xfa8c85),
                rgb(0x80c7b8), rgb(0xc2c2f5),
            ],
            layer_alpha: 0.55,
        }
    }

    /// Antiquity's helios: dark instruments on light paper.
    fn antiquity() -> Skin {
        Skin {
            name: "antiquity".into(),
            dark: false,
            bg: rgb(0xfce2ab),
            fg: rgb(0x1e2a3a),
            dim: rgb(0x6b5a30),
            faint: rgb(0x8a7748),
            face: rgb(0x1c1c1c),
            bezel: rgb(0x87704f),
            hud: rgb(0x1e6b63),
            // Darker and more saturated than the dark skin's: these are read
            // against paper, where a pale green is no colour at all.
            bands: [
                rgb(0x2d5a8a), // attenuated
                rgb(0x3d7a2e), // nominal
                rgb(0x8a6a12), // advisory
                rgb(0xa33b20), // warning
                rgb(0x7b2d3e), // deadly
            ],
            tubes: [
                rgb(0x1e4d7a), rgb(0x6b3a7a), rgb(0x2e6b3a), rgb(0x9e5a12),
                rgb(0x1e6b6b), rgb(0x94304f), rgb(0x5c6b1f), rgb(0xa33b20),
                rgb(0x2d5c52), rgb(0x4a4a8a),
            ],
            layers: [rgb(0x1e4d8a), rgb(0x2e6b2e), rgb(0xa33b20)],
            cyan: rgb(0x1e5c6b),
            warn: rgb(0x8a5a12),
            // On the face, where the background is near-black in both skins.
            face_bands: [
                rgb(0x8ab4e6), rgb(0x7ad991), rgb(0xf0cf6b),
                rgb(0xf5a259), rgb(0xf26d72),
            ],
            face_tubes: [
                rgb(0x9ccbf5), rgb(0xd4b0f0), rgb(0x9ae0a8), rgb(0xf5c581),
                rgb(0x84dede), rgb(0xf2a3c4), rgb(0xd6e089), rgb(0xf29a94),
                rgb(0x93c9bd), rgb(0xc4c4ef),
            ],
            layer_alpha: 0.78,
        }
    }

    /// What the desktop is wearing, or dark if it will not say.
    fn detect() -> Skin {
        let home = std::env::var("HOME").unwrap_or_default();
        let conf = std::path::PathBuf::from(home)
            .join(".config/copal/current/theme/theme.conf");
        let Ok(text) = std::fs::read_to_string(&conf) else {
            return Skin::dark();
        };
        let field = |key: &str| -> Option<String> {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
                .map(|v| {
                    v.split('#')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"')
                        .to_string()
                })
        };
        let mut skin = match field("VARIANT").as_deref() {
            Some("light") => Skin::antiquity(),
            _ => Skin::dark(),
        };
        if let Some(n) = field("NAME").filter(|n| !n.is_empty()) {
            skin.name = n;
        }
        skin
    }

    fn named(what: &str) -> Skin {
        match what {
            "dark" => Skin::dark(),
            "light" | "antiquity" => Skin::antiquity(),
            _ => Skin::detect(),
        }
    }

    /// A band as it reads on a dial face.
    fn face_colour(&self, b: Band) -> Color {
        match b {
            Band::Attenuated => self.face_bands[0],
            Band::Nominal => self.face_bands[1],
            Band::Advisory => self.face_bands[2],
            Band::Warning => self.face_bands[3],
            Band::Deadly => self.face_bands[4],
        }
    }

    /// A tube's colour as it reads on a dial face.
    fn face_tube(&self, k: usize) -> Color {
        self.face_tubes[k % self.face_tubes.len()]
    }

    fn colour(&self, b: Band) -> Color {
        match b {
            Band::Attenuated => self.bands[0],
            Band::Nominal => self.bands[1],
            Band::Advisory => self.bands[2],
            Band::Warning => self.bands[3],
            Band::Deadly => self.bands[4],
        }
    }

    fn tube(&self, k: usize) -> Color {
        self.tubes[k % self.tubes.len()]
    }

    /// The wash behind every other cascade tier, which marks the hand-over
    /// from one resolution to the next.
    ///
    /// LIGHTER ON A DARK PANEL AND DARKER ON A LIGHT ONE. A fixed tint that
    /// reads as a panel on one theme reads as a stain on the other, and this
    /// is the one place the two skins need opposite treatment rather than
    /// different values.
    fn tier_wash(&self) -> Color {
        if self.dark {
            Color { a: 0.05, ..self.fg }
        } else {
            Color { a: 0.07, ..rgb(0x3a2f18) }
        }
    }

    /// An iced theme carrying this skin's page colours, so the widgets that
    /// draw their own background agree with the ones that do not.
    fn theme(&self) -> Theme {
        Theme::custom(
            self.name.clone(),
            iced::theme::Palette {
                background: self.bg,
                text: self.fg,
                primary: self.bands[1],
                success: self.bands[1],
                warning: self.bands[2],
                danger: self.bands[4],
            },
        )
    }
}

// ----------------------------------------------------------------- colours ---


// ------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// THE FLOOR IS SOLVED, NOT PICKED. It is the period at which the
    /// SHORTEST window's bins are still a bar apart -- and because that floor
    /// sets the span of the axis, and the span sets the floor, it is implicit
    /// and has to be iterated to. A hand-picked 30s threw away an octave the
    /// 512-second window could honestly have shown.
    #[test]
    fn the_shortest_window_sets_the_axis_and_settles_where_its_bins_fit() {
        let p = period_floor(512, 32768);
        assert!(p > 5.0 && p < 10.0, "around seven seconds, got {}", p);
        // Self-consistent: at that floor, the shortest window's own limit IS
        // the floor. This is the fixed point, checked rather than asserted.
        let back = layer_floor(512, 512, 32768);
        assert!((back - p).abs() < 0.05, "floor {} vs layer {}", p, back);
    }

    /// Each window covers the band it is good for, and they hand over.
    #[test]
    fn a_longer_window_starts_later_and_ends_later() {
        let (s, l) = (512, 32768);
        let short = layer_floor(512, s, l);
        let mid = layer_floor(4096, s, l);
        let long = layer_floor(32768, s, l);
        assert!(short < mid && mid < long, "{} {} {}", short, mid, long);
        // Eight times the window is eight times the floor: one bar per bin is
        // a straight proportion.
        assert!((mid / short - 8.0).abs() < 1e-6);
        // And every layer still reaches its own window at the top.
        assert!(long < 32768.0, "the longest window can draw some of itself");
    }

    /// Nothing is resolvable below the Nyquist limit of a one-second sample,
    /// however wide the screen or however short the window.
    #[test]
    fn two_seconds_is_the_floor_under_the_floor() {
        assert!(period_floor(4, 8) >= 2.0);
        assert!(layer_floor(2, 2, 4) >= 2.0);
    }

    /// The panel says the gap and what it is worth, in that order. The
    /// arithmetic behind the percentage is pinned in `analysis`, which is
    /// where it now lives so that the terminal and the exported page cannot
    /// disagree with this window about it.
    #[test]
    fn the_panel_prints_the_gap_and_what_it_is_worth() {
        assert_eq!(phase_note(0.5, 2), "\u{b7} interleave 0.50s (100%)");
        // Beside the serial: the tube's newest second, in counts; and for a
        // tube that is not answering, the same width of nothing.
        assert_eq!(reading(Some(2)), " \u{b7}     2 CPS");
        assert_eq!(reading(Some(0)), " \u{b7}     0 CPS");
        assert_eq!(reading(None), " ".repeat(12));
        assert_eq!(reading(None).chars().count(), reading(Some(0)).chars().count());
        assert_eq!(
            readings_line(&[Some(2), None, Some(0)], 3),
            "A \u{b7}     2 CPS  B              C \u{b7}     0 CPS"
        );
        assert_eq!(phase_note(0.24, 9), "\u{b7} interleave 0.24s (46%)");
    }

    /// The counts give up room to the trail when there is one, and neither
    /// chart is drawn on the other.
    #[test]
    fn the_canvas_is_cut_into_charts_that_do_not_overlap() {
        for trail in [false, true] {
            for (height, cluster) in [(604.0f32, 185.0f32), (460.0, 170.0), (1000.0, 240.0)] {
                let r = regions(height, cluster, trail);
                assert_eq!(r.cascade.0, cluster);
                assert!(r.cascade.0 + r.cascade.1 < r.spectrum.0);
                assert!(r.spectrum.0 + r.spectrum.1 <= height);
            }
        }
        let (plain, with) = (regions(600.0, 180.0, false), regions(600.0, 180.0, true));
        assert!((plain.cascade.1 - 416.0 * CASCADE_SHARE).abs() < 1e-3);
        assert!((with.cascade.1 - 416.0 * CASCADE_WITH_TRAIL).abs() < 1e-3);
        assert!(with.spectrum.1 > plain.spectrum.1);
        // Forty-eight rows, four pixels apart, fit under the counts of the
        // commonest window there is.
        assert!(regions(604.0, 185.0, true).spectrum.1 >= FALL_DEPTH as f32 * TRAIL_STEP);
    }

    /// The pointer is over a bar, and the bar is one of a tier.
    #[test]
    fn the_pointer_is_over_one_bar_of_one_tier() {
        let tier = |columns: usize, seconds: f64| Tier {
            columns,
            seconds,
            values: vec![Some(1.0); columns],
        };
        let strip = vec![tier(10, 4.0), tier(10, 2.0), tier(12, 1.0), tier(8, 0.5)];
        // Forty bars in 400 pixels: ten pixels a bar.
        assert_eq!(bar_at(&strip, 0.0, 400.0), Some((0, 0)));
        assert_eq!(bar_at(&strip, 99.9, 400.0), Some((0, 9)));
        assert_eq!(bar_at(&strip, 100.0, 400.0), Some((1, 0)));
        assert_eq!(bar_at(&strip, 205.0, 400.0), Some((2, 0)));
        assert_eq!(bar_at(&strip, 399.9, 400.0), Some((3, 7)));
        assert_eq!(bar_at(&strip, 400.0, 400.0), None);
        assert_eq!(bar_at(&strip, -1.0, 400.0), None);
        assert_eq!(bar_at(&[], 10.0, 400.0), None);
    }

    /// What is said of the bar under the pointer: when, how long, and the
    /// two rates.
    #[test]
    fn a_bar_under_the_pointer_is_said_in_words() {
        let strip = vec![
            Tier { columns: 2, seconds: 2.0, values: vec![Some(0.5), Some(1.5)] },
            Tier { columns: 2, seconds: 1.0, values: vec![Some(1.0), None] },
            Tier { columns: 2, seconds: 0.5, values: vec![Some(3.0), None] },
        ];
        // The seconds end at 1000: the two-second bars are 994..996 and
        // 996..998, which is whatever o'clock the machine says it is.
        let when = clock::format(996.0, "%H:%M:%S");
        let said = hover_words(&strip, 1000, 0, (0, 1)).unwrap();
        // 1.5 a second is 90 CPM; with its neighbours, by the seconds each
        // covers, 0.5*2 + 1.5*2 + 1.0*1 counts in five seconds is 60.
        assert_eq!(said, format!("{} \u{b7} 2s \u{b7} trend 60.0 CPM \u{b7} bar 90.0 CPM", when));
        // A strip cut in samples says the time by what it is told the
        // first of them was.
        let later = hover_words(&strip, 1000, 3600, (0, 1)).unwrap();
        assert!(later.starts_with(&clock::format(4596.0, "%H:%M:%S")), "{}", later);
        // Nothing measured is said, and is not a nought.
        assert!(hover_words(&strip, 1000, 0, (1, 1)).unwrap().ends_with("bar nothing measured"));
        // The interleave is a reading, and has no time of its own.
        assert_eq!(hover_words(&strip, 1000, 0, (2, 0)).unwrap(), "one tube's second \u{b7} 3 counts");
        assert_eq!(hover_words(&strip, 1000, 0, (5, 0)), None);
    }

    /// A trace is coloured by how loud it is: green for the floor, hot over
    /// the luck line, and the hotter the wider.
    #[test]
    fn the_pen_is_hotter_and_wider_for_what_is_louder() {
        let luck = 7.59f32;
        assert_eq!(shade_of(0.0, luck), 0);
        assert_eq!(shade_of(1.0, luck), 4);
        assert_eq!(shade_of(luck, luck), 13);
        assert_eq!(shade_of(1.5 * luck, luck), SHADES - 1);
        assert_eq!(shade_of(2000.0, luck), SHADES - 1);
        let mut last = 0;
        for k in 0..=120 {
            let s = shade_of(k as f32 / 10.0, luck);
            assert!(s >= last);
            last = s;
        }
        // Under the luck line the pen is green: more green in it than red
        // or blue. At the line it is yellow, and over it, it is hot.
        for shade in 1..shade_of(luck, luck) - 1 {
            let c = heat(shade);
            assert!(c.g > c.r && c.g > c.b, "shade {} is {:?}", shade, c);
        }
        let line = heat(shade_of(luck, luck));
        assert!(line.r > 0.9 && line.g > 0.6 && line.b < 0.4, "{:?}", line);
        let hot = heat(SHADES - 2);
        assert!(hot.r > 0.9 && hot.g < 0.5, "{:?}", hot);
        let white = heat(SHADES - 1);
        assert!(white.r > 0.9 && white.g > 0.9 && white.b > 0.85, "{:?}", white);
        assert_eq!(heat(SHADES + 5), white);
        // No two shades are the same ink, and the pen only widens.
        for shade in 1..SHADES {
            assert_ne!(heat(shade), heat(shade - 1));
            assert!(pen(shade) > pen(shade - 1));
        }
        assert!((pen(0) - 0.7).abs() < 1e-6 && (pen(SHADES - 1) - 3.0).abs() < 1e-6);
        // The ground is the panel's, half way to black, and is not seen
        // through.
        let g = shaded(Color { r: 0.98, g: 0.88, b: 0.66, a: 1.0 });
        assert_eq!((g.r, g.g, g.b, g.a), (0.49, 0.44, 0.33, 1.0));
    }

    /// The axis is cut where the windows are, and the long periods have the
    /// little room they need.
    #[test]
    fn the_long_periods_are_compressed_and_the_axis_has_no_gaps() {
        // Every window answered: nine hours to one, one to five minutes,
        // and five minutes down.
        let all = stretches(7.0, 32768);
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].long, all[0].short), (32768.0, 4096.0));
        assert_eq!((all[1].long, all[1].short), (4096.0, 300.0));
        assert_eq!((all[2].long, all[2].short), (300.0, 7.0));
        assert_eq!(all[0].left, 0.0);
        assert_eq!(all[2].right, 1.0);
        assert!(all.windows(2).all(|p| p[0].right == p[1].left));
        assert!((all[2].right - all[2].left - 0.76).abs() < 1e-6);
        // Until the second has, the first is a stretch of its own.
        let first = stretches(4.0, 512);
        assert_eq!(first.len(), 2);
        assert_eq!((first[0].long, first[0].short), (512.0, 300.0));
        assert!((first[0].right - STRETCH_FIRST).abs() < 1e-6);
        let two = stretches(6.0, 4096);
        assert_eq!(two.len(), 2);
        assert_eq!((two[0].long, two[0].short), (4096.0, 300.0));

        // The ends of the axis are the ends of the panel, a cut is where
        // two stretches meet, and what is off the axis has no place.
        assert_eq!(place(&all, 32768.0), Some(0.0));
        assert_eq!(place(&all, 7.0), Some(1.0));
        assert_eq!(place(&all, 4096.0), Some(STRETCH_LONG));
        assert!((place(&all, 300.0).unwrap() - (STRETCH_LONG + STRETCH_MIDDLE)).abs() < 1e-6);
        assert_eq!(place(&all, 40000.0), None);
        assert_eq!(place(&all, 6.9), None);
        // The longer is the further left, all the way across.
        let mut last = -1.0f32;
        let mut period = 32768.0f64;
        while period >= 7.0 {
            let x = place(&all, period).unwrap();
            assert!(x >= last, "{} s is left of a longer period", period);
            last = x;
            period /= 1.07;
        }
        // Half way between two ends, by the logarithm: the period whose
        // square is their product.
        let mid = place(&all, (300.0f64 * 7.0).sqrt()).unwrap();
        assert!((mid - (0.24 + 0.38)).abs() < 1e-4, "{}", mid);
    }

    /// The period axis ends at the longest window that has a spectrum, and
    /// not at one that is nine hours from its first.
    #[test]
    fn the_axis_reaches_as_far_as_has_been_measured() {
        let layer = |window, n| Layer { window, rel: vec![1.0; n], luck: 1.0 };
        assert_eq!(ladder_ends(&[layer(512, 0), layer(4096, 0), layer(32768, 0)]), (512, 512));
        assert_eq!(ladder_ends(&[layer(512, 9), layer(4096, 0), layer(32768, 0)]), (512, 512));
        assert_eq!(ladder_ends(&[layer(512, 9), layer(4096, 9), layer(32768, 0)]), (512, 4096));
        assert_eq!(ladder_ends(&[layer(512, 9), layer(4096, 9), layer(32768, 9)]), (512, 32768));
    }

    /// Half a screen beside another window is 626 wide, and the key is one
    /// line in it: the countdown and the date are what give way.
    #[test]
    fn the_key_keeps_its_line_and_the_rest_gives_way() {
        let wide = foot_for(1258.0, Some(64), 12);
        assert_eq!((wide.stamp, wide.countdown, wide.grouped), ("%Y-%m-%d %H:%M:%S", true, true));
        let half = foot_for(626.0 - 22.0, Some(64), 12);
        assert_eq!((half.stamp, half.countdown, half.grouped), ("%H:%M:%S", false, true));
        let narrow = foot_for(500.0, Some(64), 12);
        assert_eq!((narrow.stamp, narrow.countdown, narrow.grouped), ("", false, true));
        let quarter = foot_for(373.0 - 22.0, Some(64), 12);
        assert_eq!((quarter.stamp, quarter.countdown, quarter.grouped), ("", false, false));
        // Before the first key there is nothing to make room for.
        let none = foot_for(373.0 - 22.0, None, 21);
        assert!(none.countdown && !none.stamp.is_empty());
        assert_eq!(phase_note(0.0, 2), "\u{b7} interleave 0.00s (0%)");
    }

    /// The dial band grows by a row of numbers for every five tubes, so nine
    /// counters cannot push their readings onto the cascade's captions.
    #[test]
    fn the_dial_band_makes_room_for_the_readings_beside_it() {
        assert_eq!(cluster_h(1, 1.0), CLUSTER_H, "one tube needs no grid at all");
        assert!(cluster_h(9, 1.0) > cluster_h(2, 1.0));
        assert_eq!(cluster_h(9, 1.0), CLUSTER_H + 24.0, "nine is two rows of five");
        // The faces grow with the window; the rows of per-tube readings are
        // text beside them and do not.
        assert_eq!(cluster_h(9, 2.0), CLUSTER_H * 2.0 + 24.0);
    }

    /// THE READING DOES NOT WAIT FOR THE SECOND AFTER IT. This used to close
    /// a second only when the FIRST SAMPLE OF THE NEXT ONE arrived, so the
    /// needle was always showing a second that had already finished -- and
    /// with several tubes it was whichever tube happened to tick over first
    /// that ended the wait, which is a delay that grows with the rig.
    #[test]
    fn a_second_is_readable_as_soon_as_it_has_gone_by() {
        let mut r = Recent::default();
        // Two tubes, interleaved half a second apart, five counts each.
        r.push(100.0, 5);
        r.push(100.5, 5);
        // The moment that second is behind us -- no sample from the next one
        // has arrived, and none is needed.
        assert_eq!(r.rate(101.0, 1.0), Some(300.0), "five a second is 300 CPM");
    }

    /// THE DIVISOR IS THE SAMPLES, NOT THE SPAN TIMES THE TUBES. A tube that
    /// has stopped answering must not read as the room having gone quiet:
    /// what is left is fewer measurements of the same number, not a lower
    /// number.
    #[test]
    fn a_tube_dropping_out_is_not_the_room_going_quiet() {
        let mut both = Recent::default();
        let mut alone = Recent::default();
        for i in 0..3 {
            let t = 100.0 + i as f64;
            both.push(t, 5);
            both.push(t + 0.5, 5);
            alone.push(t, 5);
        }
        // Same room, one tube or two: the same answer, from half the
        // evidence. Dividing by span * tubes would have called the second one
        // 150 CPM -- the room halved, because an instrument was unplugged.
        assert_eq!(both.rate(103.0, NEEDLE_SPAN), Some(300.0));
        assert_eq!(alone.rate(103.0, NEEDLE_SPAN), Some(300.0));
    }

    /// ARRIVAL ORDER IS NOT QUITE TIME ORDER. The tubes are read on separate
    /// threads, so a sample stamped a little earlier can be delivered a
    /// little later. Scanning back to the first sample outside the window
    /// would stop at the inversion and throw away everything before it --
    /// which, for a one-second window holding two samples, is the reading.
    #[test]
    fn a_sample_delivered_out_of_order_is_still_in_its_window() {
        let mut r = Recent::default();
        // Tube B's 9.95 lands after tube A's 10.00, as it can.
        r.push(10.00, 4);
        r.push(9.95, 6);
        // Both are inside the second ending at 10.4, and both count.
        assert_eq!(r.rate(10.4, 1.0), Some(300.0), "5 a second across two tubes");
        // And the window's edge is still the time, not the position: half a
        // second back from 10.4 excludes the 9.95 and keeps the 10.00,
        // whatever order they arrived in.
        assert_eq!(r.rate(10.4, 0.45), Some(240.0), "only the 10.00 sample");
        // The newest is the newest, whichever arrived last.
        assert_eq!(r.newest(), Some(10.00));
    }

    /// Nothing heard in the window is None, which the dial holds, and never
    /// zero, which the dial would draw as a counter reading nothing.
    #[test]
    fn a_window_with_no_arrivals_in_it_says_so() {
        let mut r = Recent::default();
        assert_eq!(r.rate(100.0, 1.0), None, "before anything at all");
        r.push(100.0, 7);
        assert_eq!(r.rate(100.5, 1.0), Some(420.0));
        // Long after, with the sample outside the window.
        assert_eq!(r.rate(120.0, 1.0), None);
        // And the half-minute pointer still has it.
        assert_eq!(r.rate(120.0, POINTER_SPAN), Some(420.0));
    }

    /// THE NEEDLE LEANS BOTH WAYS, and further one way than the other: out
    /// fast so a source passing under the tube is not smoothed away, back
    /// slower so one Poisson lump does not read as a spike.
    #[test]
    fn the_needle_moves_out_faster_than_it_settles_back() {
        // It starts where the reading is. Attaching to a service that has
        // been up since breakfast must not sweep the needle off the stop.
        let mut n = Ballistic::new(0.25, 0.9);
        assert_eq!(n.at(), None, "a needle that has read nothing says so");
        assert_eq!(n.push(200.0, 0.08), 200.0);
        assert_eq!(n.at(), Some(200.0));

        // A step up, and the same step back down, over the same time.
        let mut up = Ballistic::new(0.25, 0.9);
        up.push(100.0, 0.08);
        let mut down = Ballistic::new(0.25, 0.9);
        down.push(200.0, 0.08);
        let rose = up.push(200.0, 0.25) - 100.0;
        let fell = 200.0 - down.push(100.0, 0.25);
        assert!(rose > fell * 2.0, "rose {} fell {}", rose, fell);

        // It arrives, rather than creeping forever: a needle that never
        // reaches the reading is a needle that cannot be read off.
        let mut n = Ballistic::new(0.25, 0.9);
        n.push(0.0, 0.08);
        for _ in 0..200 {
            n.push(120.0, 0.08);
        }
        assert!((n.at().unwrap() - 120.0).abs() < 0.01, "{:?}", n.at());
    }

    /// A STEADY READING SENDS NOTHING. Twelve messages a second is twelve
    /// full re-renders of the panel on the software renderer this usually
    /// runs on, and the ordinary case is a needle that has settled.
    #[test]
    fn the_meters_only_travel_when_the_needle_has_moved() {
        let settled = Meters { needle: Some(34.0), live: Some(30.0), ..Meters::default() };
        assert!(!settled.worth_sending(&settled));
        let nudged = Meters { needle: Some(34.02), ..settled };
        assert!(!nudged.worth_sending(&settled), "a hundredth of a CPM is not a frame");
        let moved = Meters { needle: Some(35.0), ..settled };
        assert!(moved.worth_sending(&settled));
        // A reading appearing or going away is always worth drawing.
        let gone = Meters { needle: None, ..settled };
        assert!(gone.worth_sending(&settled));
        // So is the range, which moves in steps of its own.
        let ranged = Meters { range30: Some((10.0, 90.0)), ..settled };
        assert!(ranged.worth_sending(&settled));
    }

    /// THE CLUSTER GROWS INTO A WIDE WINDOW AND NEVER SHRINKS BELOW ITS
    /// MINIMUM. The small-window layout is the one measured against a
    /// 560x400 tile, and nothing about making a maximised window look right
    /// may make that worse.
    #[test]
    fn the_dials_grow_with_the_glass_and_never_below_their_minimum() {
        let at = |w: f32, h: f32| dial_scale(iced::Size::new(w, h), 2);
        // The tile this was laid out for: unchanged, exactly 1.
        assert_eq!(at(560.0, 400.0), 1.0);
        // The default window is not a tile, it is a window somebody opened,
        // and it has room to spare in both directions.
        let default = at(760.0, 900.0);
        assert!(default > 1.0 && default <= DIAL_MAX_SCALE, "{}", default);
        // A maximised window on this desk has room, and uses it.
        assert!(at(1262.0, 756.0) > 1.3, "{}", at(1262.0, 756.0));
        // But never past the point where a dial stops being an instrument.
        assert_eq!(at(6000.0, 4000.0), DIAL_MAX_SCALE);
        // A window that is wide and SHORT is still short: the charts do not
        // give up their height to a pair of dials.
        assert_eq!(at(3000.0, 400.0), 1.0, "height is a budget too");
        // And one that is tall and narrow is still narrow.
        assert_eq!(at(560.0, 4000.0), 1.0);
    }

    /// THE SCALE DOES NOT MOVE, WHICH IS THE WHOLE FIX. It used to be chosen
    /// from a list by a function of the current reading with no memory, so a
    /// value sitting near a boundary flipped the entire face back and forth
    /// several times a minute. `dial_fraction` now takes one argument, and
    /// there is no second one left to change.
    #[test]
    fn the_decades_land_where_the_face_is_marked() {
        let at = |v: f64| (dial_fraction(v) * 1000.0).round() / 1000.0;
        // Three decades: 3, 30, 300, 3000 at nought, a third, two thirds, full.
        assert_eq!(at(3.0), 0.0);
        assert_eq!(at(30.0), 0.333);
        assert_eq!(at(300.0), 0.667);
        assert_eq!(at(3000.0), 1.0);
        // A reading either side of a band floor moves the needle a little and
        // moves nothing else: there is no range left to re-pick.
        let (a, b) = (dial_fraction(239.0), dial_fraction(241.0));
        assert!((a - b).abs() < 0.01, "{} vs {}", a, b);
    }

    /// A needle never runs past either end, whatever it is handed.
    #[test]
    fn a_needle_stays_on_its_face() {
        assert_eq!(dial_fraction(0.0), 0.0, "nothing at all pegs at the bottom");
        assert_eq!(dial_fraction(-5.0), 0.0);
        assert_eq!(dial_fraction(1.0), 0.0, "under the floor is the floor");
        assert_eq!(dial_fraction(99999.0), 1.0, "a pegged needle stops");
        // Monotonic across the whole span, which is what makes an angle
        // comparable to another angle.
        let mut last = -1.0;
        for v in [3.0, 10.0, 30.0, 120.0, 240.0, 600.0, 1500.0, 3000.0] {
            let f = dial_fraction(v);
            assert!(f > last, "{} did not advance the needle", v);
            last = f;
        }
    }

    /// THE TICKS AND THE COLOURS ARE THE SAME STATEMENT TWICE. Every band
    /// floor the panel names is a mark on the face, and every one of them is
    /// on the face rather than off the end of it.
    #[test]
    fn every_named_band_has_a_place_on_the_scale() {
        for b in Band::all() {
            let f = dial_fraction(b.floor());
            assert!(
                (0.0..=1.0).contains(&f),
                "{} at {} is off the face",
                b.name(),
                b.floor()
            );
        }
        // Attenuated is the bottom stop, deadly is well up the face and not
        // against the top -- there is room to see a source climb past it.
        assert_eq!(dial_fraction(Band::Attenuated.floor()), 0.0);
        let deadly = dial_fraction(Band::Deadly.floor());
        assert!((0.6..0.9).contains(&deadly), "deadly sits at {}", deadly);
    }
}
