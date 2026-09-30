// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// Windows, spectrum and digits: the arithmetic the display is made of.

/// Counts per second in, running CPM averages out.
///
/// EACH WINDOW KEEPS A RUNNING SUM rather than re-adding its samples. The
/// obvious version is O(samples x window), which nobody notices at one sample
/// a second and which costs sixteen seconds when 850,000 samples go through
/// it. Adding the new sample and subtracting the ones that fell out the back
/// is O(1) per window per sample, whatever the window.
pub struct Windows {
    pub spans: Vec<f64>,
    pub samples: Vec<(f64, u32)>,
    pub total: u64,
    started: Option<f64>,
    sums: Vec<u64>,
    /// How many samples each window holds. A sample is one second of one
    /// tube, so this is the TUBE-SECONDS behind the sum beside it.
    held: Vec<u64>,
    heads: Vec<usize>,
    base: usize,
}

impl Windows {
    pub fn new(spans: &[f64]) -> Windows {
        Windows {
            spans: spans.to_vec(),
            samples: Vec::new(),
            total: 0,
            started: None,
            sums: vec![0; spans.len()],
            held: vec![0; spans.len()],
            heads: vec![0; spans.len()],
            base: 0,
        }
    }

    pub fn add(&mut self, when: f64, counts: u32) {
        if self.started.is_none() {
            self.started = Some(when);
        }
        self.samples.push((when, counts));
        self.total += counts as u64;
        for i in 0..self.spans.len() {
            self.sums[i] += counts as u64;
            self.held[i] += 1;
            let cutoff = when - self.spans[i];
            let mut head = self.heads[i];
            while head - self.base < self.samples.len() {
                let (t, c) = self.samples[head - self.base];
                if t > cutoff {
                    break;
                }
                self.sums[i] -= c as u64;
                self.held[i] -= 1;
                head += 1;
            }
            self.heads[i] = head;
        }
        let keep = self.heads.iter().copied().min().unwrap_or(0).saturating_sub(1);
        if keep > self.base {
            self.samples.drain(..keep - self.base);
            self.base = keep;
        }
    }

    /// The absolute position of `samples[0]` among every sample ever added:
    /// old samples are dropped from the front once no window needs them.
    pub fn first_index(&self) -> usize {
        self.base
    }

    pub fn elapsed(&self) -> f64 {
        match (self.started, self.samples.last()) {
            (Some(s), Some(&(t, _))) => t - s,
            _ => 0.0,
        }
    }

    /// CPM over the last span seconds, or None until the window is full.
    ///
    /// None is not a failure and must not be drawn as zero: it means "not
    /// enough signal yet to say", and the difference matters most in the first
    /// five minutes, which is exactly when someone is watching.
    pub fn average(&self, span: f64) -> Option<f64> {
        if self.samples.is_empty() || self.elapsed() < span {
            return None;
        }
        let i = self.spans.iter().position(|&s| s == span)?;
        Some(self.sums[i] as f64 * 60.0 / span)
    }

    /// The counts in the last `span` seconds and the tube-seconds that took
    /// them, or None until the window is full.
    pub fn behind(&self, span: f64) -> Option<(u64, u64)> {
        if self.samples.is_empty() || self.elapsed() < span {
            return None;
        }
        let i = self.spans.iter().position(|&s| s == span)?;
        Some((self.sums[i], self.held[i]))
    }

    /// The rate of the ROOM over the last `span` seconds, in CPM, from
    /// windows that hold the samples of several tubes.
    ///
    /// COUNTS OVER THE TUBE-SECONDS THAT TOOK THEM, not over the seconds on
    /// the clock times the tubes plugged in. Those are the same number while
    /// every tube answers every second. They stop being the same the moment
    /// one does not -- a counter whose USB port reset, a reader that missed
    /// four seconds -- and then a divisor taken from the tube count goes on
    /// dividing by an instrument that was not measuring: two tubes, one of
    /// them silent, reported half the room. The windows know how many
    /// samples they hold, and a sample is one second of one tube, so the
    /// divisor is what was measured and nothing else.
    ///
    /// A tube that leaves is therefore out of the mean from the second it
    /// goes quiet, and in it again from the second it answers, with nothing
    /// to tell either time.
    pub fn mean(&self, span: f64) -> Option<f64> {
        let (counts, seconds) = self.behind(span)?;
        (seconds > 0).then(|| counts as f64 * 60.0 / seconds as f64)
    }

    /// One sigma on `mean`, in CPM.
    ///
    /// Arrivals are Poisson, so the uncertainty is the count behind the
    /// number and nothing else: N arrivals give a relative error of
    /// 1/sqrt(N). N is the counts the window holds -- from however many
    /// tubes were answering, for however long each was.
    pub fn sigma(&self, span: f64) -> Option<f64> {
        let (counts, _) = self.behind(span)?;
        let cpm = self.mean(span)?;
        (counts > 0).then(|| cpm / (counts as f64).sqrt())
    }
}

pub const LEVEL_RAISED: f64 = 100.0;
pub const LEVEL_HIGH: f64 = 300.0;

#[derive(PartialEq, Clone, Copy)]
pub enum Level {
    Calm,
    Raised,
    High,
}

/// Which of three bands a reading is in.
///
/// KEPT BESIDE `band` BELOW, which is the five-band scale everything that can
/// show five bands now uses. This one remains because the log format, the
/// exported page and the terminal monitor are all written against three, and
/// widening them is a separate change to a separate file format.
pub fn level(cpm: f64) -> Level {
    if cpm >= LEVEL_HIGH {
        Level::High
    } else if cpm >= LEVEL_RAISED {
        Level::Raised
    } else {
        Level::Calm
    }
}

// ------------------------------------------------------------------ bands ---
//
// THE FLOOR OF EACH NAMED BAND, in counts per minute. These are the numbers a
// person operating the instrument works in, so they are named rather than
// left as thresholds: a reading is not "above 240", it is a WARNING.
//
// The scale is not linear and is not meant to be. Each step is roughly a
// doubling with the low end stretched, because the interesting question at 3
// CPM ("is this tube even working?") and the interesting question at 600 CPM
// ("how quickly can I leave?") are different questions and want different
// resolution.
pub const BAND_ATTENUATED: f64 = 3.0;
pub const BAND_NOMINAL: f64 = 30.0;
pub const BAND_ADVISORY: f64 = 120.0;
pub const BAND_WARNING: f64 = 240.0;
pub const BAND_DEADLY: f64 = 600.0;

/// A reading, named.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum Band {
    /// Below 3 CPM. Not a quiet room -- a tube that is shielded, unplugged,
    /// dying or lying. Natural background does not go this low, so this band
    /// is a fault report and not a reassurance.
    Attenuated,
    /// Ordinary background, 30 to 120.
    Nominal,
    /// 120 to 240: worth knowing about, not worth acting on.
    Advisory,
    /// 240 to 600.
    Warning,
    /// 600 and up.
    Deadly,
}

/// The band a reading falls in.
///
/// NOTE THE FLOOR OF `Nominal` IS 30 AND NOT 0. Between 3 and 30 is where a
/// counter reads when something is between it and the world, and calling that
/// "nominal" would be the most dangerous thing this scale could do -- an
/// instrument reading low because it has failed must not look like an
/// instrument reading low because the room is clean.
pub fn band(cpm: f64) -> Band {
    if cpm >= BAND_DEADLY {
        Band::Deadly
    } else if cpm >= BAND_WARNING {
        Band::Warning
    } else if cpm >= BAND_ADVISORY {
        Band::Advisory
    } else if cpm >= BAND_NOMINAL {
        Band::Nominal
    } else {
        Band::Attenuated
    }
}

impl Band {
    pub fn name(self) -> &'static str {
        match self {
            Band::Attenuated => "attenuated",
            Band::Nominal => "nominal",
            Band::Advisory => "advisory",
            Band::Warning => "warning",
            Band::Deadly => "deadly",
        }
    }

    /// Where this band begins, in CPM.
    pub fn floor(self) -> f64 {
        match self {
            Band::Attenuated => 0.0,
            Band::Nominal => BAND_NOMINAL,
            Band::Advisory => BAND_ADVISORY,
            Band::Warning => BAND_WARNING,
            Band::Deadly => BAND_DEADLY,
        }
    }

    /// Every band, lowest first, for anything drawing the whole scale.
    pub fn all() -> [Band; 5] {
        [Band::Attenuated, Band::Nominal, Band::Advisory, Band::Warning, Band::Deadly]
    }
}

// ------------------------------------------------------------------- fft ---
//
// Iterative radix-2 Cooley-Tukey over a plain complex pair. Twenty-five lines
// against a dependency, which is the same trade the serial port makes.
#[derive(Clone, Copy)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn add(self, o: C) -> C {
        C { re: self.re + o.re, im: self.im + o.im }
    }
    fn sub(self, o: C) -> C {
        C { re: self.re - o.re, im: self.im - o.im }
    }
    fn norm(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

fn fft(values: &[f64]) -> Vec<C> {
    let n = values.len();
    assert!(n > 0 && n & (n - 1) == 0, "fft needs a power-of-two length");
    let mut a: Vec<C> = values.iter().map(|&v| C { re: v, im: 0.0 }).collect();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut size = 2usize;
    while size <= n {
        let ang = -2.0 * std::f64::consts::PI / size as f64;
        let step = C { re: ang.cos(), im: ang.sin() };
        let half = size / 2;
        let mut start = 0usize;
        while start < n {
            let mut w = C { re: 1.0, im: 0.0 };
            for k in start..start + half {
                let u = a[k];
                let v = a[k + half].mul(w);
                a[k] = u.add(v);
                a[k + half] = u.sub(v);
                w = w.mul(step);
            }
            start += size;
        }
        size <<= 1;
    }
    a
}

/// The power in bins 1 to n/2 - 1 of `values`, whatever their number.
///
/// BY THE TRANSFORM WHERE IT CAN BE, AND BY THE SUM WHERE IT CANNOT. The
/// radix-2 transform wants a power of two, and a window is sometimes asked
/// for in the seconds somebody thinks in: three hundred is five minutes and
/// is not a power of two. Padding it to 512 with noughts would give bins
/// that are not three hundred seconds' and are not independent of their
/// neighbours, which is what `chance_max_of` counts on. So it is summed as
/// it is written -- X(k) = sum of x(i) e^(-2 pi i k / n) -- from a table of
/// one turn. That is n/2 times n steps: 45,000 for three hundred seconds,
/// once every ten.
fn powers(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n < 4 {
        return Vec::new();
    }
    if n & (n - 1) == 0 {
        return fft(values)[1..n / 2].iter().map(|c| c.norm()).collect();
    }
    let turn: Vec<(f64, f64)> = (0..n)
        .map(|j| (2.0 * std::f64::consts::PI * j as f64 / n as f64).sin_cos())
        .collect();
    (1..n / 2)
        .map(|k| {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            let mut at = 0usize;
            for v in values {
                let (sin, cos) = turn[at];
                re += v * cos;
                im -= v * sin;
                at += k;
                if at >= n {
                    at -= n;
                }
            }
            re * re + im * im
        })
        .collect()
}

/// How tall a power is drawn, from nought to one, on a scale that ends at
/// `top`.
///
/// BY ITS LOGARITHM, AND AGAINST THE LOUDEST THING IN VIEW. Drawn in
/// proportion, a spectrum is as tall as its tallest bar and the rest of it
/// is whatever is left: the longest periods hold the room's slow drift and
/// every tube that stopped and started, stand forty times the mean, and
/// leave the floor -- where a faint line would be -- a pixel high. A fixed
/// gain cures that and cuts the top off what is loud. The logarithm keeps
/// both: `ln(1 + v)`, which is nought at nought and nearly `v` while `v`
/// is small, so the floor is drawn as it is, and which grows by the same
/// step for every doubling above it, so forty is two and a half times as
/// tall as three and not thirteen.
///
/// `top` is the loudest in view, so the tallest thing drawn is as tall as
/// there is room for, whatever it is: that is the automatic part. With a
/// top of 40 what is flat stands at 19% and what luck reaches, in one
/// window, at 58%.
pub fn gain(value: f64, top: f64) -> f64 {
    if !(value > 0.0) || !(top > 0.0) {
        return 0.0;
    }
    (value.ln_1p() / top.ln_1p()).clamp(0.0, 1.0)
}

/// The tallest of `bins` bins in an average of `n` independent periodograms
/// of noise, by luck alone. The account of it is on `Spectrum::chance_max`.
pub fn chance_max_of(bins: usize, n: f64) -> f64 {
    if n < 1.0 || bins < 2 {
        return f64::INFINITY;
    }
    let x = ((bins - 1) as f64).ln() / n;
    1.0 + (2.0 * x).sqrt() + 2.0 * x / 3.0
}

/// The Hann taper for a window of `n` samples.
fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())
        .collect()
}

/// The periodograms one at a time, kept in the order they were taken.
///
/// WHAT `Spectrum` THROWS AWAY. It adds every periodogram into one running
/// total, which is how a faint period is found and how WHEN is lost: a line
/// that was there for ten minutes an hour ago and one that has been there
/// all along come out as the same bar. Here each window's spectrum is a row
/// of its own, the newest in front, and the last `depth` of them are kept.
///
/// ONE ROW IS NOISE, AND IS MEANT TO BE. A single bin of a single
/// periodogram is an exponential draw, and the tallest of 127 of them
/// stands seven times the mean by luck alone -- `chance_max`, which is
/// `Spectrum`'s figure at one run. So a spike in a row says nothing. A
/// spike IN THE SAME PLACE in row after row is a ridge, and a long enough
/// ridge is a period: that is the whole of what this is for, and the
/// average of every row is the spectrum the panel already had.
///
/// LONG ENOUGH, because A ROW IS NOT A NEW MEASUREMENT. One taken eight
/// seconds after another shares 248 of its 256 seconds with it, so a spike
/// that luck put in one is in its neighbours too, and chance draws short
/// ridges of its own. How short was measured, on 400,000 seconds of
/// Poisson counts at each of two rates: a run of rows over the line in one
/// bin had a median of 5 rows, 99 in 100 were 13 or fewer, and the longest
/// of 981 was 16. Half a window, which is `chance_rows`; a ridge longer
/// than that is not luck's. Held against the long average, at 300 seconds
/// and 10, luck runs a little longer: 17 and 22 were the longest of 1319,
/// which is past half a window twice in a thousand papers, and none
/// reached 24. docs/the-drum-spectrogram.md has both tables.
///
/// A row is `window` seconds, mean removed, Hann tapered, transformed, and
/// divided by its own mean so that flat reads 1.0 wherever the rate is.
/// One is taken every `hop` seconds, so neighbouring rows share most of
/// their seconds and a ridge is continuous rather than a row of dots.
///
/// OR BY THE LONG AVERAGE, which is `leveled`. Dividing a row by its own
/// mean makes every row flat at 1.0 and so makes every row the same
/// height: five minutes in which the room counted twice as much are drawn
/// no taller than the five before. Counts that arrive by chance at a rate
/// of r a second put r times the taper's energy in every bin, so the rate
/// says what flat should be -- and the rate of the last fifty minutes says
/// it without being moved by the five being measured. Against that, a row
/// reads 1.0 when the room is as it has been, and the whole of it stands
/// higher when the room is not.
pub struct Waterfall {
    pub window: usize,
    pub hop: usize,
    pub depth: usize,
    /// How many rows have been taken, ever: a row's name, for whoever
    /// wants to know whether the stack has moved.
    pub made: u64,
    buf: std::collections::VecDeque<f64>,
    since: usize,
    taper: Vec<f64>,
    rows: std::collections::VecDeque<Vec<f32>>,
    /// The seconds the long average is of, or nought for a row's own mean;
    /// those seconds, and what they add up to.
    over: usize,
    long: std::collections::VecDeque<f64>,
    long_sum: f64,
}

impl Waterfall {
    pub fn new(window: usize, hop: usize, depth: usize) -> Waterfall {
        Waterfall {
            window,
            hop: hop.max(1),
            depth: depth.max(1),
            made: 0,
            buf: std::collections::VecDeque::with_capacity(window + 1),
            since: 0,
            taper: hann(window),
            rows: std::collections::VecDeque::with_capacity(depth + 1),
            over: 0,
            long: std::collections::VecDeque::new(),
            long_sum: 0.0,
        }
    }

    /// Rows against the average of the last `over` seconds, where `new`
    /// gives them against their own. Until there are `over` seconds it is
    /// the average of what there is, which is never less than a window.
    pub fn leveled(window: usize, hop: usize, depth: usize, over: usize) -> Waterfall {
        let mut w = Waterfall::new(window, hop, depth);
        w.over = over.max(window);
        w
    }

    /// The rate the rows are held against, in counts a second; None for
    /// rows held against themselves, and before the first second.
    pub fn level(&self) -> Option<f64> {
        (self.over > 0 && !self.long.is_empty())
            .then(|| self.long_sum / self.long.len() as f64)
    }

    /// One more second. True when it made a row.
    pub fn add(&mut self, counts: u32) -> bool {
        self.buf.push_back(counts as f64);
        if self.buf.len() > self.window {
            self.buf.pop_front();
        }
        if self.over > 0 {
            self.long.push_back(counts as f64);
            self.long_sum += counts as f64;
            if self.long.len() > self.over {
                self.long_sum -= self.long.pop_front().unwrap_or(0.0);
            }
        }
        self.since += 1;
        if self.buf.len() < self.window || self.since < self.hop {
            return false;
        }
        self.since = 0;
        let mean = self.buf.iter().sum::<f64>() / self.window as f64;
        let shaped: Vec<f64> = self
            .buf
            .iter()
            .zip(&self.taper)
            .map(|(v, t)| (v - mean) * t)
            .collect();
        // The DC term is the rate, which every other number on the panel
        // already gives; the rest, against their own mean.
        let power = powers(&shaped);
        let level = match self.level() {
            // What chance puts in a bin at that rate: the rate, times the
            // energy of the taper.
            Some(rate) => rate * self.taper.iter().map(|t| t * t).sum::<f64>(),
            None => power.iter().sum::<f64>() / power.len().max(1) as f64,
        };
        let row = power
            .iter()
            .map(|p| if level > 0.0 { (p / level) as f32 } else { 0.0 })
            .collect();
        // NOTHING IS MOVED. The new row goes on the front and the oldest
        // comes off the back; how far back a row is, is where it is.
        self.rows.push_front(row);
        self.rows.truncate(self.depth);
        self.made += 1;
        true
    }

    /// The rows, newest first.
    pub fn rows(&self) -> Vec<Vec<f32>> {
        self.rows.iter().cloned().collect()
    }

    /// Seconds since the newest row was taken, which is how far the stack
    /// has slid towards the next.
    pub fn age(&self) -> usize {
        self.since
    }

    /// Seconds until there is a first row.
    pub fn wait(&self) -> usize {
        if self.made > 0 { 0 } else { self.window - self.buf.len() }
    }

    /// The period a bin of a row stands for, in seconds.
    pub fn period(&self, index: usize) -> f64 {
        self.window as f64 / (index + 1) as f64
    }

    /// How tall the tallest bin of ONE row gets by luck.
    pub fn chance_max(&self) -> f64 {
        chance_max_of(self.window / 2, 1.0)
    }

    /// How many rows running a bin stays over that line by luck: the rows
    /// in half a window. See the account at the head of `Waterfall`.
    pub fn chance_rows(&self) -> usize {
        (self.window / (2 * self.hop)).max(1)
    }
}

/// One window of a `Trail`: its length, the bins it is asked for, and what
/// it needs to work them out.
struct Reach {
    window: usize,
    bins: Vec<usize>,
    taper: Vec<f64>,
    /// The taper's energy: what chance puts in a bin, for each count a
    /// second.
    energy: f64,
    /// One turn of the circle in `window` steps, as (sine, cosine).
    turn: Vec<(f64, f64)>,
}

/// Rows of spectrum that reach the whole way across: each row from several
/// windows at once, every one of them ending at the same second.
///
/// WHY SEVERAL. A window holds no period longer than itself, so a row of
/// 300 seconds stops at five minutes and a panel whose axis runs to nine
/// hours had traces on the right-hand half of it only. The periods beyond
/// are in longer windows. So a row here is a join: from the shortest
/// window, every bin it has; from each longer one, only the bins that are
/// longer than the window before it could hold. Three hundred seconds
/// gives 149, and 512, 4096 and 32768 after it give 1, 7 and 7 -- which is
/// all there is out there, and is why the left of a spectrum is a few
/// bars far apart.
///
/// ONLY THE BINS THAT ARE ASKED FOR ARE WORKED OUT, by the sum and not the
/// transform: seven bins of 32768 seconds are 229,000 steps, where the
/// whole transform of them would be sixteen thousand bins nobody draws.
///
/// AND ONLY THE ROWS THAT ARE ASKED FOR. `add` keeps the second and does
/// nothing else. `rows` works out the rows that are on show and keeps
/// them, so a row is worked out once; a window that is handed eight hours
/// of history in a breath works out forty-eight rows at the end of it and
/// not three thousand on the way.
///
/// Every row is held against the long average, as `Waterfall::leveled`
/// holds them, and a window that has not yet its seconds is left out of
/// the row: the trace begins where there is something to draw.
///
/// THE LONG END OF A ROW IS NOT NEWS. A row of 4096 seconds shares 4086 of
/// them with the row ten seconds before, so out there forty-eight rows are
/// very nearly one row, and a streak the length of the paper is one draw
/// of chance. `Waterfall::chance_rows` is about the shortest window only.
#[derive(Clone, Debug, PartialEq)]
pub struct TrailRow {
    /// What the room counted in the row's own seconds -- the shortest
    /// window's -- against the long average: 1.0 when it was as it has
    /// been, 2.0 when it counted twice that. It is what the whole row
    /// stands at, and a front end draws the row as bright as this.
    pub level: f32,
    /// (period, power), the longest period first.
    pub bins: Vec<(f32, f32)>,
}

impl TrailRow {
    /// The bins that stand over both their neighbours and over `floor`:
    /// the row's peaks, as places in `bins`. An end of the row has one
    /// neighbour and is a peak if it stands over that.
    pub fn peaks(&self, floor: f32) -> Vec<usize> {
        let p = |i: usize| self.bins[i].1;
        (0..self.bins.len())
            .filter(|&i| {
                p(i) > floor
                    && (i == 0 || p(i) > p(i - 1))
                    && (i + 1 == self.bins.len() || p(i) >= p(i + 1))
            })
            .collect()
    }
}

pub struct Trail {
    reaches: Vec<Reach>,
    pub hop: usize,
    pub depth: usize,
    over: usize,
    /// The newest seconds, as many as the longest window and the rows on
    /// show need; and how many there have ever been.
    seconds: std::collections::VecDeque<f64>,
    keep: usize,
    pub total: u64,
    /// Rows by the second they end at, newest first.
    kept: std::collections::VecDeque<(u64, TrailRow)>,
}

impl Trail {
    /// `windows` in any order; the shortest is the one that gives every
    /// bin it has.
    pub fn new(windows: &[usize], hop: usize, depth: usize, over: usize) -> Trail {
        let mut lengths: Vec<usize> = windows.iter().copied().filter(|w| *w >= 4).collect();
        lengths.sort_unstable();
        lengths.dedup();
        let mut before = 0usize;
        let reaches: Vec<Reach> = lengths
            .iter()
            .map(|&window| {
                // Longer than the window before could hold, and no longer
                // than this one: window / k > before.
                let bins: Vec<usize> = (1..window / 2)
                    .filter(|k| before == 0 || window > before * k)
                    .collect();
                before = window;
                let taper = hann(window);
                Reach {
                    window,
                    bins,
                    energy: taper.iter().map(|t| t * t).sum(),
                    taper,
                    turn: (0..window)
                        .map(|j| (2.0 * std::f64::consts::PI * j as f64 / window as f64).sin_cos())
                        .collect(),
                }
            })
            .collect();
        let (hop, depth) = (hop.max(1), depth.max(1));
        let longest = lengths.last().copied().unwrap_or(0);
        Trail {
            reaches,
            hop,
            depth,
            over: over.max(1),
            seconds: std::collections::VecDeque::new(),
            keep: longest.max(over) + depth * hop,
            total: 0,
            kept: std::collections::VecDeque::new(),
        }
    }

    /// One more second.
    pub fn add(&mut self, counts: u32) {
        self.seconds.push_back(counts as f64);
        if self.seconds.len() > self.keep {
            self.seconds.pop_front();
        }
        self.total += 1;
    }

    /// The second the newest row ends at: the last whole hop, once the
    /// shortest window has its seconds.
    pub fn newest(&self) -> Option<u64> {
        let first = self.reaches.first()?.window as u64;
        let end = self.total / self.hop as u64 * self.hop as u64;
        (end >= first).then_some(end)
    }

    /// Seconds since the newest row, which is how far the paper has moved
    /// towards the next.
    pub fn age(&self) -> usize {
        match self.newest() {
            Some(end) => (self.total - end) as usize,
            None => 0,
        }
    }

    /// How many bins a row has when every window has its seconds.
    pub fn bins(&self) -> usize {
        self.reaches.iter().map(|r| r.bins.len()).sum()
    }

    /// How tall the tallest bin of one row gets by luck.
    pub fn chance_max(&self) -> f64 {
        chance_max_of(self.bins() + 1, 1.0)
    }

    /// The rows on show, newest first.
    pub fn rows(&mut self) -> Vec<TrailRow> {
        let Some(newest) = self.newest() else { return Vec::new() };
        let first = self.reaches[0].window as u64;
        let ends: Vec<u64> = (0..self.depth as u64)
            .map(|k| newest.saturating_sub(k * self.hop as u64))
            .take_while(|end| *end >= first)
            .collect();
        self.kept.retain(|(end, _)| ends.contains(end));
        for end in &ends {
            if !self.kept.iter().any(|(e, _)| e == end) {
                let row = self.row(*end);
                self.kept.push_back((*end, row));
            }
        }
        self.kept.make_contiguous().sort_by(|a, b| b.0.cmp(&a.0));
        self.kept.iter().map(|(_, row)| row.clone()).collect()
    }

    /// The row that ends at `end`, from the seconds that are kept.
    fn row(&mut self, end: u64) -> TrailRow {
        let oldest = self.total - self.seconds.len() as u64;
        let seconds = self.seconds.make_contiguous();
        let upto = (end - oldest.min(end)) as usize;
        if end < oldest || upto > seconds.len() {
            return TrailRow { level: 0.0, bins: Vec::new() };
        }
        // The rate the row is held against: see `Waterfall::leveled`.
        let long = &seconds[upto.saturating_sub(self.over)..upto];
        let rate = long.iter().sum::<f64>() / long.len().max(1) as f64;
        let mut row = Vec::new();
        let mut level = 0.0f32;
        for reach in self.reaches.iter().rev() {
            if upto < reach.window {
                continue;
            }
            let x = &seconds[upto - reach.window..upto];
            let mean = x.iter().sum::<f64>() / reach.window as f64;
            // The last to be set is the shortest window's, which is the
            // row's own.
            level = if rate > 0.0 { (mean / rate) as f32 } else { 0.0 };
            let shaped: Vec<f64> = x.iter().zip(&reach.taper).map(|(v, t)| (v - mean) * t).collect();
            let level = rate * reach.energy;
            for &k in &reach.bins {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                let mut at = 0usize;
                for v in &shaped {
                    let (sin, cos) = reach.turn[at];
                    re += v * cos;
                    im -= v * sin;
                    at += k;
                    if at >= reach.window {
                        at -= reach.window;
                    }
                }
                let power = if level > 0.0 { (re * re + im * im) / level } else { 0.0 };
                row.push(((reach.window as f64 / k as f64) as f32, power as f32));
            }
        }
        TrailRow { level, bins: row }
    }
}

/// A running average of periodograms over a fixed window.
///
/// Radioactive decay is Poisson and the power spectrum of a Poisson process is
/// FLAT, so a featureless strip is the useful answer: nothing is arriving on a
/// schedule. The mean is removed before each transform (the DC term is the
/// count rate every other number already gives) and a Hann taper keeps a
/// period that does not divide the window from leaking across every bin.
pub struct Spectrum {
    pub window: usize,
    pub bins: usize,
    buf: Vec<f64>,
    power: Vec<f64>,
    pub runs: u32,
    taper: Vec<f64>,
}

impl Spectrum {
    pub fn new(window: usize) -> Spectrum {
        let taper = hann(window);
        Spectrum {
            window,
            bins: window / 2,
            buf: Vec::with_capacity(window),
            power: vec![0.0; window / 2],
            runs: 0,
            taper,
        }
    }

    /// Half-overlapped (Welch): two averages out of each window's data.
    pub fn add(&mut self, counts: u32) -> bool {
        self.buf.push(counts as f64);
        if self.buf.len() < self.window {
            return false;
        }
        let mean = self.buf.iter().sum::<f64>() / self.window as f64;
        let shaped: Vec<f64> = self
            .buf
            .iter()
            .zip(&self.taper)
            .map(|(v, t)| (v - mean) * t)
            .collect();
        let spec = fft(&shaped);
        for i in 0..self.bins {
            self.power[i] += spec[i].norm();
        }
        self.runs += 1;
        self.buf.drain(..self.window / 2);
        true
    }

    pub fn wait(&self) -> usize {
        if self.runs > 0 { 0 } else { self.window - self.buf.len() }
    }

    /// Each bin against the average bin: 1.0 is what flat looks like.
    pub fn relative(&self) -> Vec<f64> {
        if self.runs == 0 {
            return Vec::new();
        }
        let avg: Vec<f64> = self.power[1..].iter().map(|p| p / self.runs as f64).collect();
        let mean = avg.iter().sum::<f64>() / avg.len() as f64;
        if mean <= 0.0 {
            return vec![0.0; avg.len()];
        }
        avg.into_iter().map(|p| p / mean).collect()
    }

    fn independent(&self) -> f64 {
        if self.runs > 1 { self.runs as f64 * 9.0 / 11.0 } else { self.runs as f64 }
    }

    pub fn sigma(&self, rel: f64) -> f64 {
        let n = self.independent();
        if n < 1.0 { 0.0 } else { (rel - 1.0) * n.sqrt() }
    }

    /// How tall the tallest bin gets on a quiet counter, by luck alone.
    ///
    /// Sigma is computed for one bin, but the eye picks the tallest of many,
    /// and the biggest of many draws is much larger than any single draw.
    ///
    /// THE MISSING TERM, AND WHAT IT COST. This used to return
    /// `1 + ln(B)/N`, which is the right answer for ONE periodogram and the
    /// wrong one for an average of N. A single bin of white noise is
    /// exponential, and the largest of B of those does land near `ln(B)`
    /// above the mean; but the average of N is Gamma(N)/N, whose upper tail
    /// is not exponential at all. Solving `N(r - 1 - ln r) = ln B` for the
    /// bin that B draws just reach, and expanding it, gives
    ///
    /// ```text
    /// r ~ 1 + sqrt(2x) + 2x/3,   x = ln(B)/N
    /// ```
    ///
    /// and the leading term is the SQUARE ROOT, which the old form dropped
    /// entirely. It only agrees at N = 1, where sqrt(2x) and 2x/3 happen to
    /// sum to roughly x for x near 5. Everywhere else it is far too low, and
    /// it gets worse the longer somebody watches: the threshold falls like
    /// 1/N while the real peak falls like 1/sqrt(N), so the two cross.
    ///
    /// Measured against a null built from this counter's own recorded counts,
    /// resampled i.i.d. so the spectrum is flat by construction, the old form
    /// called a healthy background suspect 33% of the time at half an hour,
    /// 64% at an hour and 80% at two -- the longer the session, the more
    /// reliably it cried wolf. The form above holds that under 1% at every
    /// length, and it does not cost the detection the panel is there for: a
    /// period-8s source at HALF the background rate reads 6.4x and is called
    /// 95% of the time inside half an hour. What it does not find is a source
    /// at a FIFTH of background, which reads 2.7x and stays under the bar at
    /// every length -- half an hour at this counter's 44 CPM is thirteen
    /// hundred arrivals, and that line is not in them at any threshold.
    pub fn chance_max(&self) -> f64 {
        chance_max_of(self.bins, self.independent())
    }

    pub fn period(&self, index: usize) -> f64 {
        self.window as f64 / (index + 1) as f64
    }

    pub fn loudest(&self) -> (f64, usize) {
        let rel = self.relative();
        if rel.is_empty() {
            return (0.0, 0);
        }
        let mut best = (rel[0], 0usize);
        for (i, &v) in rel.iter().enumerate() {
            if v > best.0 {
                best = (v, i);
            }
        }
        best
    }
}

/// Several windows at once, so resolution grows with observation time.
///
/// Frequency resolution is 1/T: you cannot resolve a 512-second period in 128
/// seconds of listening. A long window is strictly better in the end and
/// strictly slower to say anything, so run three and show the finest that has
/// enough averages behind it.
pub struct Ladder {
    pub rungs: Vec<Spectrum>,
}

impl Ladder {
    pub fn new() -> Ladder {
        Ladder { rungs: vec![Spectrum::new(128), Spectrum::new(256), Spectrum::new(512)] }
    }
    pub fn add(&mut self, counts: u32) {
        for r in self.rungs.iter_mut() {
            r.add(counts);
        }
    }
    pub fn best(&self) -> &Spectrum {
        for r in self.rungs.iter().rev() {
            if r.runs >= 2 {
                return r;
            }
        }
        for r in self.rungs.iter().rev() {
            if r.runs > 0 {
                return r;
            }
        }
        self.rungs.iter().min_by_key(|r| r.wait()).unwrap()
    }
}

/// Fold the bins onto the columns the screen has, taking the LOUDEST bin each
/// column covers -- a single sharp line is what is being looked for, and
/// averaging it with its quiet neighbours is how it disappears.
pub fn spectrum_columns(rel: &[f64], width: usize) -> Vec<f64> {
    if rel.is_empty() || width == 0 {
        return Vec::new();
    }
    let n = rel.len();
    (0..width)
        .map(|c| {
            let lo = c * n / width;
            let hi = ((c + 1) * n / width).max(lo + 1).min(n);
            rel[lo..hi].iter().cloned().fold(0.0f64, f64::max)
        })
        .collect()
}

pub const SPARK: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The counts as a column chart `height` rows tall. One row of block glyphs
/// has eight levels, which is enough to say something happened and not enough
/// to say how much; five rows have forty.
pub fn bar_rows(values: &[f64], width: usize, height: usize) -> Vec<Vec<char>> {
    let tail: Vec<f64> = values.iter().rev().take(width).rev().cloned().collect();
    if tail.is_empty() {
        return vec![Vec::new(); height];
    }
    let peak = tail.iter().cloned().fold(0.0f64, f64::max).max(1.0);
    (0..height)
        .map(|r| {
            let floor = ((height - 1 - r) * 8) as f64;
            tail.iter()
                .map(|&c| {
                    let units = (c * height as f64 * 8.0 / peak).round();
                    SPARK[(units - floor).clamp(0.0, 8.0) as usize]
                })
                .collect()
        })
        .collect()
}

/// One stretch of the history strip: `columns` bars, `seconds` each, oldest
/// first. A value is the mean counts per second over the seconds its bar
/// covers, or None where there is no sample yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Tier {
    pub columns: usize,
    /// How long one bar covers. SECONDS AS A FLOAT, because a second is no
    /// longer the finest a bar can be: two counters reporting out of phase
    /// give a sample every half second, and the strip grows a 0.5s tier to
    /// show it. One counter still produces whole numbers and prints them as
    /// whole numbers.
    pub seconds: f64,
    pub values: Vec<Option<f64>>,
}

/// How many stretches the strip is cut into. Six: a second a bar at the
/// right, then 2, 4, 8, 16 and 32 as it ages leftwards.
///
/// IT WAS FOUR, and stopped at eight seconds a bar. A bar of eight seconds
/// at a background of a count every three holds two or three counts, which
/// is still mostly chance; one of thirty-two holds ten, and is the first
/// bar on the strip whose height is more the room than the dice. The two
/// tiers cost every other a third of its width and take the strip from ten
/// minutes to forty.
pub const TIERS: usize = 6;

/// How much wall clock the interleave tier shows, in seconds.
///
/// IN SECONDS, NOT IN BARS, because a bar there is one tube's reading and n
/// of them make a second: a fixed bar count would show four seconds with a
/// pair and half a second with nine, and the tier would mean something
/// different on every rig. Four seconds is four whole turns of the rota
/// whatever n is -- long enough to see the tubes taking turns and to see
/// whether they agree about the second happening now, and short enough that
/// the rest of the width goes to the tiers that are measuring time.
pub const INTERLEAVE_SECONDS: usize = 4;

/// The counts, as a strip that compresses as it ages.
///
/// `TIERS` tiers of equal width, newest at the right edge. The rightmost is a
/// second a bar; each one to its left holds `k` times as long in a bar, with
/// `k` the smallest whole factor that makes the strip reach back `span`
/// seconds -- the spectrum's window, so the strip and the spectrum are views
/// of the same stretch of time. A second scrolling off the left of the fine
/// tier lands in the newest bar of the next one, which fills as its seconds
/// arrive; that bar in turn lands in the next, and that one in the last.
///
/// EQUAL WIDTHS, so each tier costs twice the time of the one on its right:
/// at k = 2 and a quarter of 160 columns, 40 s of seconds, then 80 s of
/// pairs, 160 s of fours and 320 s of eights. Every tier leftwards takes a
/// bar half as often and twice as long to fill, and the strip as a whole
/// reaches back an hour of counts in the width of a terminal.
///
/// It was three tiers, with the fine one taking half the width. The fourth
/// came out of that half: a second a bar for forty seconds is as much of the
/// present as anyone reads, and the room buys another doubling of the past.
/// The fifth and sixth came out of all of them: see `TIERS`.
///
/// BAR EDGES ARE FIXED TO THE SAMPLE COUNT, not to the screen: a k-second bar
/// always covers the same k samples, so a bar does not change as the strip
/// scrolls under it -- only the newest, still-filling one does. `first` is
/// the absolute index of `samples[0]`.
///
/// Means, not sums: a 9-second bar holding nine times the counts would dwarf
/// the fine tier, and the colour bands are rates. The same height means the
/// same rate in every tier.
pub fn tiers(samples: &[u32], first: usize, width: usize, span: usize) -> Vec<Tier> {
    let v: Vec<f64> = samples.iter().map(|&c| c as f64).collect();
    tiers_with(&v, first, width, span, TIERS, 1.0)
}

/// The cascade, generalised: any number of tiers, over samples of any length.
///
/// `unit` is how many seconds one SAMPLE covers, and `span` is the reach
/// wanted in samples. With one counter both are the obvious thing -- a sample
/// is a second -- and `tiers` above passes them. With two counters reporting
/// out of phase the caller bins to half-seconds and passes `unit = 0.5` and
/// one more tier, so the fine end of the strip resolves twice as finely and
/// everything coarser than it is unchanged.
pub fn tiers_with(
    samples: &[f64],
    first: usize,
    width: usize,
    span: usize,
    count: usize,
    unit: f64,
) -> Vec<Tier> {
    if count == 0 || width == 0 {
        return Vec::new();
    }
    // Equal shares, and what does not divide goes to the fine tier, which is
    // the one whose rightmost bar is the moment happening now.
    let q = width / count;
    let fine_cols = width - q * (count - 1);
    // The smallest k whose tiers reach back as far as was asked. Each tier
    // left multiplies by k again, so the reach grows as k^(count-1) and the
    // answer is nearly always 2.
    let reach = |k: usize| -> usize {
        let mut u = 1usize;
        let mut total = fine_cols;
        for _ in 1..count {
            u *= k;
            total += q * u;
        }
        total
    };
    let mut k = 2usize;
    while reach(k) < span && k < 60 {
        k += 1;
    }
    let n = (first + samples.len()) as i64;
    let first = first as i64;
    let at = |a: i64| -> Option<f64> {
        (a >= first && a < n).then(|| samples[(a - first) as usize])
    };
    let mean = |lo: i64, hi: i64| -> Option<f64> {
        let (lo, hi) = (lo.max(first), hi.min(n));
        (hi > lo).then(|| {
            (lo..hi).map(|a| at(a).unwrap_or(0.0)).sum::<f64>() / (hi - lo) as f64
        })
    };
    let fine: Vec<Option<f64>> = (0..fine_cols as i64)
        .map(|j| at(n - fine_cols as i64 + j))
        .collect();
    let mut out = vec![Tier { columns: fine_cols, seconds: unit, values: fine }];
    // Walk left a tier at a time. `b` is where the tier to the right begins:
    // everything older than it is this tier's to group, and the group it
    // starts on becomes the boundary for the next one out.
    let mut b = n - fine_cols as i64;
    let mut step = 1i64;
    for _ in 1..count {
        step *= k as i64;
        let c = q as i64;
        let top = (b - 1).div_euclid(step);
        let values: Vec<Option<f64>> = (0..c)
            .map(|j| {
                let g = top - c + 1 + j;
                mean(g * step, (g * step + step).min(b))
            })
            .collect();
        b = (top - c + 1) * step;
        out.push(Tier { columns: q, seconds: step as f64 * unit, values });
    }
    // Coarsest first: the strip is drawn left to right, and time runs that
    // way too.
    out.reverse();
    out
}

/// The cascade for several interleaved tubes: whole seconds, and one tier
/// below them at 1/n.
///
/// WHY THE FINE TIER IS NOT PART OF THE PROGRESSION. Every other tier here
/// aggregates TIME -- a 4-second bar is four seconds of the room, and halving
/// it to two is a finer view of the room. The fine tier aggregates nothing: a
/// bar in it is ONE tube's one-second reading, placed where it arrived, and
/// 1/n is its spacing and not its integration window. Tiers between the two --
/// 2/9, 4/9, 8/9 of a second -- are therefore neither. They average
/// measurements that each already span a whole second, so they are smoothed
/// views of the same second rather than sharper views of time, and at nine
/// tubes they cost half the width of the strip to show fifty seconds in units
/// nobody thinks in.
///
/// So: `TIERS` aggregating tiers at 1, 2, 4, 8, 16 and 32 seconds, and one
/// interleave tier under them at 1/n. Seven in all, whether n is two or nine. The ratio is 2 between every
/// aggregating tier and n at the single boundary below them -- and that
/// boundary is worth marking, because it is exactly where the strip stops
/// measuring time and starts measuring arrival.
///
/// AND IT IS THE NARROWEST TIER, NOT THE WIDEST. An equal fifth of the strip
/// gave the interleave forty-eight bars, which at two tubes is twenty-four
/// seconds of arrival order -- a quarter of the width spent on the one tier
/// that is not measuring time, and spent on a stretch of it long enough that
/// nobody reads the far end. What the interleave is FOR is the last few
/// seconds: whether the tubes are taking turns, and whether they agree about
/// the second happening now. `INTERLEAVE_SECONDS` of it answers that and the
/// width it gives back goes to the aggregating tiers, which reach further for
/// having it.
pub fn tiers_interleaved(
    samples: &[f64],
    first: usize,
    width: usize,
    tubes: usize,
) -> Vec<Tier> {
    let tubes = tubes.max(1);
    if width == 0 {
        return Vec::new();
    }
    // THE INTERLEAVE TIER IS CAPPED, and capped in SECONDS rather than in
    // bars, because what it shows is one tube's reading per bar and n of them
    // make a second. Four seconds is four whole turns of the rota at any tube
    // count: eight bars with a pair, thirty-six with nine, and in both cases
    // the same stretch of wall clock.
    let fine_cols = (INTERLEAVE_SECONDS * tubes)
        .clamp(1, width.saturating_sub(TIERS).max(1));
    let rest = width - fine_cols;
    // The width the interleave gave back, split equally; what does not divide
    // goes to the one-second tier, which is the one whose rightmost bar is
    // the second happening now.
    let q = rest / TIERS;
    let extra = rest - q * TIERS;
    let n = (first + samples.len()) as i64;
    let base = first as i64;
    let at = |a: i64| -> Option<f64> {
        (a >= base && a < n).then(|| samples[(a - base) as usize])
    };
    let mean = |lo: i64, hi: i64| -> Option<f64> {
        let (lo, hi) = (lo.max(base), hi.min(n));
        (hi > lo).then(|| {
            (lo..hi).map(|a| at(a).unwrap_or(0.0)).sum::<f64>() / (hi - lo) as f64
        })
    };
    let unit = 1.0 / tubes as f64;
    let fine: Vec<Option<f64>> = (0..fine_cols as i64)
        .map(|j| at(n - fine_cols as i64 + j))
        .collect();
    let mut out = vec![Tier { columns: fine_cols, seconds: unit, values: fine }];
    // The first aggregating tier is one whole second, which is `tubes`
    // samples; each one left of it is twice that.
    let mut step = tubes as i64;
    let mut b = n - fine_cols as i64;
    for t in 0..TIERS {
        let c = (q + if t == 0 { extra } else { 0 }) as i64;
        let top = (b - 1).div_euclid(step);
        let values: Vec<Option<f64>> = (0..c)
            .map(|j| {
                let g = top - c + 1 + j;
                mean(g * step, (g * step + step).min(b))
            })
            .collect();
        b = (top - c + 1) * step;
        out.push(Tier {
            columns: c as usize,
            seconds: step as f64 / tubes as f64,
            values,
        });
        step *= 2;
    }
    out.reverse();
    out
}

/// One sample as it arrived: which tube, when, and what it counted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrival {
    pub who: usize,
    pub when: f64,
    pub counts: u32,
}

/// A strip, and which tube drew each bar of its interleave tier.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Strip {
    /// Coarsest first, as `tiers_interleaved` gives them.
    pub tiers: Vec<Tier>,
    /// One per bar of the last tier, when that tier is the interleave:
    /// the tube the bar is a reading of. Empty when there is no interleave.
    pub sources: Vec<Option<u8>>,
    /// How many tubes are in the interleave: the ones answering.
    pub live: usize,
}

/// A tube that has not been heard from for this long is not answering.
///
/// A sample comes once a second, and a reader gives a counter two and a half
/// before it calls it gone. The same figure here, so the display and the
/// reader agree about which tubes there are.
pub const QUIET_AFTER: f64 = 2.5;

/// The cascade for several tubes, some of which may have stopped answering.
///
/// BY THE CLOCK, NOT BY THE COUNT. `tiers_interleaved` takes a second to be
/// `tubes` samples, which it is for as long as every tube reports every
/// second. When one stops, the samples arrive at half the pace and the strip
/// goes on cutting them in pairs: a bar labelled one second holds two, the
/// whole history stretches, and the interleave tier shows one tube taking
/// turns with itself. So a bar here is the wall seconds it says it is -- the
/// mean, per tube-second, of whatever was measured in them -- and a second
/// in which nothing was measured is empty rather than nought.
///
/// THE INTERLEAVE IS THE TUBES THAT ARE ANSWERING. One that has gone quiet
/// is left out of it, and the tier's unit is 1/n for the n that are left;
/// with one left there is nothing to interleave and the tier is not drawn.
/// A tube that comes back is in it again with its first sample.
pub fn tiers_arrivals(arrivals: &[Arrival], live: &[bool], width: usize) -> Strip {
    if width == 0 || arrivals.is_empty() {
        return Strip::default();
    }
    let newest = arrivals.iter().map(|a| a.when).fold(f64::MIN, f64::max);
    let answering = |who: usize| live.get(who).copied().unwrap_or(false);
    let n = live.iter().filter(|l| **l).count();
    let last = newest.floor() as i64;
    let first = arrivals.iter().map(|a| a.when).fold(f64::MAX, f64::min).floor() as i64;

    // Whole seconds: what was counted in each, and by how many samples.
    let span = (last - first + 1).max(1) as usize;
    let mut sum = vec![0u64; span];
    let mut held = vec![0u32; span];
    for a in arrivals {
        let k = (a.when.floor() as i64 - first) as usize;
        sum[k] += a.counts as u64;
        held[k] += 1;
    }
    // The mean over a run of seconds, per tube-second; None where nothing
    // in the run was measured.
    let mean = |lo: i64, hi: i64| -> Option<f64> {
        let (lo, hi) = (lo.max(first), hi.min(last + 1));
        if hi <= lo {
            return None;
        }
        let (mut c, mut s) = (0u64, 0u64);
        for t in lo..hi {
            c += sum[(t - first) as usize];
            s += held[(t - first) as usize] as u64;
        }
        (s > 0).then(|| c as f64 / s as f64)
    };

    let mut tiers = Vec::new();
    let mut sources = Vec::new();
    let mut rest = width;
    // Where the aggregating tiers end: at the second happening now, or --
    // when there is an interleave -- where the interleave begins.
    let mut b = last + 1;
    if n > 1 {
        let cols = (INTERLEAVE_SECONDS * n).clamp(1, width.saturating_sub(TIERS).max(1));
        // The newest arrivals of the tubes that are answering, and no older
        // than the tier is long: a tube that came back a moment ago must
        // not bring the second it left in with it.
        let since = newest - INTERLEAVE_SECONDS as f64;
        let mut fresh: Vec<&Arrival> = arrivals
            .iter()
            .filter(|a| answering(a.who) && a.when > since)
            .collect();
        fresh.sort_by(|x, y| x.when.partial_cmp(&y.when).unwrap_or(std::cmp::Ordering::Equal));
        let fresh: Vec<&Arrival> = fresh.into_iter().rev().take(cols).rev().collect();
        let pad = cols - fresh.len();
        let mut values: Vec<Option<f64>> = vec![None; pad];
        sources = vec![None; pad];
        for a in fresh {
            values.push(Some(a.counts as f64));
            sources.push(Some(a.who.min(255) as u8));
        }
        tiers.push(Tier { columns: cols, seconds: 1.0 / n as f64, values });
        rest = width - cols;
        b = last + 1 - INTERLEAVE_SECONDS as i64;
    }
    let q = rest / TIERS;
    let extra = rest - q * TIERS;
    let mut step = 1i64;
    for t in 0..TIERS {
        let c = (q + if t == 0 { extra } else { 0 }) as i64;
        // Bar edges on whole multiples of the step, so a bar does not change
        // as the strip scrolls under it -- only the newest one does.
        let top = (b - 1).div_euclid(step);
        let values: Vec<Option<f64>> = (0..c)
            .map(|j| {
                let g = top - c + 1 + j;
                mean(g * step, (g * step + step).min(b))
            })
            .collect();
        b = (top - c + 1) * step;
        tiers.push(Tier { columns: c as usize, seconds: step as f64, values });
        step *= 2;
    }
    tiers.reverse();
    Strip { tiers, sources, live: n }
}

/// How many bars either side of a bar its trend is taken over: nine bars
/// in all.
///
/// IT WAS ONE, three bars, and the line followed the bars it was meant to
/// steady: at a count every three seconds, three one-second bars are one
/// count between them, and the mean of that is the dice again. Nine bars of
/// a second are three counts and nine of thirty-two seconds are a hundred.
pub const TREND_SIDE: usize = 4;

/// The trend line: for every bar, the mean of it and its neighbours.
///
/// A MOVING AVERAGE, and what it is for is the thing a bar cannot show. A
/// bar at background is two or three counts, and the next is as likely to
/// be twice it as half; the eye follows the tallest and sees a rise that is
/// not there. Three bars taken together are steadier than any of them, and
/// a line through those is where the rate is going.
///
/// THE STRIP IS ONE RUN OF BARS TO IT, not six. The newest bar of the
/// eight-second tier and the oldest of the four-second are neighbours in
/// time, so they are neighbours here, and the line crosses from tier to
/// tier without a step.
///
/// WEIGHTED BY THE SECONDS A BAR COVERS, which is what makes that crossing
/// honest. Every bar is a rate, so they can be averaged -- but a bar of two
/// seconds beside one of one is twice the evidence, and a plain mean of the
/// pair would count the single second as though it were two.
///
/// A BAR WITH NOTHING MEASURED IN IT has no trend and lends nothing to its
/// neighbours': empty, not nought. And THE INTERLEAVE HAS NONE AT ALL. Its
/// bars are single readings of different tubes placed where they arrived,
/// each already a whole second long, and a mean of three of them is a
/// second counted three times.
///
/// One value for each bar of each tier, in the tiers' own order.
pub fn trend(tiers: &[Tier]) -> Vec<Vec<Option<f64>>> {
    trend_over(tiers, TREND_SIDE)
}

/// The trend, over `side` bars either side of each.
pub fn trend_over(tiers: &[Tier], side: usize) -> Vec<Vec<Option<f64>>> {
    // Every bar of every aggregating tier, left to right: what it says and
    // how many seconds it says it of.
    let run: Vec<(Option<f64>, f64)> = tiers
        .iter()
        .filter(|t| t.seconds >= 1.0)
        .flat_map(|t| t.values.iter().map(move |v| (*v, t.seconds)))
        .collect();
    let mut at = 0usize;
    tiers
        .iter()
        .map(|t| {
            if t.seconds < 1.0 {
                return vec![None; t.values.len()];
            }
            let line = (at..at + t.values.len())
                .map(|i| {
                    run[i].0?;
                    let lo = i.saturating_sub(side);
                    let hi = (i + side + 1).min(run.len());
                    let (mut counts, mut seconds) = (0.0f64, 0.0f64);
                    for (v, s) in &run[lo..hi] {
                        if let Some(v) = v {
                            counts += v * s;
                            seconds += s;
                        }
                    }
                    (seconds > 0.0).then(|| counts / seconds)
                })
                .collect();
            at += t.values.len();
            line
        })
        .collect()
}

/// The seconds each bar of a strip covers: where it begins and where it
/// ends, for every bar of every tier.
///
/// THE SAME WALK THE STRIP WAS CUT BY, so a bar is said to be when it is.
/// `end` is where the aggregating tiers end -- the second after the newest
/// one in them -- and from there each tier leftwards is cut on whole
/// multiples of its step, its newest bar stopping short where the tier to
/// its right begins. A tier finer than a second is the interleave: its
/// bars are arrivals and not stretches of time, and it has none.
///
/// In whatever the strip was cut in. For several tubes that is seconds by
/// the clock; for one it is samples, which are seconds, counted from the
/// first.
pub fn bar_spans(tiers: &[Tier], end: i64) -> Vec<Vec<(i64, i64)>> {
    let mut out: Vec<Vec<(i64, i64)>> = vec![Vec::new(); tiers.len()];
    let unit = tiers
        .iter()
        .map(|t| t.seconds)
        .filter(|s| *s >= 1.0)
        .fold(f64::MAX, f64::min);
    let mut b = end;
    for (k, t) in tiers.iter().enumerate().rev() {
        if t.seconds < 1.0 {
            continue;
        }
        let c = t.values.len() as i64;
        if (t.seconds - unit).abs() < 1e-9 {
            // The finest: a bar a second, up to the end.
            out[k] = (0..c).map(|j| (b - c + j, b - c + j + 1)).collect();
            b -= c;
            continue;
        }
        let step = (t.seconds / unit).round() as i64;
        let top = (b - 1).div_euclid(step);
        out[k] = (0..c)
            .map(|j| {
                let g = top - c + 1 + j;
                (g * step, (g * step + step).min(b))
            })
            .collect();
        b = (top - c + 1) * step;
    }
    out
}

/// Which tubes are answering at `now`, from when each was last heard.
///
/// `gone` is what the reader said: a tube it has given up on is not
/// answering however recently it spoke. A tube it has not given up on is
/// still not answering if it has been quiet for `QUIET_AFTER`.
pub fn answering(heard: &[Option<f64>], gone: &[bool], now: f64) -> Vec<bool> {
    heard
        .iter()
        .enumerate()
        .map(|(k, h)| {
            !gone.get(k).copied().unwrap_or(false)
                && h.map(|t| now - t <= QUIET_AFTER).unwrap_or(false)
        })
        .collect()
}

/// How long a bar covers, as a label: "8", "1", "1/2", "1/9".
///
/// A FRACTION BELOW A SECOND, because that is what the number IS. With n
/// counters interleaving, the finest tier is one nth of a second and `0.111`
/// is a worse way of saying 1/9 -- longer, less exact, and it hides the very
/// thing the reader wants to know, which is how many tubes are feeding it.
pub fn bar_seconds(seconds: f64) -> String {
    if (seconds - seconds.round()).abs() < 1e-9 {
        return format!("{}", seconds.round() as i64);
    }
    // UNDER A SECOND, A FRACTION. Nine tubes make the fine tiers 1/9, 2/9,
    // 4/9 and 8/9 of a second, and written that way the doubling is on the
    // face of the label; written as 0.11, 0.22, 0.44 it is arithmetic the
    // reader has to do. Over a second the magnitude is what matters and a
    // decimal says it better than 128/9 does.
    if seconds > 0.0 && seconds < 1.0 {
        for d in 2..=64i64 {
            let n = seconds * d as f64;
            if (n - n.round()).abs() < 1e-6 && n.round() >= 1.0 {
                return format!("{}/{}", n.round() as i64, d);
            }
        }
    }
    format!("{:.1}", seconds)
}

/// What a measured interleave is worth, against what n tubes could manage.
///
/// TUBES ONLY SHARPEN TIME IF THEY DISAGREE ABOUT WHEN A SECOND STARTS. Each
/// has its own clock and its own phase and none can be steered, so the offset
/// is whatever it is. THE IDEAL IS 1/n OF A SECOND, not half of one: two
/// tubes perfectly interleaved are half a second apart, nine are a ninth.
/// Measuring against a fixed half-second -- which this did until nine
/// counters were plugged in -- marks a perfect nine-way interleave down to
/// 36%, and marks a pair that fires together as better than it is.
///
/// A RATIO, NOT A DISTANCE FROM IDEAL. The obvious form -- one minus the
/// relative error -- hits zero the moment the gap is twice the ideal and goes
/// negative after, so nine free-running tubes averaging 0.24s against an
/// ideal of 0.11s were reported as 0%: a rig that is in fact spreading its
/// samples over most of the second, dismissed as doing nothing. The smaller
/// over the larger is scale-free, symmetric, and degrades the way the thing
/// it measures does.
///
/// HERE RATHER THAN IN A FRONT END, for the reason `span_words` is here: the
/// window, the terminal and the exported page all report this number, and a
/// figure that disagreed between them would be the exact class of bug the
/// differential suite exists to catch.
///
/// 1.0 is a full grid in time. 0.0 is every tube reporting at once, which
/// still multiplies the counts and still buys the precision but adds no
/// resolution at all -- and claiming "1/9s per bar" in that case would be a
/// lie the display tells itself.
pub fn interleave_quality(gap: f64, tubes: usize) -> f64 {
    let ideal = 1.0 / tubes.max(2) as f64;
    if gap <= 0.0 {
        return 0.0;
    }
    ideal.min(gap) / ideal.max(gap)
}

/// How many tiers a strip fed by `counters` tubes should have.
///
/// ONE MORE THAN USUAL, AND ONLY ONE, however many tubes there are. See
/// `tiers_interleaved` for why the answer is not "one per doubling": the
/// extra tier is the interleave, and there is only ever one of those.
pub fn tiers_for(counters: usize) -> usize {
    TIERS + if counters > 1 { 1 } else { 0 }
}

/// "79s", "6m", "1h 4m": a stretch of time in the fewest words that are still
/// right to the unit shown.
///
/// HERE RATHER THAN IN EITHER FRONT END, because both draw the same strip and
/// a caption that disagreed between them would be the exact class of bug the
/// differential suite exists to catch.
pub fn span_words(seconds: f64) -> String {
    let s = seconds.round().max(0.0) as usize;
    match s {
        s if s < 120 => format!("{}s", s),
        s if s < 3600 => format!("{}m", s / 60),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

/// `bar_rows` against a peak given from outside, so tiers drawn side by side
/// share one scale. None is an empty column.
pub fn bar_rows_to(values: &[Option<f64>], height: usize, peak: f64) -> Vec<Vec<char>> {
    let peak = peak.max(1.0);
    (0..height)
        .map(|r| {
            let floor = ((height - 1 - r) * 8) as f64;
            values
                .iter()
                .map(|v| match v {
                    Some(c) => {
                        let units = (c * height as f64 * 8.0 / peak).round();
                        SPARK[(units - floor).clamp(0.0, 8.0) as usize]
                    }
                    None => ' ',
                })
                .collect()
        })
        .collect()
}

// ----------------------------------------------------------------- tests ---
//
// The crate had none. It shares a screen layout, a set of averaging windows
// and a spectrum with the Python program, and "shares" has already meant
// "drifted from" once: the big digits were pinned to column 54 in both, the
// Python was fixed and this was not, so the readout was drawn through the
// counter's serial number at every terminal width.
#[cfg(test)]
mod tests {
    use super::*;

    /// Samples a second apart, as the heartbeat delivers them.
    fn fill(w: &mut Windows, counts: &[u32]) {
        for (i, &c) in counts.iter().enumerate() {
            w.add(i as f64, c);
        }
    }

    #[test]
    fn a_window_says_nothing_until_it_is_full() {
        // NOT ZERO. The difference between "no reading yet" and "a reading of
        // zero" is the whole reason average() returns an Option, and it
        // matters most in the first five minutes -- which is exactly when
        // somebody is watching.
        let mut w = Windows::new(&[3.0, 30.0]);
        fill(&mut w, &[1, 1, 1]);
        assert_eq!(w.average(3.0), None, "three samples span two seconds");
        w.add(3.0, 1);
        assert_eq!(w.average(3.0), Some(60.0));
        assert_eq!(w.average(30.0), None);
    }

    #[test]
    fn cpm_is_the_windows_counts_scaled_to_a_minute() {
        let mut w = Windows::new(&[10.0]);
        fill(&mut w, &[2; 11]);
        assert_eq!(w.average(10.0), Some(120.0));
    }

    #[test]
    fn a_span_nobody_asked_for_has_no_answer() {
        let mut w = Windows::new(&[3.0]);
        fill(&mut w, &[1; 40]);
        assert_eq!(w.average(300.0), None);
    }

    #[test]
    fn the_sample_list_does_not_grow_without_bound() {
        // One list serves every window, trimmed to the longest of them. A
        // monitor left running for a week must not be a monitor holding a
        // week of samples.
        let mut w = Windows::new(&[3.0, 30.0]);
        fill(&mut w, &[1; 600]);
        assert!(w.samples.len() < 40, "kept {} samples", w.samples.len());
        assert_eq!(w.total, 600);
    }

    /// The named scale, at its own boundaries. The awkward one is 3 to 30:
    /// a counter reading there is not reporting a clean room, it is reporting
    /// itself, and the band is named so that cannot be misread.
    #[test]
    fn a_reading_is_named_rather_than_compared() {
        assert_eq!(band(0.0), Band::Attenuated);
        assert_eq!(band(2.9), Band::Attenuated);
        assert_eq!(band(29.9), Band::Attenuated, "under 30 is not yet nominal");
        assert_eq!(band(30.0), Band::Nominal);
        assert_eq!(band(119.9), Band::Nominal);
        assert_eq!(band(120.0), Band::Advisory);
        assert_eq!(band(239.9), Band::Advisory);
        assert_eq!(band(240.0), Band::Warning);
        assert_eq!(band(599.9), Band::Warning);
        assert_eq!(band(600.0), Band::Deadly);
        assert_eq!(band(65535.0), Band::Deadly, "the counter's ceiling is still deadly");
    }

    /// The bands sort the way the readings do, so a maximum over a stretch of
    /// them is the worst of them.
    #[test]
    fn the_bands_order_themselves() {
        let mut all = Band::all();
        all.reverse();
        all.sort();
        assert_eq!(all, Band::all());
        assert!(Band::Deadly > Band::Nominal);
        assert_eq!(Band::all().iter().copied().max().unwrap(), Band::Deadly);
    }

    #[test]
    fn the_bands_are_where_the_constants_say() {
        assert!(level(0.0) == Level::Calm);
        assert!(level(LEVEL_RAISED - 0.1) == Level::Calm);
        assert!(level(LEVEL_RAISED) == Level::Raised);
        assert!(level(LEVEL_HIGH - 0.1) == Level::Raised);
        assert!(level(LEVEL_HIGH) == Level::High);
    }

    #[test]
    fn a_chart_is_as_tall_as_it_was_asked_for_and_as_wide_as_it_has_data() {
        let rows = bar_rows(&[1.0, 2.0, 3.0, 4.0], 4, 5);
        assert_eq!(rows.len(), 5);
        for r in &rows {
            assert_eq!(r.len(), 4);
        }
        // The tallest column reaches the top row; nothing reaches above it.
        assert_eq!(rows[0][3], '█');
        assert_eq!(rows[0][0], ' ');
        // And the bottom row is full under every non-zero column.
        assert_eq!(rows[4][3], '█');
    }

    #[test]
    fn a_chart_of_nothing_is_blank_rather_than_full() {
        // peak is floored at 1.0 precisely so a screen of zeroes does not
        // divide by zero and does not draw a full block for every second.
        let rows = bar_rows(&[0.0; 6], 6, 3);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.iter().all(|&c| c == ' ')));
    }

    #[test]
    fn a_chart_shows_the_most_recent_samples_when_there_are_too_many() {
        let values: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let rows = bar_rows(&values, 10, 1);
        assert_eq!(rows[0].len(), 10);
        // The last sample is the largest, so the rightmost column is full.
        assert_eq!(*rows[0].last().unwrap(), '█');
    }

    #[test]
    fn the_spectrum_answers_nothing_until_it_has_a_window() {
        let mut s = Spectrum::new(16);
        assert_eq!(s.wait(), 16);
        for i in 0..15 {
            assert!(!s.add(i % 3), "fired before the window was full");
        }
        assert_eq!(s.wait(), 1);
        assert!(s.add(1), "the sixteenth sample completes the window");
        assert_eq!(s.wait(), 0);
        assert_eq!(s.relative().len(), s.bins - 1);
    }

    #[test]
    fn windows_half_overlap_so_two_averages_come_out_of_each_windows_data() {
        let mut s = Spectrum::new(8);
        let mut fired = 0;
        for i in 0..24 {
            if s.add((i % 5) as u32) {
                fired += 1;
            }
        }
        // 24 samples, window 8, half-overlapped: fires at 8, then every 4.
        assert_eq!(fired, 5);
        assert_eq!(s.runs, 5);
    }

    #[test]
    fn a_constant_input_has_no_spectrum_at_all() {
        // The mean is subtracted before the transform, so a flat line is all
        // zeroes and every bin is equal -- which is what flat means.
        let mut s = Spectrum::new(32);
        for _ in 0..64 {
            s.add(7);
        }
        let rel = s.relative();
        assert!(!rel.is_empty());
        assert!(rel.iter().all(|v| v.is_finite()), "{:?}", rel);
    }

    #[test]
    fn a_periodic_input_puts_a_peak_where_its_period_is() {
        // Something arriving on a schedule is the case the spectrum earns its
        // place on: decay does not have a period, so a peak is contamination.
        //
        // A SINUSOID, NOT AN IMPULSE TRAIN. A train of spikes every eighth
        // second has equal energy in every harmonic of that period, so the
        // loudest bin is as likely to be 8/3 s as 8 s -- correct physics, and
        // a test that asserts otherwise is testing its author's expectation.
        let mut s = Spectrum::new(64);
        for i in 0..512 {
            let phase = 2.0 * std::f64::consts::PI * i as f64 / 8.0;
            s.add((10.0 + 8.0 * phase.sin()).round() as u32);
        }
        let (top, where_) = s.loudest();
        assert!(top > 5.0, "a period-8 signal should stand out, got {}", top);
        let period = s.period(where_);
        assert!(
            (period - 8.0).abs() < 0.5,
            "peak at {}s, expected 8s",
            period
        );
    }

    #[test]
    fn poisson_arrivals_do_not_produce_a_peak_worth_reporting() {
        // The other half of the claim, and the one the screen leans on: a
        // healthy counter watching background must read as flat. chance_max()
        // is what "flat" is measured against -- the largest of this many bins,
        // not one of them -- so the test is that the real peak stays under it.
        let mut s = Spectrum::new(64);
        let mut seed = 0x2545f491u32;
        for _ in 0..2048 {
            // Poisson(2) by Knuth, on a small xorshift: no dependency here
            // either, and a fixed seed so a failure is reproducible.
            let mut k = 0u32;
            let mut p = 1.0f64;
            let target = (-2.0f64).exp();
            loop {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                p *= (seed as f64) / (u32::MAX as f64);
                if p <= target {
                    break;
                }
                k += 1;
            }
            s.add(k);
        }
        let (top, _) = s.loudest();
        assert!(
            top < s.chance_max() * 1.25,
            "background read as periodic: peak {:.2}x against chance {:.2}x",
            top,
            s.chance_max()
        );
    }

    /// A source that is flat BY CONSTRUCTION, at the lengths people watch for.
    ///
    /// THE REGRESSION THIS PINS. `chance_max()` used to be `1 + ln(B)/N`,
    /// which sinks like 1/N while the real tallest bin only sinks like
    /// 1/sqrt(N) -- so the longer the session, the more reliably a healthy
    /// counter was reported as contaminated. It crossed over at about half an
    /// hour and was calling background suspect four times in five by two.
    ///
    /// The samples here are drawn i.i.d. from an over-dispersed distribution
    /// (variance about twice the mean, which is what this counter actually
    /// does -- see the entropy module), so there IS no periodic component to
    /// find and every flag raised is a false one. Independent draws, so the
    /// spectrum is white whatever the marginal shape.
    #[test]
    fn a_flat_source_is_not_called_periodic_however_long_it_is_watched() {
        let mut seed = 0x2545f491u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        // A quiet mode most seconds and an occasional burst, which is the
        // shape the real counts have. Variance over mean is 1.25 here against
        // the real counter's 2.2, and that gap does not matter: i.i.d. draws
        // give a white spectrum whatever their marginal, which is the only
        // property this test needs.
        let mut draw = || {
            let u = (next() >> 8) as f64 / (1u32 << 24) as f64;
            if u < 0.55 { 0 } else if u < 0.85 { 1 } else if u < 0.96 { 2 }
            else if u < 0.99 { 3 } else { 5 }
        };

        for &(secs, want_runs) in &[(1800usize, 6u32), (3600, 13), (7200, 27)] {
            let mut fired = 0;
            let trials = 20;
            for _ in 0..trials {
                let mut s = Spectrum::new(512);
                for _ in 0..secs {
                    s.add(draw());
                }
                assert_eq!(s.runs, want_runs, "{} seconds", secs);
                let (top, _) = s.loudest();
                if top >= s.chance_max() * 1.25 {
                    fired += 1;
                }
            }
            assert!(
                fired <= 1,
                "{} seconds of a provably flat source read as periodic {} \
                 times in {} (chance_max {:.2}x)",
                secs, fired, trials, Spectrum::new(512).chance_max()
            );
        }
    }

    /// The arithmetic itself, at the two numbers the README quotes.
    #[test]
    fn the_tallest_bin_by_luck_keeps_its_square_root_term() {
        // Two windows of 256, so 127 bins and N = 2 * 9/11 independent
        // averages: x = ln(127)/N = 2.96, and 1 + sqrt(2x) + 2x/3 = 5.4x.
        // The old form returned 1 + x = 3.96, which is BELOW the median peak
        // of a flat source and is the whole of the bug.
        let mut two = Spectrum::new(256);
        for i in 0..384 {
            two.add((i % 3) as u32);
        }
        assert_eq!(two.runs, 2);
        assert!((two.chance_max() - 5.41).abs() < 0.02,
                "two windows: {:.2}x", two.chance_max());

        // Twenty-eight windows of 512: 255 bins, N = 22.9, and 1.86x rather
        // than the 1.2x the exponential form claimed.
        let mut many = Spectrum::new(512);
        for i in 0..7424 {
            many.add((i % 3) as u32);
        }
        assert_eq!(many.runs, 28);
        assert!((many.chance_max() - 1.86).abs() < 0.02,
                "twenty-eight windows: {:.2}x", many.chance_max());

        // And it must still be a threshold a real line clears easily: the
        // period-8s test above lands above 5x on two windows and stays there.
        assert!(many.chance_max() < two.chance_max(),
                "more averaging must lower the bar, not raise it");
    }

    #[test]
    fn columns_take_the_loudest_bin_they_cover() {
        // A single sharp line is what is being looked for, so averaging a
        // column would be the one operation guaranteed to hide it.
        let rel = vec![1.0, 1.0, 9.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let cols = spectrum_columns(&rel, 4);
        assert_eq!(cols.len(), 4);
        assert_eq!(cols[1], 9.0);
        assert_eq!(cols[0], 1.0);
    }

    #[test]
    fn columns_survive_being_asked_for_more_than_there_are_bins() {
        let cols = spectrum_columns(&[1.0, 2.0], 8);
        assert_eq!(cols.len(), 8);
        assert!(cols.iter().all(|v| *v == 1.0 || *v == 2.0));
        assert!(spectrum_columns(&[], 8).is_empty());
        assert!(spectrum_columns(&[1.0], 0).is_empty());
    }

    #[test]
    fn the_ladder_prefers_the_window_that_has_actually_run() {
        // 128, 256 and 512 side by side: the 128 answers first, and the 512
        // is the better answer once it has anything to say.
        let mut l = Ladder::new();
        for i in 0..200 {
            l.add((i % 4) as u32);
        }
        assert!(l.best().runs > 0, "nothing has answered after 200 samples");
    }

    #[test]
    fn the_strip_reaches_back_the_spectrum_window() {
        let t = tiers(&[], 0, 159, 512);
        assert_eq!(t.len(), TIERS);
        // Six sixths, the odd three columns to the fine tier, and k = 2:
        // 26 * (32 + 16 + 8 + 4 + 2) + 29 = 1641 seconds in 159 columns.
        assert_eq!(t.iter().map(|x| x.columns).collect::<Vec<_>>(), vec![26, 26, 26, 26, 26, 29]);
        assert_eq!(
            t.iter().map(|x| x.seconds).collect::<Vec<_>>(),
            vec![32.0, 16.0, 8.0, 4.0, 2.0, 1.0]
        );
        let reach: f64 = t.iter().map(|x| x.columns as f64 * x.seconds).sum();
        assert_eq!(reach, 1641.0);
    }

    /// A bar shorter than a second is a fraction, because that is what it is.
    #[test]
    fn a_sub_second_bar_is_labelled_as_the_fraction_it_is() {
        assert_eq!(bar_seconds(8.0), "8");
        assert_eq!(bar_seconds(1.0), "1");
        assert_eq!(bar_seconds(0.5), "1/2");
        assert_eq!(bar_seconds(1.0 / 9.0), "1/9");
        assert_eq!(bar_seconds(1.0 / 3.0), "1/3");
        // The fine tiers of a nine-tube strip, where the doubling should be
        // readable straight off the labels.
        assert_eq!(bar_seconds(2.0 / 9.0), "2/9");
        assert_eq!(bar_seconds(4.0 / 9.0), "4/9");
        assert_eq!(bar_seconds(8.0 / 9.0), "8/9");
        // And over a second, the magnitude rather than the fraction.
        assert_eq!(bar_seconds(16.0 / 9.0), "1.8");
        assert_eq!(bar_seconds(128.0 / 9.0), "14.2");
    }

    /// ONE MORE TIER PER DOUBLING, so the cascade stays dyadic however many
    /// tubes feed it. Without this, nine counters would need k = 5 and the
    /// strip would stop being something a person can read across.
    #[test]
    fn exactly_one_tier_is_added_for_the_interleave() {
        assert_eq!(tiers_for(1), TIERS, "one tube is the strip as it was");
        assert_eq!(tiers_for(2), TIERS + 1);
        assert_eq!(tiers_for(9), TIERS + 1, "nine tubes is still one interleave");
        assert_eq!(tiers_for(0), TIERS, "no tubes is not fewer than one");
    }

    /// THE SHAPE OF THE STRIP, whatever the tube count: whole seconds
    /// doubling leftwards, and one interleave tier below them at 1/n.
    #[test]
    fn nine_tubes_give_whole_seconds_and_one_interleave_tier() {
        let n = 9usize;
        let samples: Vec<f64> = (0..8000).map(|i| (i % 5) as f64).collect();
        let t = tiers_interleaved(&samples, 0, 240, n);
        assert_eq!(t.len(), TIERS + 1, "one tier for the interleave, not four");
        let seconds: Vec<String> = t.iter().map(|x| bar_seconds(x.seconds)).collect();
        assert_eq!(seconds, vec!["32", "16", "8", "4", "2", "1", "1/9"]);
        // Every aggregating tier is twice the one on its right...
        for pair in t[..TIERS].windows(2) {
            assert!((pair[0].seconds / pair[1].seconds - 2.0).abs() < 1e-9);
        }
        // ...and the interleave tier sits one whole second below them, which
        // is the boundary between measuring time and measuring arrival.
        assert!((t[TIERS - 1].seconds - 1.0).abs() < 1e-9);
        assert!((t[TIERS].seconds - 1.0 / 9.0).abs() < 1e-9);
    }

    /// Two tubes get the same shape, which is what the two-counter strip
    /// already drew before any of this was generalised.
    #[test]
    fn two_tubes_are_the_same_shape_as_nine() {
        let samples: Vec<f64> = (0..4000).map(|i| (i % 3) as f64).collect();
        let t = tiers_interleaved(&samples, 0, 240, 2);
        let seconds: Vec<String> = t.iter().map(|x| bar_seconds(x.seconds)).collect();
        assert_eq!(seconds, vec!["32", "16", "8", "4", "2", "1", "1/2"]);
    }

    /// THE IDEAL INTERLEAVE IS 1/n, NOT HALF A SECOND. A perfect nine-way
    /// interleave was being marked at 36% against a hard-coded pair. Moved
    /// here from the window when the exported page started reporting the same
    /// number: three front ends, one arithmetic.
    #[test]
    fn the_interleave_is_judged_against_what_this_many_tubes_could_manage() {
        let pc = |g, n| (100.0 * interleave_quality(g, n)).round() as i64;
        assert_eq!(pc(0.5, 2), 100, "a pair half a second apart is perfect");
        assert_eq!(pc(1.0 / 9.0, 9), 100, "and so is nine a ninth apart");
        // Tubes firing together buy precision and no time at all.
        assert_eq!(pc(0.0, 2), 0);
        // A pair's ideal is not nine's.
        assert!(pc(0.5, 9) < 100);
        // TWICE THE IDEAL GAP IS HALF A GRID, NOT NO GRID. One minus the
        // relative error called this zero, and called anything wider zero
        // too -- so nine free-running tubes spreading their samples over most
        // of the second were reported as doing nothing.
        assert_eq!(pc(0.25, 2), 50);
        assert_eq!(pc(1.0, 2), 50, "and it is symmetric either side");
        assert_eq!(pc(0.24, 9), 46);
        // Never a number that would read as better than perfect.
        for n in [2usize, 3, 9] {
            for g in [0.0, 0.01, 0.11, 0.5, 1.0, 7.5] {
                let q = interleave_quality(g, n);
                assert!((0.0..=1.0).contains(&q), "{} tubes at {}s: {}", n, g, q);
            }
        }
    }

    /// THE INTERLEAVE TIER IS FOUR SECONDS WIDE, whatever the tube count.
    /// It was an equal fifth of the strip, which at two tubes spent a quarter
    /// of the width on twenty-four seconds of arrival order.
    #[test]
    fn the_interleave_tier_shows_four_seconds_however_many_tubes() {
        for n in [2usize, 3, 4, 9] {
            let samples: Vec<f64> = (0..8000).map(|i| (i % 5) as f64).collect();
            let t = tiers_interleaved(&samples, 0, 240, n);
            let fine = t.last().expect("a strip has an interleave tier");
            assert_eq!(fine.columns, INTERLEAVE_SECONDS * n,
                       "{} tubes: {} bars", n, fine.columns);
            // Which is the same stretch of wall clock on every rig.
            assert!((fine.columns as f64 * fine.seconds - INTERLEAVE_SECONDS as f64).abs()
                        < 1e-9);
        }
    }

    /// And the width it gave back went to the tiers that measure time: the
    /// whole strip is still exactly as wide as it was asked to be.
    #[test]
    fn the_width_the_interleave_gave_back_went_to_the_other_tiers() {
        for n in [2usize, 3, 9] {
            for width in [80usize, 160, 240] {
                let t = tiers_interleaved(&[], 0, width, n);
                let total: usize = t.iter().map(|x| x.columns).sum();
                assert_eq!(total, width, "{} tubes at {} columns", n, width);
            }
        }
        // The equal-fifth strip reached 15 * 48 = 720s of aggregation; this
        // one reaches further, which is the point of taking the width back.
        let t = tiers_interleaved(&[], 0, 240, 2);
        let aggregated: f64 = t[..TIERS].iter().map(|x| x.columns as f64 * x.seconds).sum();
        assert!(aggregated > 720.0, "only {}s", aggregated);
    }

    /// And the strip still reaches back far enough to be worth having.
    #[test]
    fn the_interleaved_strip_still_reaches_back_minutes() {
        let t = tiers_interleaved(&(0..8000).map(|_| 1.0).collect::<Vec<f64>>(), 0, 240, 9);
        let reach: f64 = t.iter().map(|x| x.columns as f64 * x.seconds).sum();
        assert!(reach > 600.0, "only {}s", reach);
    }

    #[test]
    fn every_tier_leftwards_is_one_more_doubling() {
        // The whole point of the strip: a bar arrives half as often and the
        // tier takes twice as long to fill, each step to the left. Written
        // down as a test because it is the property, not the layout, that
        // must survive a change to either.
        // 162 columns, which six tiers share with none left over.
        let t = tiers(&[], 0, 162, 512);
        assert_eq!(t.len(), TIERS);
        for w in t.windows(2) {
            let (left, right) = (&w[0], &w[1]);
            assert_eq!(left.seconds, right.seconds * 2.0,
                       "a bar left is not two bars right");
            assert_eq!(left.columns as f64 * left.seconds, right.columns as f64 * right.seconds * 2.0,
                       "a tier left does not take twice as long to fill");
        }
    }

    #[test]
    fn a_bar_is_the_mean_of_exactly_its_seconds() {
        // 200 samples: the value of each is its own index, so a bar's mean
        // says which samples it holds.
        let s: Vec<u32> = (0..200).collect();
        let t = tiers(&s, 0, 30, 30);
        assert!(t.iter().all(|x| x.columns == 5));
        let (oldest, far, near, fine) = (&t[2], &t[3], &t[4], &t[5]);
        // The fine tier is the newest five, one each.
        assert_eq!(fine.values[4], Some(199.0));
        assert_eq!(fine.values[0], Some(195.0));
        // k = 2 reaches 5 + 10 + 20 + 40 = 75 >= 30. The near tier's newest
        // bar is the one sample 194 that is not in the fine tier yet; the
        // next holds 192 and 193.
        assert_eq!(near.seconds, 2.0);
        assert_eq!(near.values[4], Some(194.0));
        assert_eq!(near.values[3], Some(192.5));
        // The far tier starts where the near tier's oldest bar does (186),
        // in fours: 184..186 is 184.5, and the four before it 180..184.
        assert_eq!(far.seconds, 4.0);
        assert_eq!(far.values[4], Some(184.5));
        assert_eq!(far.values[3], Some(181.5));
        // And the new one, in eights, from where the far tier began (168):
        // 160..168 is 163.5.
        assert_eq!(oldest.seconds, 8.0);
        assert_eq!(oldest.values[4], Some(163.5));
        assert_eq!(oldest.values[3], Some(155.5));
        // The sixteens begin where the eights did (128): 112..128, and the
        // sixteen before them.
        assert_eq!(t[1].seconds, 16.0);
        assert_eq!(t[1].values[4], Some(119.5));
        assert_eq!(t[1].values[3], Some(103.5));
        // And the thirty-twos from where the sixteens began (48). The bar
        // that ends there holds 32..48 and no more: its edges are whole
        // multiples of thirty-two, and it is not stretched to reach. The
        // one before it is the first thirty-two samples there were, and the
        // one before that is before anything was counted.
        assert_eq!(t[0].seconds, 32.0);
        assert_eq!(t[0].values[4], Some(39.5));
        assert_eq!(t[0].values[3], Some(15.5));
        assert_eq!(t[0].values[2], None);
    }

    #[test]
    fn a_bar_does_not_change_as_the_strip_scrolls_under_it() {
        let s: Vec<u32> = (0..400).map(|i| (i * 7919 % 13) as u32).collect();
        let before = tiers(&s[..300], 0, 40, 80);
        let after = tiers(&s[..302], 0, 40, 80);
        // Two seconds later, with k = 2, every complete near bar has moved one
        // column left and is otherwise the same bar.
        let near = TIERS - 2;
        let k = before[near].seconds as usize;
        assert_eq!(k, 2);
        let b = &before[near].values;
        let a = &after[near].values;
        assert_eq!(&a[..a.len() - 2], &b[1..b.len() - 1]);
    }

    #[test]
    fn early_on_the_old_tiers_are_empty_not_zero() {
        let t = tiers(&[3, 4, 5], 0, 40, 512);
        for coarse in &t[..TIERS - 1] {
            assert!(coarse.values.iter().all(|v| v.is_none()));
        }
        assert_eq!(t[TIERS - 1].values.iter().filter(|v| v.is_some()).count(), 3);
    }

    /// Two tubes in a room at 120 CPM: two counts a second each.
    fn room(seconds: std::ops::Range<i64>, tubes: &[usize]) -> Vec<Arrival> {
        let mut v = Vec::new();
        for t in seconds {
            for &k in tubes {
                v.push(Arrival { who: k, when: t as f64 + 0.5 * k as f64, counts: 2 });
            }
        }
        v
    }

    #[test]
    fn a_tube_that_stops_is_out_of_the_mean_and_the_room_is_what_it_was() {
        let mut w = Windows::new(&[30.0]);
        for a in room(0..60, &[0, 1]) {
            w.add(a.when, a.counts);
        }
        // Both answering: the mean is the room, and so is the old figure.
        assert_eq!(w.mean(30.0), Some(120.0));
        assert_eq!(w.average(30.0).map(|v| v / 2.0), Some(120.0));
        // Tube 1 stops. Thirty seconds on, the window holds tube 0 alone.
        for a in room(60..100, &[0]) {
            w.add(a.when, a.counts);
        }
        assert_eq!(w.mean(30.0), Some(120.0));
        // What the division by the tube count made of the same window.
        assert_eq!(w.average(30.0).map(|v| v / 2.0), Some(60.0));
        // Half the arrivals behind it, so root two the error.
        assert_eq!(w.behind(30.0), Some((60, 30)));
        let one = w.sigma(30.0).unwrap();
        // And it comes back: the window fills with both again.
        for a in room(100..140, &[0, 1]) {
            w.add(a.when, a.counts);
        }
        assert_eq!(w.mean(30.0), Some(120.0));
        assert_eq!(w.behind(30.0), Some((120, 60)));
        let two = w.sigma(30.0).unwrap();
        assert!((one / two - 2f64.sqrt()).abs() < 1e-9, "{one} {two}");
    }

    #[test]
    fn a_window_a_tube_was_in_for_half_of_is_divided_by_what_was_measured() {
        let mut w = Windows::new(&[30.0]);
        for a in room(0..45, &[0, 1]) {
            w.add(a.when, a.counts);
        }
        for a in room(45..60, &[0]) {
            w.add(a.when, a.counts);
        }
        // The last thirty seconds: thirty samples of tube 0, and the sixteen
        // of tube 1 that fall in them -- its are stamped on the half second.
        assert_eq!(w.behind(30.0), Some((92, 46)));
        assert_eq!(w.mean(30.0), Some(120.0));
    }

    #[test]
    fn a_reader_that_missed_seconds_does_not_lower_the_room() {
        let mut w = Windows::new(&[30.0]);
        for a in room(0..90, &[0, 1]) {
            // Tube 1's reader drops every third second.
            if a.who == 1 && (a.when.floor() as i64) % 3 == 0 {
                continue;
            }
            w.add(a.when, a.counts);
        }
        assert_eq!(w.mean(30.0), Some(120.0));
    }

    #[test]
    fn the_strip_is_cut_by_the_clock() {
        let both = room(1000..1100, &[0, 1]);
        let s = tiers_arrivals(&both, &[true, true], 108);
        assert_eq!(s.live, 2);
        assert_eq!(s.tiers.len(), TIERS + 1);
        let fine = s.tiers.last().unwrap();
        assert_eq!((fine.columns, fine.seconds), (INTERLEAVE_SECONDS * 2, 0.5));
        // Taking turns, and every bar a reading.
        assert!(fine.values.iter().all(|v| *v == Some(2.0)));
        let turns: Vec<u8> = s.sources.iter().flatten().copied().collect();
        assert_eq!(turns, [0, 1, 0, 1, 0, 1, 0, 1]);
        let second = &s.tiers[TIERS - 1];
        assert_eq!(second.seconds, 1.0);
        assert!(second.values.iter().all(|v| *v == Some(2.0)));
    }

    #[test]
    fn a_tube_that_stops_leaves_the_interleave_and_the_seconds_stay_seconds() {
        let mut a = room(1000..1060, &[0, 1]);
        a.extend(room(1060..1100, &[0]));
        let s = tiers_arrivals(&a, &[true, false], 150);
        // One tube answering: nothing to interleave.
        assert_eq!((s.live, s.tiers.len()), (1, TIERS));
        assert!(s.sources.is_empty());
        let second = s.tiers.last().unwrap();
        assert_eq!((second.seconds, second.columns), (1.0, 25));
        // Twenty-five bars are the last twenty-five SECONDS, all of them
        // tube 0's alone, and the rate in each is the room's.
        assert!(second.values.iter().all(|v| *v == Some(2.0)));
        // The two-second tier reaches back across the moment it stopped:
        // the same rate either side, because a bar is a mean per tube-second.
        assert!(s.tiers[TIERS - 2].values.iter().all(|v| *v == Some(2.0)));
    }

    #[test]
    fn a_tube_that_comes_back_is_in_the_interleave_with_its_first_sample() {
        let mut a = room(1000..1060, &[0, 1]);
        a.extend(room(1060..1098, &[0]));
        a.extend(room(1098..1100, &[0, 1]));
        let s = tiers_arrivals(&a, &[true, true], 108);
        assert_eq!((s.live, s.tiers.len()), (2, TIERS + 1));
        let turns: Vec<u8> = s.sources.iter().flatten().copied().collect();
        // Four seconds of arrivals: two of tube 0 alone, two of both.
        assert_eq!(turns, [0, 0, 0, 1, 0, 1]);
        // And the bars it has not filled yet are empty, not nought.
        assert_eq!(s.tiers.last().unwrap().values.iter().filter(|v| v.is_none()).count(), 2);
    }

    #[test]
    fn a_second_nothing_measured_is_empty_and_not_nought() {
        let mut a = room(1000..1010, &[0]);
        a.extend(room(1020..1030, &[0]));
        let s = tiers_arrivals(&a, &[true], 180);
        let second = s.tiers.last().unwrap();
        let shown: Vec<Option<f64>> = second.values.iter().rev().take(30).rev().cloned().collect();
        assert_eq!(shown.iter().filter(|v| v.is_none()).count(), 10);
        assert_eq!(shown.iter().filter(|v| **v == Some(2.0)).count(), 20);
    }

    /// Counts a second with nothing periodic in them: a fixed scramble, so
    /// the test is the same test every time it runs.
    fn scramble(n: usize, seed: u64) -> Vec<u32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((x >> 33) % 4) as u32
            })
            .collect()
    }

    #[test]
    fn a_period_is_a_ridge_in_the_same_bin_of_every_row() {
        // Eight seconds: up for four, down for four, on top of the scramble.
        let swell = [3u32, 4, 3, 2, 1, 0, 1, 2];
        let mut w = Waterfall::new(256, 8, 48);
        for (t, c) in scramble(1024, 7).into_iter().enumerate() {
            w.add(c + swell[t % 8]);
        }
        // 256 seconds to the first row and one every eight after it.
        assert_eq!(w.made, 1 + (1024 - 256) / 8);
        let rows = w.rows();
        assert_eq!(rows.len(), 48);
        // 256 / 8 is the thirty-second line, which is index 31.
        assert_eq!(w.period(31), 8.0);
        let luck = w.chance_max() as f32;
        assert!((7.2..7.5).contains(&luck), "{}", luck);
        for (k, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), 127);
            let loudest = (0..row.len()).max_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap();
            assert_eq!(loudest, 31, "row {} is loudest somewhere else", k);
            assert!(row[31] > luck, "row {} reads {} there", k, row[31]);
        }
    }

    #[test]
    fn noise_makes_spikes_and_no_ridge() {
        let mut w = Waterfall::new(256, 8, 48);
        for c in scramble(1024, 11) {
            w.add(c);
        }
        let rows = w.rows();
        let luck = w.chance_max() as f32;
        // Flat reads 1.0: every row is divided by its own mean.
        for row in &rows {
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            assert!((mean - 1.0).abs() < 1e-4, "{}", mean);
        }
        // No bin is over the line in even a quarter of the rows.
        for bin in 0..127 {
            let over = rows.iter().filter(|r| r[bin] > luck).count();
            assert!(over < 12, "bin {} is over the line in {} rows of 48", bin, over);
        }
    }

    #[test]
    fn luck_draws_short_ridges_and_no_long_ones() {
        // Eleven hours of nothing periodic, and every row of it.
        let mut w = Waterfall::new(256, 8, 48);
        let luck = w.chance_max() as f32;
        let mut run = [0usize; 127];
        let (mut longest, mut ridges) = (0usize, 0usize);
        for c in scramble(40_000, 3) {
            if !w.add(c) {
                continue;
            }
            let rows = w.rows();
            for (bin, v) in rows[0].iter().enumerate() {
                if *v > luck {
                    run[bin] += 1;
                    longest = longest.max(run[bin]);
                } else {
                    ridges += (run[bin] > 0) as usize;
                    run[bin] = 0;
                }
            }
        }
        // It does draw them: a spike is in its neighbours' seconds too.
        assert!(ridges > 20, "only {} in eleven hours", ridges);
        assert!(longest > 1, "no spike outlived its row");
        // And none as long as half a window.
        assert_eq!(w.chance_rows(), 16);
        assert!(longest <= w.chance_rows(), "luck ran {} rows", longest);
    }

    #[test]
    fn the_waterfall_has_nothing_until_it_has_a_window() {
        let mut w = Waterfall::new(256, 8, 48);
        assert_eq!(w.wait(), 256);
        for _ in 0..255 {
            assert!(!w.add(1));
        }
        assert_eq!((w.wait(), w.rows().len()), (1, 0));
        // The first row is the second the window fills; a room that counts
        // the same every second has no spectrum, and the row is noughts.
        assert!(w.add(1));
        assert_eq!((w.wait(), w.age(), w.made), (0, 0, 1));
        assert!(w.rows()[0].iter().all(|v| *v == 0.0));
        for k in 1..8 {
            assert!(!w.add(1));
            assert_eq!(w.age(), k);
        }
        assert!(w.add(1));
        assert_eq!(w.rows().len(), 2);
    }

    #[test]
    fn the_trend_is_a_bar_and_its_neighbours() {
        let some = |v: &[f64]| v.iter().map(|x| Some(*x)).collect::<Vec<_>>();
        let t = vec![
            Tier { columns: 3, seconds: 2.0, values: some(&[1.0, 4.0, 1.0]) },
            Tier { columns: 3, seconds: 1.0, values: some(&[4.0, 0.0, 6.0]) },
            Tier { columns: 2, seconds: 0.5, values: some(&[9.0, 9.0]) },
        ];
        let line = trend_over(&t, 1);
        assert_eq!(line.iter().map(|l| l.len()).collect::<Vec<_>>(), [3, 3, 2]);
        // The first bar has one neighbour, the second two.
        assert_eq!(line[0][0], Some(2.5));
        assert_eq!(line[0][1], Some(2.0));
        // ACROSS THE BOUNDARY, by the seconds each bar covers: two bars of
        // two seconds and one of one are 2*4 + 2*1 + 1*4 counts in five.
        assert_eq!(line[0][2], Some(14.0 / 5.0));
        // And from the other side of it: 2*1 + 4 + 0 counts in four.
        assert_eq!(line[1][0], Some(1.5));
        assert_eq!(line[1][1], Some(10.0 / 3.0));
        // The newest bar does not reach into the interleave for a
        // neighbour, and the interleave has no line.
        assert_eq!(line[1][2], Some(3.0));
        assert_eq!(line[2], [None, None]);
    }

    #[test]
    fn the_trend_is_empty_where_nothing_was_measured() {
        let t = vec![Tier {
            columns: 5,
            seconds: 1.0,
            values: vec![None, Some(3.0), None, Some(1.0), Some(5.0)],
        }];
        // An empty bar has no trend, and is not a nought in its
        // neighbour's: the bar beside two empty ones is itself.
        assert_eq!(trend_over(&t, 1)[0], [None, Some(3.0), None, Some(3.0), Some(3.0)]);
        assert!(trend(&[]).is_empty());
    }

    #[test]
    fn a_height_is_a_logarithm_against_the_loudest_in_view() {
        assert_eq!(gain(0.0, 40.0), 0.0);
        assert_eq!(gain(40.0, 40.0), 1.0);
        // Over the top is the top, and nothing is less than nothing.
        assert_eq!(gain(400.0, 40.0), 1.0);
        assert_eq!(gain(-1.0, 40.0), 0.0);
        assert_eq!(gain(1.0, 0.0), 0.0);
        assert_eq!(gain(f64::NAN, 40.0), 0.0);
        // What is flat can be seen beside something forty times it, which
        // in proportion it could not: a fortieth of sixty pixels is one.
        assert!((gain(1.0, 40.0) - 0.1867).abs() < 1e-3);
        assert!((gain(7.5, 40.0) - 0.5763).abs() < 1e-3);
        // The louder is the taller, all the way up.
        let mut last = 0.0;
        for k in 1..=400 {
            let g = gain(k as f64 / 10.0, 40.0);
            assert!(g > last, "{} is not taller than the one before", k);
            last = g;
        }
        // And every doubling is the same step, once it is well over one.
        let step = |v: f64| gain(2.0 * v, 1000.0) - gain(v, 1000.0);
        assert!((step(50.0) - step(200.0)).abs() < 0.002);
    }

    #[test]
    fn the_trend_is_of_nine_bars() {
        // One bar of nine counts a second in a run of noughts: the line
        // stands at one for the nine bars that can see it, and at nought
        // for the rest. Near an end there are fewer bars to share it.
        let mut values = vec![Some(0.0); 21];
        values[10] = Some(9.0);
        let line = &trend(&[Tier { columns: 21, seconds: 1.0, values }])[0];
        assert_eq!(TREND_SIDE, 4);
        for (i, v) in line.iter().enumerate() {
            let near = (6..=14).contains(&i);
            assert_eq!(*v, Some(if near { 1.0 } else { 0.0 }), "bar {}", i);
        }
        let mut values = vec![Some(0.0); 21];
        values[0] = Some(9.0);
        let line = &trend(&[Tier { columns: 21, seconds: 1.0, values }])[0];
        assert_eq!(line[0], Some(9.0 / 5.0));
        assert_eq!(line[4], Some(1.0));
        assert_eq!(line[5], Some(0.0));
    }

    #[test]
    fn a_bar_is_said_to_be_when_it_is() {
        // Each second's count is its own number, so a bar's mean says
        // which seconds it holds; and those are the seconds it is said
        // to hold.
        let both: Vec<Arrival> = (1000..1400)
            .flat_map(|t| {
                [0usize, 1].map(|who| Arrival { who, when: t as f64 + 0.25 * who as f64, counts: t as u32 })
            })
            .collect();
        let s = tiers_arrivals(&both, &[true, true], 128);
        let end = 1400 - INTERLEAVE_SECONDS as i64;
        let spans = bar_spans(&s.tiers, end);
        assert_eq!(spans.len(), s.tiers.len());
        assert!(spans.last().unwrap().is_empty(), "the interleave has no spans");
        let mut bars = 0;
        for (tier, at) in s.tiers.iter().zip(&spans).filter(|(t, _)| t.seconds >= 1.0) {
            assert_eq!(tier.values.len(), at.len());
            for (v, (from, to)) in tier.values.iter().zip(at) {
                let Some(v) = v else { continue };
                assert!(to > from && (to - from) as f64 <= tier.seconds);
                // The oldest bar with anything in it begins before the
                // first count did: it is the mean of what it has.
                let from = (*from).max(1000);
                let mean = (from..*to).map(|t| t as f64).sum::<f64>() / (to - from) as f64;
                assert_eq!(*v, mean, "{}s bar said to be {}..{}", tier.seconds, from, to);
                bars += 1;
            }
        }
        assert!(bars > 80, "only {} bars were checked", bars);
        // The newest second bar is the second before the interleave.
        assert_eq!(spans[TIERS - 1].last(), Some(&(end - 1, end)));
        // And one tube's strip, which is cut in samples.
        let one: Vec<u32> = (0..300).collect();
        let t = tiers(&one, 0, 60, 60);
        for (tier, at) in t.iter().zip(bar_spans(&t, 300)) {
            for (v, (from, to)) in tier.values.iter().zip(at) {
                let Some(v) = v else { continue };
                let mean = (from..to).map(|t| t as f64).sum::<f64>() / (to - from) as f64;
                assert_eq!(*v, mean);
            }
        }
    }

    /// Counts that arrive by chance at `rate` a second: Knuth's draw, from
    /// the same fixed scramble.
    fn by_chance(n: usize, rate: f64, seed: u64) -> Vec<u32> {
        let mut x = seed;
        let mut unit = move || {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        let limit = (-rate).exp();
        (0..n)
            .map(|_| {
                let (mut k, mut p) = (0u32, unit());
                while p > limit {
                    k += 1;
                    p *= unit();
                }
                k
            })
            .collect()
    }

    #[test]
    fn a_row_is_held_against_the_long_average() {
        // Fifty minutes of a room at 0.9 a second, then five at twice it.
        let mut counts = by_chance(3000, 0.9, 21);
        counts.extend(by_chance(300, 1.8, 22));
        let mean = |row: &[f32]| row.iter().sum::<f32>() / row.len() as f32;
        let (mut own, mut long) = (Waterfall::new(300, 10, 48), Waterfall::leveled(300, 10, 48, 3000));
        assert_eq!((own.level(), long.level()), (None, None));
        let mut before = Vec::new();
        for (t, c) in counts.iter().enumerate() {
            own.add(*c);
            if long.add(*c) && t < 3000 {
                before.push(mean(&long.rows()[0]));
            }
        }
        // While the room was as it had been, flat read 1.0: not row by row,
        // which is what dividing by its own mean is for, but over them.
        let usual = before.iter().sum::<f32>() / before.len() as f32;
        assert!((usual - 1.0).abs() < 0.05, "flat read {}", usual);
        assert!(before.iter().all(|m| (0.6..1.5).contains(m)), "{:?}", before);
        // The five minutes at twice the rate stand twice as high, against
        // an average they have moved by a tenth; and against themselves
        // they are as flat as every other row.
        let level = long.level().unwrap();
        assert!((0.9..1.1).contains(&level), "the long average is {}", level);
        let raised = mean(&long.rows()[0]);
        assert!((1.6..2.2).contains(&raised), "the raised row reads {}", raised);
        assert!((mean(&own.rows()[0]) - 1.0).abs() < 1e-4);
        // A window is the least it is an average of.
        assert_eq!(Waterfall::leveled(300, 10, 48, 60).over, 300);
    }

    #[test]
    fn a_row_reaches_as_far_as_its_longest_window() {
        let mut t = Trail::new(&[4096, 300, 512], 10, 48, 3000);
        // Every bin of the shortest; of the others, what is longer than
        // the one before could hold: 512 itself, and 4096 down to 585.
        assert_eq!(t.bins(), 149 + 1 + 7);
        assert_eq!((t.newest(), t.rows().len()), (None, 0));
        // A room at 0.9 a second, with a swell every 1024 seconds in it.
        let counts: Vec<u32> = by_chance(6000, 0.9, 31)
            .into_iter()
            .enumerate()
            .map(|(i, c)| c + if i % 1024 < 512 { 1 } else { 0 })
            .collect();
        for c in &counts[..299] {
            t.add(*c);
        }
        assert_eq!(t.newest(), None);
        t.add(counts[299]);
        assert_eq!((t.newest(), t.age()), (Some(300), 0));
        // Five minutes in, a row is five minutes wide.
        let early = t.rows();
        assert_eq!((early.len(), early[0].bins.len()), (1, 149));
        assert_eq!(early[0].bins[0].0, 300.0);
        // Five minutes is all it has counted, so the row is as the room
        // has been: they are the same seconds.
        assert_eq!(early[0].level, 1.0);
        for c in &counts[300..] {
            t.add(*c);
        }
        assert_eq!((t.newest(), t.age()), (Some(6000), 0));
        let rows = t.rows();
        assert_eq!(rows.len(), 48);
        for row in &rows {
            // The swell is up for eight minutes and down for eight, so
            // five minutes of it are over the long average or under.
            assert!((0.5..1.6).contains(&row.level), "the row stands at {}", row.level);
            let peaks = row.peaks(t.chance_max() as f32);
            assert!(peaks.iter().any(|i| row.bins[*i].0 == 1024.0), "the swell is not a peak");
            let row = &row.bins;
            assert_eq!(row.len(), 157);
            // From the longest period to the shortest, and none twice.
            assert_eq!(row[0].0, 4096.0);
            assert!(row.windows(2).all(|p| p[0].0 > p[1].0), "the periods are not in order");
            assert!((row[156].0 - 300.0 / 149.0).abs() < 1e-4);
            // The swell is the loudest thing out where it is, and is over
            // the line: 4096 / 4.
            let far = &row[..7];
            let loudest = far.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            assert_eq!(loudest.0, 1024.0);
            assert!(loudest.1 as f64 > t.chance_max(), "it reads {}", loudest.1);
        }
        // Asked for every ten seconds or once at the end, a row is the row.
        let mut often = Trail::new(&[4096, 300, 512], 10, 48, 3000);
        for (i, c) in counts.iter().enumerate() {
            often.add(*c);
            if i % 10 == 9 {
                often.rows();
            }
        }
        assert_eq!(often.rows(), rows);
        // And between rows it says how long since the last.
        often.add(1);
        often.add(1);
        assert_eq!((often.newest(), often.age()), (Some(6000), 2));
        assert_eq!(often.rows(), rows);
    }

    #[test]
    fn a_row_stands_as_high_as_the_room_counted() {
        // Fifty minutes at 0.9 a second and five at thirty times it.
        let mut counts = by_chance(3000, 0.9, 51);
        counts.extend(by_chance(300, 27.0, 52));
        let mut t = Trail::new(&[300], 10, 48, 3000);
        for c in &counts[..3000] {
            t.add(*c);
        }
        let before = t.rows();
        assert!(before.iter().all(|r| (0.8..1.2).contains(&r.level)));
        for c in &counts[3000..] {
            t.add(*c);
        }
        let after = t.rows();
        // The long average has the five minutes in it, a tenth of it, so
        // thirty times the room is not thirty times the average: 27 over
        // (0.9 * 2700 + 27 * 300) / 3000 is 7.7.
        assert!((7.0..8.4).contains(&after[0].level), "it stands at {}", after[0].level);
        // And the row of five minutes before is as it was.
        assert!((0.8..1.2).contains(&after[30].level), "{}", after[30].level);
        // The whole of the raised row is up: by chance alone, every bin of
        // it is what the rate is.
        let mean = after[0].bins.iter().map(|b| b.1).sum::<f32>() / 149.0;
        assert!((6.0..10.0).contains(&mean), "its bins average {}", mean);
    }

    #[test]
    fn a_peak_stands_over_its_neighbours_and_over_the_floor() {
        let row = |powers: &[f32]| TrailRow {
            level: 1.0,
            bins: powers.iter().enumerate().map(|(i, p)| (100.0 - i as f32, *p)).collect(),
        };
        assert_eq!(row(&[1.0, 9.0, 2.0, 8.5, 8.0, 1.0, 3.0]).peaks(7.5), [1, 3]);
        // Under the floor is not a peak, however it stands over its own.
        assert_eq!(row(&[1.0, 5.0, 1.0]).peaks(7.5), Vec::<usize>::new());
        // An end is one, and a level top is one peak and not two.
        assert_eq!(row(&[9.0, 1.0, 1.0, 8.0]).peaks(7.5), [0, 3]);
        assert_eq!(row(&[1.0, 8.0, 8.0, 1.0]).peaks(7.5), [1]);
        assert_eq!(row(&[]).peaks(7.5), Vec::<usize>::new());
    }

    #[test]
    fn a_height_adjusts_itself_to_what_is_loudest() {
        // Something two thousand times the mean is the top of the scale
        // and is all of it; what is flat beside it can still be seen, and
        // what luck reaches is more than a quarter of the way.
        assert_eq!(gain(2000.0, 2000.0), 1.0);
        assert!((gain(1.0, 2000.0) - 0.0912).abs() < 1e-3);
        assert!((gain(7.59, 2000.0) - 0.2829).abs() < 1e-3);
        // In proportion, flat would be a twentieth of a pixel in a hundred.
        assert!(1.0 / 2000.0 * 100.0 < 0.06);
        // And when it has gone, the scale is what is left.
        assert!((gain(1.0, 7.59) - 0.3224).abs() < 1e-3);
    }

    #[test]
    fn the_short_end_of_a_row_is_the_waterfalls_row() {
        let counts = by_chance(3600, 0.9, 41);
        let mut fall = Waterfall::leveled(300, 10, 48, 3000);
        let mut trail = Trail::new(&[300], 10, 48, 3000);
        for c in &counts {
            fall.add(*c);
            trail.add(*c);
        }
        let (a, b) = (fall.rows(), trail.rows());
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.len(), y.bins.len());
            for (k, (p, (period, q))) in x.iter().zip(&y.bins).enumerate() {
                assert_eq!(*period, (300.0 / (k + 1) as f64) as f32);
                assert!((p - q).abs() <= 1e-4 * p.max(1.0), "{} and {}", p, q);
            }
        }
    }

    #[test]
    fn a_window_of_any_length_is_transformed() {
        // Where both can be asked, the sum and the transform agree.
        let x: Vec<f64> = scramble(256, 5).into_iter().map(|c| c as f64 - 1.5).collect();
        let by_fft = powers(&x);
        let by_sum: Vec<f64> = (1..128)
            .map(|k| {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (i, v) in x.iter().enumerate() {
                    let a = 2.0 * std::f64::consts::PI * (k * i) as f64 / 256.0;
                    re += v * a.cos();
                    im -= v * a.sin();
                }
                re * re + im * im
            })
            .collect();
        assert_eq!(by_fft.len(), 127);
        for (a, b) in by_fft.iter().zip(&by_sum) {
            assert!((a - b).abs() <= 1e-6 * a.max(1.0), "{} and {}", a, b);
        }
        // Three hundred seconds, which is not a power of two: 149 bins,
        // and a swell every ten seconds is the thirtieth of them.
        let swell = [3u32, 4, 4, 3, 2, 1, 0, 0, 1, 2];
        let mut w = Waterfall::new(300, 10, 48);
        for (t, c) in scramble(1200, 7).into_iter().enumerate() {
            w.add(c + swell[t % 10]);
        }
        assert_eq!(w.made, 1 + (1200 - 300) / 10);
        assert_eq!((w.period(29), w.chance_rows()), (10.0, 15));
        let luck = w.chance_max() as f32;
        assert!((7.4..7.6).contains(&luck), "{}", luck);
        for (k, row) in w.rows().iter().enumerate() {
            assert_eq!(row.len(), 149);
            let loudest = (0..row.len()).max_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap();
            assert_eq!(loudest, 29, "row {} is loudest somewhere else", k);
            assert!(row[29] > luck);
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            assert!((mean - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn a_tube_is_answering_until_it_is_quiet_or_gone() {
        let heard = [Some(100.0), Some(96.0), None, Some(100.0)];
        let gone = [false, false, false, true];
        assert_eq!(answering(&heard, &gone, 100.5), [true, false, false, false]);
    }
}
