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
    self, band, bar_seconds, interleave_quality, span_words, tiers_interleaved,
    tiers_with, Band, Spectrum, Tier, Windows, TIERS,
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

        // ---- the scale, in the words the colours stand for --------------
        //
        // THE FACE IS FIXED NOW, SO THE LEGEND CAN BE. A moving range made a
        // key useless -- the colour at a given angle meant something
        // different a minute later -- and three decades that never move mean
        // every band sits at the same place on every dial, for good. So the
        // names are printed once, in their own colours, at the foot of the
        // cluster: the dial says where the reading is and this says what the
        // colour under it is called.
        //
        // A reading is not "above 240", it is a WARNING, and that is the
        // whole reason the bands are named at all.
        //
        // The floors are the same constants the arcs, the ticks and every
        // number on the panel are drawn from, so they cannot disagree.
        let mut legend = row![
            mono(format!("{:.0}\u{2013}{:.0} CPM \u{b7} log", DIAL_FLOOR, DIAL_CEIL))
                .size(9)
                .color(self.skin.faint),
        ]
        .spacing(8);
        for b in Band::all() {
            // THE LOWEST BAND HAS NO FLOOR WORTH PRINTING. `Attenuated` runs
            // from zero counts, which is true and reads as a mistake beside a
            // dial whose bottom stop is three: the name and the colour are
            // the whole of what it has to say.
            legend = legend.push(
                mono(if b.floor() > 0.0 {
                    format!("{} {:.0}", b.name(), b.floor())
                } else {
                    b.name().to_string()
                })
                .size(9)
                .color(self.skin.colour(b)),
            );
        }

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
        let panel = stack![
            chart,
            column![
                container(row![dial_faces, Space::new().width(6.0), numbers].spacing(0))
                    .height(Length::Fixed(band_h)),
                legend,
                caps,
                Space::new().height(Fill),
            ]
            .spacing(0),
        ]
        .height(Fill);

        // ---- the spectrum's axis and its verdict ------------------------
        let (shortest, longest) = ladder_ends(&s.layers);
        let axis = row![
            mono(span_words(longest as f64)).size(10).color(self.skin.dim),
            Space::new().width(Fill),
            mono("period \u{b7} log").size(10).color(self.skin.faint),
            Space::new().width(Fill),
            mono(span_words(period_floor(shortest, longest))).size(10).color(self.skin.dim),
        ];
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
        let stamp = mono(clock::format(clock::now(), "%Y-%m-%d %H:%M:%S"))
            .size(10)
            .color(self.skin.faint);
        let countdown = mono(match &s.random {
            // After a line has been drawn, the pool status IS the countdown
            // and needs no label in front of it.
            Some(_) => s.pool.clone(),
            None => format!("random \u{b7} {}", s.pool),
        })
        .size(10)
        .color(self.skin.faint);
        let emission: Element<Message> = match &s.random {
            Some((who, hex, _at, suspect)) => row![
                mono(format!("{} ", tube_name(*who)))
                    .size(11)
                    .color(self.skin.tube(*who)),
                mono(entropy::group_hex(hex))
                    .size(11)
                    .color(if *suspect { self.skin.warn } else { self.skin.cyan }),
            ]
            .into(),
            None => Space::new().into(),
        };
        let footline = row![
            stamp,
            Space::new().width(Fill),
            emission,
            Space::new().width(Fill),
            countdown,
        ]
        .spacing(10)
        .align_y(iced::Center);

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

fn mono(s: impl text::IntoFragment<'static>) -> text::Text<'static> {
    text(s).font(MONO)
}

/// A, B, C: short enough to sit beside a number without crowding it.
fn tube_name(k: usize) -> String {
    format!("{}", (b'A' + (k as u8 % 26)) as char)
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
                                if merged.len() == STRIP_KEEP {
                                    merged.pop_front();
                                    sources.pop_front();
                                    dropped += 1;
                                }
                                merged.push_back(counts as f64);
                                sources.push_back(who as u8);
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
                        peak: peak_hold,
                        sources: sources.iter().rev().take(STRIP_COLS).rev().copied().collect(),
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
        assert_eq!(phase_note(0.24, 9), "\u{b7} interleave 0.24s (46%)");
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
