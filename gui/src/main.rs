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
    band, bar_seconds, span_words, tiers_interleaved, tiers_with, Band, Spectrum,
    Tier, Windows, TIERS,
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
const CLUSTER_H: f32 = 132.0;

/// How tall the dial band actually is, given how many rows of per-tube
/// readings have to sit beside it.
///
/// THE BAND CANNOT BE A CONSTANT ONCE THE TUBE COUNT IS NOT. Nine counters is
/// two more rows of numbers than one counter, and a fixed band let them
/// overflow into the cascade's captions -- two pieces of text on top of each
/// other, which is the one thing a dense panel must never do.
fn cluster_h(tubes: usize) -> f32 {
    let rows = if tubes > 1 { tubes.div_ceil(5) } else { 0 };
    CLUSTER_H + rows as f32 * 12.0
}
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

/// The skin, chosen once at startup: `--theme`, or what the desktop is
/// wearing. Beside LOGS and for the same reason -- there is nowhere to hang a
/// captured value on a plain function pointer.
static SKIN: std::sync::OnceLock<Skin> = std::sync::OnceLock::new();

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
            "--theme" => {
                if let Some(t) = args.next() {
                    let _ = SKIN.set(Skin::named(&t));
                }
            }
            "-h" | "--help" => {
                println!("radbeeper-gui -- a window onto the counters");
                println!();
                println!("  --logs DIR    where the serving radbeeper keeps its socket");
                println!("  --theme T     dark, antiquity, or auto (the default:");
                println!("                whatever ~/.config/copal/current says)");
                println!();
                println!("It opens no serial port. Start `radbeeper service` first, or");
                println!("`radbeeper watch`, and this attaches to whichever is serving.");
                return Ok(());
            }
            _ => {}
        }
    }
    let _ = SKIN.set(Skin::detect());
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
    /// The combined reading at one-second resolution -- what the collecting
    /// meter's needle sits at, and what pushes its bugs about.
    live: Option<f64>,
    /// The half-minute and minute ranges that needle has been bouncing
    /// between, drifting inward.
    range30: Option<(f64, f64)>,
    range60: Option<(f64, f64)>,
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
    Adrift(String),
    /// The window changed size. See `App::view`: what gets dropped first.
    Resized(iced::Size),
}

// ------------------------------------------------------------------- state ---

struct App {
    skin: Skin,
    shot: Option<Snapshot>,
    adrift: String,
    cpm_per_usvh: f64,
    /// What the compositor has actually given us, which on a tiling desktop
    /// is whatever is left after every other window has had its share.
    size: iced::Size,
}

impl App {
    fn new() -> (App, iced::Task<Message>) {
        (
            App {
                skin: SKIN.get().cloned().unwrap_or_else(Skin::dark),
                shot: None,
                adrift: "looking for the counters...".into(),
                cpm_per_usvh: 153.8,
                size: iced::Size::new(760.0, 900.0),
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
            Message::Adrift(why) => {
                self.shot = None;
                self.adrift = why;
            }
            Message::Resized(size) => self.size = size,
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
        let tint = s.headline.map(|c| self.skin.colour(band(c))).unwrap_or(self.skin.dim);

        // ---- who is on the other end ------------------------------------
        //
        // ONE LINE WHATEVER THE COUNT. Nine counters listed one per line is
        // nine lines of a panel that is trying to be dense; the per-tube
        // readings below already carry the letters and the colours, so this
        // only has to say what they are and where.
        let firmwares: std::collections::BTreeSet<&str> =
            s.counters.iter().map(|c| c.version.as_str()).collect();
        let who: Element<Message> = if tubes <= 2 {
            let mut col = column![].spacing(0);
            for (k, c) in s.counters.iter().enumerate() {
                col = col.push(
                    mono(format!(
                        "{} {} \u{b7} {} \u{b7} {}",
                        tube_name(k), c.path, c.version, c.serial_no
                    ))
                    .size(10)
                    .color(if tubes > 1 { self.skin.tube(k) } else { self.skin.dim }),
                );
            }
            col.into()
        } else {
            mono(format!(
                "{} tubes \u{b7} {} \u{b7} {} \u{2026} {}",
                tubes,
                firmwares.into_iter().collect::<Vec<_>>().join(", "),
                s.counters.first().map(|c| c.path.as_str()).unwrap_or(""),
                s.counters.last().map(|c| c.path.as_str()).unwrap_or("")
            ))
            .size(10)
            .color(self.skin.dim)
            .into()
        };

        // ---- the dials --------------------------------------------------
        //
        // EVENS ON THE LEFT FACE, ODDS ON THE RIGHT. With one counter there
        // is one face and one needle; with two, a face each; with nine, five
        // needles on the left and four on the right. Needles on one face
        // share a scale, because the only question several tubes in one room
        // raise is whether they agree, and two angles can only be compared
        // when they mean the same thing.
        // ONE METER RAW, ONE METER COLLECTED. The left face carries a needle
        // per tube -- every input, unaveraged, in its own colour -- so a tube
        // that has wandered off is a needle that has wandered off. The right
        // face carries ONE needle, the whole-second collection at the
        // resolution the log is written in, and the ranges it has been
        // bouncing between. The first answers "do they agree?", the second
        // answers "what is the room doing?", and neither answers the other.
        //
        // They share a scale, because two dials that do not are two dials
        // that cannot be compared, which is the only reason to draw them side
        // by side.
        let top = s
            .per
            .iter()
            .filter_map(|v| *v)
            .chain(s.live)
            .chain(s.range60.map(|(_, hi)| hi))
            .fold(0.0f64, f64::max);
        let full = dial_range(top);
        let faces: Vec<Face> = vec![
            Face {
                needles: (0..tubes).map(|k| (k, s.per.get(k).copied().flatten())).collect(),
                full,
                range30: None,
                range60: None,
            },
            Face {
                needles: vec![(usize::MAX, s.live)],
                full,
                range30: s.range30,
                range60: s.range60,
            },
        ];

        let chart = canvas(Chart {
            skin: self.skin.clone(),
            cluster: cluster_h(tubes),
            peak: s.peak,
            dials: faces.clone(),
            strip: s.strip.clone(),
            sources: s.sources.clone(),
            tubes,
            every: s.every,
            n: s.samples,
            layers: s.layers.clone(),
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
            let caption: Element<Message> = if fi == 0 {
                let mut letters = row![].spacing(3);
                for (k, _) in face.needles.iter().take(10) {
                    letters = letters.push(
                        mono(tube_name(*k)).size(9).color(self.skin.tube(*k)),
                    );
                }
                letters.into()
            } else {
                mono(match (s.range30, s.range60) {
                    (Some((l, h)), _) => format!("{:.0}\u{2013}{:.0}", l, h),
                    _ => "range".into(),
                })
                .size(9)
                .color(Color { a: 0.85, ..self.skin.hud })
                .into()
            };
            dial_faces = dial_faces.push(
                container(
                    column![
                        Space::new().height(54.0),
                        mono(match mean {
                            Some(v) => format!("{:.0}", v),
                            None => "--".into(),
                        })
                        .size(19)
                        .color(mean.map(|v| self.skin.colour(band(v))).unwrap_or(self.skin.dim)),
                        caption,
                        mono(if fi == 0 { "raw".to_string() } else { "1s \u{b7} held".to_string() })
                            .size(9)
                            .color(self.skin.faint),
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
                match (tubes, s.phase) {
                    (1, _) => String::new(),
                    (n, Some(p)) => format!("  {} tubes {}", n, phase_note(p, n)),
                    (n, None) => format!("  {} tubes averaged", n),
                }
            ))
            .size(10)
            .color(self.skin.faint),
        ]
        .spacing(1);

        // ---- the cascade captions, over the tiers they describe ---------
        // A CAPTION HAS TO FIT ITS TIER. Four tiers across a window have room
        // for "F 4s/bar . 3m"; eight do not, and a caption that wraps lands on
        // the strip it is labelling. Past five tiers it says the one thing
        // that cannot be inferred -- how much time a bar holds -- and drops
        // the rest, which the tier widths already show.
        let terse = s.strip.len() > 5;
        let mut caps = row![].spacing(0);
        for (ti, t) in s.strip.iter().enumerate() {
            caps = caps.push(
                container(
                    mono(if terse {
                        format!("{}s", bar_seconds(t.seconds))
                    } else {
                        format!(
                            "{}{}s/bar \u{b7} {}",
                            if ti == 0 { "" } else { "F " },
                            bar_seconds(t.seconds),
                            span_words(t.columns as f64 * t.seconds)
                        )
                    })
                    .size(if terse { 9 } else { 10 })
                    .color(self.skin.dim),
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
                container(row![dial_faces, Space::new().width(6.0), numbers].spacing(0))
                    .height(Length::Fixed(cluster_h(tubes))),
                caps,
                Space::new().height(Fill),
            ]
            .spacing(0),
        ];

        // ---- the spectrum's axis and its verdict ------------------------
        let (shortest, longest) = ladder_ends(&s.layers);
        let axis = row![
            mono(span_words(longest as f64)).size(10).color(self.skin.dim),
            Space::new().width(Fill),
            mono("period \u{b7} log").size(10).color(self.skin.faint),
            Space::new().width(Fill),
            mono(span_words(period_floor(shortest, longest))).size(10).color(self.skin.dim),
        ];
        let mut verdicts = column![].spacing(0);
        for (i, l) in s.layers.iter().enumerate() {
            verdicts = verdicts.push(
                mono(layer_verdict(l, shortest, longest))
                    .size(10)
                    .color(if l.runs == 0 { self.skin.faint } else { self.skin.layers[i] }),
            );
        }

        // ---- one line for the emission, one for the clock ---------------
        let random: Element<Message> = match &s.random {
            Some((who, hex, at, suspect)) => column![
                row![
                    mono(format!("{} ", tube_name(*who)))
                        .size(11)
                        .color(self.skin.tube(*who)),
                    mono(entropy::group_hex(hex))
                        .size(11)
                        .color(if *suspect { self.skin.warn } else { self.skin.cyan }),
                ],
                mono(format!(
                    "{} bits from {} at {} \u{b7} {}{}",
                    entropy::ENTROPY_BITS as i64,
                    s.counters.get(*who).map(|c| c.serial_no.as_str()).unwrap_or("?"),
                    at,
                    s.pool,
                    if *suspect { " \u{b7} SPECTRUM NOT FLAT, suspect" } else { "" }
                ))
                .size(10)
                .color(self.skin.faint),
            ]
            .spacing(0)
            .into(),
            None => mono(format!("random \u{b7} {}", s.pool)).size(10).color(self.skin.faint).into(),
        };

        let now = mono(clock::format(clock::now(), "%Y-%m-%d %H:%M:%S"))
            .size(10)
            .color(self.skin.faint);

        // WHAT GOES FIRST WHEN THERE IS NO ROOM. A tiling compositor will
        // hand this window a quarter of a screen without asking, and
        // everything above was sized as though it would not: the charts are
        // Fill and everything else is fixed, so at 373 pixels the fixed
        // content took the lot and the cascade collapsed to nothing. The
        // charts ARE the instrument -- they are the last thing to go, not the
        // first. So the log table goes, then the per-layer verdicts, then the
        // axis, in that order, and what is left keeps its shape.
        let room = self.size.height;
        let (want_table, want_verdicts, want_axis) =
            (room >= 620.0, room >= 500.0, room >= 430.0);
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
                if want_verdicts { verdicts.into() } else { Element::from(Space::new()) },
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

/// What the measured interleave is worth, against what n tubes could manage.
///
/// TUBES ONLY SHARPEN TIME IF THEY DISAGREE ABOUT WHEN A SECOND STARTS. Each
/// has its own clock and its own phase and none can be steered, so the offset
/// is whatever it is. THE IDEAL IS 1/n OF A SECOND, not half of one: two tubes
/// perfectly interleaved are half a second apart, nine are a ninth. Measuring
/// it against a fixed half-second -- which this did until nine counters were
/// plugged in -- marks a perfect nine-way interleave down to 36%, and marks a
/// pair that fires together as better than it is.
///
/// Near the ideal is a full grid in time. Near zero is every tube reporting at
/// once, which still multiplies the counts and still buys the precision but
/// adds no resolution at all -- and claiming "1/9s per bar" in that case would
/// be a lie the display tells itself.
fn phase_note(gap: f64, tubes: usize) -> String {
    let ideal = 1.0 / tubes.max(2) as f64;
    // A RATIO, NOT A DISTANCE FROM IDEAL. The obvious form -- one minus the
    // relative error -- hits zero the moment the gap is twice the ideal and
    // goes negative after, so nine free-running tubes averaging 0.24s against
    // an ideal of 0.11s were reported as 0%: a rig that is in fact spreading
    // its samples out over most of the second, dismissed as doing nothing.
    // The smaller over the larger is scale-free, symmetric, and degrades the
    // way the thing it measures does.
    let quality = if gap > 0.0 { ideal.min(gap) / ideal.max(gap) } else { 0.0 };
    format!("\u{b7} interleave {:.2}s ({:.0}%)", gap, quality * 100.0)
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
/// A dial face: every needle on it, and the scale they share.
#[derive(Debug, Clone)]
struct Face {
    /// (tube index, its reading), or (usize::MAX, reading) for the collected
    /// one, which belongs to no single tube.
    needles: Vec<(usize, Option<f64>)>,
    full: f64,
    /// The half-minute and minute ranges, on the collecting meter only. The
    /// raw meter has none: a range needs one needle to be the range OF.
    range30: Option<(f64, f64)>,
    range60: Option<(f64, f64)>,
}

struct Chart {
    skin: Skin,
    /// The dial band's height, which depends on the tube count. See cluster_h.
    cluster: f32,
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

        for (k, face) in self.dials.iter().enumerate() {
            let full = face.full;
            let cx = DIAL_W * (k as f32 + 0.5);
            let cy = self.cluster * 0.5;
            let c = iced::Point::new(cx, cy);

            // The face, and the bezel around it.
            frame.fill(&Path::circle(c, DIAL_R), self.skin.face);
            frame.stroke(
                &Path::circle(c, DIAL_R),
                Stroke::default().with_width(2.0).with_color(Color { a: 0.5, ..self.skin.bezel }),
            );

            // An arc of the scale, at whatever radius, in whatever colour.
            let arc_at = |frame: &mut canvas::Frame, from: f64, to: f64,
                          radius: f32, width: f32, tint: Color| {
                let a0 = start + sweep * dial_fraction(from, full) as f32;
                let a1 = start + sweep * dial_fraction(to, full) as f32;
                if a1 <= a0 + 0.004 {
                    return;
                }
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: c,
                        radius: DIAL_R * radius,
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
                let to = bands.get(i + 1).map(|n| n.floor()).unwrap_or(full);
                arc_at(frame, b.floor(), to.min(full), 0.82, 5.0,
                       Color { a: 0.75, ..self.skin.face_colour(*b) });
            }

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
                        .with_color(Color { a: if major { 0.75 } else { 0.35 }, ..self.skin.fg }),
                );
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
                let a = start + sweep * dial_fraction(value, full) as f32;
                let (r0, r1) = (DIAL_R * (radius - 0.05), DIAL_R * (radius + 0.05));
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

            // A NEEDLE PER TUBE ON THIS self.skin.face, each in its own colour and each
            // a little shorter than the one before, so two tubes reading the
            // same number are two needles that can still be told apart
            // instead of one that has swallowed the other.
            let n = face.needles.len().max(1);
            for (j, (tube, value)) in face.needles.iter().enumerate() {
                let Some(v) = value else { continue };
                let a = start + sweep * dial_fraction(*v, full) as f32;
                let reach = DIAL_R * (0.74 - 0.05 * (j as f32).min(4.0));
                let tip = iced::Point::new(cx + reach * a.cos(), cy + reach * a.sin());
                // A counterweight the other side of the hub, as a real needle
                // has: it is what stops the dial looking like a clock hand.
                let tail = iced::Point::new(
                    cx - DIAL_R * 0.16 * a.cos(),
                    cy - DIAL_R * 0.16 * a.sin(),
                );
                frame.stroke(
                    &Path::line(tail, tip),
                    Stroke::default()
                        .with_width(if n > 3 { 1.8 } else { 2.5 })
                        .with_color(if *tube == usize::MAX {
                            self.skin.face_colour(band(*v))
                        } else {
                            self.skin.face_tube(*tube)
                        }),
                );
            }
            frame.fill(&Path::circle(c, 4.0), self.skin.bezel);
            frame.fill(&Path::circle(c, 2.0), Color { a: 0.9, ..self.skin.face });
        }
    }

    fn cascade(&self, frame: &mut canvas::Frame, bounds: Rectangle) {
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
        let top = self.cluster;
        // WHATEVER IS LEFT, SPLIT THREE TO TWO. The panel grows with the
        // window rather than leaving a band of empty grey under the table,
        // and the cascade gets the larger share because it is the one people
        // watch second by second.
        let rest = (bounds.height - self.cluster - 4.0).max(40.0);
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
                    self.skin.tier_wash(),
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
        let rest = (bounds.height - self.cluster - 4.0).max(40.0);
        let top = self.cluster + rest * CASCADE_SHARE + 4.0;
        // A MARGIN AT THE FOOT. The canvas fills its share and the readouts
        // sit under it, so a spectrum drawn to the last pixel puts its tallest
        // bars through the emission line on a squeezed window.
        let h = (bounds.height - top - 6.0).max(1.0);
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
                    Color { a: self.skin.layer_alpha, ..tint },
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
                let mut pool = entropy::Entropy::default();
                // The samples in ARRIVAL order, as rates, which is what the
                // cascade is cut from. One counter is one bar a second; two
                // is a bar every half second on average, because two tubes on
                // their own clocks interleave.
                let mut merged: VecDeque<f64> = VecDeque::with_capacity(STRIP_KEEP);
                let mut sources: VecDeque<u8> = VecDeque::with_capacity(STRIP_KEEP);
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
                // What the collecting meter shows, and where it has been.
                let mut live: Option<f64> = None;
                let mut d30 = Drift::new(30.0);
                let mut d60 = Drift::new(60.0);
                let mut peak_hold = 0.0f64;
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
                                Some(was) => {
                                    for r in ladder.iter_mut() {
                                        r.add(sec_sum);
                                    }
                                    // A SECOND HAS CLOSED, so the collecting
                                    // meter has a reading: the whole-second
                                    // sum across every tube, as a rate, which
                                    // is the mean the tubes agree on and not
                                    // their sum.
                                    let cpm = sec_sum as f64 * 60.0 / tubes as f64;
                                    let dt = ((this - was) as f64).clamp(1.0, 60.0);
                                    if !replaying {
                                        d30.push(cpm, dt);
                                        d60.push(cpm, dt);
                                    }
                                    live = Some(cpm);
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
                            if let Some(p) = present.get_mut(who) {
                                *p = true;
                            }
                            each[who] = Windows::new(&id.spans);
                        }
                        Event::Gone { who } => {
                            if let Some(p) = present.get_mut(who) {
                                *p = false;
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
                    if replaying || sent.elapsed() < Duration::from_millis(900) {
                        continue;
                    }
                    sent = Instant::now();
                    let series: Vec<f64> = merged.iter().copied().collect();
                    // Whole seconds, and one tier below them for the
                    // interleave; see analysis::tiers_interleaved.
                    let strip = if tubes > 1 {
                        tiers_interleaved(&series, dropped, STRIP_COLS, tubes)
                    } else {
                        tiers_with(&series, dropped, STRIP_COLS, LADDER[0], TIERS, 1.0)
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
                        // A TUBE THAT HAS STOPPED ANSWERING READS `--`, not
                        // its last number. A stale reading on a dial is worse
                        // than no reading: it is the instrument claiming to
                        // still be measuring.
                        per: (0..tubes)
                            .map(|k| {
                                if !present.get(k).copied().unwrap_or(false) {
                                    return None;
                                }
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
                        strip,
                        live,
                        range30: d30.range(),
                        range60: d60.range(),
                        peak: peak_hold,
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

    /// THE IDEAL INTERLEAVE IS 1/n, NOT HALF A SECOND. A perfect nine-way
    /// interleave was being marked at 36% against a hard-coded pair.
    #[test]
    fn the_interleave_is_judged_against_what_this_many_tubes_could_manage() {
        assert!(phase_note(0.5, 2).contains("100%"), "{}", phase_note(0.5, 2));
        assert!(phase_note(1.0 / 9.0, 9).contains("100%"), "{}", phase_note(1.0 / 9.0, 9));
        // Tubes firing together buy precision and no time at all.
        assert!(phase_note(0.0, 2).contains("0%"));
        // And a pair's ideal is not nine's.
        assert!(!phase_note(0.5, 9).contains("100%"));
        // Twice the ideal gap is half a grid, not no grid: the old form
        // called this zero, and called anything wider than it zero too.
        assert!(phase_note(0.25, 2).contains("50%"), "{}", phase_note(0.25, 2));
        assert!(phase_note(1.0, 2).contains("50%"), "{}", phase_note(1.0, 2));
        assert!(phase_note(0.24, 9).contains("46%"), "{}", phase_note(0.24, 9));
    }

    /// The dial band grows by a row of numbers for every five tubes, so nine
    /// counters cannot push their readings onto the cascade's captions.
    #[test]
    fn the_dial_band_makes_room_for_the_readings_beside_it() {
        assert_eq!(cluster_h(1), CLUSTER_H, "one tube needs no grid at all");
        assert!(cluster_h(9) > cluster_h(2));
        assert_eq!(cluster_h(9), CLUSTER_H + 24.0, "nine is two rows of five");
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
