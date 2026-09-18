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
use std::time::{Duration, Instant};

use iced::widget::{canvas, column, container, row, stack, text, Space};
use iced::{Color, Element, Fill, Font, Length, Rectangle, Renderer, Subscription, Theme};
use radbeeper::analysis::{
    bar_seconds, level, span_words, tiers_with, Level, Spectrum, Tier, Windows, TIERS,
};
use radbeeper::broker::{self, Client, CounterId, Event};
use radbeeper::{clock, entropy, log};

/// A NAMED MONOSPACE, NOT `Font::MONOSPACE`. The generic family resolves
/// through fontconfig to whatever the machine calls "monospace", and on a
/// desktop with a big font collection that came out as a DOS codepage bitmap
/// face: no lowercase, no solidus, no hyphen. The port line read
/// "<box>DEV<box>TTYUSB0" and the dose read "USV<box>H". DejaVu Sans Mono is
/// on every Linux with fontconfig and covers what this window draws.
const MONO: Font = Font::with_name("DejaVu Sans Mono");

/// Samples kept for the cascade strip.
const STRIP_KEEP: usize = 8192;

/// Rows of log kept under everything else.
const ROWS: usize = 6;

/// Columns the cascade is cut into, whatever the window's width.
///
/// Fixed, so the captions above the strip -- which are text WIDGETS, laid out
/// by the same engine as everything else -- can be given the tiers' widths as
/// fill portions and land over the right bars. The canvas stretches these
/// columns to the width it is handed.
const STRIP_COLS: usize = 240;

/// How tall each band of the instrument panel is inside the one canvas.
///
/// THE WHOLE PANEL IS ONE CANVAS, dials and charts together, because only the
/// last canvas widget in a view is drawn on this renderer. Everything with
/// letters in it is a widget stacked over the top -- see `view`.
/// The dials take a fixed band at the top; the two charts share whatever the
/// window has left, three parts to two. A fixed height for those would leave a
/// band of empty grey under the table on a tall window and clip them on a
/// short one.
const CLUSTER_H: f32 = 132.0;
/// The cascade's share of the space under the dials.
const CASCADE_SHARE: f32 = 0.62;

/// One dial: the face, and the space it is given.
const DIAL_R: f32 = 56.0;
const DIAL_W: f32 = 126.0;

/// Full-scale marks a dial will choose between, smallest first.
///
/// A GAUGE WITH A MOVING SCALE IS NOT A GAUGE. The needle has to mean the same
/// thing minute to minute, so the range is picked from this short list and
/// only ever moves up a step -- background sits in the low quarter of the
/// 300 mark and stays there, and a source that pegs it moves it once.
const RANGES: [f64; 7] = [60.0, 120.0, 300.0, 600.0, 1200.0, 3000.0, 6000.0];

/// The window the big number is an average over, when it has one.
const HEADLINE: f64 = 30.0;

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
fn ladder_ends(layers: &[Layer]) -> (usize, usize) {
    let shortest = layers.iter().map(|l| l.window).min().unwrap_or(2);
    let longest = layers.iter().map(|l| l.window).max().unwrap_or(2);
    (shortest, longest)
}

/// Where the socket is, when it is not where it usually is.
///
/// A ONCE-SET GLOBAL, because the subscription is built from a plain function
/// pointer with nowhere to hang a captured path. One flag, read at startup,
/// never written again.
static LOGS: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn logs_dir() -> std::path::PathBuf {
    LOGS.get().cloned().unwrap_or_else(log::state_dir)
}

fn main() -> iced::Result {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--logs" => {
                if let Some(d) = args.next() {
                    let _ = LOGS.set(std::path::PathBuf::from(d));
                }
            }
            "-h" | "--help" => {
                println!("radbeeper-gui -- a window onto the counters");
                println!();
                println!("  --logs DIR    where the serving radbeeper keeps its socket");
                println!();
                println!("It opens no serial port. Start `radbeeper service` first, or");
                println!("`radbeeper watch`, and this attaches to whichever is serving.");
                return Ok(());
            }
            _ => {}
        }
    }
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .theme(App::theme)
        .window_size((760.0, 900.0))
        .run()
}

// ---------------------------------------------------------------- messages ---

/// One spectrum of the overlay, as the interface needs it.
#[derive(Debug, Clone)]
struct Layer {
    window: usize,
    rel: Vec<f64>,
    luck: f64,
    runs: u32,
    /// Seconds until this one has anything to say, 0 once it has.
    wait: usize,
    /// Its loudest bin, and the period that bin stands for.
    top: f64,
    period: f64,
}

/// Everything the interface knows, computed by the feed thread.
#[derive(Debug, Clone)]
struct Snapshot {
    counters: Vec<CounterId>,
    /// span, the mean CPM across the tubes, and its one-sigma.
    averages: Vec<(f64, Option<f64>, Option<f64>)>,
    /// Each tube on its own, over the headline window.
    per: Vec<Option<f64>>,
    elapsed: f64,
    headline: Option<f64>,
    headline_sigma: Option<f64>,
    now: u32,
    total: u64,
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
    random: Option<(String, String, bool)>,
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
    Adrift(String),
}

// ------------------------------------------------------------------- state ---

struct App {
    shot: Option<Snapshot>,
    adrift: String,
    cpm_per_usvh: f64,
}

impl App {
    fn new() -> (App, iced::Task<Message>) {
        (
            App { shot: None, adrift: "looking for the counters...".into(), cpm_per_usvh: 153.8 },
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
            Message::Adrift(why) => {
                self.shot = None;
                self.adrift = why;
            }
        }
    }

    /// A NAMED FUNCTION, NOT A CLOSURE. `|_| Theme::CatppuccinMocha` looks
    /// like the obvious spelling and does not compile: the builder wants a
    /// function that works for any lifetime of the borrow, and inference will
    /// not generalise a closure that far.
    fn theme(&self) -> Theme {
        Theme::CatppuccinMocha
    }

    fn subscription(&self) -> Subscription<Message> {
        // ONE SOURCE, AND NO TIMER BESIDE IT. A snapshot lands every second
        // while anything is attached, which is the same beat a clock tick
        // would have had -- and `iced::time::every` needs a tokio or smol
        // backend this crate deliberately does not carry.
        Subscription::run(feed)
    }

    fn view(&self) -> Element<'_, Message> {
        let Some(s) = self.shot.as_ref() else {
            return container(
                column![
                    text("no counter").size(30).color(DIM),
                    text(self.adrift.clone()).size(13).color(DIM),
                ]
                .spacing(6)
                .align_x(iced::Center),
            )
            .center(Fill)
            .into();
        };

        let tubes = s.tubes();
        let tint = s.headline.map(|c| colour(level(c))).unwrap_or(DIM);

        // ---- the counters, one tight line each -------------------------
        let mut who = column![].spacing(0);
        for (k, c) in s.counters.iter().enumerate() {
            who = who.push(
                mono(format!(
                    "{} {} \u{b7} {} \u{b7} {}",
                    tube_name(k),
                    c.path,
                    c.version,
                    c.serial_no
                ))
                .size(10)
                .color(if tubes > 1 && k == 1 { TUBE_B } else if tubes > 1 { TUBE_A } else { DIM }),
            );
        }

        // ---- the dials -------------------------------------------------
        //
        // ONE PER TUBE WHEN THERE ARE TWO, because the only question a second
        // instrument answers is whether it agrees with the first, and two
        // needles side by side answer it before any number is read. With one
        // counter the single dial shows the reading everything else does.
        let dials: Vec<(Option<f64>, f64)> = if tubes > 1 {
            s.per.iter().map(|v| (*v, dial_range(v.unwrap_or(0.0)))).collect()
        } else {
            vec![(s.headline, dial_range(s.headline.unwrap_or(0.0)))]
        };
        let dials_w = DIAL_W * dials.len() as f32;

        let chart = canvas(Chart {
            dials: dials.clone(),
            strip: s.strip.clone(),
            sources: s.sources.clone(),
            tubes,
            every: s.every,
            n: s.samples,
            layers: s.layers.clone(),
        })
        .width(Fill)
        .height(Fill);

        // The numerals that belong on the faces: the reading in the lower
        // half of each dial, and what it is under that, as a marine gauge
        // puts its digits on a black face.
        let mut faces = row![].spacing(0);
        for (k, (v, full)) in dials.iter().enumerate() {
            faces = faces.push(
                container(
                    column![
                        Space::new().height(56.0),
                        mono(match v {
                            Some(v) => format!("{:.0}", v),
                            None => "--".into(),
                        })
                        .size(19)
                        .color(v.map(|v| colour(level(v))).unwrap_or(DIM)),
                        mono(if tubes > 1 {
                            format!("{} \u{b7} {:.0}", tube_name(k), full)
                        } else {
                            format!("CPM \u{b7} {:.0}", full)
                        })
                        .size(9)
                        .color(FAINT),
                    ]
                    .spacing(0)
                    .align_x(iced::Center),
                )
                .width(Length::Fixed(DIAL_W))
                .align_x(iced::Center),
            );
        }

        // ---- the dense numeric block, beside the dials -----------------
        let mut windows = column![].spacing(0);
        for (span, avg, sd) in &s.averages {
            let label = mono(format!("{:>6}s", *span as i64)).size(10).color(DIM);
            let body: Element<Message> = match avg {
                Some(cpm) => row![
                    mono(format!("{:>8.1}", cpm)).size(10).color(colour(level(*cpm))),
                    mono(format!("{:>8.3}", cpm / self.cpm_per_usvh)).size(10).color(DIM),
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
                    .color(FAINT),
                ]
                .spacing(5)
                .into(),
                None => mono(format!(
                    "  filling {:>6}s",
                    (span - s.elapsed).max(0.0).round() as i64
                ))
                .size(10)
                .color(FAINT)
                .into(),
            };
            windows = windows.push(row![label, body].spacing(6));
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
                    .color(DIM),
                    mono(match s.headline {
                        Some(v) => format!("{:.3} uSv/h", v / self.cpm_per_usvh),
                        None => "-- uSv/h".into(),
                    })
                    .size(11)
                    .color(DIM),
                ]
                .spacing(0),
            ]
            .spacing(8),
            windows,
            mono(format!(
                "now {:<4} run {} in {}s{}",
                s.now,
                s.total,
                s.elapsed.round() as i64,
                match (tubes, s.phase) {
                    (1, _) => String::new(),
                    (n, Some(p)) => format!("  {} tubes {}", n, phase_note(p)),
                    (n, None) => format!("  {} tubes averaged", n),
                }
            ))
            .size(10)
            .color(FAINT),
        ]
        .spacing(1);

        // ---- the cascade captions, over the tiers they describe ---------
        let mut caps = row![].spacing(0);
        for (ti, t) in s.strip.iter().enumerate() {
            caps = caps.push(
                container(
                    mono(format!(
                        "{}{}s/bar \u{b7} {}",
                        if ti == 0 { "" } else { "F " },
                        bar_seconds(t.seconds),
                        span_words(t.columns as f64 * t.seconds)
                    ))
                    .size(10)
                    .color(DIM),
                )
                .width(Length::FillPortion(t.columns as u16)),
            );
        }

        // EVERYTHING WITH LETTERS IN IT GOES OVER THE TOP. The panel is one
        // canvas because only the last canvas in a view is drawn here, and a
        // canvas cannot contain text at all, so the readouts and the captions
        // are widgets in a stack above it -- laid out by the same engine as
        // the rest, at the same sizes, in the same font.
        let panel = stack![
            chart,
            column![
                container(row![faces, Space::new().width(6.0), numbers].spacing(0))
                    .height(Length::Fixed(CLUSTER_H)),
                caps,
                Space::new().height(Fill),
            ]
            .spacing(0),
        ];
        let _ = dials_w;

        // ---- the spectrum's axis and its verdict ------------------------
        let (shortest, longest) = ladder_ends(&s.layers);
        let axis = row![
            mono(span_words(longest as f64)).size(10).color(DIM),
            Space::new().width(Fill),
            mono("period \u{b7} log").size(10).color(FAINT),
            Space::new().width(Fill),
            mono(span_words(period_floor(shortest, longest))).size(10).color(DIM),
        ];
        let mut verdicts = column![].spacing(0);
        for (i, l) in s.layers.iter().enumerate() {
            verdicts = verdicts.push(
                mono(layer_verdict(l, shortest, longest))
                    .size(10)
                    .color(if l.runs == 0 { FAINT } else { LAYER[i] }),
            );
        }

        // ---- one line for the emission, one for the clock ---------------
        let random: Element<Message> = match &s.random {
            Some((hex, at, suspect)) => column![
                mono(entropy::group_hex(hex))
                    .size(11)
                    .color(if *suspect { WARN } else { CYAN }),
                mono(format!(
                    "{} bits at {} \u{b7} {}{}",
                    entropy::ENTROPY_BITS as i64,
                    at,
                    s.pool,
                    if *suspect { " \u{b7} SPECTRUM NOT FLAT, suspect" } else { "" }
                ))
                .size(10)
                .color(FAINT),
            ]
            .spacing(0)
            .into(),
            None => mono(format!("random \u{b7} {}", s.pool)).size(10).color(FAINT).into(),
        };

        let now = mono(clock::format(clock::now(), "%Y-%m-%d %H:%M:%S"))
            .size(10)
            .color(FAINT);

        let table: Element<Message> = if s.rows.is_empty() {
            Space::new().into()
        } else {
            let mut t = column![mono(cells(&s.columns)).size(9).color(FAINT)].spacing(0);
            for r in &s.rows {
                t = t.push(mono(cells(r)).size(9).color(DIM));
            }
            t.into()
        };

        container(
            column![
                who,
                panel,
                axis,
                verdicts,
                random,
                now,
                table,
            ]
            .spacing(2),
        )
        .padding(10)
        .into()
    }
}

fn mono(s: impl text::IntoFragment<'static>) -> text::Text<'static> {
    text(s).font(MONO)
}

/// A, B, C: short enough to sit beside a number without crowding it.
fn tube_name(k: usize) -> String {
    format!("{}", (b'A' + (k as u8 % 26)) as char)
}

/// What the measured interleave is worth.
///
/// TWO COUNTERS ONLY SHARPEN TIME IF THEY DISAGREE ABOUT WHEN A SECOND
/// STARTS. Each has its own clock and its own phase and neither can be
/// steered, so the offset is whatever it is -- half a second is a perfect
/// interleave and near zero is two tubes reporting together, which still
/// doubles the counts and buys the precision but adds no time resolution at
/// all. Measuring it is the only honest thing to do, since claiming "0.5s per
/// bar" when the two fire together would be a lie the display tells itself.
fn phase_note(gap: f64) -> String {
    let ideal = 0.5;
    let quality = 1.0 - (gap - ideal).abs() / ideal;
    format!("\u{b7} interleave {:.2}s ({:.0}%)", gap, (quality.max(0.0) * 100.0))
}

fn layer_verdict(l: &Layer, shortest: usize, longest: usize) -> String {
    if l.runs == 0 {
        return format!(
            "{:>14} filling, {} to go",
            format!(
                "{:>7}\u{2013}{}",
                span_words(layer_floor(l.window, shortest, longest)),
                span_words(l.window as f64)
            ),
            span_words(l.wait as f64)
        );
    }
    let band = format!(
        "{:>7}\u{2013}{}",
        span_words(layer_floor(l.window, shortest, longest)),
        span_words(l.window as f64)
    );
    if l.top < l.luck * 1.25 {
        format!(
            "{:>14} flat \u{b7} {} window{}",
            band,
            l.runs,
            if l.runs == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{:>14} peak at {} \u{b7} {:.1}x (luck {:.1}x)",
            band,
            span_words(l.period),
            l.top,
            l.luck
        )
    }
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
fn dial_fraction(value: f64, full: f64) -> f64 {
    (value / full).clamp(0.0, 1.0)
}

/// The smallest full scale on the list that holds `value` with room to read.
fn dial_range(value: f64) -> f64 {
    let want = value * 1.15;
    *RANGES.iter().find(|r| **r >= want).unwrap_or(RANGES.last().unwrap())
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
struct Chart {
    /// One reading per dial, with the full scale each has settled on.
    dials: Vec<(Option<f64>, f64)>,
    strip: Vec<Tier>,
    /// Which tube the newest samples came from, for the finest tier.
    sources: Vec<u8>,
    tubes: usize,
    every: i64,
    /// Absolute index of the newest sample, for the log-row ticks.
    n: i64,
    layers: Vec<Layer>,
}

impl canvas::Program<Message> for Chart {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if bounds.width < 40.0 {
            return vec![frame.into_geometry()];
        }
        self.cluster(&mut frame);
        self.cascade(&mut frame, bounds);
        self.spectrum(&mut frame, bounds);
        vec![frame.into_geometry()]
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

        for (k, (value, full)) in self.dials.iter().enumerate() {
            let cx = DIAL_W * (k as f32 + 0.5);
            let cy = CLUSTER_H * 0.5;
            let c = iced::Point::new(cx, cy);

            // The face, and the bezel around it.
            frame.fill(&Path::circle(c, DIAL_R), Color { a: 0.55, ..FACE });
            frame.stroke(
                &Path::circle(c, DIAL_R),
                Stroke::default().with_width(2.0).with_color(Color { a: 0.5, ..BEZEL }),
            );

            // The bands, as an arc just inside the bezel: calm, then raised,
            // then the red at the top of the scale.
            let band = |frame: &mut canvas::Frame, from: f64, to: f64, tint: Color| {
                let a0 = start + sweep * dial_fraction(from, *full) as f32;
                let a1 = start + sweep * dial_fraction(to, *full) as f32;
                if a1 <= a0 {
                    return;
                }
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: c,
                        radius: DIAL_R * 0.86,
                        start_angle: Radians(a0),
                        end_angle: Radians(a1),
                    });
                });
                frame.stroke(&arc, Stroke::default().with_width(5.0).with_color(tint));
            };
            band(frame, 0.0, radbeeper::analysis::LEVEL_RAISED, Color { a: 0.55, ..CALM });
            band(frame, radbeeper::analysis::LEVEL_RAISED,
                 radbeeper::analysis::LEVEL_HIGH, Color { a: 0.55, ..RAISED });
            band(frame, radbeeper::analysis::LEVEL_HIGH, *full, Color { a: 0.55, ..HIGH });

            // Ticks: eleven majors across the sweep, four minors between.
            for i in 0..=50 {
                let t = i as f32 / 50.0;
                let a = start + sweep * t;
                let major = i % 5 == 0;
                let r1 = DIAL_R * if major { 0.66 } else { 0.73 };
                let r2 = DIAL_R * 0.79;
                let p1 = iced::Point::new(cx + r1 * a.cos(), cy + r1 * a.sin());
                let p2 = iced::Point::new(cx + r2 * a.cos(), cy + r2 * a.sin());
                frame.stroke(
                    &Path::line(p1, p2),
                    Stroke::default()
                        .with_width(if major { 2.0 } else { 1.0 })
                        .with_color(Color { a: if major { 0.75 } else { 0.35 }, ..FG }),
                );
            }

            // The needle, and the hub it turns on.
            if let Some(v) = value {
                let a = start + sweep * dial_fraction(*v, *full) as f32;
                let tip = iced::Point::new(
                    cx + DIAL_R * 0.72 * a.cos(),
                    cy + DIAL_R * 0.72 * a.sin(),
                );
                // A counterweight the other side of the hub, as a real needle
                // has: it is what stops the dial looking like a clock hand.
                let tail = iced::Point::new(
                    cx - DIAL_R * 0.16 * a.cos(),
                    cy - DIAL_R * 0.16 * a.sin(),
                );
                frame.stroke(
                    &Path::line(tail, tip),
                    Stroke::default().with_width(2.5).with_color(colour(level(*v))),
                );
            }
            frame.fill(&Path::circle(c, 4.0), BEZEL);
            frame.fill(&Path::circle(c, 2.0), Color { a: 0.9, ..FACE });
        }
    }

    fn cascade(&self, frame: &mut canvas::Frame, bounds: Rectangle) {
        let total: usize = self.strip.iter().map(|t| t.columns).sum();
        if total == 0 {
            return;
        }
        let bar = bounds.width / total as f32;
        let peak = self
            .strip
            .iter()
            .flat_map(|t| t.values.iter().flatten())
            .cloned()
            .fold(0.0f64, f64::max)
            .max(1.0);
        let tick = 3.0f32;
        let top = CLUSTER_H;
        // WHATEVER IS LEFT, SPLIT THREE TO TWO. The panel grows with the
        // window rather than leaving a band of empty grey under the table,
        // and the cascade gets the larger share because it is the one people
        // watch second by second.
        let rest = (bounds.height - CLUSTER_H - 4.0).max(40.0);
        let height = (rest * CASCADE_SHARE - tick).max(1.0);
        let mut x0 = 0.0f32;

        for (ti, tier) in self.strip.iter().enumerate() {
            let w = tier.columns as f32 * bar;
            // A tier's own patch of background, a shade apart from its
            // neighbours, so the hand-over from one resolution to the next is
            // visible rather than something you have to be told about.
            if ti % 2 == 1 {
                frame.fill_rectangle(
                    iced::Point::new(x0, top + tick),
                    iced::Size::new(w, height),
                    Color { a: 0.05, ..FG },
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
                        Some(0) => TUBE_A,
                        Some(_) => TUBE_B,
                        None => DIM,
                    }
                } else {
                    // Coloured by the rate a whole minute at that height
                    // would be, so the strip and the number above it agree
                    // about what "raised" means.
                    colour(level(*v * 60.0))
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
                            DIM,
                        );
                    }
                }
            }
            x0 += w;
        }
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
    fn spectrum(&self, frame: &mut canvas::Frame, bounds: Rectangle) {
        let rest = (bounds.height - CLUSTER_H - 4.0).max(40.0);
        let top = CLUSTER_H + rest * CASCADE_SHARE + 4.0;
        let h = (bounds.height - top - 1.0).max(1.0);
        let live: Vec<&Layer> = self.layers.iter().filter(|l| !l.rel.is_empty()).collect();
        if live.is_empty() {
            return;
        }
        let (shortest, longest) = ladder_ends(&self.layers);
        let floor_at = period_floor(shortest, longest);
        let lo = floor_at.ln();
        let hi = (longest as f64).ln();
        let span = (hi - lo).max(1e-9);
        // LONG PERIODS ON THE LEFT, as the terminal monitor has always drawn
        // them and as the captions under this panel say. It ran the other way
        // for exactly as long as it took somebody to read the axis.
        let x_of = |period: f64| -> f32 {
            (((hi - period.max(floor_at).ln()) / span) as f32).clamp(0.0, 1.0)
                * bounds.width
        };
        // One scale for every layer, so a peak twice the height of another
        // really is twice as loud. Floored at a little above the luck line so
        // a flat spectrum does not fill the panel with noise.
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
            let y = top + h - (luck / peak) as f32 * h;
            frame.fill_rectangle(
                iced::Point::new(0.0, y),
                iced::Size::new(bounds.width, 1.0),
                Color { a: 0.30, ..DIM },
            );
        }

        for l in &live {
            let idx = self
                .layers
                .iter()
                .position(|x| x.window == l.window)
                .unwrap_or(0);
            let tint = LAYER[idx.min(LAYER.len() - 1)];
            // Bins are dense at the short-period end of a log axis, so fold
            // them onto columns and keep the LOUDEST in each: a single sharp
            // line is what is being looked for, and averaging it with its
            // quiet neighbours is how it disappears.
            // ONLY THE BAND THIS WINDOW CAN RESOLVE. Below its floor the
            // bins are closer together than a bar is wide, and folding them
            // would draw the largest of a crowd rather than a line.
            let floor = layer_floor(l.window, shortest, longest);
            let mut col: Vec<f64> = vec![0.0; bounds.width.max(1.0) as usize];
            for (i, v) in l.rel.iter().enumerate() {
                let period = l.window as f64 / (i + 1) as f64;
                if period < floor {
                    continue;
                }
                let x = x_of(period) as usize;
                if x < col.len() && *v > col[x] {
                    col[x] = *v;
                }
            }
            for (x, v) in col.iter().enumerate() {
                if *v <= 0.0 {
                    continue;
                }
                let bh = (*v / peak) as f32 * h;
                frame.fill_rectangle(
                    iced::Point::new(x as f32, top + h - bh),
                    iced::Size::new(1.0, bh),
                    Color { a: 0.55, ..tint },
                );
            }
        }
    }
}

// -------------------------------------------------------------------- feed ---

/// The mean rate across the tubes, from windows holding every tube's samples.
fn mean_of(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    w.average(span).map(|v| v / tubes.max(1) as f64)
}

/// One sigma on that mean, in CPM.
///
/// Arrivals are Poisson, so the whole of the uncertainty is the count behind
/// the number: N arrivals give a relative error of 1/sqrt(N), and the counts
/// behind a mean of `tubes` tubes over `span` seconds is `cpm * span * tubes /
/// 60`. THIS is what a second counter buys -- the same figure, known to within
/// a factor of root two better -- and printing it is the only way the benefit
/// is visible at all.
fn sigma_of(cpm: f64, span: f64, tubes: usize) -> Option<f64> {
    let n = cpm * span * tubes.max(1) as f64 / 60.0;
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
                let id = client.identity().clone();
                let tubes = id.len().max(1);
                let columns = log::columns(&log::header(&id.spans));
                // One set of windows per tube, and one across all of them.
                let mut each: Vec<Windows> =
                    (0..tubes).map(|_| Windows::new(&id.spans)).collect();
                let mut all = Windows::new(&id.spans);
                let mut ladder: Vec<Spectrum> =
                    LADDER.iter().map(|w| Spectrum::new(*w)).collect();
                let mut pool = entropy::Entropy::default();
                // The samples in ARRIVAL order, as rates, which is what the
                // cascade is cut from. One counter is one bar a second; two
                // is a bar every half second on average, because two tubes on
                // their own clocks interleave.
                let mut merged: VecDeque<f64> = VecDeque::with_capacity(STRIP_KEEP);
                let mut sources: VecDeque<u8> = VecDeque::with_capacity(STRIP_KEEP);
                let mut dropped = 0usize;
                let mut rows: VecDeque<Vec<String>> = VecDeque::with_capacity(ROWS);
                let mut random: Option<(String, String, bool)> = None;
                let mut now = 0u32;
                // Whole seconds, summed across the tubes, for the spectra: a
                // period is a property of the room and every tube is looking
                // at the same room, so adding them is simply more signal on
                // one time base.
                let mut sec_bin: Option<i64> = None;
                let mut sec_sum: u32 = 0;
                // For the interleave measurement: when the previous sample
                // landed, and which tube it came from.
                let mut last: Option<(usize, f64)> = None;
                let mut gaps = (0.0f64, 0u32);
                let mut replaying = true;
                let mut sent = Instant::now() - Duration::from_secs(1);

                // Ten seconds, where a sample is due every one: a gap that
                // long means the server has gone, not that a counter is quiet.
                while let Some(event) = client.next(Duration::from_secs(10)) {
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
                            if merged.len() == STRIP_KEEP {
                                merged.pop_front();
                                sources.pop_front();
                                dropped += 1;
                            }
                            merged.push_back(counts as f64);
                            sources.push_back(who as u8);
                            now = counts;
                            let this = when.floor() as i64;
                            match sec_bin {
                                Some(b) if b == this => sec_sum += counts,
                                Some(_) => {
                                    for r in ladder.iter_mut() {
                                        r.add(sec_sum);
                                    }
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
                        Event::Random { hex, at, suspect, .. } => {
                            random = Some((hex, at, suspect));
                            pool.reset();
                        }
                        Event::Row { row, .. } => {
                            if rows.len() == ROWS {
                                rows.pop_front();
                            }
                            rows.push_back(row.split('\t').map(str::to_string).collect());
                        }
                    }
                    if replaying || sent.elapsed() < Duration::from_millis(900) {
                        continue;
                    }
                    sent = Instant::now();
                    let shortest = LADDER.iter().copied().min().unwrap_or(2);
                    let longest = LADDER.iter().copied().max().unwrap_or(2);
                    let headline = mean_of(&all, HEADLINE, tubes)
                        .or_else(|| id.spans.last().and_then(|s| mean_of(&all, *s, tubes)));
                    let shot = Snapshot {
                        counters: id.counters.clone(),
                        averages: id
                            .spans
                            .iter()
                            .map(|s| {
                                let m = mean_of(&all, *s, tubes);
                                (*s, m, m.and_then(|v| sigma_of(v, *s, tubes)))
                            })
                            .collect(),
                        per: (0..tubes)
                            .map(|k| {
                                each[k].average(HEADLINE).or_else(|| {
                                    id.spans.last().and_then(|s| each[k].average(*s))
                                })
                            })
                            .collect(),
                        elapsed: all.elapsed(),
                        headline,
                        headline_sigma: headline
                            .and_then(|v| sigma_of(v, HEADLINE, tubes)),
                        now,
                        total: all.total,
                        phase: (gaps.1 > 0).then(|| gaps.0 / gaps.1 as f64),
                        strip: tiers_with(
                            &merged.iter().copied().collect::<Vec<f64>>(),
                            dropped,
                            STRIP_COLS,
                            LADDER[0] * tubes,
                            TIERS + if tubes > 1 { 1 } else { 0 },
                            1.0 / tubes as f64,
                        ),
                        sources: sources.iter().rev().take(STRIP_COLS).rev().copied().collect(),
                        samples: (dropped + merged.len()) as i64,
                        every: log::DEFAULT_LOG_EVERY.round() as i64,
                        layers: ladder
                            .iter()
                            .map(|r| {
                                let rel = r.relative();
                                // THE LOUDEST BIN THAT IS DRAWN, not the
                                // loudest there is. A peak the panel cannot
                                // show is a peak nobody can check, and a
                                // caption pointing at empty axis is worse
                                // than no caption.
                                let floor = layer_floor(r.window, shortest, longest);
                                let mut top = 0.0f64;
                                let mut at = 0usize;
                                for (i, v) in rel.iter().enumerate() {
                                    if r.period(i) >= floor && *v > top {
                                        top = *v;
                                        at = i;
                                    }
                                }
                                Layer {
                                    window: r.window,
                                    rel,
                                    luck: r.chance_max(),
                                    runs: r.runs,
                                    wait: r.wait(),
                                    top,
                                    period: r.period(at),
                                }
                            })
                            .collect(),
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

// ----------------------------------------------------------------- colours ---

const FG: Color = Color::from_rgb(0.80, 0.84, 0.92);
/// The dial face and its bezel: a black-faced instrument, lit from nowhere.
const FACE: Color = Color::from_rgb(0.04, 0.05, 0.08);
const BEZEL: Color = Color::from_rgb(0.62, 0.66, 0.74);
const DIM: Color = Color::from_rgb(0.62, 0.64, 0.72);
const FAINT: Color = Color::from_rgb(0.45, 0.47, 0.55);
const CALM: Color = Color::from_rgb(0.40, 0.85, 0.55);
const RAISED: Color = Color::from_rgb(0.98, 0.79, 0.35);
const HIGH: Color = Color::from_rgb(0.96, 0.40, 0.42);
const WARN: Color = Color::from_rgb(0.98, 0.79, 0.35);
const CYAN: Color = Color::from_rgb(0.45, 0.82, 0.92);
const TUBE_A: Color = Color::from_rgb(0.55, 0.80, 0.98);
const TUBE_B: Color = Color::from_rgb(0.85, 0.70, 0.98);

/// PRIMARIES, one per spectrum window, because the overlay is read by colour
/// and nothing else. Red is the long view, green the middle, blue the short.
const LAYER: [Color; 3] = [
    Color::from_rgb(0.40, 0.75, 1.00),
    Color::from_rgb(0.45, 0.95, 0.55),
    Color::from_rgb(1.00, 0.45, 0.45),
];

/// The same three bands everything else uses, so one counter does not look
/// calm in one place and raised in another.
fn colour(l: Level) -> Color {
    match l {
        Level::Calm => CALM,
        Level::Raised => RAISED,
        Level::High => HIGH,
    }
}

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

    /// A dial reads the same fraction of its sweep for the same fraction of
    /// its range, and never runs past either end.
    #[test]
    fn a_needle_stays_on_its_face() {
        assert_eq!(dial_fraction(0.0, 600.0), 0.0);
        assert_eq!(dial_fraction(300.0, 600.0), 0.5);
        assert_eq!(dial_fraction(600.0, 600.0), 1.0);
        assert_eq!(dial_fraction(9000.0, 600.0), 1.0, "a pegged needle stops");
        assert_eq!(dial_fraction(-5.0, 600.0), 0.0);
    }

    /// The range is picked from the list and leaves headroom, so background
    /// does not sit against the stop and a spike is still on the face.
    #[test]
    fn the_range_is_one_of_the_marks_and_holds_the_reading() {
        assert_eq!(dial_range(25.0), 60.0);
        assert_eq!(dial_range(55.0), 120.0, "55 needs more than the 60 mark");
        assert_eq!(dial_range(880.0), 1200.0);
        assert_eq!(dial_range(99999.0), 6000.0, "beyond the last mark it pegs");
    }
}
